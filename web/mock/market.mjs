// Market-data views of the mock indexer: trades, candles, 24 h stats and depth, all derived from the fill events and the listed
// book of the MockChain. Contract: the indexer market-data API (trades = all fill events of ONE transaction on ONE token; every price is
// sompi per `price_basis` token base units = the token's standard scale, i.e. sompi per whole token; every view also carries `decimals`;
// bigint arithmetic, floor division, as crates/kob-executor/src/indexer/market.rs).
import { termsOf } from './model.mjs';
import { badRequest, notFound } from './util.mjs';
import { MAX_PAGE_SIZE, bookView, ctxOf, intParam, limitParam, parseHash, tokensView } from './views.mjs';

export const INTERVALS = { '1m': 60_000, '5m': 300_000, '1h': 3_600_000, '1d': 86_400_000 };
export const MAX_CANDLES = 1500;
const DAY_MS = 86_400_000;

const conf = (ctx, daa) => (ctx.nodeDaa === null ? null : Math.max(0, ctx.nodeDaa - daa));
const settled = (ctx, daa) => {
  const c = conf(ctx, daa);
  return c !== null && c >= ctx.settle;
};
const str = (v) => (v === null || v === undefined ? null : String(v));
const minB = (a, b) => (a < b ? a : b);
const maxB = (a, b) => (a > b ? a : b);

/** The allowlisted token behind a `{token}` path segment: 400 for malformed hex, 404 for a token the indexer does not know. */
function tokenOf(chain, id) {
  const token = parseHash('token', id);
  const tok = chain.tokens.get(token);
  if (!tok) throw notFound('unknown token');
  return tok;
}

/** Aggregated levels of the LISTED book (the `/v1/books` view, all levels up to the page cap). */
function listedLevels(chain, token) {
  const b = bookView(chain, token, new URLSearchParams({ depth: String(MAX_PAGE_SIZE), aggregate: 'true' }));
  return { asks: b.asks, bids: b.bids };
}

/**
 * `price_basis` (bigint or null): the token's standard scale (`10^decimals`, at most 10^9: sompi per whole token) when its decimals are known,
 * else the scale of its first order (market.rs `price_basis`), else 1.
 */
export function basisOf(chain, tok) {
  if (tok.scale !== null && tok.scale !== undefined) return BigInt(tok.scale);
  const first = [...chain.orders.values()].filter((o) => o.token === tok.covenant_id).sort((a, b) => a.genesis.seq - b.genesis.seq)[0];
  const scale = first ? termsOf(first.state).scale : 0n;
  return scale > 0n ? scale : 1n;
}

/** `price` sompi per `scale` base units as sompi per `basis` base units (rounded down). */
const perBasis = (price, scale, basis) => (price * basis) / (scale > 0n ? scale : 1n);

/**
 * All trades of a token in chain order (ascending id), prices per `basis` (market.rs `trade_of`). A trade groups the fill events of one
 * txid; amount / quote are the ask side's (the bid side's when no ask filled; the quote of a fill is `floor(n * price / scale)`); the price is
 * the amount-weighted per-basis price of the RESTING side (the side whose orders are older: smaller min (genesis daa, genesis block seq); on a
 * tie the asks rest); `side` is the aggressor's.
 */
export function tradesOf(chain, tok, basis) {
  if (basis === null) return [];
  const groups = new Map();
  for (const e of chain.events) {
    if (e.kind !== 'fill' || e.token !== tok.covenant_id || e.amount === null || e.price === null) continue;
    const o = chain.orders.get(e.covenant_id);
    if (!o) continue;
    const n = BigInt(e.amount);
    if (n <= 0n) continue;
    let g = groups.get(e.txid);
    if (!g) groups.set(e.txid, (g = { id: e.id, txid: e.txid, daa: e.daa, ts: e.ts, fills: 0, sides: new Map() }));
    if (e.id < g.id) Object.assign(g, { id: e.id, daa: e.daa, ts: e.ts });
    g.fills += 1;
    const t = termsOf(o.state);
    const price = BigInt(e.price);
    const scale = t.scale > 0n ? t.scale : 1n;
    const s = g.sides.get(e.side) ?? { amount: 0n, quote: 0n, weighted: 0n, gen: [o.genesis.daa, o.genesis.seq] };
    s.amount += n;
    s.quote += (n * price) / scale;
    s.weighted += n * perBasis(price, scale, basis);
    if (o.genesis.daa < s.gen[0] || (o.genesis.daa === s.gen[0] && o.genesis.seq < s.gen[1])) s.gen = [o.genesis.daa, o.genesis.seq];
    g.sides.set(e.side, s);
  }
  const out = [];
  for (const g of groups.values()) {
    const ask = g.sides.get(1);
    const bid = g.sides.get(2);
    const vol = ask ?? bid;
    let resting;
    if (ask && bid) resting = ask.gen[0] < bid.gen[0] || (ask.gen[0] === bid.gen[0] && ask.gen[1] <= bid.gen[1]) ? 1 : 2;
    else resting = ask ? 1 : 2;
    const r = resting === 1 ? ask : bid;
    if (!vol || vol.amount === 0n || r.amount === 0n) continue;
    out.push({
      id: g.id,
      txid: g.txid,
      ts: g.ts,
      daa: g.daa,
      price: r.weighted / r.amount,
      amount: vol.amount,
      quote: vol.quote,
      side: resting === 1 ? 'buy' : 'sell',
      fills: g.fills,
    });
  }
  return out.sort((a, b) => a.id - b.id);
}

const tradeView = (t, ctx) => ({
  id: t.id,
  txid: t.txid,
  ts: t.ts,
  daa: t.daa,
  price: t.price.toString(),
  amount: t.amount.toString(),
  quote: t.quote.toString(),
  side: t.side,
  fills: t.fills,
  confirmations: conf(ctx, t.daa),
  settled: settled(ctx, t.daa),
});

/** `GET /v1/trades/{token}?limit=&before=`: newest first, keyset paged by trade id. */
export function tradesPage(chain, tokenId, q) {
  const tok = tokenOf(chain, tokenId);
  const limit = limitParam(q, 'limit', 50);
  const before = intParam(q, 'before');
  const basis = basisOf(chain, tok);
  const ctx = ctxOf(chain);
  const rows = tradesOf(chain, tok, basis)
    .filter((t) => before === null || t.id < before)
    .reverse();
  const page = rows.slice(0, limit);
  return {
    token: tok.covenant_id,
    price_basis: str(basis),
    decimals: tok.decimals ?? null,
    items: page.map((t) => tradeView(t, ctx)),
    next_cursor: rows.length > limit ? String(page[page.length - 1].id) : null,
  };
}

/**
 * `GET /v1/candles/{token}?interval=&from=&to=&limit=`: OHLCV per bucket, ascending, empty buckets omitted. `from` / `to` bound the bucket
 * START `t` (inclusive / exclusive). With `from` the FIRST `limit` buckets at or after it are returned, otherwise the LAST `limit`.
 */
export function candlesView(chain, tokenId, q) {
  const tok = tokenOf(chain, tokenId);
  const interval = q.get('interval');
  if (interval === null || !Object.hasOwn(INTERVALS, interval)) throw badRequest(`interval must be one of ${Object.keys(INTERVALS).join(', ')}`);
  const ms = INTERVALS[interval];
  const from = intParam(q, 'from');
  const to = intParam(q, 'to');
  const limit = limitParam(q, 'limit', 500, MAX_CANDLES);
  const { basis, buckets } = kasCandleBuckets(chain, tok, ms, from, to);
  const all = [...buckets.values()].sort((a, b) => a.t - b.t);
  const picked = from !== null ? all.slice(0, limit) : all.slice(-limit);
  return {
    token: tok.covenant_id,
    interval,
    price_basis: str(basis),
    decimals: tok.decimals ?? null,
    items: picked.map((c) => ({
      t: c.t,
      o: c.o.toString(),
      h: c.h.toString(),
      l: c.l.toString(),
      c: c.c.toString(),
      volume: c.volume.toString(),
      quote_volume: c.quote_volume.toString(),
      trades: c.trades,
    })),
  };
}

/**
 * The KAS candle buckets of a token (prices per its `price_basis`, from its KAS-book trades only), keyed by bucket start (unix ms), optionally
 * limited to `[from, to)`. Shared by the token candles and the pair candles derived from two KAS series (pairs.mjs).
 */
export function kasCandleBuckets(chain, tok, ms, from = null, to = null) {
  const basis = basisOf(chain, tok);
  const buckets = new Map();
  for (const t of tradesOf(chain, tok, basis)) {
    const start = Math.floor(t.ts / ms) * ms;
    if ((from !== null && start < from) || (to !== null && start >= to)) continue;
    const c = buckets.get(start);
    if (!c) {
      buckets.set(start, { t: start, o: t.price, h: t.price, l: t.price, c: t.price, volume: t.amount, quote_volume: t.quote, trades: 1 });
      continue;
    }
    c.h = maxB(c.h, t.price);
    c.l = minB(c.l, t.price);
    c.c = t.price;
    c.volume += t.amount;
    c.quote_volume += t.quote;
    c.trades += 1;
  }
  return { basis, buckets };
}

/** Per-basis price of an aggregated book level: `price * basis / scale` (floor). */
const levelPrice = (lv, basis) => (BigInt(lv.price) * basis) / BigInt(lv.scale || 1);

/** Book levels merged by equal per-basis price, best first (asks ascending, bids descending). */
function mergedSide(levels, basis, side) {
  const byPrice = new Map();
  for (const lv of levels) {
    const price = levelPrice(lv, basis);
    const key = price.toString();
    const m = byPrice.get(key) ?? { price, amount: 0n, orders: 0, estimated: false };
    m.amount += BigInt(lv.amount);
    m.orders += lv.orders;
    m.estimated = m.estimated || lv.amount_estimated;
    byPrice.set(key, m);
  }
  return [...byPrice.values()].sort((a, b) => (a.price === b.price ? 0 : (a.price < b.price) === (side === 1) ? -1 : 1));
}

/**
 * `GET /v1/stats/{token}`: rolling 24 h window ending at the newest chain time (`chain.nowMs()`, trades with `ts >= now - 24h`) plus the
 * book top. `change_24h_bps` = (last - open_24h) * 10000 / open_24h, integer division truncated toward zero. Counts / volumes of an
 * empty window are 0 / "0"; prices of an empty window or empty side are null.
 */
export function statsView(chain, tokenId) {
  const tok = tokenOf(chain, tokenId);
  const levels = listedLevels(chain, tok.covenant_id);
  const basis = basisOf(chain, tok);
  const now = chain.nowMs();
  const trades = tradesOf(chain, tok, basis);
  const last = trades[trades.length - 1] ?? null;
  const win = trades.filter((t) => t.ts >= now - DAY_MS);
  let high = null;
  let low = null;
  let volume = 0n;
  let quoteVolume = 0n;
  for (const t of win) {
    high = high === null ? t.price : maxB(high, t.price);
    low = low === null ? t.price : minB(low, t.price);
    volume += t.amount;
    quoteVolume += t.quote;
  }
  const open = win[0]?.price ?? null;
  const change = open !== null && open > 0n && last ? Number(((last.price - open) * 10_000n) / open) : null;
  const bestAsk = basis === null ? null : (mergedSide(levels.asks, basis, 1)[0]?.price ?? null);
  const bestBid = basis === null ? null : (mergedSide(levels.bids, basis, 2)[0]?.price ?? null);
  const mid = bestAsk !== null && bestBid !== null ? (bestAsk + bestBid) / 2n : null;
  const spread = mid !== null && mid > 0n ? Number(((bestAsk - bestBid) * 10_000n) / mid) : null;
  const counts = tokensView(chain).tokens.find((t) => t.covenant_id === tok.covenant_id);
  return {
    token: tok.covenant_id,
    price_basis: str(basis),
    decimals: tok.decimals ?? null,
    ts: now,
    last: str(last?.price),
    last_ts: last?.ts ?? null,
    last_side: last?.side ?? null,
    open_24h: str(open),
    high_24h: str(high),
    low_24h: str(low),
    change_24h_bps: change,
    volume_24h: volume.toString(),
    quote_volume_24h: quoteVolume.toString(),
    trades_24h: win.length,
    best_bid: str(bestBid),
    best_ask: str(bestAsk),
    mid: str(mid),
    spread_bps: spread,
    open_asks: counts?.open_asks ?? 0,
    open_bids: counts?.open_bids ?? 0,
  };
}

/** `GET /v1/depth/{token}?levels=`: the listed book merged by per-basis price, best first, with cumulative amount / quote. */
export function depthView(chain, tokenId, q) {
  const tok = tokenOf(chain, tokenId);
  const n = limitParam(q, 'levels', 50);
  const levels = listedLevels(chain, tok.covenant_id);
  const basis = basisOf(chain, tok);
  const side = (lvls, s) => {
    if (basis === null) return [];
    let cumAmount = 0n;
    let cumQuote = 0n;
    return mergedSide(lvls, basis, s)
      .slice(0, n)
      .map((m) => {
        cumAmount += m.amount;
        cumQuote += (m.amount * m.price) / basis;
        return { price: m.price.toString(), amount: m.amount.toString(), orders: m.orders, cum_amount: cumAmount.toString(), cum_quote: cumQuote.toString(), estimated: m.estimated };
      });
  };
  return { token: tok.covenant_id, price_basis: str(basis),
    decimals: tok.decimals ?? null, ts: chain.nowMs(), daa: chain.daa(), bids: side(levels.bids, 2), asks: side(levels.asks, 1) };
}
