import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { clearLastMarket, lastMarketHash, lastMarketKey, marketRouteOf, rememberMarket, tradeTarget } from './last-market';
import { pairRoute, tokenRoute } from './router';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);

/** A Storage in memory: vitest runs in node, which has none. */
function fakeStorage(): Storage {
  const m = new Map<string, string>();
  return {
    get length() { return m.size; },
    clear: () => m.clear(),
    getItem: (k) => m.get(k) ?? null,
    key: (i) => [...m.keys()][i] ?? null,
    removeItem: (k) => void m.delete(k),
    setItem: (k, v) => void m.set(k, String(v)),
  };
}

describe('the last market (the Trade tab)', () => {
  const g = globalThis as { localStorage?: Storage };
  beforeEach(() => {
    g.localStorage = fakeStorage();
  });
  afterEach(() => {
    delete g.localStorage;
  });

  it('only a token market or a pair is a market; a ticket preset is not kept', () => {
    expect(marketRouteOf(tokenRoute(A, 'sell', 5))).toEqual(tokenRoute(A));
    expect(marketRouteOf(pairRoute(A, B))).toEqual(pairRoute(A, B));
    expect(marketRouteOf({ name: 'market' })).toBeNull();
    expect(marketRouteOf({ name: 'portfolio' })).toBeNull();
  });

  it('nothing is stored until a market is opened', () => {
    expect(lastMarketHash('testnet-10')).toBeNull();
    rememberMarket('testnet-10', { name: 'activity' });
    rememberMarket('testnet-10', { name: 'market' });
    expect(lastMarketHash('testnet-10')).toBeNull();
  });

  it('remembers the exact route, per network, the latest one', () => {
    rememberMarket('testnet-10', tokenRoute(A));
    expect(lastMarketHash('testnet-10')).toBe(`#/market/${A}`);
    expect(lastMarketHash('mainnet')).toBeNull();
    rememberMarket('testnet-10', pairRoute(A, B));
    expect(lastMarketHash('testnet-10')).toBe(`#/market/${A}/${B}`);
  });

  it('drops a stored value that is no market route', () => {
    for (const bad of ['', '#/portfolio', '#/market', '#/market/nothex', 'junk', `#/market/${A}?ticket=sell`]) {
      g.localStorage!.setItem(lastMarketKey('testnet-10'), bad);
      expect(lastMarketHash('testnet-10'), bad).toBeNull();
      expect(g.localStorage!.getItem(lastMarketKey('testnet-10')), `${bad} removed`).toBeNull();
    }
  });

  it('forgets a market that does not resolve', () => {
    rememberMarket('testnet-10', tokenRoute(A));
    clearLastMarket('testnet-10');
    expect(lastMarketHash('testnet-10')).toBeNull();
  });

  it('works without storage (a private window): no Trade tab, no error', () => {
    delete g.localStorage;
    rememberMarket('testnet-10', tokenRoute(A));
    expect(lastMarketHash('testnet-10')).toBeNull();
    expect(() => clearLastMarket('testnet-10')).not.toThrow();
  });

  it('survives a storage that throws', () => {
    g.localStorage = new Proxy(fakeStorage(), { get: () => () => { throw new Error('blocked'); } });
    expect(() => rememberMarket('testnet-10', tokenRoute(A))).not.toThrow();
    expect(lastMarketHash('testnet-10')).toBeNull();
  });
});

describe('where the Trade tab leads (always shown)', () => {
  const token = (covenantId: string, tradable: boolean) => ({ covenantId, tradable });
  const registry = (...tokens: ReturnType<typeof token>[]) => ({ tokens }) as unknown as Parameters<typeof tradeTarget>[1];

  it('the last market opened wins, whatever the registry lists', () => {
    expect(tradeTarget(`#/market/${B}`, registry(token(A, true)))).toBe(`#/market/${B}`);
  });

  it('with none, the first tradable token of the registry (no request is made for it)', () => {
    expect(tradeTarget(null, registry(token(B, false), token(A, true)))).toBe(`#/market/${A}`);
  });

  it('with none of those, the market list: the tab is never missing', () => {
    expect(tradeTarget(null, registry())).toBe('#/market');
    expect(tradeTarget(null, registry(token(A, false)))).toBe('#/market');
  });
});
