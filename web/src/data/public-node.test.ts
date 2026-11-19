import { describe, expect, it } from 'vitest';
import { FAILED_TTL_MS, PICK_TTL_MS, PUBLIC_RESOLVERS, createPublicNodePicker, nodeUrlOfAnswer } from './public-node';

type Reply = { url: string } | 'error' | 'hang' | 'bad';
/** A fetch whose resolvers answer as `replies` says (by host), after `delayMs`; records the URLs asked. */
function fakeFetch(replies: Record<string, Reply>, delayMs = 5) {
  const asked: string[] = [];
  const aborted: string[] = [];
  const fetch = (url: string, init: { signal: AbortSignal }) => {
    asked.push(url);
    const host = new URL(url).host;
    const r = replies[host] ?? 'error';
    return new Promise<{ ok: boolean; json(): Promise<unknown> }>((resolve, reject) => {
      init.signal.addEventListener('abort', () => (aborted.push(host), reject(new DOMException('aborted', 'AbortError'))));
      if (r === 'hang') return;
      setTimeout(() => {
        if (r === 'error') resolve({ ok: false, json: async () => null });
        else if (r === 'bad') resolve({ ok: true, json: async () => ({ url: 'ws://plain.example/x' }) });
        else resolve({ ok: true, json: async () => ({ uid: '1', url: r.url }) });
      }, delayMs);
    });
  };
  return { fetch, asked, aborted };
}

const mem = () => {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k), m };
};
const R = ['https://a.example', 'https://b.example', 'https://c.example', 'https://d.example', 'https://e.example', 'https://f.example'];
const NODE = 'wss://node-1.example/kaspa/testnet-10/wrpc/borsh';

describe('public node picker', () => {
  it('lists the SDK resolvers', () => {
    expect(PUBLIC_RESOLVERS).toHaveLength(16);
    expect(PUBLIC_RESOLVERS.every((u) => /^https:\/\/[a-z]+\.kaspa\.(stream|red|green|blue)$/.test(u))).toBe(true);
  });

  it('takes only a wss:// node without credentials', () => {
    expect(nodeUrlOfAnswer({ url: NODE })).toBe(NODE);
    expect(nodeUrlOfAnswer({ url: 'ws://x.example/a' })).toBeNull();
    expect(nodeUrlOfAnswer({ url: 'wss://u:p@x.example/a' })).toBeNull();
    expect(nodeUrlOfAnswer({ url: 'https://x.example' })).toBeNull();
    expect(nodeUrlOfAnswer({ url: 42 })).toBeNull();
    expect(nodeUrlOfAnswer(null)).toBeNull();
  });

  it('asks several resolvers at once, takes the first node named and aborts the others; a hung resolver does not hold it', async () => {
    const f = fakeFetch({ 'a.example': 'hang', 'b.example': 'hang', 'c.example': 'error', 'd.example': { url: NODE } });
    const p = createPublicNodePicker({ fetch: f.fetch, resolvers: R, storage: null, random: () => 0.5, startAtOnce: 4, staggerMs: 1_000 });
    const t0 = Date.now();
    await expect(p.pick('testnet-10')).resolves.toBe(NODE);
    expect(Date.now() - t0).toBeLessThan(500);
    expect(f.asked.slice(0, 4).map((u) => new URL(u).host).sort()).toEqual(['a.example', 'b.example', 'c.example', 'd.example']);
    expect(f.asked[0]).toMatch(/\/v2\/kaspa\/testnet-10\/tls\/wrpc\/borsh$/);
    expect(f.aborted).toEqual(expect.arrayContaining(['a.example', 'b.example']));
  });

  it('asks the next resolver when one fails, and gives null when none names a node', async () => {
    const f = fakeFetch({ 'a.example': 'error', 'b.example': 'bad' });
    const p = createPublicNodePicker({ fetch: f.fetch, resolvers: R, storage: null, startAtOnce: 1, staggerMs: 10_000 });
    await expect(p.pick('testnet-10')).resolves.toBeNull();
    expect(f.asked).toHaveLength(R.length);
  });

  it('starts one more resolver every stagger while none answered', async () => {
    const f = fakeFetch(Object.fromEntries(R.map((r, i) => [new URL(r).host, i === 5 ? { url: NODE } : 'hang'])));
    const p = createPublicNodePicker({ fetch: f.fetch, resolvers: R, storage: null, random: () => 0.5, startAtOnce: 2, staggerMs: 20 });
    await expect(p.pick('testnet-10')).resolves.toBe(NODE);
    expect(f.asked).toHaveLength(6);
  });

  it('keeps the pick for the session (shared by concurrent picks), per network, and asks again once it is forgotten or old', async () => {
    let now = 1_000_000;
    const store = mem();
    const f = fakeFetch(Object.fromEntries(R.map((r) => [new URL(r).host, { url: NODE }])));
    const p = createPublicNodePicker({ fetch: f.fetch, resolvers: R, storage: store, now: () => now, startAtOnce: 1 });
    const [a, b] = await Promise.all([p.pick('testnet-10'), p.pick('testnet-10')]);
    expect([a, b]).toEqual([NODE, NODE]);
    expect(f.asked).toHaveLength(1);
    // a reload: a new picker reads the session's pick
    const p2 = createPublicNodePicker({ fetch: f.fetch, resolvers: R, storage: store, now: () => now, startAtOnce: 1 });
    await expect(p2.pick('testnet-10')).resolves.toBe(NODE);
    expect(f.asked).toHaveLength(1);
    await p2.pick('mainnet');
    expect(f.asked.at(-1)).toMatch(/\/v2\/kaspa\/mainnet\//);
    p2.forget(NODE);
    // every resolver names the forgotten node: all are asked, then it is used as nothing else is offered
    await expect(p2.pick('testnet-10')).resolves.toBe(NODE);
    expect(f.asked).toHaveLength(2 + R.length);
    now += PICK_TTL_MS + 1;
    await p2.pick('testnet-10');
    expect(f.asked).toHaveLength(3 + R.length);
  });

  it('passes over a node that failed lately: the other resolvers are asked for another, also after a reload, until FAILED_TTL_MS', async () => {
    let now = 1_000_000;
    const store = mem();
    const DEAD = 'wss://dead.example/kaspa/testnet-10/wrpc/borsh';
    // the first resolvers asked name the dead node, a later one another node
    const f = fakeFetch({ 'a.example': { url: DEAD }, 'b.example': { url: DEAD }, 'c.example': 'error', 'd.example': { url: NODE } });
    const opts = { fetch: f.fetch, resolvers: R, storage: store, now: () => now, random: () => 0.5, startAtOnce: 1, staggerMs: 10_000 };
    const p = createPublicNodePicker(opts);
    await expect(p.pick('testnet-10')).resolves.toBe(DEAD);
    p.forget(DEAD);
    await expect(p.pick('testnet-10')).resolves.toBe(NODE);
    expect(f.asked.slice(1).map((u) => new URL(u).host)).toEqual(['a.example', 'b.example', 'c.example', 'd.example']);
    // a reload within the window remembers it
    p.forget(NODE);
    const p2 = createPublicNodePicker(opts);
    await expect(p2.pick('testnet-10')).resolves.toBe(DEAD); // both failed: the first offered is the fallback
    expect(JSON.parse(store.m.get('kob.node.failed')!).map((e: [string, number]) => e[0])).toEqual([DEAD, NODE]);
    // after the window the dead node is taken again at once
    now += FAILED_TTL_MS + 1;
    const p3 = createPublicNodePicker({ ...opts, storage: mem() });
    const before = f.asked.length;
    await expect(p3.pick('testnet-10')).resolves.toBe(DEAD);
    expect(f.asked.length - before).toBe(1);
  });

  it('a failed node offered with hung resolvers: the others are waited for within their timeout, then it is used', async () => {
    const DEAD = 'wss://dead.example/kaspa/testnet-10/wrpc/borsh';
    const f = fakeFetch(Object.fromEntries(R.map((r, i) => [new URL(r).host, i === 0 ? { url: DEAD } : 'hang'])));
    const p = createPublicNodePicker({ fetch: f.fetch, resolvers: R, storage: null, random: () => 0.5, startAtOnce: 6, timeoutMs: 100 });
    p.forget(DEAD);
    const t0 = Date.now();
    await expect(p.pick('testnet-10')).resolves.toBe(DEAD);
    expect(Date.now() - t0).toBeGreaterThanOrEqual(90);
    expect(Date.now() - t0).toBeLessThan(1_000);
    expect(f.asked).toHaveLength(R.length);
  });
});
