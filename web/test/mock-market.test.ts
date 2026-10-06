// Market-data endpoints of the mock indexer (mock/market.mjs): /v1/trades, /v1/candles, /v1/stats, /v1/depth, and the seeded price history.
// Every expected figure below is derived by hand from the explicit trades / orders seeded in the test (8 decimals: scale = price_basis =
// 1e8 base units, prices are sompi per whole token).
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { startMockServer, type MockServer } from '../mock/server.mjs';

const MKT = 'ab'.repeat(32);
const EMPTY = 'ef'.repeat(32);
const NODEC = 'cd'.repeat(32);
const HOUR = 3_600_000;

interface Res { status: number; body: any }
const getOn = async (srv: MockServer, path: string): Promise<Res> => {
  const r = await fetch(srv.url + path);
  return { status: r.status, body: await r.json() };
};
const postOn = async (srv: MockServer, path: string, body: unknown = {}): Promise<Res> => {
  const r = await fetch(srv.url + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  return { status: r.status, body: await r.json() };
};

/** Every trade of a token, newest first, following `next_cursor`. */
async function allTrades(srv: MockServer, token: string): Promise<any[]> {
  const out: any[] = [];
  let cursor: string | null = null;
  do {
    const r: Res = await getOn(srv, `/v1/trades/${token}?limit=200${cursor ? `&before=${cursor}` : ''}`);
    expect(r.status).toBe(200);
    out.push(...r.body.items);
    cursor = r.body.next_cursor;
  } while (cursor);
  return out;
}

describe('mock market data (explicit trades)', () => {
  let srv: MockServer;
  const get = (p: string) => getOn(srv, p);
  let H = 0; // an hour boundary 3-5 h ago, not the last hour of a UTC day (so T1..T5 share one 1d bucket)
  let T0 = 0;

  beforeAll(async () => {
    srv = await startMockServer({ port: 0, seed: false });
    const now = Date.now();
    H = Math.floor((now - 3 * HOUR) / HOUR) * HOUR;
    if (new Date(H).getUTCHours() === 23) H -= HOUR;
    T0 = now - 30 * HOUR;
    const tok = (ticker: string, covenant_id: string, decimals: number | null = 8) => ({ ticker, covenant_id, decimals });
    const r = await postOn(srv, '/mock/seed', {
      tokens: [tok('MKT', MKT), tok('EMPTY', EMPTY), tok('NODEC', NODEC, null)],
      trades: [
        // T0: outside the 24 h window
        { token: MKT, ts: T0, legs: [{ side: 'ask', price: 2400000, amount: 100000000 }] },
        // T1: a resting ask taken directly -> buy 3 tokens at 2_500_000
        { token: MKT, ts: H + 10_000, legs: [{ side: 'ask', price: 2500000, amount: 300000000 }] },
        // T2: resting bid (older) 2_490_000 + aggressor ask 2_480_000 -> sell at the bid's 2_490_000; amount / quote from the ask side
        { token: MKT, ts: H + 50_000, legs: [{ side: 'bid', price: 2490000, amount: 200000000, age_daa: 500 }, { side: 'ask', price: 2480000, amount: 200000000, age_daa: 0, maker: 'carol' }] },
        // T3: two resting asks in one tx -> VWAP (2.5e6 + 5.02e6) * 1e8 / 3e8 = 2_506_666.67 -> floor 2_506_666
        { token: MKT, ts: H + 70_000, legs: [{ side: 'ask', price: 2500000, amount: 100000000 }, { side: 'ask', price: 2510000, amount: 200000000 }] },
        // T4: resting ask (older) 2_510_000 + aggressor bid 2_530_000 -> buy at 2_510_000
        { token: MKT, ts: H + 600_000, legs: [{ side: 'bid', price: 2530000, amount: 100000000, age_daa: 0, maker: 'carol' }, { side: 'ask', price: 2510000, amount: 100000000, age_daa: 3000 }] },
        // T5: a resting bid taken directly -> sell 4 tokens at 2_470_000
        { token: MKT, ts: H + HOUR + 5_000, legs: [{ side: 'bid', price: 2470000, amount: 400000000 }] },
        // no decimals (no standard scale): the basis is the scale of the token's first order (10000)
        { token: NODEC, ts: H, legs: [{ side: 'ask', price: 300, amount: 10000, scale: 10000 }] },
      ],
      orders: [
        { token: MKT, side: 'ask', price: 2520000, amount: 200000000 },
        { token: MKT, side: 'ask', price: 2520000, amount: 300000000 },
        { token: MKT, side: 'ask', price: 2540000, amount: 100000000 },
        { token: MKT, side: 'ask', price: 2540000, amount: 100000000 }, // the same price: one level of 2 orders
        { token: MKT, side: 'bid', price: 2480000, amount: 200000000 },
        { token: MKT, side: 'bid', price: 2460000, amount: 500000000 },
      ],
    });
    expect(r.status).toBe(200);
  });
  afterAll(async () => {
    await srv?.close();
  });

  it('groups fills by transaction: newest first, aggressor side, resting-side price, ask-side amount / quote', async () => {
    const r = await get(`/v1/trades/${MKT}`);
    expect(r.status).toBe(200);
    expect(r.body).toMatchObject({ token: MKT, price_basis: '100000000', next_cursor: null });
    const items = r.body.items;
    expect(items.map((t: any) => [t.price, t.amount, t.quote, t.side, t.fills])).toEqual([
      ['2470000', '400000000', '9880000', 'sell', 1],
      ['2510000', '100000000', '2510000', 'buy', 2],
      ['2506666', '300000000', '7520000', 'buy', 2],
      ['2490000', '200000000', '4960000', 'sell', 2],
      ['2500000', '300000000', '7500000', 'buy', 1],
      ['2400000', '100000000', '2400000', 'buy', 1],
    ]);
    expect(items.map((t: any) => t.ts)).toEqual([H + HOUR + 5_000, H + 600_000, H + 70_000, H + 50_000, H + 10_000, T0]);
    for (let i = 1; i < items.length; i++) expect(items[i].id).toBeLessThan(items[i - 1].id);
    for (const t of items) {
      expect(t.txid).toMatch(/^[0-9a-f]{64}$/);
      expect(t.settled).toBe(true);
      // 10 DAA per second: the trade's DAA sits (now - ts) / 100 below the node
      expect(Math.abs(t.confirmations - (Date.now() - t.ts) / 100)).toBeLessThan(50);
    }
    // the trade id is the smallest fill event id of the transaction
    const fills = (await get(`/v1/fills?token=${MKT}&limit=200`)).body.items.filter((f: any) => f.txid === items[1].txid);
    expect(fills).toHaveLength(2);
    expect(items[1].id).toBe(Math.min(...fills.map((f: any) => f.id)));
  });

  it('pages trades with limit / before and validates the token', async () => {
    const p1 = (await get(`/v1/trades/${MKT}?limit=2`)).body;
    expect(p1.items).toHaveLength(2);
    expect(p1.next_cursor).toBe(String(p1.items[1].id));
    const p2 = (await get(`/v1/trades/${MKT}?limit=3&before=${p1.next_cursor}`)).body;
    expect(p2.items.map((t: any) => t.price)).toEqual(['2506666', '2490000', '2500000']);
    const p3 = (await get(`/v1/trades/${MKT}?limit=3&before=${p2.next_cursor}`)).body;
    expect(p3.items.map((t: any) => t.price)).toEqual(['2400000']);
    expect(p3.next_cursor).toBeNull();
    expect((await get(`/v1/trades/${MKT.toUpperCase()}?limit=1`)).body.items).toHaveLength(1);

    expect((await get(`/v1/trades/${EMPTY}`)).body).toEqual({ token: EMPTY, price_basis: '100000000', decimals: 8, items: [], next_cursor: null });
    const nodec = (await get(`/v1/trades/${NODEC}`)).body;
    expect(nodec).toMatchObject({ price_basis: '10000', decimals: null });
    expect(nodec.items[0]).toMatchObject({ price: '300', amount: '10000', quote: '300', side: 'buy' });

    const unknown = await get(`/v1/trades/${'12'.repeat(32)}`);
    expect(unknown.status).toBe(404);
    expect(unknown.body.error.code).toBe('not_found');
    for (const [path, status] of [
      ['/v1/trades/xyz', 400],
      [`/v1/trades/${MKT}?limit=0`, 400],
      [`/v1/trades/${MKT}?before=x`, 400],
      ['/v1/candles/zz?interval=1m', 400],
      [`/v1/stats/${'12'.repeat(32)}`, 404],
      [`/v1/depth/${'12'.repeat(32)}`, 404],
    ] as const) {
      const e = await get(path);
      expect(e.status, path).toBe(status);
      expect(e.body.error.code, path).toBe(status === 400 ? 'bad_request' : 'not_found');
    }
  });

  it('builds OHLCV candles per interval, omits empty buckets and honours from / to / limit', async () => {
    const c = (q: string) => get(`/v1/candles/${MKT}?${q}`);
    const m1 = (await c('interval=1m')).body;
    expect(m1).toMatchObject({ token: MKT, interval: '1m', price_basis: '100000000' });
    const t0bucket = Math.floor(T0 / 60_000) * 60_000;
    expect(m1.items).toEqual([
      { t: t0bucket, o: '2400000', h: '2400000', l: '2400000', c: '2400000', volume: '100000000', quote_volume: '2400000', trades: 1 },
      { t: H, o: '2500000', h: '2500000', l: '2490000', c: '2490000', volume: '500000000', quote_volume: '12460000', trades: 2 },
      { t: H + 60_000, o: '2506666', h: '2506666', l: '2506666', c: '2506666', volume: '300000000', quote_volume: '7520000', trades: 1 },
      { t: H + 600_000, o: '2510000', h: '2510000', l: '2510000', c: '2510000', volume: '100000000', quote_volume: '2510000', trades: 1 },
      { t: H + HOUR, o: '2470000', h: '2470000', l: '2470000', c: '2470000', volume: '400000000', quote_volume: '9880000', trades: 1 },
    ]);
    const m5 = (await c('interval=5m')).body.items;
    expect(m5.map((x: any) => x.t)).toEqual([Math.floor(T0 / 300_000) * 300_000, H, H + 600_000, H + HOUR]);
    expect(m5[1]).toEqual({ t: H, o: '2500000', h: '2506666', l: '2490000', c: '2506666', volume: '800000000', quote_volume: '19980000', trades: 3 });
    const h1 = (await c('interval=1h')).body.items;
    expect(h1.slice(1)).toEqual([
      { t: H, o: '2500000', h: '2510000', l: '2490000', c: '2510000', volume: '900000000', quote_volume: '22490000', trades: 4 },
      { t: H + HOUR, o: '2470000', h: '2470000', l: '2470000', c: '2470000', volume: '400000000', quote_volume: '9880000', trades: 1 },
    ]);
    const d1 = (await c('interval=1d')).body.items;
    const day = Math.floor(H / 86_400_000) * 86_400_000;
    expect(d1[d1.length - 1]).toMatchObject({ t: day, o: '2500000', h: '2510000', l: '2470000', c: '2470000', volume: '1300000000', trades: 5 });

    // default: the LAST `limit` buckets; with `from`: the first `limit` buckets at or after it; `to` exclusive (on the bucket start)
    expect((await c('interval=1m&limit=2')).body.items.map((x: any) => x.t)).toEqual([H + 600_000, H + HOUR]);
    expect((await c(`interval=1m&from=${H + 60_000}`)).body.items.map((x: any) => x.t)).toEqual([H + 60_000, H + 600_000, H + HOUR]);
    expect((await c(`interval=1m&from=${H + 1}&limit=1`)).body.items.map((x: any) => x.t)).toEqual([H + 60_000]);
    expect((await c(`interval=1m&from=${H}&to=${H + 600_000}`)).body.items.map((x: any) => x.t)).toEqual([H, H + 60_000]);
    expect((await c(`interval=1m&to=${H}`)).body.items.map((x: any) => x.t)).toEqual([t0bucket]);
    expect((await c('interval=1h&limit=5000')).status).toBe(200); // capped at 1500, not an error

    for (const q of ['interval=2m', 'interval=', '', 'interval=1m&from=abc', 'interval=1m&limit=-1']) {
      const r = await c(q);
      expect(r.status, q).toBe(400);
      expect(r.body.error.code).toBe('bad_request');
    }
    expect((await get(`/v1/candles/${EMPTY}?interval=1h`)).body.items).toEqual([]);
  });

  it('serves 24 h stats relative to the chain time with the book top', async () => {
    const s = (await get(`/v1/stats/${MKT}`)).body;
    expect(Math.abs(s.ts - Date.now())).toBeLessThan(5_000);
    expect(s).toEqual({
      token: MKT,
      price_basis: '100000000',
      decimals: 8,
      ts: s.ts,
      last: '2470000',
      last_ts: H + HOUR + 5_000,
      last_side: 'sell',
      open_24h: '2500000', // T0 (30 h ago) is outside the window
      high_24h: '2510000',
      low_24h: '2470000',
      change_24h_bps: -120, // (2_470_000 - 2_500_000) * 10000 / 2_500_000
      volume_24h: '1300000000',
      quote_volume_24h: '32370000',
      trades_24h: 5,
      best_bid: '2480000',
      best_ask: '2520000',
      mid: '2500000',
      spread_bps: 160, // 40_000 * 10000 / 2_500_000
      open_asks: 4,
      open_bids: 2,
    });
    // moving the chain clock 30 h forward empties the window: volumes 0, window prices null, last stays
    await postOn(srv, '/mock/advance-daa', { seconds: 30 * 3600 });
    const later = (await get(`/v1/stats/${MKT}`)).body;
    expect(later).toMatchObject({ last: '2470000', open_24h: null, high_24h: null, low_24h: null, change_24h_bps: null, volume_24h: '0', quote_volume_24h: '0', trades_24h: 0 });
  });

  it('serves null stats for a known token without trades or book', async () => {
    const s = (await get(`/v1/stats/${EMPTY}`)).body;
    expect(s).toMatchObject({
      token: EMPTY,
      price_basis: '100000000',
      last: null,
      last_ts: null,
      last_side: null,
      open_24h: null,
      high_24h: null,
      low_24h: null,
      change_24h_bps: null,
      volume_24h: '0',
      quote_volume_24h: '0',
      trades_24h: 0,
      best_bid: null,
      best_ask: null,
      mid: null,
      spread_bps: null,
      open_asks: 0,
      open_bids: 0,
    });
  });

  it('serves depth merged by per-basis price, best first, with cumulative sums', async () => {
    const d = (await get(`/v1/depth/${MKT}`)).body;
    expect(d.token).toBe(MKT);
    expect(d.price_basis).toBe('100000000');
    expect(typeof d.ts).toBe('number');
    expect(typeof d.daa).toBe('number');
    expect(d.asks).toEqual([
      { price: '2520000', amount: '500000000', orders: 2, cum_amount: '500000000', cum_quote: '12600000', estimated: false },
      // two orders at 2_540_000: one level of 2 orders
      { price: '2540000', amount: '200000000', orders: 2, cum_amount: '700000000', cum_quote: '17680000', estimated: false },
    ]);
    expect(d.bids).toEqual([
      { price: '2480000', amount: '200000000', orders: 1, cum_amount: '200000000', cum_quote: '4960000', estimated: true },
      { price: '2460000', amount: '500000000', orders: 1, cum_amount: '700000000', cum_quote: '17260000', estimated: true },
    ]);
    const one = (await get(`/v1/depth/${MKT}?levels=1`)).body;
    expect([one.asks.length, one.bids.length]).toEqual([1, 1]);
    expect((await get(`/v1/depth/${MKT}?levels=0`)).status).toBe(400);
    expect((await get(`/v1/depth/${EMPTY}`)).body).toMatchObject({ asks: [], bids: [] });
  });
});

describe('mock market data (default seed and seeded history)', () => {
  const servers: MockServer[] = [];
  const start = async (opts: Parameters<typeof startMockServer>[0]) => {
    const s = await startMockServer({ port: 0, ...opts });
    servers.push(s);
    return s;
  };
  afterAll(async () => {
    await Promise.all(servers.map((s) => s.close()));
  });
  const tokenOf = (s: MockServer) => [...s.chain.tokens.keys()][0] as string;

  it('the default seed has exactly its 8 single-fill trades and no history', async () => {
    const s = await start({});
    const items = await allTrades(s, tokenOf(s));
    expect(items).toHaveLength(8);
    expect(items.every((t) => t.fills === 1)).toBe(true);
    // ask fills are taker buys, bid fills taker sells; the per-basis price is the fill price (scale = basis)
    const fills = (await getOn(s, `/v1/fills?token=${tokenOf(s)}&limit=50`)).body.items;
    for (const t of items) {
      const f = fills.find((x: any) => x.txid === t.txid);
      expect(t.side).toBe(f.side === 1 ? 'buy' : 'sell');
      expect(t.price).toBe(f.price);
    }
    const st = (await getOn(s, `/v1/stats/${tokenOf(s)}`)).body;
    expect(st).toMatchObject({ best_ask: '2510000', best_bid: '2490000', mid: '2500000', spread_bps: 80, open_asks: 11, open_bids: 11, trades_24h: 8 });
  });

  it('seeds a deterministic, realistic history over the requested span', async () => {
    const spec = { seed: 7, trades: 300, hours: 24 };
    const [a, b, c] = await Promise.all([start({ history: spec }), start({ history: spec }), start({ history: { ...spec, seed: 8 } })]);
    const ta = await allTrades(a, tokenOf(a));
    const tb = await allTrades(b, tokenOf(b));
    const tc = await allTrades(c, tokenOf(c));
    expect(ta).toHaveLength(308);
    const proj = (xs: any[]) => xs.map((t) => [t.txid, t.price, t.amount, t.quote, t.side, t.fills]);
    expect(proj(tb)).toEqual(proj(ta));
    expect(proj(tc)).not.toEqual(proj(ta));

    // chain order: ids ascend with time (events re-sorted after seeding in the past); the default seed's 8 fills are the newest
    const asc = [...ta].reverse();
    for (let i = 1; i < asc.length; i++) expect(asc[i].ts).toBeGreaterThanOrEqual(asc[i - 1].ts);
    const now = Date.now();
    expect(asc[0].ts).toBeGreaterThan(now - 24 * HOUR - 5_000);
    expect(asc[0].ts).toBeLessThan(now - 22 * HOUR);
    expect(asc[asc.length - 9].ts).toBeLessThan(ta[7].ts);

    const hist = ta.slice(8);
    expect(hist.some((t) => t.fills === 2)).toBe(true);
    expect(hist.filter((t) => t.side === 'buy').length).toBeGreaterThan(50);
    expect(hist.filter((t) => t.side === 'sell').length).toBeGreaterThan(50);
    expect(hist.every((t) => BigInt(t.price) % 10_000n === 0n)).toBe(true); // tick 0.0001 KAS per whole token
    const whole = hist.map((t) => Number(BigInt(t.amount) / 100_000_000n));
    expect(Math.min(...whole)).toBe(1);
    expect(Math.max(...whole)).toBeGreaterThan(20);
    const prices = hist.map((t) => Number(t.price));
    expect(new Set(prices).size).toBeGreaterThan(5);
    // the walk ends at the default mid, so the chart joins the seeded book
    expect(Math.abs(prices[0] - 2_500_000)).toBeLessThanOrEqual(20_000);

    const hours = (await getOn(a, `/v1/candles/${tokenOf(a)}?interval=1h`)).body.items;
    expect(hours.length).toBeGreaterThanOrEqual(20);
    expect(hours.reduce((n: number, x: any) => n + x.trades, 0)).toBe(308);
    const stats = (await getOn(a, `/v1/stats/${tokenOf(a)}`)).body;
    expect(stats.trades_24h).toBe(ta.filter((t) => t.ts >= stats.ts - 24 * HOUR).length);
    expect(stats.trades_24h).toBeGreaterThan(300);
    expect(stats.change_24h_bps).toBe(Number(((BigInt(stats.last) - BigInt(stats.open_24h)) * 10_000n) / BigInt(stats.open_24h)));
  });

  it('seeds history through the control API (POST /mock/seed and /mock/reset)', async () => {
    const s = await start({});
    const r = await postOn(s, '/mock/seed', { history: { trades: 40, hours: 2, seed: 3 } });
    expect(r.status).toBe(200);
    expect(r.body.history).toMatchObject({ token: tokenOf(s), trades: 40 });
    expect(r.body.history.fills).toBeGreaterThanOrEqual(40);
    expect(r.body.history.to_ts - r.body.history.from_ts).toBeGreaterThan(HOUR);
    expect(await allTrades(s, tokenOf(s))).toHaveLength(48);
    await postOn(s, '/mock/reset', { history: { trades: 10, hours: 1 } });
    expect(await allTrades(s, tokenOf(s))).toHaveLength(18);
    await postOn(s, '/mock/reset', {});
    expect(await allTrades(s, tokenOf(s))).toHaveLength(8);
    const bad = await postOn(s, '/mock/seed', { history: { trades: 0 } });
    expect(bad.status).toBe(400);
    const none = await start({ seed: false });
    expect((await postOn(none, '/mock/seed', { history: true })).status).toBe(400);
  });
});
