// The market data of standalone mode (data/standalone.ts): trades, candles, 24 h figures and depth built from what the browser saw since the mode
// started: the fills of KOB orders in the node's new blocks (kob/stream-orders.ts `fillOf`, each counted once the node shows one of its
// transaction's outputs) and the partial book (data/stream-book.ts). The views have the indexer's response shapes (`/v1/trades`, `/v1/candles`,
// `/v1/stats`, `/v1/depth`) and its rules (crates/kob-executor/src/indexer/market.rs, the mock's mock/market.mjs): every price is sompi per
// `price_basis` base units (the token's standard scale when the registry knows its decimals, else the scale of the first order seen of it), a trade
// groups the fills of one transaction on one token, priced at the resting side's amount-weighted price, its side the aggressor's, its amount and
// quote the asks' (the bids' when no ask filled). Nothing older than the mode is in them: the views that show them say "since HH:MM".
import type { BookView, CandleInterval, CandlesView, DepthView, LevelView, StatsView, TradesView } from './indexer-types';
import type { StreamFill } from '../kob/stream-orders';
import type { Hex } from '../kob/types';

export const INTERVAL_MS: Record<CandleInterval, number> = { '1m': 60_000, '5m': 300_000, '1h': 3_600_000, '1d': 86_400_000 };
const DAY_MS = 86_400_000;
/** Most fills kept per token (the oldest are forgotten). */
export const MAX_FILLS_PER_TOKEN = 5_000;

/** A fill the node confirmed, with when it happened. */
export interface SeenFill extends StreamFill {
  /** order of arrival (trade ids) */
  seq: number;
  /** block time, ms */
  ts: number;
  daa: bigint;
}

interface Trade { id: number; txid: Hex; ts: number; daa: bigint; price: bigint; amount: bigint; quote: bigint; side: 'buy' | 'sell'; fills: number }

const perBasis = (price: bigint, scale: bigint, basis: bigint): bigint => (price * basis) / (scale > 0n ? scale : 1n);

/** The trades of a token's fills (ascending), prices per `basis`. */
export function tradesOfFills(fills: readonly SeenFill[], basis: bigint): Trade[] {
  const groups = new Map<Hex, { id: number; txid: Hex; ts: number; daa: bigint; fills: number; sides: Map<'sell' | 'buy', { amount: bigint; quote: bigint; weighted: bigint; first: number }> }>();
  for (const f of fills) {
    if (f.amount <= 0n) continue;
    let g = groups.get(f.txid);
    if (!g) groups.set(f.txid, (g = { id: f.seq, txid: f.txid, ts: f.ts, daa: f.daa, fills: 0, sides: new Map() }));
    g.fills += 1;
    const s = g.sides.get(f.side) ?? { amount: 0n, quote: 0n, weighted: 0n, first: f.seq };
    s.amount += f.amount;
    s.quote += (f.amount * f.price) / (f.scale > 0n ? f.scale : 1n);
    s.weighted += f.amount * perBasis(f.price, f.scale, basis);
    s.first = Math.min(s.first, f.seq);
    g.sides.set(f.side, s);
  }
  const out: Trade[] = [];
  for (const g of groups.values()) {
    const ask = g.sides.get('sell');
    const bid = g.sides.get('buy');
    const vol = ask ?? bid!;
    // the resting side: the one seen first (asks on a tie); the aggressor took it
    const resting = ask && bid ? (ask.first <= bid.first ? 'sell' : 'buy') : ask ? 'sell' : 'buy';
    const r = resting === 'sell' ? ask! : bid!;
    if (vol.amount === 0n || r.amount === 0n) continue;
    out.push({ id: g.id, txid: g.txid, ts: g.ts, daa: g.daa, price: r.weighted / r.amount, amount: vol.amount, quote: vol.quote, side: resting === 'sell' ? 'buy' : 'sell', fills: g.fills });
  }
  return out.sort((a, b) => a.id - b.id);
}

/** `/v1/trades/{token}`: newest first, `before` = a trade id. */
export function tradesView(token: Hex, fills: readonly SeenFill[], basis: bigint, q: { limit?: number; before?: number | string } = {}): TradesView {
  const limit = Math.max(1, Math.min(q.limit ?? 50, 500));
  const before = q.before === undefined || q.before === null ? null : Number(q.before);
  const rows = tradesOfFills(fills, basis).filter((t) => before === null || t.id < before).reverse();
  const page = rows.slice(0, limit);
  return {
    token, price_basis: basis.toString(), decimals: null,
    items: page.map((t) => ({ id: t.id, txid: t.txid, ts: t.ts, daa: Number(t.daa), price: t.price.toString(), amount: t.amount.toString(), quote: t.quote.toString(), side: t.side, fills: t.fills, confirmations: null, settled: false })),
    next_cursor: rows.length > limit ? String(page[page.length - 1]!.id) : null,
  };
}

/** `/v1/candles/{token}`: OHLCV per bucket, ascending, empty buckets omitted; `from` / `to` bound the bucket start; the first or last `limit`. */
export function candlesView(token: Hex, fills: readonly SeenFill[], basis: bigint, q: { interval: CandleInterval; from?: number; to?: number; limit?: number }): CandlesView {
  const ms = INTERVAL_MS[q.interval];
  const limit = Math.max(1, Math.min(q.limit ?? 500, 1500));
  const buckets = new Map<number, { t: number; o: bigint; h: bigint; l: bigint; c: bigint; volume: bigint; quote_volume: bigint; trades: number }>();
  for (const t of tradesOfFills(fills, basis)) {
    const start = Math.floor(t.ts / ms) * ms;
    if ((q.from !== undefined && start < q.from) || (q.to !== undefined && start >= q.to)) continue;
    const c = buckets.get(start);
    if (!c) {
      buckets.set(start, { t: start, o: t.price, h: t.price, l: t.price, c: t.price, volume: t.amount, quote_volume: t.quote, trades: 1 });
      continue;
    }
    if (t.price > c.h) c.h = t.price;
    if (t.price < c.l) c.l = t.price;
    c.c = t.price;
    c.volume += t.amount;
    c.quote_volume += t.quote;
    c.trades += 1;
  }
  const all = [...buckets.values()].sort((a, b) => a.t - b.t);
  const picked = q.from !== undefined ? all.slice(0, limit) : all.slice(-limit);
  return {
    token, interval: q.interval, price_basis: basis.toString(), decimals: null,
    items: picked.map((c) => ({ t: c.t, o: c.o.toString(), h: c.h.toString(), l: c.l.toString(), c: c.c.toString(), volume: c.volume.toString(), quote_volume: c.quote_volume.toString(), trades: c.trades })),
  };
}

/** Levels merged by equal per-basis price, best first. */
function mergedSide(levels: readonly LevelView[], basis: bigint, side: 'sell' | 'buy'): { price: bigint; amount: bigint; orders: number; estimated: boolean }[] {
  const by = new Map<string, { price: bigint; amount: bigint; orders: number; estimated: boolean }>();
  for (const lv of levels) {
    const price = (BigInt(lv.price) * basis) / BigInt(lv.scale || 1);
    const m = by.get(price.toString()) ?? { price, amount: 0n, orders: 0, estimated: false };
    m.amount += BigInt(lv.amount);
    m.orders += lv.orders;
    m.estimated ||= lv.amount_estimated;
    by.set(price.toString(), m);
  }
  return [...by.values()].sort((a, b) => (a.price === b.price ? 0 : (a.price < b.price) === (side === 'sell') ? -1 : 1));
}

const levelsOf = (rows: BookView['asks']): LevelView[] => rows.filter((r): r is LevelView => 'orders' in r);

/** `/v1/stats/{token}`: the 24 h window ending now (only fills seen since the mode started) and the top of the partial book (`book`: aggregated). */
export function statsView(token: Hex, fills: readonly SeenFill[], basis: bigint, book: BookView, nowMs: number): StatsView {
  const trades = tradesOfFills(fills, basis);
  const last = trades[trades.length - 1] ?? null;
  const win = trades.filter((t) => t.ts >= nowMs - DAY_MS);
  let high: bigint | null = null;
  let low: bigint | null = null;
  let volume = 0n;
  let quoteVolume = 0n;
  for (const t of win) {
    high = high === null || t.price > high ? t.price : high;
    low = low === null || t.price < low ? t.price : low;
    volume += t.amount;
    quoteVolume += t.quote;
  }
  const open = win[0]?.price ?? null;
  const change = open !== null && open > 0n && last ? Number(((last.price - open) * 10_000n) / open) : null;
  const asks = mergedSide(levelsOf(book.asks), basis, 'sell');
  const bids = mergedSide(levelsOf(book.bids), basis, 'buy');
  const bestAsk = asks[0]?.price ?? null;
  const bestBid = bids[0]?.price ?? null;
  const mid = bestAsk !== null && bestBid !== null ? (bestAsk + bestBid) / 2n : null;
  const spread = mid !== null && mid > 0n ? Number(((bestAsk! - bestBid!) * 10_000n) / mid) : null;
  const ref = mid ?? bestAsk ?? bestBid;
  let depth: bigint | null = null;
  if (ref !== null && ref > 0n) {
    depth = 0n;
    for (const m of asks) if (m.price * 100n <= ref * 102n) depth += (m.amount * m.price) / basis;
    for (const m of bids) if (m.price * 100n >= ref * 98n) depth += (m.amount * m.price) / basis;
  }
  const s = (v: bigint | null): string | null => (v === null ? null : v.toString());
  return {
    token, price_basis: basis.toString(), decimals: null, ts: nowMs, last: s(last?.price ?? null), last_ts: last?.ts ?? null, last_side: last?.side ?? null,
    open_24h: s(open), high_24h: s(high), low_24h: s(low), change_24h_bps: change, volume_24h: volume.toString(), quote_volume_24h: quoteVolume.toString(), trades_24h: win.length,
    best_bid: s(bestBid), best_ask: s(bestAsk), mid: s(mid), spread_bps: spread,
    open_asks: book.asks.reduce((n, r) => n + ('orders' in r ? r.orders : 1), 0), open_bids: book.bids.reduce((n, r) => n + ('orders' in r ? r.orders : 1), 0), depth_2pct_quote: s(depth),
  };
}

/** `/v1/depth/{token}`: the partial book (aggregated) merged by per-basis price, best first, with cumulative amount and quote. */
export function depthView(token: Hex, book: BookView, basis: bigint, nowMs: number, daa: bigint, levels = 50): DepthView {
  const side = (rows: BookView['asks'], s: 'sell' | 'buy') => {
    let cumAmount = 0n;
    let cumQuote = 0n;
    return mergedSide(levelsOf(rows), basis, s)
      .slice(0, levels)
      .map((m) => {
        cumAmount += m.amount;
        cumQuote += (m.amount * m.price) / basis;
        return { price: m.price.toString(), amount: m.amount.toString(), orders: m.orders, cum_amount: cumAmount.toString(), cum_quote: cumQuote.toString(), estimated: m.estimated };
      });
  };
  return { token, price_basis: basis.toString(), decimals: null, ts: nowMs, daa: Number(daa), bids: side(book.bids, 'buy'), asks: side(book.asks, 'sell') };
}
