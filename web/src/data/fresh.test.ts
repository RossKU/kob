import { afterEach, describe, expect, it } from 'vitest';
import { FRESH_MS, bypassValue, isLiveRoute, markOwnAction, recentOwnAction, resetOwnAction } from './fresh';
import { HttpIndexer } from './indexer';

const H1 = 'a'.repeat(64);
const H2 = 'b'.repeat(64);

afterEach(() => resetOwnAction());

describe('the window after an own action', () => {
  it('is closed until the user acts, open for FRESH_MS after, then closed again', () => {
    expect(recentOwnAction(1_000)).toBe(false);
    markOwnAction(1_000);
    expect(recentOwnAction(1_000 + FRESH_MS - 1)).toBe(true);
    expect(recentOwnAction(1_000 + FRESH_MS)).toBe(false);
  });

  it('gives every read a value of its own', () => {
    const seen = new Set(Array.from({ length: 200 }, () => bypassValue(1_790_000_000_000)));
    expect(seen.size).toBe(200);
  });

  it('knows the live routes (what an order changes) and not the lists, candles or health', () => {
    for (const p of [`/v1/books/${H1}`, `/v1/depth/${H1}`, `/v1/trades/${H1}`, '/v1/orders', `/v1/orders/${H1}`, `/v1/orders/${H1}/events`, '/v1/fills', '/v1/strays', '/v1/token-utxos', `/v1/stats/${H1}`, `/v1/pairs/${H1}/${H2}/book`, `/v1/pairs/${H1}/${H2}/fills`]) {
      expect(isLiveRoute(p), p).toBe(true);
    }
    for (const p of ['/v1/tokens', '/v1/pairs', '/v1/stats', `/v1/candles/${H1}`, `/v1/pairs/${H1}/${H2}/candles`, '/v1/health', `/v1/tokens/${H1}/declaration`]) {
      expect(isLiveRoute(p), p).toBe(false);
    }
  });
});

describe('HttpIndexer reads after an own action', () => {
  const stub = () => {
    const calls: URL[] = [];
    const f = (async (u: RequestInfo | URL) => {
      calls.push(new URL(String(u)));
      return new Response(JSON.stringify({ bids: [], asks: [], items: [], tokens: [] }), { status: 200, headers: { 'content-type': 'application/json' } });
    }) as typeof fetch;
    return { f, calls };
  };

  it('adds the bypass parameter to the live reads only while the window is open', async () => {
    const { f, calls } = stub();
    const ix = new HttpIndexer({ baseUrl: 'https://kob.example', fetch: f });
    await ix.book(H1);
    expect(calls[0]!.searchParams.has('_nc')).toBe(false);
    markOwnAction();
    await ix.book(H1, { depth: 5 });
    await ix.book(H1, { depth: 5 });
    await ix.orders({ maker: H2 });
    await ix.tokens();
    await ix.candles(H1, { interval: '1h' });
    const [a, b, c, d, e] = calls.slice(1);
    expect(a!.searchParams.get('depth')).toBe('5');
    expect(a!.searchParams.get('_nc')).toBeTruthy();
    expect(b!.searchParams.get('_nc')).not.toBe(a!.searchParams.get('_nc'));
    expect(c!.searchParams.get('maker')).toBe(H2);
    expect(c!.searchParams.get('_nc')).toBeTruthy();
    expect(d!.searchParams.has('_nc'), 'the token list is not a read an order changes').toBe(false);
    expect(e!.searchParams.has('_nc'), 'candles neither').toBe(false);
  });

  it('a read can ask for it, and the window can be refused per read', async () => {
    const { f, calls } = stub();
    const ix = new HttpIndexer({ baseUrl: 'https://kob.example', fetch: f });
    await ix.book(H1, {}, { fresh: true });
    markOwnAction();
    await ix.book(H1, {}, { fresh: false });
    expect(calls[0]!.searchParams.has('_nc')).toBe(true);
    expect(calls[1]!.searchParams.has('_nc')).toBe(false);
  });
});
