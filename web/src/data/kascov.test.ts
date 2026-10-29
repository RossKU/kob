import { createHash } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import {
  createIconLoader, createKascovClient, fetchVerifiedImage, generatedAtOf, iconSources, kascovImagesBase, kascovImageUrl, kascovNetwork, kascovTokensUrl,
  KASCOV_BACKOFF_MAX_MS, KASCOV_REFRESH_MS, parseKascovToken, parseKascovTokens, readCapped, safeImageUrl, sha256Hex, sniffImage,
} from './kascov';

const ID = (c: string) => c.repeat(64);
const sha = (b: Uint8Array) => createHash('sha256').update(b).digest('hex');

// a 1x1 PNG, a JPEG header, a WebP header, an SVG
const PNG = Uint8Array.from(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==', 'base64'));
const JPEG = Uint8Array.from([0xff, 0xd8, 0xff, 0xe0, 0, 16, 0x4a, 0x46, 0x49, 0x46, 0, 1]);
const WEBP = Uint8Array.from([0x52, 0x49, 0x46, 0x46, 0, 0, 0, 0, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x20]);
const SVG = new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>');

const entry = (over: Record<string, unknown> = {}) => ({
  covenant_id: ID('a'), claimed_ticker: 'ROCKET', claimed_name: 'Rocket Token', claimed_decimals: 3,
  claimed_image: 'https://storage.googleapis.com/bucket/a.png', claimed_image_hash: sha(PNG),
  standard: { view_template_hash: ID('b'), template_hash: ID('c'), review: 'reviewed' }, status: 'verified', alive: true, ...over,
});
const doc = (tokens: unknown[], over: Record<string, unknown> = {}) => ({ network: 'testnet-10', generated_at_ms: 1_000, tokens, ...over });

const json = (body: unknown, init: ResponseInit = {}) => new Response(typeof body === 'string' ? body : JSON.stringify(body), { status: 200, ...init });

describe('urls', () => {
  it('data and images of the default base; a base with a path or a trailing slash; img is beside the last segment', () => {
    expect(kascovTokensUrl('mainnet')).toBe('https://kascov.io/data/mainnet/tokens.json');
    expect(kascovTokensUrl('testnet-10', 'https://names.example.org/kascov/data/')).toBe('https://names.example.org/kascov/data/testnet-10/tokens.json');
    expect(kascovImagesBase()).toBe('https://kascov.io/img');
    expect(kascovImagesBase('https://names.example.org/kascov/data/')).toBe('https://names.example.org/kascov/img');
    expect(kascovImageUrl('mainnet', ID('a'))).toBe(`https://kascov.io/img/mainnet/${ID('a')}`);
  });
  it('only the two networks kascov serves', () => {
    expect(kascovNetwork('mainnet')).toBe('mainnet');
    expect(kascovNetwork('testnet-10')).toBe('testnet-10');
    expect(kascovNetwork('testnet-11')).toBeNull();
    expect(kascovNetwork(undefined)).toBeNull();
  });
});

describe('safeImageUrl', () => {
  it('https only, no credentials', () => {
    expect(safeImageUrl('https://storage.googleapis.com/x/y.png')).toBe('https://storage.googleapis.com/x/y.png');
    for (const bad of ['http://x.example/a.png', 'ipfs://Qm123', 'data:image/png;base64,AAAA', 'javascript:alert(1)', 'https://u:p@x.example/a.png', 'not a url', '', 42, null, `https://x.example/${'a'.repeat(3000)}`]) {
      expect(safeImageUrl(bad), String(bad)).toBeNull();
    }
  });
});

describe('parseKascovToken', () => {
  it('reads the claims, the program hash and whether kascov marks the token verified, nothing about review or alive', () => {
    const t = parseKascovToken(entry());
    expect(t).toEqual({
      covenantId: ID('a'), ticker: 'ROCKET', name: 'Rocket Token', decimals: 3, templateHash: ID('b'), imageHash: sha(PNG), imageUrl: 'https://storage.googleapis.com/bucket/a.png',
      verified: true,
    });
    // review / alive change nothing; status only flips the third-party remark (kascov's `verified`, no other word counts)
    expect(parseKascovToken(entry({ alive: false, standard: { view_template_hash: ID('b'), review: 'rejected' } }))).toEqual(t);
    for (const status of ['invalid', 'unvalidated', 'active', undefined, 7]) expect(parseKascovToken(entry({ status }))!.verified, String(status)).toBe(false);
    expect(parseKascovToken(entry({ status: 'Verified' }))!.verified).toBe(false);
  });

  it('restricts every string (control and bidi characters become U+FFFD, length capped), normalises ids and hashes to lower case', () => {
    const t = parseKascovToken(entry({ covenant_id: ID('A'), claimed_ticker: 'AB‮CD\u0000', claimed_name: `${'N'.repeat(100)}`, claimed_image_hash: sha(PNG).toUpperCase() }))!;
    expect(t.covenantId).toBe(ID('a'));
    expect(t.ticker).toBe('AB�CD�');
    expect(t.name!.length).toBeLessThanOrEqual(65);
    expect(t.imageHash).toBe(sha(PNG));
  });

  it('missing, empty or wrongly typed fields are null; an image URL without a hash is dropped (nothing could be checked); bad decimals are null', () => {
    const t = parseKascovToken({ covenant_id: ID('a'), claimed_ticker: 7, claimed_name: '   ', claimed_decimals: 2.5, claimed_image: 'https://x.example/a.png', standard: 'weird' })!;
    expect(t).toEqual({ covenantId: ID('a'), ticker: null, name: null, decimals: null, templateHash: null, imageHash: null, imageUrl: null, verified: false });
    expect(parseKascovToken(entry({ claimed_decimals: 19 }))!.decimals).toBeNull();
    expect(parseKascovToken(entry({ claimed_decimals: 0 }))!.decimals).toBe(0);
    expect(parseKascovToken(entry({ claimed_image: 'http://x.example/a.png' }))!.imageUrl).toBeNull();
    expect(parseKascovToken(entry({ claimed_image_hash: 'zz' }))!.imageHash).toBeNull();
    expect(parseKascovToken(entry({ standard: { view_template_hash: 'abc' } }))!.templateHash).toBeNull();
  });

  it('an entry without a usable covenant id is nothing', () => {
    for (const bad of [null, 7, 'x', [], {}, { covenant_id: 'abc' }, { covenant_id: ID('g') }]) expect(parseKascovToken(bad)).toBeNull();
  });
});

describe('parseKascovTokens', () => {
  it('a bad entry is skipped, a repeated id keeps the first', () => {
    const out = parseKascovTokens(doc([entry(), { nope: 1 }, entry({ covenant_id: ID('d') }), entry({ claimed_ticker: 'DUP' })]), 'testnet-10')!;
    expect(out.map((t) => t.covenantId)).toEqual([ID('a'), ID('d')]);
    expect(out[0]!.ticker).toBe('ROCKET');
  });
  it('an empty list is a list', () => {
    expect(parseKascovTokens(doc([]), 'testnet-10')).toEqual([]);
  });
  it('a changed shape is null: no tokens array, a non-object root, all entries unusable, a response for another network', () => {
    for (const bad of [null, [], 'x', 7, {}, { tokens: {} }, { tokens: 'x' }, doc([{ a: 1 }, null]), doc([entry()], { network: 'mainnet' })]) {
      expect(parseKascovTokens(bad, 'testnet-10'), JSON.stringify(bad)?.slice(0, 40)).toBeNull();
    }
    // no `network` field at all is tolerated
    expect(parseKascovTokens({ tokens: [entry()] }, 'testnet-10')).toHaveLength(1);
  });
  it('generated_at_ms: a plain positive integer, else null', () => {
    expect(generatedAtOf({ generated_at_ms: 1793106504574 })).toBe(1793106504574);
    for (const bad of [{}, { generated_at_ms: '1' }, { generated_at_ms: -1 }, { generated_at_ms: 1.5 }, null]) expect(generatedAtOf(bad)).toBeNull();
  });
});

describe('createKascovClient', () => {
  const clock = () => {
    let t = 1_000_000;
    return { now: () => t, advance: (ms: number) => void (t += ms) };
  };

  it('fetches the network\'s file once, shares one request between callers and reuses the answer for the refresh period', async () => {
    const c = clock();
    const fetch = vi.fn(async (_url: unknown, _init?: RequestInit) => json(doc([entry()])));
    const k = createKascovClient({ fetch: fetch as unknown as typeof globalThis.fetch, now: c.now });
    const [a, b] = await Promise.all([k.load('testnet-10'), k.load('testnet-10')]);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch.mock.calls[0]![0]).toBe('https://kascov.io/data/testnet-10/tokens.json');
    expect(fetch.mock.calls[0]![1]).toMatchObject({ credentials: 'omit', referrerPolicy: 'no-referrer' });
    expect(a).toBe(b);
    expect(a.ok && a.tokens).toHaveLength(1);
    c.advance(KASCOV_REFRESH_MS - 1);
    await k.load('testnet-10');
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(k.nextInMs('testnet-10')).toBe(1_000);
    c.advance(1);
    await k.load('testnet-10');
    expect(fetch).toHaveBeenCalledTimes(2);
    // another base or network is its own entry
    await k.load('testnet-10', { base: 'https://names.example.org/data' });
    expect(fetch.mock.calls[2]![0]).toBe('https://names.example.org/data/testnet-10/tokens.json');
  });

  it('the refresh period is 2 minutes', () => {
    expect(KASCOV_REFRESH_MS).toBe(120_000);
  });

  it('a file kascov did not rebuild (same generated_at_ms) keeps the parsed list: the same object', async () => {
    const c = clock();
    let n = 0;
    const fetch = vi.fn(async () => json(doc([entry()], { generated_at_ms: n++ < 2 ? 1_000 : 2_000 })));
    const k = createKascovClient({ fetch: fetch as unknown as typeof globalThis.fetch, now: c.now });
    const first = await k.load('testnet-10');
    c.advance(KASCOV_REFRESH_MS);
    const second = await k.load('testnet-10');
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(second).toBe(first);
    c.advance(KASCOV_REFRESH_MS);
    const third = await k.load('testnet-10');
    expect(third).not.toBe(first);
    expect(third.ok && third.generatedAtMs).toBe(2_000);
  });

  it('a network kascov does not serve, or an empty base (the feature off), makes no request', async () => {
    const fetch = vi.fn();
    const k = createKascovClient({ fetch: fetch as unknown as typeof globalThis.fetch });
    expect(await k.load('testnet-11')).toEqual({ ok: false, reason: 'unsupported' });
    expect(await k.load('mainnet', { base: '' })).toEqual({ ok: false, reason: 'unsupported' });
    expect(fetch).not.toHaveBeenCalled();
  });

  describe('failures are results, never throws', () => {
    const failing = async (impl: () => Promise<Response>, timeoutMs?: number) => {
      const k = createKascovClient({ fetch: impl as unknown as typeof globalThis.fetch, ...(timeoutMs ? { timeoutMs } : {}) });
      return k.load('testnet-10');
    };
    it('unreachable', async () => expect(await failing(async () => Promise.reject(new TypeError('Failed to fetch')))).toEqual({ ok: false, reason: 'network' }));
    it('an HTTP error', async () => expect(await failing(async () => json('x', { status: 503 }))).toEqual({ ok: false, reason: 'http' }));
    it('not JSON', async () => expect(await failing(async () => json('<html>nope</html>'))).toEqual({ ok: false, reason: 'shape' }));
    it('a changed shape', async () => expect(await failing(async () => json({ tokens: { a: 1 } }))).toEqual({ ok: false, reason: 'shape' }));
    it('another network\'s file', async () => expect(await failing(async () => json(doc([entry()], { network: 'mainnet' })))).toEqual({ ok: false, reason: 'shape' }));
    it('too large (declared and streamed)', async () => {
      const k = createKascovClient({ fetch: (async () => json(doc([entry()]), { headers: { 'content-length': '99999999' } })) as unknown as typeof globalThis.fetch, maxBytes: 1_000 });
      expect(await k.load('testnet-10')).toEqual({ ok: false, reason: 'too-large' });
      const k2 = createKascovClient({ fetch: (async () => json(doc(Array.from({ length: 50 }, (_, i) => entry({ covenant_id: String(i).padStart(64, '0') }))))) as unknown as typeof globalThis.fetch, maxBytes: 1_000 });
      expect(await k2.load('testnet-10')).toEqual({ ok: false, reason: 'too-large' });
    });
    it('a slow phone link: an answer that takes 20 s is still taken (the default timeout is 30 s)', async () => {
      vi.useFakeTimers();
      try {
        const slowOk = (_u: unknown, init?: RequestInit) => new Promise<Response>((res, rej) => {
          init?.signal?.addEventListener('abort', () => rej(new DOMException('aborted', 'AbortError')));
          setTimeout(() => res(json(doc([entry()]))), 20_000);
        });
        const k = createKascovClient({ fetch: slowOk as unknown as typeof globalThis.fetch });
        const p = k.load('testnet-10');
        await vi.advanceTimersByTimeAsync(20_001);
        expect((await p).ok).toBe(true);
        const never = (_u: unknown, init?: RequestInit) => new Promise<Response>((_r, rej) => init?.signal?.addEventListener('abort', () => rej(new DOMException('aborted', 'AbortError'))));
        const k2 = createKascovClient({ fetch: never as unknown as typeof globalThis.fetch });
        const p2 = k2.load('testnet-10');
        await vi.advanceTimersByTimeAsync(30_001);
        expect(await p2).toEqual({ ok: false, reason: 'timeout' });
      } finally {
        vi.useRealTimers();
      }
    });
    it('slow: the request is cut after the timeout', async () => {
      const slow = (_u: unknown, init?: RequestInit) => new Promise<Response>((_res, rej) => init?.signal?.addEventListener('abort', () => rej(new DOMException('aborted', 'AbortError'))));
      expect(await failing(slow as unknown as () => Promise<Response>, 20)).toEqual({ ok: false, reason: 'timeout' });
    });
    it('a body that stalls after the headers is cut as well', async () => {
      const stall = (_u: unknown, init?: RequestInit) =>
        Promise.resolve(new Response(new ReadableStream({ start(ctl) { init?.signal?.addEventListener('abort', () => ctl.error(new DOMException('aborted', 'AbortError'))); } })));
      expect(await failing(stall as unknown as () => Promise<Response>, 20)).toEqual({ ok: false, reason: 'timeout' });
    });
  });

  it('after a failure the next try waits 30 s, then 60 s, doubling to 10 minutes; a success resets it', async () => {
    const c = clock();
    let up = false;
    const fetch = vi.fn(async () => (up ? json(doc([entry()])) : Promise.reject(new TypeError('down'))));
    const k = createKascovClient({ fetch: fetch as unknown as typeof globalThis.fetch, now: c.now });
    const waits: number[] = [];
    for (let i = 0; i < 8; i++) {
      await k.load('testnet-10');
      waits.push(k.nextInMs('testnet-10'));
      // a call inside the wait does not fetch
      const calls = fetch.mock.calls.length;
      await k.load('testnet-10');
      expect(fetch.mock.calls.length).toBe(calls);
      c.advance(waits.at(-1)!);
    }
    expect(waits).toEqual([30_000, 60_000, 120_000, 240_000, 480_000, KASCOV_BACKOFF_MAX_MS, KASCOV_BACKOFF_MAX_MS, KASCOV_BACKOFF_MAX_MS]);
    up = true;
    const r = await k.load('testnet-10');
    expect(r.ok).toBe(true);
    expect(k.nextInMs('testnet-10')).toBe(KASCOV_REFRESH_MS);
  });

  it('a failure after a success keeps serving the earlier list (names do not change)', async () => {
    const c = clock();
    let up = true;
    const fetch = vi.fn(async () => (up ? json(doc([entry()])) : Promise.reject(new TypeError('down'))));
    const k = createKascovClient({ fetch: fetch as unknown as typeof globalThis.fetch, now: c.now });
    const first = await k.load('testnet-10');
    up = false;
    c.advance(KASCOV_REFRESH_MS);
    const again = await k.load('testnet-10');
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(again).toBe(first);
    expect(k.nextInMs('testnet-10')).toBe(30_000);
  });

  it('an aborted signal ends only that caller\'s wait; the shared request still fills the cache', async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const fetch = vi.fn(async () => {
      await gate;
      return json(doc([entry()]));
    });
    const k = createKascovClient({ fetch: fetch as unknown as typeof globalThis.fetch });
    const ctl = new AbortController();
    const waiting = k.load('testnet-10', { signal: ctl.signal });
    ctl.abort();
    expect(await waiting).toEqual({ ok: false, reason: 'network' });
    release();
    const later = await k.load('testnet-10');
    expect(later.ok).toBe(true);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(await k.load('testnet-10', { signal: ctl.signal })).toEqual({ ok: true, tokens: expect.any(Array), generatedAtMs: 1_000 });
  });
});

describe('readCapped', () => {
  it('reads a body within the cap, refuses a larger one by its header or its bytes', async () => {
    expect(await readCapped(new Response('abc'), 10)).toEqual(new TextEncoder().encode('abc'));
    expect(await readCapped(new Response('abcdefghij'), 5)).toBeNull();
    expect(await readCapped(new Response('abc', { headers: { 'content-length': '1000' } }), 5)).toBeNull();
  });
});

describe('sniffImage', () => {
  it('raster formats by magic bytes; SVG, HTML, text and short input are refused', () => {
    expect(sniffImage(PNG)).toBe('image/png');
    expect(sniffImage(JPEG)).toBe('image/jpeg');
    expect(sniffImage(WEBP)).toBe('image/webp');
    expect(sniffImage(new TextEncoder().encode('GIF89a......'))).toBe('image/gif');
    expect(sniffImage(SVG)).toBeNull();
    expect(sniffImage(new TextEncoder().encode('<html>'))).toBeNull();
    expect(sniffImage(new Uint8Array())).toBeNull();
    expect(sniffImage(PNG.slice(0, 5))).toBeNull();
  });
});

describe('sha256Hex', () => {
  it('is the SHA-256 of the raw bytes, lower-case hex (what claimed_image_hash is)', async () => {
    expect(await sha256Hex(PNG)).toBe(sha(PNG));
    expect(await sha256Hex(new Uint8Array())).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
  });
});

describe('fetchVerifiedImage', () => {
  const bytesResponse = (b: Uint8Array, init: ResponseInit = {}) => new Response(b as unknown as BodyInit, { status: 200, ...init });
  const U1 = 'https://kascov.io/img/testnet-10/x';
  const U2 = 'https://storage.googleapis.com/bucket/a.png';

  it('shows an image whose bytes hash to the committed hash', async () => {
    const fetch = vi.fn(async (_u: unknown, _init?: RequestInit) => bytesResponse(PNG));
    const img = await fetchVerifiedImage([U1], sha(PNG), { fetch: fetch as unknown as typeof globalThis.fetch });
    expect(img).toMatchObject({ mime: 'image/png', url: U1 });
    expect(img!.bytes).toEqual(PNG);
    expect(fetch.mock.calls[0]![1]).toMatchObject({ credentials: 'omit', referrerPolicy: 'no-referrer' });
  });

  it('a hash mismatch is no icon, and the next source is tried', async () => {
    const other = Uint8Array.from([...PNG.slice(0, PNG.length - 1), 0]);
    const fetch = vi.fn(async (u: unknown) => bytesResponse(u === U1 ? other : PNG));
    const img = await fetchVerifiedImage([U1, U2], sha(PNG), { fetch: fetch as unknown as typeof globalThis.fetch });
    expect(img!.url).toBe(U2);
    expect(await fetchVerifiedImage([U1], sha(PNG), { fetch: (async () => bytesResponse(other)) as unknown as typeof globalThis.fetch })).toBeNull();
  });

  it('a host without CORS (the fetch is refused), a 404, a timeout and an oversize file are no icon', async () => {
    const refused = vi.fn(async () => Promise.reject(new TypeError('Failed to fetch')));
    expect(await fetchVerifiedImage([U2], sha(PNG), { fetch: refused as unknown as typeof globalThis.fetch })).toBeNull();
    expect(await fetchVerifiedImage([U1], sha(PNG), { fetch: (async () => new Response('', { status: 404 })) as unknown as typeof globalThis.fetch })).toBeNull();
    const slow = (_u: unknown, init?: RequestInit) => new Promise<Response>((_r, rej) => init?.signal?.addEventListener('abort', () => rej(new DOMException('x', 'AbortError'))));
    expect(await fetchVerifiedImage([U1], sha(PNG), { fetch: slow as unknown as typeof globalThis.fetch, timeoutMs: 20 })).toBeNull();
    expect(await fetchVerifiedImage([U1], sha(PNG), { fetch: (async () => bytesResponse(PNG)) as unknown as typeof globalThis.fetch, maxBytes: 10 })).toBeNull();
  });

  it('bytes that hash right but are not a raster image (an SVG with a script) are refused', async () => {
    expect(await fetchVerifiedImage([U1], sha(SVG), { fetch: (async () => bytesResponse(SVG)) as unknown as typeof globalThis.fetch })).toBeNull();
  });

  it('only https URLs are fetched; a bad hash fetches nothing; no crypto means no icon', async () => {
    const fetch = vi.fn(async () => bytesResponse(PNG));
    const f = fetch as unknown as typeof globalThis.fetch;
    expect(await fetchVerifiedImage(['http://x.example/a.png', 'ipfs://Qm', 'data:image/png;base64,AAAA', 'javascript:1'], sha(PNG), { fetch: f })).toBeNull();
    expect(await fetchVerifiedImage([U1], 'not a hash', { fetch: f })).toBeNull();
    expect(fetch).not.toHaveBeenCalled();
    expect(await fetchVerifiedImage([U1], sha(PNG), { fetch: f, sha256: async () => null })).toBeNull();
  });

  it('an aborted signal stops before the next source', async () => {
    const ctl = new AbortController();
    const fetch = vi.fn(async () => {
      ctl.abort();
      return new Response('', { status: 404 });
    });
    expect(await fetchVerifiedImage([U1, U2], sha(PNG), { fetch: fetch as unknown as typeof globalThis.fetch, signal: ctl.signal })).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});

describe('iconSources', () => {
  it('kascov\'s saved copy first, then the creator\'s own URL; nothing without a committed hash', () => {
    const t = { covenantId: ID('a'), imageHash: sha(PNG), imageUrl: 'https://storage.googleapis.com/bucket/a.png' };
    // the creator's own host is never a source (F10): the viewer's IP goes to kascov.io only
    expect(iconSources('mainnet', t)).toEqual([`https://kascov.io/img/mainnet/${ID('a')}`]);
    expect(iconSources('mainnet', { ...t, imageUrl: null })).toEqual([`https://kascov.io/img/mainnet/${ID('a')}`]);
    expect(iconSources('mainnet', { ...t, imageHash: null })).toEqual([]);
    expect(iconSources('mainnet', t, 'https://names.example.org/data')[0]).toBe(`https://names.example.org/img/mainnet/${ID('a')}`);
  });
});

describe('createIconLoader', () => {
  const t = (id: string, hash = sha(PNG), imageUrl: string | null = null) => ({ covenantId: ID(id), imageHash: hash, imageUrl });

  it('returns a blob URL of the checked bytes; a hash proven once serves every token that commits to it: one fetch, one URL', async () => {
    const fetch = vi.fn(async () => new Response(PNG as unknown as BodyInit));
    const makeUrl = vi.fn(() => 'blob:ok');
    const l = createIconLoader({ fetch: fetch as unknown as typeof globalThis.fetch, makeUrl });
    const a = await l.load('testnet-10', t('a'));
    const b = await l.load('testnet-10', t('b'));
    expect([a, b]).toEqual(['blob:ok', 'blob:ok']);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(makeUrl).toHaveBeenCalledTimes(1);
    expect(makeUrl).toHaveBeenCalledWith(expect.any(Uint8Array), 'image/png');
  });

  it("a mismatch gives null and is not retried for the same token within the failure period; once another token's bytes verify the hash, it is that hash's image for all", async () => {
    let t0 = 0;
    const fetch = vi.fn(async (u: unknown) => new Response((String(u).endsWith(ID('a')) ? JPEG : PNG) as unknown as BodyInit));
    const l = createIconLoader({ fetch: fetch as unknown as typeof globalThis.fetch, makeUrl: () => 'blob:ok', now: () => t0, failTtlMs: 1_000 });
    expect(await l.load('testnet-10', t('a'))).toBeNull();
    expect(await l.load('testnet-10', t('a'))).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(await l.load('testnet-10', t('b'))).toBe('blob:ok');
    // bytes that hashed right for one token are the image of that hash for every token that commits to it
    expect(await l.load('testnet-10', t('a'))).toBe('blob:ok');
    t0 = 2_000;
    expect(await l.load('testnet-10', t('a'))).toBe('blob:ok');
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('a token whose sources serve other bytes cannot make the icon of another token fail, whichever asks first', async () => {
    const fetch = vi.fn(async (u: unknown) => new Response((String(u).endsWith(ID('a')) ? JPEG : PNG) as unknown as BodyInit));
    const l = createIconLoader({ fetch: fetch as unknown as typeof globalThis.fetch, makeUrl: () => 'blob:ok' });
    const [bad, good] = await Promise.all([l.load('testnet-10', t('a')), l.load('testnet-10', t('b'))]);
    expect(bad === null || bad === 'blob:ok').toBe(true);
    expect(good).toBe('blob:ok');
    // the same request twice at once is one fetch
    const f2 = vi.fn(async () => new Response(PNG as unknown as BodyInit));
    const l2 = createIconLoader({ fetch: f2 as unknown as typeof globalThis.fetch, makeUrl: () => 'blob:ok' });
    await Promise.all([l2.load('testnet-10', t('c')), l2.load('testnet-10', t('c'))]);
    expect(f2).toHaveBeenCalledTimes(1);
  });

  it('no hash, no request', async () => {
    const fetch = vi.fn();
    const l = createIconLoader({ fetch: fetch as unknown as typeof globalThis.fetch });
    expect(await l.load('mainnet', { covenantId: ID('a'), imageHash: null, imageUrl: 'https://x.example/a.png' })).toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it('at most `limit` images are fetched at once', async () => {
    let live = 0;
    let peak = 0;
    const fetch = async () => {
      live++;
      peak = Math.max(peak, live);
      await new Promise((r) => setTimeout(r, 5));
      live--;
      return new Response(PNG as unknown as BodyInit);
    };
    const l = createIconLoader({ fetch: fetch as unknown as typeof globalThis.fetch, makeUrl: () => 'blob:ok', limit: 2 });
    const hashes = Array.from({ length: 6 }, (_, i) => String(i).padStart(64, '0'));
    // every request answers the same PNG, so only hash(PNG) verifies: the others fail after their (limited) fetch
    await Promise.all(hashes.map((h, i) => l.load('testnet-10', t(String(i + 1), h))));
    expect(peak).toBeLessThanOrEqual(2);
    expect(peak).toBeGreaterThan(0);
  });
});
