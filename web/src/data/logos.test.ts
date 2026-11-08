// Token logos from the creator's address (data/logos.ts, data/logo-url.ts): which addresses are loaded at all, the IPFS check against the CID, the gateway fetch.
import { createHash } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { asImageHash, checkHttpsImage, createHttpsChecker, createIpfsLoader, fetchIpfsImage, ipfsGatewayUrl, parseCid, parseLogoRef, refusedHost, safeHttpsLogo, verifyIpfsBytes } from './logos';

const sha = (b: Uint8Array) => Promise.resolve(createHash('sha256').update(b).digest('hex'));
const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4, 5, 6, 7, 8]);

// ---- encoders for building CIDs in the tests (the code under test only decodes)
const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
function base58(bytes: Uint8Array): string {
  let n = 0n;
  for (const b of bytes) n = n * 256n + BigInt(b);
  let out = '';
  for (; n > 0n; n /= 58n) out = B58[Number(n % 58n)] + out;
  for (const b of bytes) {
    if (b !== 0) break;
    out = '1' + out;
  }
  return out;
}
function base32(bytes: Uint8Array): string {
  const A = 'abcdefghijklmnopqrstuvwxyz234567';
  let bits = 0;
  let acc = 0;
  let out = '';
  for (const b of bytes) {
    acc = (acc << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += A[(acc >> (bits - 5)) & 31];
      bits -= 5;
    }
    acc &= (1 << bits) - 1;
  }
  if (bits > 0) out += A[(acc << (5 - bits)) & 31];
  return out;
}
const digest = (b: Uint8Array) => new Uint8Array(createHash('sha256').update(b).digest());
const cidV1Raw = (file: Uint8Array) => 'b' + base32(new Uint8Array([0x01, 0x55, 0x12, 0x20, ...digest(file)]));

const HELLO = new TextEncoder().encode('hello world\n');
// `echo "hello world" | ipfs add` (go-ipfs, default chunker): a one-block UnixFS file
const HELLO_CID_V0 = 'QmT78zSuBmuS4z925WZfrqQ1qHaJ56DQaTfyMUF7F8ff5o';

describe('which hosts and addresses are refused', () => {
  it('IP literals in every spelling, localhost, names without a dot and local suffixes', () => {
    for (const h of ['127.0.0.1', '10.0.0.5', '192.168.1.1', '[::1]', '::1', 'localhost', 'app.localhost', 'intranet', 'printer.local', 'nas.lan', '2130706433', '0x7f000001', '']) {
      expect(refusedHost(h), h).toBe(true);
    }
    for (const h of ['kasperolabs.com', 'storage.googleapis.com', 'a.b.example.org', 'ipfs.io']) expect(refusedHost(h), h).toBe(false);
  });

  it('safeHttpsLogo: https only, no credentials, no SVG, no IP literal; spelled-out IPs are normalised by the URL parser and caught', () => {
    expect(safeHttpsLogo('https://kasperolabs.com/kaspero-logo.jpg')).toBe('https://kasperolabs.com/kaspero-logo.jpg');
    for (const bad of [
      'http://kasperolabs.com/a.png',
      'data:image/png;base64,AAAA',
      'javascript:alert(1)',
      'https://u:p@example.org/a.png',
      'https://127.0.0.1/a.png',
      'https://[::1]/a.png',
      'https://localhost/a.png',
      'https://2130706433/a.png',
      'https://0x7f.1/a.png',
      'https://example.org/logo.svg',
      'https://example.org/logo.SVG',
      'https://example.org/logo.svgz',
      'https://example.org/a b.png',
      'https://' + 'a'.repeat(2100) + '.example.org/a.png',
      '',
      7,
      null,
    ]) {
      expect(safeHttpsLogo(bad), String(bad)).toBeNull();
    }
  });
});

describe('parseLogoRef', () => {
  const V1 = cidV1Raw(PNG);
  it('takes https and ipfs addresses; nothing else', () => {
    expect(parseLogoRef(' https://kasperolabs.com/kaspero-logo.jpg ')).toEqual({ kind: 'https', url: 'https://kasperolabs.com/kaspero-logo.jpg' });
    expect(parseLogoRef(`ipfs://${HELLO_CID_V0}`)).toEqual({ kind: 'ipfs', cid: HELLO_CID_V0, path: '' });
    expect(parseLogoRef(`ipfs://${V1}/logo.png`)).toEqual({ kind: 'ipfs', cid: V1, path: '/logo.png' });
    // an SVG address (also a compressed .svgz) is kept, marked: it is shown only from checked bytes (data/svg-logo.ts)
    expect(parseLogoRef(`ipfs://${V1}/a.svg`)).toEqual({ kind: 'ipfs', cid: V1, path: '/a.svg', svg: true });
    expect(parseLogoRef(`ipfs://${V1}/a.svgz`)).toEqual({ kind: 'ipfs', cid: V1, path: '/a.svgz', svg: true });
    expect(parseLogoRef('https://example.org/logo.SVG')).toEqual({ kind: 'https', url: 'https://example.org/logo.SVG', svg: true });
    expect(parseLogoRef('https://example.org/logo.svgz')).toEqual({ kind: 'https', url: 'https://example.org/logo.svgz', svg: true });
    for (const bad of ['ipfs://', 'ipfs://notacid', `ipfs://${V1}/../x`, 'https://127.0.0.1/a.svg', `ipfs://${V1}?x=1`, `ipfs://${V1}#f`, `ipfs://${V1}/a b`, 'ipfs:/' + HELLO_CID_V0, 'ftp://x.example/a.png', 'data:image/png;base64,AA', '', '   ', 5, undefined]) {
      expect(parseLogoRef(bad), String(bad)).toBeNull();
    }
  });
});

describe('the CID against the bytes', () => {
  it('knows the one-block UnixFS encoding: hello world matches the CIDv0 go-ipfs prints', async () => {
    expect(await verifyIpfsBytes(HELLO_CID_V0, '', HELLO, sha)).toBe(true);
    expect(await verifyIpfsBytes(HELLO_CID_V0, '', new TextEncoder().encode('hello world!\n'), sha)).toBe(false);
  });

  it('a CIDv1 raw leaf is the file itself; a CIDv1 dag-pb is the wrapped block', async () => {
    expect(await verifyIpfsBytes(cidV1Raw(PNG), '', PNG, sha)).toBe(true);
    expect(await verifyIpfsBytes(cidV1Raw(PNG), '', new Uint8Array([...PNG, 9]), sha)).toBe(false);
    // the dag-pb block of the same file: built by the same rule the checker uses, so only the codec switch is tested here
    expect(await verifyIpfsBytes(`b${base32(new Uint8Array([0x01, 0x70, 0x12, 0x20, ...base58Decode(HELLO_CID_V0).slice(2)]))}`, '', HELLO, sha)).toBe(true);
  });

  it('cannot be checked (null, the gateway is relied on) for a path, a many-block size, an unknown codec or hash, or no SHA-256', async () => {
    expect(await verifyIpfsBytes(HELLO_CID_V0, '/logo.png', HELLO, sha)).toBeNull();
    expect(await verifyIpfsBytes(HELLO_CID_V0, '', new Uint8Array(262_145), sha)).toBeNull();
    expect(await verifyIpfsBytes('b' + base32(new Uint8Array([0x01, 0x71, 0x12, 0x20, ...digest(PNG)])), '', PNG, sha)).toBeNull(); // dag-cbor
    expect(await verifyIpfsBytes('b' + base32(new Uint8Array([0x01, 0x55, 0x13, 0x20, ...digest(PNG)])), '', PNG, sha)).toBeNull(); // sha2-512 code
    expect(await verifyIpfsBytes(HELLO_CID_V0, '', HELLO, () => Promise.resolve(null))).toBeNull();
    expect(parseCid('QmT78zSuBmuS4z925WZfrqQ1qHaJ56DQaTfyMUF7F8ff5')).toBeNull();
  });
});

function base58Decode(s: string): Uint8Array {
  let n = 0n;
  for (const c of s) n = n * 58n + BigInt(B58.indexOf(c));
  const out: number[] = [];
  for (; n > 0n; n >>= 8n) out.unshift(Number(n & 0xffn));
  return new Uint8Array(out);
}

describe('ipfsGatewayUrl', () => {
  const ref = { kind: 'ipfs' as const, cid: HELLO_CID_V0, path: '/a.png' };
  it('joins the gateway, the CID and the path; refuses a missing, http or local gateway', () => {
    expect(ipfsGatewayUrl('https://dweb.link/ipfs/', ref)).toBe(`https://dweb.link/ipfs/${HELLO_CID_V0}/a.png`);
    expect(ipfsGatewayUrl('', ref)).toBeNull();
    expect(ipfsGatewayUrl('http://dweb.link/ipfs', ref)).toBeNull();
    expect(ipfsGatewayUrl('https://127.0.0.1/ipfs', ref)).toBeNull();
    expect(ipfsGatewayUrl('not a url', ref)).toBeNull();
  });
});

describe('fetchIpfsImage', () => {
  const rawRef = { kind: 'ipfs' as const, cid: cidV1Raw(PNG), path: '' };
  const ok = (bytes: Uint8Array, status = 200) => vi.fn(() => Promise.resolve(new Response(bytes as Uint8Array<ArrayBuffer>, { status })));

  it('returns a checked image whose bytes are what the CID names, asking with no credentials and following the gateway\'s redirect', async () => {
    const f = ok(PNG);
    const img = await fetchIpfsImage('https://dweb.link/ipfs', rawRef, { fetch: f as unknown as typeof fetch, sha256: sha });
    expect(img).toMatchObject({ mime: 'image/png', checked: true });
    expect(f).toHaveBeenCalledWith(`https://dweb.link/ipfs/${rawRef.cid}`, expect.objectContaining({ credentials: 'omit', referrerPolicy: 'no-referrer', redirect: 'follow' }));
  });

  it('refuses bytes that are not the CID\'s, a non-raster file (SVG, HTML), an error status and an empty body', async () => {
    const other = new Uint8Array([...PNG, 0xff]);
    expect(await fetchIpfsImage('https://dweb.link/ipfs', rawRef, { fetch: ok(other) as unknown as typeof fetch, sha256: sha })).toBeNull();
    const svg = new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg"/>');
    expect(await fetchIpfsImage('https://dweb.link/ipfs', { ...rawRef, cid: cidV1Raw(svg) }, { fetch: ok(svg) as unknown as typeof fetch, sha256: sha })).toBeNull();
    expect(await fetchIpfsImage('https://dweb.link/ipfs', rawRef, { fetch: ok(PNG, 404) as unknown as typeof fetch, sha256: sha })).toBeNull();
    expect(await fetchIpfsImage('https://dweb.link/ipfs', rawRef, { fetch: ok(new Uint8Array()) as unknown as typeof fetch, sha256: sha })).toBeNull();
  });

  it('a file that cannot be checked (a path) is accepted from the gateway as a raster image, marked unchecked', async () => {
    const img = await fetchIpfsImage('https://dweb.link/ipfs', { ...rawRef, path: '/logo.png' }, { fetch: ok(PNG) as unknown as typeof fetch, sha256: sha });
    expect(img).toMatchObject({ mime: 'image/png', checked: false });
  });

  it('no gateway, a network error and a body over the cap give null', async () => {
    const f = vi.fn();
    expect(await fetchIpfsImage('', rawRef, { fetch: f as unknown as typeof fetch })).toBeNull();
    expect(f).not.toHaveBeenCalled();
    expect(await fetchIpfsImage('https://dweb.link/ipfs', rawRef, { fetch: (() => Promise.reject(new Error('offline'))) as unknown as typeof fetch })).toBeNull();
    expect(await fetchIpfsImage('https://dweb.link/ipfs', rawRef, { fetch: ok(PNG) as unknown as typeof fetch, maxBytes: 8, sha256: sha })).toBeNull();
  });
});

describe('createIpfsLoader', () => {
  const ref = { kind: 'ipfs' as const, cid: cidV1Raw(PNG), path: '' };
  it('one request and one blob URL per address; a failure is not retried at once, and is after the failure window', async () => {
    let t = 0;
    const f = vi.fn(() => Promise.resolve(new Response(PNG as Uint8Array<ArrayBuffer>)));
    const makeUrl = vi.fn(() => 'blob:one');
    const l = createIpfsLoader({ fetch: f as unknown as typeof fetch, sha256: sha, now: () => t, makeUrl, failTtlMs: 1000 });
    const [a, b] = await Promise.all([l.load('https://dweb.link/ipfs', ref), l.load('https://dweb.link/ipfs', ref)]);
    expect([a, b]).toEqual(['blob:one', 'blob:one']);
    expect(await l.load('https://dweb.link/ipfs', ref)).toBe('blob:one');
    expect([f.mock.calls.length, makeUrl.mock.calls.length]).toEqual([1, 1]);

    const bad = vi.fn(() => Promise.resolve(new Response('', { status: 500 })));
    const l2 = createIpfsLoader({ fetch: bad as unknown as typeof fetch, sha256: sha, now: () => t, failTtlMs: 1000 });
    expect(await l2.load('https://dweb.link/ipfs', ref)).toBeNull();
    expect(await l2.load('https://dweb.link/ipfs', ref)).toBeNull();
    expect(bad).toHaveBeenCalledTimes(1);
    t = 1500;
    expect(await l2.load('https://dweb.link/ipfs', ref)).toBeNull();
    expect(bad).toHaveBeenCalledTimes(2);
  });
});

describe('the creator\'s image_hash', () => {
  const ref = { kind: 'ipfs' as const, cid: cidV1Raw(PNG), path: '' };
  const PNG_HASH = createHash('sha256').update(PNG).digest('hex');
  const WRONG_HASH = createHash('sha256').update('another file').digest('hex');
  const ok = (bytes: Uint8Array, status = 200) => vi.fn(() => Promise.resolve(new Response(bytes as Uint8Array<ArrayBuffer>, { status })));

  it('asImageHash takes 64 lowercase hex only', () => {
    expect(asImageHash(PNG_HASH)).toBe(PNG_HASH);
    for (const bad of [PNG_HASH.toUpperCase(), PNG_HASH.slice(1), `sha256:${PNG_HASH}`, '', null, 7]) expect(asImageHash(bad)).toBeNull();
  });

  it('an ipfs:// image is compared with the hash too: equal is hash-checked, different is refused and reported', async () => {
    const onMismatch = vi.fn();
    const img = await fetchIpfsImage('https://dweb.link/ipfs', ref, { fetch: ok(PNG) as unknown as typeof fetch, sha256: sha, imageHash: PNG_HASH, onMismatch });
    expect(img).toMatchObject({ checked: true, hashChecked: true });
    expect(onMismatch).not.toHaveBeenCalled();
    expect(await fetchIpfsImage('https://dweb.link/ipfs', ref, { fetch: ok(PNG) as unknown as typeof fetch, sha256: sha, imageHash: WRONG_HASH, onMismatch })).toBeNull();
    expect(onMismatch).toHaveBeenCalledTimes(1);
  });

  it('the loader keeps a hash mismatch apart from a failed fetch', async () => {
    const l = createIpfsLoader({ fetch: ok(PNG) as unknown as typeof fetch, sha256: sha, makeUrl: () => 'blob:ok' });
    expect(await l.load('https://dweb.link/ipfs', ref, PNG_HASH)).toBe('blob:ok');
    expect(l.mismatched('https://dweb.link/ipfs', ref, PNG_HASH)).toBe(false);
    expect(await l.load('https://dweb.link/ipfs', ref, WRONG_HASH)).toBeNull();
    expect(l.mismatched('https://dweb.link/ipfs', ref, WRONG_HASH)).toBe(true);
    const down = createIpfsLoader({ fetch: ok(PNG, 500) as unknown as typeof fetch, sha256: sha });
    expect(await down.load('https://dweb.link/ipfs', ref, PNG_HASH)).toBeNull();
    expect(down.mismatched('https://dweb.link/ipfs', ref, PNG_HASH)).toBe(false);
  });

  it('an https:// image is read in CORS mode with no credentials and no referrer: equal bytes match, other bytes mismatch', async () => {
    const f = ok(PNG);
    const r = await checkHttpsImage('https://e.example/a.png', PNG_HASH, { fetch: f as unknown as typeof fetch, sha256: sha });
    expect(r).toMatchObject({ kind: 'match', mime: 'image/png' });
    expect(f).toHaveBeenCalledWith('https://e.example/a.png', expect.objectContaining({ mode: 'cors', credentials: 'omit', referrerPolicy: 'no-referrer' }));
    expect(await checkHttpsImage('https://e.example/a.png', WRONG_HASH, { fetch: ok(PNG) as unknown as typeof fetch, sha256: sha })).toEqual({ kind: 'mismatch' });
  });

  it('an https:// image that cannot be read (no CORS header, an error status, too large, offline) is unreadable; a matching non-raster file is refused', async () => {
    const blocked = vi.fn(() => Promise.reject(new TypeError('Failed to fetch')));
    expect(await checkHttpsImage('https://e.example/a.png', PNG_HASH, { fetch: blocked as unknown as typeof fetch, sha256: sha })).toEqual({ kind: 'unreadable' });
    expect(await checkHttpsImage('https://e.example/a.png', PNG_HASH, { fetch: ok(PNG, 404) as unknown as typeof fetch, sha256: sha })).toEqual({ kind: 'unreadable' });
    expect(await checkHttpsImage('https://e.example/a.png', PNG_HASH, { fetch: ok(PNG) as unknown as typeof fetch, sha256: sha, maxBytes: 8 })).toEqual({ kind: 'unreadable' });
    expect(await checkHttpsImage('https://e.example/a.png', PNG_HASH, { fetch: ok(PNG) as unknown as typeof fetch, sha256: () => Promise.resolve(null) })).toEqual({ kind: 'unreadable' });
    const svg = new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg"/>');
    const svgHash = createHash('sha256').update(svg).digest('hex');
    expect(await checkHttpsImage('https://e.example/a.svg', svgHash, { fetch: ok(svg) as unknown as typeof fetch, sha256: sha })).toEqual({ kind: 'refused' });
  });

  it('an SVG whose ORIGINAL bytes hash to the commitment is shown as the picture drawn from it; one that hashes otherwise is a mismatch', async () => {
    const svg = new TextEncoder().encode('<?xml version="1.0"?>\n<!-- logo -->\n<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>');
    const svgHash = createHash('sha256').update(svg).digest('hex');
    const webp = new Uint8Array([0x52, 0x49, 0x46, 0x46, 0, 0, 0, 0, 0x57, 0x45, 0x42, 0x50]);
    const rasterize = vi.fn(async () => ({ bytes: webp, mime: 'image/webp' as const }));
    expect(await checkHttpsImage('https://e.example/a.svg', svgHash, { fetch: ok(svg) as unknown as typeof fetch, sha256: sha, rasterize })).toEqual({ kind: 'match', bytes: webp, mime: 'image/webp' });
    expect(rasterize).toHaveBeenCalledWith(svg, 'image/svg+xml');
    // the hash of the drawn picture is not the commitment: the original's is
    const webpHash = createHash('sha256').update(webp).digest('hex');
    expect(await checkHttpsImage('https://e.example/a.svg', webpHash, { fetch: ok(svg) as unknown as typeof fetch, sha256: sha, rasterize })).toEqual({ kind: 'mismatch' });
    // through IPFS: the CID names the original SVG
    const ref = { kind: 'ipfs' as const, cid: cidV1Raw(svg), path: '', svg: true as const };
    expect(await fetchIpfsImage('https://dweb.link/ipfs', ref, { fetch: ok(svg) as unknown as typeof fetch, sha256: sha, rasterize })).toEqual({ bytes: webp, mime: 'image/webp', checked: true });
  });

  it('the checker asks once per address and hash, hands out one blob URL, and asks an unreadable one again after the failure window', async () => {
    let t = 0;
    const f = ok(PNG);
    const makeUrl = vi.fn(() => 'blob:png');
    const c = createHttpsChecker({ fetch: f as unknown as typeof fetch, sha256: sha, makeUrl, now: () => t, failTtlMs: 1000 });
    const [a, b] = await Promise.all([c.check('https://e.example/a.png', PNG_HASH), c.check('https://e.example/a.png', PNG_HASH)]);
    expect([a, b]).toEqual([{ kind: 'match', src: 'blob:png' }, { kind: 'match', src: 'blob:png' }]);
    expect([f.mock.calls.length, makeUrl.mock.calls.length]).toEqual([1, 1]);
    const down = vi.fn(() => Promise.reject(new TypeError('blocked')));
    const c2 = createHttpsChecker({ fetch: down as unknown as typeof fetch, sha256: sha, now: () => t, failTtlMs: 1000 });
    expect(await c2.check('https://e.example/a.png', PNG_HASH)).toEqual({ kind: 'unreadable' });
    await c2.check('https://e.example/a.png', PNG_HASH);
    expect(down).toHaveBeenCalledTimes(1);
    t = 2000;
    await c2.check('https://e.example/a.png', PNG_HASH);
    expect(down).toHaveBeenCalledTimes(2);
  });
});
