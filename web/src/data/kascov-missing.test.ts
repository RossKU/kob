// The kascov.io facts of the icon loader and the explorer link that came with the token page of a token only kascov.io names:
// a saved copy kascov.io has none of is not asked for again for a day (a 404 is a console line each time), and the link to the explorer.
import { createHash } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { createIconLoader, createMissingCopies, fetchVerifiedImage, kascovTokenUrl, MISSING_COPY_KEY, MISSING_COPY_TTL_MS } from './kascov';

const ID = (c: string) => c.repeat(64);
const sha = (b: Uint8Array) => createHash('sha256').update(b).digest('hex');
const PNG = Uint8Array.from(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==', 'base64'));

describe('kascovTokenUrl: the link to the token on the explorer', () => {
  it('the explorer origin of the data set, the network, the covenant id', () => {
    expect(kascovTokenUrl('testnet-10', ID('a'))).toBe(`https://kascov.io/#/testnet-10/c/${ID('a')}`);
    expect(kascovTokenUrl('mainnet', ID('a'), 'https://names.example.org/kascov/data/')).toBe(`https://names.example.org/#/mainnet/c/${ID('a')}`);
  });
  it('https only, no credentials, a plain covenant id; nothing when the data set is off', () => {
    expect(kascovTokenUrl('mainnet', ID('a'), 'http://kascov.io/data')).toBeNull();
    expect(kascovTokenUrl('mainnet', ID('a'), 'https://user:pw@kascov.io/data')).toBeNull();
    expect(kascovTokenUrl('mainnet', ID('a'), '')).toBeNull();
    expect(kascovTokenUrl('mainnet', 'not-an-id')).toBeNull();
    expect(kascovTokenUrl('mainnet', ID('A'))).toBeNull();
  });
});

class Mem {
  m = new Map<string, string>();
  getItem(k: string) {
    return this.m.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.m.set(k, v);
  }
  removeItem(k: string) {
    this.m.delete(k);
  }
}

describe('a saved copy kascov.io has none of is asked for once a day, not on every visit', () => {
  const tok = (id: string, imageUrl: string | null = null) => ({ covenantId: ID(id), imageHash: sha(PNG), imageUrl });
  const copyOf = (id: string) => `https://kascov.io/img/testnet-10/${ID(id)}`;

  it('fetchVerifiedImage reports a 404 source and tries the next one; another failure is not "no copy"', async () => {
    const seen: string[] = [];
    const fetch = vi.fn(async (u: unknown) => (String(u) === copyOf('a') ? new Response('nope', { status: 404 }) : new Response(PNG as unknown as BodyInit)));
    const r = await fetchVerifiedImage([copyOf('a'), 'https://img.example/a.png'], sha(PNG), { fetch: fetch as unknown as typeof globalThis.fetch, onMissing: (u) => seen.push(u) });
    expect(r?.url).toBe('https://img.example/a.png');
    expect(seen).toEqual([copyOf('a')]);
    const seen2: string[] = [];
    await fetchVerifiedImage([copyOf('a')], sha(PNG), { fetch: (async () => new Response('x', { status: 500 })) as unknown as typeof globalThis.fetch, onMissing: (u) => seen2.push(u) });
    expect(seen2).toEqual([]);
  });

  it('the loader asks for the copy once, remembers the 404 in storage, then goes straight to the creator host (or nothing)', async () => {
    const storage = new Mem();
    let clock = 1_000;
    const fetch = vi.fn(async () => new Response('nope', { status: 404 }));
    const mk = () => createIconLoader({ fetch: fetch as unknown as typeof globalThis.fetch, makeUrl: () => 'blob:ok', failTtlMs: 0, missing: createMissingCopies(storage, () => clock) });
    expect(await mk().load('testnet-10', tok('a'))).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(JSON.parse(storage.getItem(MISSING_COPY_KEY)!)).toEqual({ [`testnet-10:${ID('a')}`]: 1_000 });
    // a new page load (a new loader, the same storage): no request at all for it
    expect(await mk().load('testnet-10', tok('a'))).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(1);
    // with the creator's own URL the copy is skipped but the creator's host is still tried
    expect(await mk().load('testnet-10', tok('a', 'https://img.example/a.png'))).toBeNull();
    expect(fetch.mock.calls.map((c) => String((c as unknown as unknown[])[0]))).toEqual([copyOf('a'), 'https://img.example/a.png']);
    // another token is not affected; a day later the copy is asked for again (kascov.io may have saved it by then)
    expect(await mk().load('testnet-10', tok('b'))).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(3);
    clock += MISSING_COPY_TTL_MS + 1;
    expect(await mk().load('testnet-10', tok('a'))).toBeNull();
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it('a copy that answered is never marked missing; a storage that fails or holds rubbish is an empty one', async () => {
    const storage = new Mem();
    const l = createIconLoader({ fetch: (async () => new Response(PNG as unknown as BodyInit)) as unknown as typeof globalThis.fetch, makeUrl: () => 'blob:ok', missing: createMissingCopies(storage) });
    expect(await l.load('testnet-10', tok('a'))).toBe('blob:ok');
    expect(storage.getItem(MISSING_COPY_KEY)).toBeNull();
    storage.setItem(MISSING_COPY_KEY, '[1,2');
    expect(createMissingCopies(storage).has('testnet-10', ID('a'))).toBe(false);
    const broken = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
      removeItem: () => undefined,
    };
    const m = createMissingCopies(broken);
    m.add('testnet-10', ID('a'));
    expect(m.has('testnet-10', ID('a'))).toBe(true); // this session's memory
    expect(createMissingCopies(null).has('testnet-10', ID('a'))).toBe(false);
  });

  it('the stored list is bounded and expired entries are dropped on the next write', () => {
    const storage = new Mem();
    let clock = 0;
    const m = createMissingCopies(storage, () => clock);
    m.add('testnet-10', ID('a'));
    clock += MISSING_COPY_TTL_MS + 1;
    m.add('testnet-10', ID('b'));
    expect(Object.keys(JSON.parse(storage.getItem(MISSING_COPY_KEY)!))).toEqual([`testnet-10:${ID('b')}`]);
  });
});
