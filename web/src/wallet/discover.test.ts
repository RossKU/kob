import { describe, expect, it } from 'vitest';
import { createAdapters, discoverWallets, watchWallets, type WalletHost } from './discover';
import { ADAPTERS, enabledAdapters } from './index';
import { fakeKasware, fakeKaspire, fakeKastle } from '../testing/fake-wallet-providers';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAKER_SK } from '../testing/token-fixtures';

const sdk = loadKaspaSdkNode();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** a host with a tiny event target, like `window` */
function host(): WalletHost & { fire(type: string): void } {
  const handlers = new Map<string, Set<() => void>>();
  return {
    addEventListener(t, cb) { (handlers.get(t) ?? handlers.set(t, new Set()).get(t)!).add(cb); },
    removeEventListener(t, cb) { handlers.get(t)?.delete(cb); },
    fire(t) { for (const cb of [...(handlers.get(t) ?? [])]) cb(); },
  };
}

describe('createAdapters', () => {
  it('offers KasWare and Kaspire, and Kastle only when enabled; detect() follows the host', () => {
    const h = host();
    expect(createAdapters({ host: h }).map((a) => a.id)).toEqual(['kasware', 'kaspire']);
    const all = createAdapters({ host: h, kastle: true });
    expect(all.map((a) => a.id)).toEqual(['kasware', 'kaspire', 'kastle']);
    expect(all.map((a) => a.detect())).toEqual([false, false, false]);
    h.kastle = fakeKastle({ sdk, sk: MAKER_SK });
    expect(all.map((a) => a.detect())).toEqual([false, false, true]);
  });

  it('default adapters and enabledAdapters', () => {
    expect(Object.keys(ADAPTERS)).toEqual(['kasware', 'kaspire', 'kastle']);
    expect(enabledAdapters({ kastle: false }).map((a) => a.id)).toEqual(['kasware', 'kaspire']);
    expect(enabledAdapters({ kastle: true }).map((a) => a.id)).toEqual(['kasware', 'kaspire', 'kastle']);
    // no window.* in node: nothing detected
    expect(Object.values(ADAPTERS).some((a) => a.detect())).toBe(false);
  });
});

describe('watchWallets', () => {
  it('reports a wallet injected AFTER start (polling) and again when a second one appears', async () => {
    const h = host();
    const seen: string[][] = [];
    const stop = watchWallets((d) => seen.push(d.map((a) => a.id)), { host: h, intervalMs: 5, timeoutMs: 0 });
    expect(seen).toEqual([[]]); // initial state: none yet
    await sleep(20);
    h.kasware = fakeKasware({ sdk, sk: MAKER_SK });
    await sleep(30);
    h.kaspire = fakeKaspire({ sdk, sk: MAKER_SK });
    await sleep(30);
    stop();
    expect(seen).toEqual([[], ['kasware'], ['kasware', 'kaspire']]);
  });

  it('reacts to kaspire#initialized without waiting for the poll, and stop() ends everything', async () => {
    const h = host();
    const seen: string[][] = [];
    const stop = watchWallets((d) => seen.push(d.map((a) => a.id)), { host: h, intervalMs: 10_000, timeoutMs: 0 });
    h.kaspire = fakeKaspire({ sdk, sk: MAKER_SK });
    h.fire('kaspire#initialized');
    expect(seen).toEqual([[], ['kaspire']]);
    stop();
    h.kasware = fakeKasware({ sdk, sk: MAKER_SK });
    h.fire('kaspire#initialized');
    expect(seen).toHaveLength(2);
  });

  it('does not report Kastle unless enabled', async () => {
    const h = host();
    h.kastle = fakeKastle({ sdk, sk: MAKER_SK });
    const off: string[][] = [];
    watchWallets((d) => off.push(d.map((a) => a.id)), { host: h, intervalMs: 5, timeoutMs: 20 })();
    expect(off).toEqual([[]]);
    const on: string[][] = [];
    watchWallets((d) => on.push(d.map((a) => a.id)), { host: h, kastle: true, intervalMs: 5, timeoutMs: 20 })();
    expect(on).toEqual([['kastle']]);
  });

  it('stops polling after the timeout', async () => {
    const h = host();
    const seen: string[][] = [];
    watchWallets((d) => seen.push(d.map((a) => a.id)), { host: h, intervalMs: 5, timeoutMs: 30 });
    await sleep(80);
    h.kasware = fakeKasware({ sdk, sk: MAKER_SK });
    await sleep(40);
    expect(seen).toEqual([[]]); // polling ended before the late injection, and no event fired
  });
});

describe('discoverWallets', () => {
  it('resolves shortly after the first wallet shows up (late injection), with those found in the settle window', async () => {
    const h = host();
    setTimeout(() => { h.kasware = fakeKasware({ sdk, sk: MAKER_SK }); }, 30);
    setTimeout(() => { h.kaspire = fakeKaspire({ sdk, sk: MAKER_SK }); }, 45);
    const t0 = Date.now();
    const found = await discoverWallets({ host: h, intervalMs: 5, settleMs: 60, timeoutMs: 2000 });
    expect(found.map((a) => a.id)).toEqual(['kasware', 'kaspire']);
    expect(Date.now() - t0).toBeLessThan(1000);
  });

  it('resolves with an empty list when nothing is installed', async () => {
    const t0 = Date.now();
    expect(await discoverWallets({ host: host(), intervalMs: 5, timeoutMs: 60 })).toEqual([]);
    expect(Date.now() - t0).toBeGreaterThanOrEqual(50);
  });

  it('finds wallets that are already there at once', async () => {
    const h = host();
    h.kasware = fakeKasware({ sdk, sk: MAKER_SK });
    const t0 = Date.now();
    const found = await discoverWallets({ host: h, settleMs: 10, timeoutMs: 2000 });
    expect(found.map((a) => a.id)).toEqual(['kasware']);
    expect(Date.now() - t0).toBeLessThan(500);
  });
});
