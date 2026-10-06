import { afterEach, describe, expect, it } from 'vitest';
import { WebSocketServer, WebSocket, type RawData } from 'ws';
import type { AddressInfo } from 'node:net';
import {
  HttpIndexer, IndexerError, IndexerFeed, isValidChannel, sompi, sompiOrNull, toFeedUrl, type FeedEventMap, type WebSocketCtor,
} from './indexer';

const H1 = '11'.repeat(32);
const H2 = 'ab'.repeat(32);
const json = (body: unknown, status = 200, headers: Record<string, string> = {}) =>
  new Response(typeof body === 'string' ? body : JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...headers } });

function stub(handler: (url: URL, n: number) => Response | Promise<Response>) {
  const calls: URL[] = [];
  const f = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input));
    calls.push(url);
    if (init?.signal?.aborted) throw new DOMException('aborted', 'AbortError');
    return handler(url, calls.length);
  }) as typeof fetch;
  return { f, calls };
}

describe('sompi converters', () => {
  it('parse decimal strings exactly (beyond 2^53) and refuse everything else', () => {
    expect(sompi('9007199254740993')).toBe(9_007_199_254_740_993n);
    expect(sompi(12)).toBe(12n);
    expect(sompi(5n)).toBe(5n);
    expect(sompiOrNull(null)).toBeNull();
    expect(sompiOrNull('0')).toBe(0n);
    for (const bad of ['-1', '1.5', '0x10', '', ' 7', 1.5, -3, 2 ** 60]) expect(() => sompi(bad as never), String(bad)).toThrow(RangeError);
  });
});

describe('HttpIndexer requests', () => {
  it('builds the documented URLs', async () => {
    const { f, calls } = stub(() => json({ items: [], tokens: [], next_cursor: null }));
    const ix = new HttpIndexer({ baseUrl: 'https://kob.example/', fetch: f });
    await ix.health();
    await ix.tokens();
    await ix.book(H1, { depth: 5, aggregate: false });
    await ix.book(H1);
    await ix.order(H1);
    await ix.orders({ maker: 'ABCD', token: H2, status: 'open', limit: 10, cursor: 'c1' });
    await ix.orderEvents(H1, { after: 7, limit: 3 });
    await ix.fills({ token: H1, side: 'ask', limit: 5, before: 99 });
    await ix.strays({ maker: 'ab12', limit: 9 });
    await ix.tokenEvents({ token: H2, limit: 1 });
    await ix.tokenUtxos({ owner: H1, token: H2 });
    expect(calls.map((u) => u.pathname + u.search)).toEqual([
      '/v1/health',
      '/v1/tokens',
      `/v1/books/${H1}?depth=5&aggregate=false`,
      `/v1/books/${H1}`,
      `/v1/orders/${H1}`,
      `/v1/orders?maker=abcd&token=${H2}&status=open&limit=10&cursor=c1`,
      `/v1/orders/${H1}/events?after=7&limit=3`,
      `/v1/fills?token=${H1}&side=ask&limit=5&before=99`,
      '/v1/strays?maker=ab12&limit=9',
      `/v1/token-events?token=${H2}&limit=1`,
      `/v1/token-utxos?owner=${H1}&token=${H2}&spent=false`,
    ]);
    expect(calls[0]!.origin).toBe('https://kob.example');
  });

  it('unwraps list envelopes and tolerates a missing one', async () => {
    const { f } = stub((u) => {
      if (u.pathname === '/v1/tokens') return json({ tokens: [{ ticker: 'X', covenant_id: 'ab'.repeat(32) }, { ticker: 'BAD', covenant_id: 'ab‮cd' + 'f'.repeat(59) }] });
      if (u.pathname === '/v1/strays') return json({});
      return json({ items: [{ txid: 'a' }] });
    });
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    // an item whose covenant id is not 32-byte lower-case hex is dropped (it would become a label, route and map key)
    expect(await ix.tokens()).toEqual([{ ticker: 'X', covenant_id: 'ab'.repeat(32) }]);
    expect(await ix.strays()).toEqual([]);
    expect(await ix.tokenEvents()).toEqual([{ txid: 'a' }]);
  });

  it('validates identifiers before any request', async () => {
    const { f, calls } = stub(() => json({}));
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    await expect(ix.order('nothex')).rejects.toMatchObject({ kind: 'bad-request' });
    await expect(ix.book('ab')).rejects.toBeInstanceOf(IndexerError);
    await expect(ix.orders({ maker: 'xyz' })).rejects.toMatchObject({ kind: 'bad-request' });
    await expect(ix.tokenUtxos({ owner: 'abc' })).rejects.toMatchObject({ kind: 'bad-request' });
    expect(calls).toHaveLength(0);
  });

  it('order / orderEvents map an unknown id (404) to null, other errors throw', async () => {
    const { f } = stub((u) => (u.pathname.endsWith('/events') ? json({ error: { code: 'internal', message: 'boom' } }, 500) : json({ error: { code: 'not_found', message: 'unknown covenant id' } }, 404)));
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    expect(await ix.order(H1)).toBeNull();
    await expect(ix.orderEvents(H1)).rejects.toMatchObject({ status: 500, code: 'internal', transient: true });
  });

  it('allOrders follows next_cursor and stops at maxPages', async () => {
    const { f, calls } = stub((u) => {
      const c = u.searchParams.get('cursor');
      const n = c ? Number(c) : 0;
      return json({ items: [{ covenant_id: 'o' + n }], next_cursor: String(n + 1) });
    });
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    const all = await ix.allOrders({ token: H1, limit: 1 }, { maxPages: 3 });
    expect(all.map((o) => o.covenant_id)).toEqual(['o0', 'o1', 'o2']);
    expect(calls.map((u) => u.searchParams.get('cursor'))).toEqual([null, '1', '2']);
    const finite = stub((u) => json(u.searchParams.get('cursor') ? { items: [{ covenant_id: 'b' }], next_cursor: null } : { items: [{ covenant_id: 'a' }], next_cursor: 'z' }));
    expect((await new HttpIndexer({ baseUrl: 'http://x', fetch: finite.f }).allOrders()).map((o) => o.covenant_id)).toEqual(['a', 'b']);
  });
});

describe('HttpIndexer errors and backoff', () => {
  it('surfaces the typed {error:{code,message}} body', async () => {
    const { f } = stub(() => json({ error: { code: 'bad_request', message: 'status must be one of open' } }, 400));
    const err = await new HttpIndexer({ baseUrl: 'http://x', fetch: f }).orders({ status: 'zzz' }).catch((e) => e);
    expect(err).toBeInstanceOf(IndexerError);
    expect(err).toMatchObject({ kind: 'http', status: 400, code: 'bad_request', message: 'status must be one of open', transient: false });
  });

  it('non-JSON error bodies still give a typed error; a 200 with a non-JSON body is a parse error', async () => {
    const e1 = await new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => new Response('Bad Gateway', { status: 502 })).f }).health().catch((e) => e);
    expect(e1).toMatchObject({ status: 502, code: 'http_502', transient: true });
    const e2 = await new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => new Response('<html>', { status: 200 })).f }).health().catch((e) => e);
    expect(e2).toMatchObject({ kind: 'parse' });
  });

  it('429 honours Retry-After, then succeeds', async () => {
    const waits: number[] = [];
    const { f, calls } = stub((_u, n) => (n < 3 ? json({ error: { code: 'rate_limited', message: 'slow down' } }, 429, { 'retry-after': '2' }) : json({ ok: true })));
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f, sleep: async (ms) => void waits.push(ms) });
    expect(await ix.health()).toEqual({ ok: true });
    expect(calls).toHaveLength(3);
    expect(waits).toEqual([2000, 2000]);
  });

  it('gives up after maxRetries with the rate-limit error, caps the wait, backs off without a header', async () => {
    const waits: number[] = [];
    const { f, calls } = stub(() => json({ error: { code: 'rate_limited', message: 'slow down' } }, 429, { 'retry-after': '120' }));
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f, maxRetries: 2, maxBackoffMs: 5000, sleep: async (ms) => void waits.push(ms) });
    const err = await ix.health().catch((e) => e);
    expect(err).toMatchObject({ status: 429, code: 'rate_limited', retryAfterSec: 120, transient: true });
    expect(calls).toHaveLength(3);
    expect(waits).toEqual([5000, 5000]);
    const w2: number[] = [];
    const s2 = stub(() => json({}, 429));
    await new HttpIndexer({ baseUrl: 'http://x', fetch: s2.f, maxRetries: 3, sleep: async (ms) => void w2.push(ms) }).health().catch(() => undefined);
    expect(w2).toEqual([500, 1000, 2000]);
  });

  it('503 is retried only when the server says when (db_busy carries Retry-After), 500 never', async () => {
    const s1 = stub((_u, n) => (n === 1 ? json({ error: { code: 'db_busy', message: 'busy' } }, 503, { 'retry-after': '1' }) : json({ ok: 1 })));
    expect(await new HttpIndexer({ baseUrl: 'http://x', fetch: s1.f, sleep: async () => undefined }).health()).toEqual({ ok: 1 });
    const s2 = stub(() => json({ error: { code: 'overloaded', message: 'x' } }, 503));
    await expect(new HttpIndexer({ baseUrl: 'http://x', fetch: s2.f, sleep: async () => undefined }).health()).rejects.toMatchObject({ status: 503 });
    expect(s2.calls).toHaveLength(1);
    const s3 = stub(() => json({}, 500, { 'retry-after': '1' }));
    await expect(new HttpIndexer({ baseUrl: 'http://x', fetch: s3.f, sleep: async () => undefined }).health()).rejects.toMatchObject({ status: 500 });
    expect(s3.calls).toHaveLength(1);
  });

  it('times out, and cancels on the caller AbortSignal, including during a backoff wait', async () => {
    const hang = ((_i: unknown, init?: RequestInit) =>
      new Promise((_r, rej) => init?.signal?.addEventListener('abort', () => rej(new DOMException('aborted', 'AbortError'))))) as unknown as typeof fetch;
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: hang, timeoutMs: 20 });
    expect(await ix.health().catch((e) => e)).toMatchObject({ kind: 'timeout', code: 'timeout', transient: true });
    expect(await ix.health({ timeoutMs: 5 }).catch((e) => e)).toMatchObject({ kind: 'timeout' });

    const ctl = new AbortController();
    const p = new HttpIndexer({ baseUrl: 'http://x', fetch: hang, timeoutMs: 5000 }).health({ signal: ctl.signal });
    setTimeout(() => ctl.abort(), 10);
    expect(await p.catch((e) => e)).toMatchObject({ kind: 'aborted', code: 'aborted' });
    expect(await new HttpIndexer({ baseUrl: 'http://x', fetch: hang }).health({ signal: AbortSignal.abort() }).catch((e) => e)).toMatchObject({ kind: 'aborted' });

    // abort while waiting out a Retry-After (real abortable sleep)
    const busy = stub(() => json({}, 429, { 'retry-after': '30' }));
    const c2 = new AbortController();
    const p2 = new HttpIndexer({ baseUrl: 'http://x', fetch: busy.f }).health({ signal: c2.signal });
    setTimeout(() => c2.abort(), 20);
    expect(await p2.catch((e) => e)).toMatchObject({ kind: 'aborted' });
  });

  it('network failures are typed', async () => {
    const down = (async () => { throw new TypeError('fetch failed'); }) as unknown as typeof fetch;
    expect(await new HttpIndexer({ baseUrl: 'http://x', fetch: down }).tokens().catch((e) => e)).toMatchObject({ kind: 'network', status: 0, transient: true });
  });
});

describe('HttpIndexer.tokenUtxos', () => {
  it('degrades to null on 404 / 501 / 405 and remembers it for a while (no repeated 404s)', async () => {
    for (const status of [404, 501, 405]) {
      let t = 1_000;
      const { f, calls } = stub(() => json({ error: { code: 'not_found', message: 'no such route' } }, status));
      const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f, now: () => t, unsupportedTtlMs: 60_000 });
      expect(await ix.tokenUtxos({ owner: H1 })).toBeNull();
      expect(await ix.tokenUtxos({ owner: H1 })).toBeNull();
      expect(calls).toHaveLength(1);
      t += 61_000;
      expect(await ix.tokenUtxos({ owner: H1 })).toBeNull();
      expect(calls).toHaveLength(2);
    }
  });

  it('follows next_cursor across pages and stops at maxPages', async () => {
    const mk = (n: number) => ({ txid: H1, index: n, token: H2, owner: H1, amount: '5', value: '10', role: 'owned', spent: false });
    const { f, calls } = stub((u) => {
      const c = u.searchParams.get('cursor');
      return c === null ? json({ items: [mk(0), mk(1)], next_cursor: 'c1' }) : c === 'c1' ? json({ items: [mk(2)], next_cursor: 'c2' }) : json({ items: [mk(3)], next_cursor: null });
    });
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    expect((await ix.tokenUtxos({ owner: H1, token: H2 }))!.map((x) => x.index)).toEqual([0, 1, 2, 3]);
    expect(calls.map((u) => u.searchParams.get('cursor'))).toEqual([null, 'c1', 'c2']);
    const capped = stub(() => json({ items: [mk(0)], next_cursor: 'again' }));
    expect(await new HttpIndexer({ baseUrl: 'http://x', fetch: capped.f }).tokenUtxos({ owner: H1, maxPages: 3 })).toHaveLength(3);
    expect(capped.calls).toHaveLength(3);
  });

  it('returns the items when supported (envelope or bare array); other errors still throw', async () => {
    const item = { txid: H1, index: 0, token: H2, owner: H1, amount: '5', value: '10', role: 'owned', spent: false };
    expect(await new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({ items: [item] })).f }).tokenUtxos({ owner: H1 })).toEqual([item]);
    expect(await new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json([item])).f }).tokenUtxos({ owner: H1 })).toEqual([item]);
    expect(await new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({})).f }).tokenUtxos({ owner: H1 })).toEqual([]);
    await expect(new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({ error: { code: 'internal', message: 'x' } }, 500)).f }).tokenUtxos({ owner: H1 })).rejects.toMatchObject({ status: 500 });
  });
});

describe('HttpIndexer market data (trades, candles, stats, depth)', () => {
  const basis = '100000000';
  it('builds the contract URLs and returns the bodies', async () => {
    const trades = { token: H1, price_basis: basis, items: [{ id: 3, txid: H2, ts: 1, daa: 2, price: '5', amount: '6', quote: '7', side: 'buy', fills: 1, confirmations: 0, settled: false }], next_cursor: '3' };
    const candles = { token: H1, interval: '5m', price_basis: basis, items: [{ t: 0, o: '1', h: '2', l: '1', c: '2', volume: '3', quote_volume: '4', trades: 1 }] };
    const stats = { token: H1, price_basis: basis, ts: 5, last: null };
    const depth = { token: H1, price_basis: basis, ts: 5, daa: 6, bids: [], asks: [] };
    const { f, calls } = stub((u) =>
      json(u.pathname.startsWith('/v1/trades') ? trades : u.pathname.startsWith('/v1/candles') ? candles : u.pathname.startsWith('/v1/stats') ? stats : depth),
    );
    const ix = new HttpIndexer({ baseUrl: 'https://kob.example', fetch: f });
    expect(await ix.trades(H1, { limit: 20, before: 812 })).toEqual(trades);
    expect(await ix.candles(H1, { interval: '5m', from: 10, to: 20, limit: 300 })).toEqual(candles);
    expect(await ix.stats(H1)).toEqual(stats);
    expect(await ix.depth(H1, { levels: 40 })).toEqual(depth);
    await ix.trades(H1.toUpperCase());
    await ix.candles(H1, { interval: '1d' });
    expect(calls.map((u) => u.pathname + u.search)).toEqual([
      `/v1/trades/${H1}?limit=20&before=812`,
      `/v1/candles/${H1}?interval=5m&from=10&to=20&limit=300`,
      `/v1/stats/${H1}`,
      `/v1/depth/${H1}?levels=40`,
      `/v1/trades/${H1}`,
      `/v1/candles/${H1}?interval=1d`,
    ]);
  });

  it('refuses a bad token id or interval before any request', async () => {
    const { f, calls } = stub(() => json({}));
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    await expect(ix.trades('xyz')).rejects.toMatchObject({ kind: 'bad-request' });
    await expect(ix.candles(H1, { interval: '2m' as never })).rejects.toMatchObject({ kind: 'bad-request' });
    expect(calls).toHaveLength(0);
  });

  it('degrades to null (no data) when the indexer does not serve a route (404 / 405 / 501) or answers a malformed body', async () => {
    for (const status of [404, 405, 501]) {
      const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({ error: { code: 'not_found', message: 'no such route' } }, status)).f });
      expect(await ix.trades(H1)).toBeNull();
      expect(await ix.candles(H1, { interval: '1m' })).toBeNull();
      expect(await ix.stats(H1)).toBeNull();
      expect(await ix.depth(H1)).toBeNull();
    }
    const odd = new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({ nope: true })).f });
    expect(await odd.trades(H1)).toBeNull();
    expect(await odd.candles(H1, { interval: '1h' })).toBeNull();
    expect(await odd.stats(H1)).toBeNull();
    expect(await odd.depth(H1)).toBeNull();
  });

  it('still throws real failures (500, network)', async () => {
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({ error: { code: 'internal', message: 'x' } }, 500)).f });
    await expect(ix.stats(H1)).rejects.toMatchObject({ status: 500 });
    await expect(ix.depth(H1)).rejects.toMatchObject({ status: 500 });
  });
});

describe('feed url and channels', () => {
  it('derives the ws url from the indexer base', () => {
    expect(toFeedUrl('http://127.0.0.1:8080')).toBe('ws://127.0.0.1:8080/v1/ws');
    expect(toFeedUrl('https://kob.example/api/')).toBe('wss://kob.example/api/v1/ws');
    expect(toFeedUrl('wss://kob.example')).toBe('wss://kob.example/v1/ws');
    expect(new HttpIndexer({ baseUrl: 'https://kob.example/', fetch: (() => json({})) as never }).feedUrl).toBe('wss://kob.example/v1/ws');
  });
  it('accepts exactly the server channel names', () => {
    for (const c of ['health', 'reorg', 'fills', `fills:${H1}`, `book:${H2}`, `order:${H1}`]) expect(isValidChannel(c), c).toBe(true);
    for (const c of ['fill', 'book', 'book:abc', `orders:${H1}`, `book:${H1}00`, '']) expect(isValidChannel(c), c).toBe(false);
  });
});

// ------------------------------------------------------------------------------------------------ feed against a real ws server

interface Session { ws: WebSocket; received: Record<string, unknown>[] }

class TestServer {
  wss: WebSocketServer;
  ready: Promise<void>;
  sessions: Session[] = [];
  urls: string[] = [];
  constructor() {
    this.wss = new WebSocketServer({ port: 0, host: '127.0.0.1' });
    this.ready = new Promise<void>((r) => this.wss.once('listening', () => r()));
    this.wss.on('connection', (ws, req) => {
      this.urls.push(req.url ?? '');
      const s: Session = { ws, received: [] };
      this.sessions.push(s);
      ws.on('message', (data: RawData) => {
        const msg = JSON.parse(data.toString()) as { op: string; channels?: string[] };
        s.received.push(msg);
        if (msg.op === 'subscribe') ws.send(JSON.stringify({ type: 'subscribed', data: { channels: msg.channels, errors: [] } }));
        if (msg.op === 'ping') ws.send(JSON.stringify({ type: 'pong' }));
      });
    });
  }
  get port() { return (this.wss.address() as AddressInfo).port; }
  get base() { return `http://127.0.0.1:${this.port}`; }
  async stop() {
    for (const c of this.wss.clients) c.terminate();
    await new Promise<void>((r) => this.wss.close(() => r()));
  }
}

const servers: TestServer[] = [];
const feeds: IndexerFeed[] = [];
afterEach(async () => {
  for (const f of feeds.splice(0)) f.close();
  for (const s of servers.splice(0)) await s.stop();
});
const mkServer = async () => { const s = new TestServer(); servers.push(s); await s.ready; return s; };
const mkFeed = (base: string, extra: Record<string, unknown> = {}) => {
  const f = new IndexerFeed({ url: base, webSocket: WebSocket as unknown as WebSocketCtor, backoffBaseMs: 10, backoffMaxMs: 40, random: () => 1, pingIntervalMs: 0, ...extra });
  feeds.push(f);
  return f;
};
const until = async (cond: () => boolean, ms = 3000) => {
  const t0 = Date.now();
  while (!cond()) {
    if (Date.now() - t0 > ms) throw new Error('condition not met in time');
    await new Promise((r) => setTimeout(r, 5));
  }
};
function record(feed: IndexerFeed) {
  const log: { ev: string; v: unknown }[] = [];
  for (const ev of ['open', 'close', 'health', 'reorg', 'fill', 'book', 'order', 'resync', 'subscribed', 'error'] as (keyof FeedEventMap)[]) {
    feed.on(ev, (v) => void log.push({ ev, v }));
  }
  return log;
}

describe('IndexerFeed', () => {
  it('connects to /v1/ws, subscribes (before and after open) and dispatches typed events', async () => {
    const srv = await mkServer();
    const feed = mkFeed(srv.base);
    const log = record(feed);
    feed.subscribe(['fills', `book:${H1}`]); // queued until open
    feed.connect();
    await feed.whenOpen();
    await until(() => srv.sessions[0]!.received.length >= 1);
    expect(srv.urls[0]).toBe('/v1/ws');
    expect(srv.sessions[0]!.received[0]).toEqual({ op: 'subscribe', channels: ['fills', `book:${H1}`] });
    feed.subscribe([`order:${H2}`, 'fills']); // 'fills' is already wanted: only the new channel is sent
    await until(() => srv.sessions[0]!.received.length >= 2);
    expect(srv.sessions[0]!.received[1]).toEqual({ op: 'subscribe', channels: [`order:${H2}`] });
    expect(feed.channels()).toEqual(['fills', `book:${H1}`, `order:${H2}`].sort());

    const fill = { order: H1, token: H2, side: 1, price: 500, amount: 2000, payout: 990, txid: H1, block: H2, daa: 42 };
    const ws = srv.sessions[0]!.ws;
    const push = (o: unknown) => ws.send(JSON.stringify(o));
    push({ channel: 'fills', type: 'fill', data: fill });
    push({ channel: `book:${H1}`, type: 'book', data: { token: H1, cursor_daa: 43 } });
    push({ channel: `order:${H2}`, type: 'order', data: { covenant_id: H2, cursor_daa: 44 } });
    push({ channel: 'reorg', type: 'reorg', data: { reverted_blocks: 2, added_blocks: 3, cursor_daa: 45 } });
    push({ channel: 'health', type: 'cursor', data: { cursor_hash: null, cursor_daa: 46, added_blocks: 1, reverted_blocks: 0 } });
    push({ channel: 'health', type: 'health', data: { state: 'following', cursor_daa: 46, node_daa: 50, lag_daa: 4 } });
    push({ type: 'resync', data: { missed_events: 17 } });
    push({ type: 'error', data: { message: 'subscription limit of 3 reached' } });
    await until(() => log.some((l) => l.ev === 'error'));

    const by = (ev: string) => log.filter((l) => l.ev === ev).map((l) => l.v);
    expect(by('fill')).toEqual([{ channel: 'fills', fill }]);
    expect(by('book')).toEqual([{ token: H1, cursor_daa: 43 }]);
    expect(by('order')).toEqual([{ covenant_id: H2, cursor_daa: 44 }]);
    expect(by('reorg')).toEqual([{ reverted_blocks: 2, added_blocks: 3, cursor_daa: 45 }]);
    expect(by('health')).toHaveLength(2);
    expect((by('health')[1] as { data: { lag_daa: number } }).data.lag_daa).toBe(4);
    expect(by('resync')).toEqual([{ reason: 'server', missedEvents: 17 }]);
    expect(by('error')).toEqual([{ message: 'subscription limit of 3 reached' }]);
    expect(by('subscribed').length).toBeGreaterThanOrEqual(2);
  });

  it('ignores malformed and unknown frames and keeps working; a throwing listener does not break the feed', async () => {
    const srv = await mkServer();
    const feed = mkFeed(srv.base);
    const got: unknown[] = [];
    feed.on('book', () => { throw new Error('listener bug'); });
    feed.on('book', (v) => void got.push(v));
    feed.connect();
    await feed.whenOpen();
    await until(() => srv.sessions.length === 1);
    const ws = srv.sessions[0]!.ws;
    ws.send('not json');
    ws.send('"a string"');
    ws.send(JSON.stringify({ channel: 'x' }));
    ws.send(JSON.stringify({ type: 'pong' }));
    ws.send(JSON.stringify({ channel: `book:${H1}`, type: 'book', data: { token: H1, cursor_daa: 1 } }));
    await until(() => got.length === 1);
    expect(feed.status).toBe('open');
  });

  it('reconnects with backoff after a drop, re-subscribes to everything still wanted and emits resync(reconnect)', async () => {
    const srv = await mkServer();
    const feed = mkFeed(srv.base);
    const log = record(feed);
    const statuses: string[] = [];
    feed.on('status', (s) => statuses.push(s.status));
    feed.subscribe(['health', `book:${H1}`, `order:${H2}`]);
    feed.connect();
    await feed.whenOpen();
    await until(() => srv.sessions[0]!.received.length === 1);
    feed.unsubscribe([`order:${H2}`]);
    await until(() => srv.sessions[0]!.received.length === 2);
    expect(srv.sessions[0]!.received[1]).toEqual({ op: 'unsubscribe', channels: [`order:${H2}`] });

    srv.sessions[0]!.ws.terminate();
    await until(() => srv.sessions.length === 2 && log.some((l) => l.ev === 'resync'));
    await until(() => srv.sessions[1]!.received.length >= 1);
    expect(srv.sessions[1]!.received[0]).toEqual({ op: 'subscribe', channels: ['health', `book:${H1}`] });
    expect(log.filter((l) => l.ev === 'open').map((l) => l.v)).toEqual([{ reconnect: false }, { reconnect: true }]);
    expect(log.find((l) => l.ev === 'close')!.v).toMatchObject({ willReconnect: true });
    expect(log.filter((l) => l.ev === 'resync').map((l) => l.v)).toEqual([{ reason: 'reconnect' }]);
    expect(statuses).toEqual(['connecting', 'open', 'reconnecting', 'reconnecting', 'open']);
  });

  it('keeps retrying with growing attempts while the server is down, then recovers', async () => {
    const probe = new WebSocketServer({ port: 0, host: '127.0.0.1' });
    await new Promise<void>((r) => probe.once('listening', () => r()));
    const port = (probe.address() as AddressInfo).port;
    await new Promise<void>((r) => probe.close(() => r()));
    const feed = mkFeed(`http://127.0.0.1:${port}`);
    const attempts: number[] = [];
    feed.on('status', (s) => s.status === 'reconnecting' && attempts.push(s.attempt));
    feed.subscribe(['reorg']);
    feed.connect();
    await until(() => attempts.length >= 4);
    expect(attempts.slice(0, 4)).toEqual([1, 2, 3, 4]);
    const srv = await mkServer(); // the port cannot be rebound reliably: point a new feed at a live server instead
    const feed2 = mkFeed(srv.base);
    feed2.subscribe(['reorg']);
    feed2.connect();
    await feed2.whenOpen();
    feed.close();
    expect(feed.status).toBe('closed');
  });

  it('close() stops reconnecting for good; connect() can restart it', async () => {
    const srv = await mkServer();
    const feed = mkFeed(srv.base);
    feed.connect();
    await feed.whenOpen();
    feed.close();
    expect(feed.status).toBe('closed');
    await new Promise((r) => setTimeout(r, 120));
    expect(srv.sessions).toHaveLength(1);
    expect(srv.sessions[0]!.ws.readyState).not.toBe(WebSocket.OPEN);
    feed.connect();
    await feed.whenOpen();
    expect(srv.sessions).toHaveLength(2);
  });

  it('sends a client keepalive ping and validates channel names', async () => {
    const srv = await mkServer();
    const feed = mkFeed(srv.base, { pingIntervalMs: 20 });
    expect(() => feed.subscribe(['nope'])).toThrow(/invalid feed channel/);
    expect(() => feed.subscribe([`book:${'g'.repeat(64)}`])).toThrow(/invalid/);
    feed.connect();
    await feed.whenOpen();
    await until(() => srv.sessions[0]!.received.filter((m) => m.op === 'ping').length >= 2);
  });

  it('splits a large subscription into several messages (server messages are capped at 4 KiB)', async () => {
    const srv = await mkServer();
    const feed = mkFeed(srv.base);
    const chans = Array.from({ length: 60 }, (_, i) => `order:${i.toString(16).padStart(64, '0')}`);
    feed.connect();
    await feed.whenOpen();
    feed.subscribe(chans);
    await until(() => srv.sessions[0]!.received.length === 3);
    const sizes = srv.sessions[0]!.received.map((m) => (m.channels as string[]).length);
    expect(sizes).toEqual([24, 24, 12]);
    expect(Math.max(...srv.sessions[0]!.received.map((m) => JSON.stringify(m).length))).toBeLessThan(4096);
  });

  it('whenOpen rejects on timeout; a missing WebSocket implementation is reported', async () => {
    const feed = mkFeed('http://127.0.0.1:1', { backoffBaseMs: 1000 });
    await expect(feed.whenOpen(30)).rejects.toThrow(/did not open/);
    const g = globalThis as { WebSocket?: unknown };
    const saved = g.WebSocket;
    g.WebSocket = undefined;
    try {
      expect(() => new IndexerFeed({ url: 'http://x' })).toThrow(/no WebSocket/);
    } finally {
      g.WebSocket = saved;
    }
  });
});

describe('token/token pair routes', () => {
  it('GET /v1/pairs/{base}/{quote}/book and /v1/pairs?token=; 404 / 405 / 501 resolve to null (pair book unavailable)', async () => {
    const book = {
      base: H1, quote: H2, daa_score: 5,
      asks: [{ source: 'direct', price_num: '1', price_den: '1', amount: '10', orders: 1 }, { source: 'entry', price_num: '2', price_den: '1', amount: '5', orders: 1 }, { source: 'route', price_num: '53', price_den: '1000', amount: '100', orders: 0 }],
      bids: [],
    };
    const summary = { base: H1, quote: H2, direct_asks: 1, direct_bids: 0, entry_asks: 2, entry_bids: 0, conditionals: 3 };
    const { f, calls } = stub((u) => (u.pathname.endsWith('/book') ? json(book) : json([summary, { base: 'zz', quote: H2 }])));
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    expect(await ix.pairBook(H1, H2, { depth: 20 })).toEqual(book);
    expect(await ix.pairs(H1)).toEqual([summary]);
    expect(calls.map((c) => `${c.pathname}${c.search}`)).toEqual([`/v1/pairs/${H1}/${H2}/book?depth=20`, `/v1/pairs?token=${H1}`]);
    for (const status of [404, 405, 501]) {
      const ix2 = new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({ error: { code: 'not_found', message: 'no' } }, status)).f });
      expect(await ix2.pairBook(H1, H2), String(status)).toBeNull();
      expect(await ix2.pairs(), String(status)).toBeNull();
    }
    await expect(new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({}, 500)).f, maxRetries: 0 }).pairBook(H1, H2)).rejects.toMatchObject({ status: 500 });
    await expect(ix.pairBook('nothex', H2)).rejects.toMatchObject({ kind: 'bad-request' });
    await expect(ix.pairBook(H1, H1)).rejects.toMatchObject({ kind: 'bad-request' });
  });

  it('GET /v1/pairs/{base}/{quote}/candles and /fills (5.4): rates from the two KAS series, fills as volume; unsupported -> null', async () => {
    const rate = (v: string) => ({ value: v, num: v, den: '1' });
    const candles = {
      base: H1, quote: H2, interval: '1h', price_basis: '1000', quote_price_basis: '100', decimals: 3, quote_decimals: 2, price_source: 'kas_books',
      items: [{ t: 3_600_000, o: rate('10'), h: rate('12'), l: rate('9'), c: rate('11'), a_traded: true, b_traded: false, pair_volume_a: '5', pair_volume_b: '50', pair_fills: 1 }],
    };
    const fill = {
      id: 7, txid: H1, ts: 1, daa: 2, order: H2, contract: 'KobPair', side: 'ask', amount_a: '4000', amount_b: '4000', price: '1000', price_num: '1', price_den: '1',
      a_scale: 1000, tip_kas: '0', counterparty: 'netting', price_source: 'none', confirmations: 3, settled: false,
    };
    const fills = { base: H1, quote: H2, volume_24h: { amount_a: '4000', amount_b: '4000', fills: 1 }, items: [fill], next_cursor: '7' };
    const { f, calls } = stub((u) => (u.pathname.endsWith('/candles') ? json(candles) : json(fills)));
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: f });
    expect(await ix.pairCandles(H1, H2, { interval: '1h', limit: 10 })).toEqual(candles);
    expect(await ix.pairFills(H1, H2, { limit: 5, before: 9 })).toEqual(fills);
    expect(calls.map((c) => `${c.pathname}${c.search}`)).toEqual([`/v1/pairs/${H1}/${H2}/candles?interval=1h&limit=10`, `/v1/pairs/${H1}/${H2}/fills?limit=5&before=9`]);
    await expect(ix.pairCandles(H1, H2, { interval: '2h' as never })).rejects.toMatchObject({ kind: 'bad-request' });
    for (const status of [404, 405, 501]) {
      const ix2 = new HttpIndexer({ baseUrl: 'http://x', fetch: stub(() => json({ error: { code: 'not_found', message: 'no' } }, status)).f });
      expect(await ix2.pairCandles(H1, H2, { interval: '1m' }), String(status)).toBeNull();
      expect(await ix2.pairFills(H1, H2), String(status)).toBeNull();
    }
  });
});
