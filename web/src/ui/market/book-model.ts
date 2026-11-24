// View model of the order book. It turns indexer levels into display rows with cumulative depth,
// spread and mid price. The module is pure and uses bigint arithmetic only.
//
// Price convention (protocol v3, docs/ops/executor.md 5.2): an indexer level's `price` is sompi
// per `scale` base units of the token, where `scale` is a number, and its `amount` is in base
// units. The book shows prices per whole token of the token's standard scale (`10^decimals`, the
// wallet's scale). A row of another scale, as an open-list token's orders may have, is converted
// to it for display and rounded to the nearest sompi.
import type { BookOrderView, LevelView } from '../../data/indexer-types';
import type { Rational } from '../../kob/pair';
import { quoteOf, roundDiv } from '../../kob/units';

export interface BookRow {
  /** Sompi per whole token, in the market's scale. */
  price: bigint;
  /** Base units. */
  amount: bigint;
  orders: number;
  /**
   * The level's amount is an estimate (bid budgets, unproven states). It is shown with a marker.
   */
  estimated: boolean;
  /** Base units from the best price up to and including this level. */
  cumAmount: bigint;
  /**
   * Sompi of KAS value from the best price up to and including this level, summing each level's
   * `amount x price / scale` rounded down.
   */
  cumValue: bigint;
  /** Cumulative depth relative to the deeper side, 0 to 100, as the bar width. */
  depthPct: number;
  /**
   * The inverted price (tokens per KAS) of a level grouped in the inverted view. It is shown
   * instead of the inverse of `price` (see `orientation.ts`).
   */
  inv?: Rational;
  /**
   * The curve levels (table indexes) at this level. A curve sell order's levels are book levels
   * like any other, and marked.
   */
  curve?: number[];
}

export interface BookModel {
  /** Best (lowest) price first, which is the order of the depth accumulation. */
  asks: BookRow[];
  /** Best (highest) price first. */
  bids: BookRow[];
  /**
   * Asks in display order. The highest price is at the top and the best ask is directly above the
   * spread.
   */
  asksDisplay: BookRow[];
  /**
   * The touch, which is the best ask or bid of the ungrouped levels. Grouping rounds asks up and
   * bids down, so the grouped rows would widen the spread and move the mid.
   */
  bestAsk: bigint | null;
  bestBid: bigint | null;
  /** Best ask minus best bid, ungrouped. Negative for a crossed book. */
  spread: bigint | null;
  /** The spread relative to the mid price in basis points, rounded down. */
  spreadBps: number | null;
  mid: bigint | null;
  totalAskAmount: bigint;
  totalBidAmount: bigint;
  anyEstimated: boolean;
  empty: boolean;
}

/**
 * One price level. `price` is sompi per whole token of the market's scale and `amount` is in base
 * units. `value` is the KAS, in sompi, the level's orders are worth at their own prices. It is
 * `amount x price / scale` per ungrouped level, rounded down and summed over a group. A grouped
 * level's `price` is its worst price, so `amount x price` would overstate it. When `value` is
 * absent the value is `amount x price / scale`.
 */
export interface Level { price: bigint; amount: bigint; orders: number; estimated: boolean; inv?: Rational; value?: bigint; curve?: number[] }

/** Merges the curve marks of two levels. None stays absent. */
export const mergeCurve = (a: number[] | undefined, b: number[] | undefined): number[] | undefined => (a || b ? [...(a ?? []), ...(b ?? [])] : undefined);

const isOrder = (r: LevelView | BookOrderView): r is BookOrderView => 'covenant_id' in r;

const parseBig = (v: unknown): bigint | null => {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number') return Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : null;
  if (typeof v === 'string' && /^\d+$/.test(v)) return BigInt(v);
  return null;
};

/**
 * Turns aggregated levels, or per-order entries grouped by price, into levels in sompi per whole
 * token of `scale` base units. `scale` is the market's scale, and a row of another scale is
 * converted. Malformed rows are dropped and never guessed. So are levels of amount 0, such as a
 * bid whose escrow no longer funds a base unit. That is not liquidity and it stays in its maker's
 * orders.
 */
export function levelsFromView(rows: readonly (LevelView | BookOrderView)[], scale: bigint): Level[] {
  const by = new Map<bigint, Level>();
  for (const r of rows) {
    const p = parseBig(r.price);
    if (p === null || p <= 0n) continue;
    // A row quotes per its own scale. It is the same whole token unless an order chose another,
    // which is converted to the market's scale.
    const rowScale = parseBig(r.scale) ?? scale;
    const price = rowScale === scale || rowScale <= 0n ? p : roundDiv(p * scale, rowScale);
    if (price <= 0n) continue;
    let amount: bigint;
    let orders: number;
    let estimated: boolean;
    let curve: number[] | undefined;
    if (isOrder(r)) {
      // One row per remaining level of a curve order (`curve_level`). All its rows count as one
      // order.
      if (typeof r.curve_level === 'number') curve = [r.curve_level];
      // What a taker can fill from it now (`fillable_now`, R6B-I4), never more than it has left.
      const all = parseBig(r.amount_left);
      const now = r.fillable_now == null ? null : parseBig(r.fillable_now);
      const left = all !== null && now !== null && now < all ? now : all;
      amount = left ?? 0n;
      orders = 1;
      estimated = r.amount_estimated || left === null;
    } else {
      amount = parseBig(r.amount) ?? 0n;
      orders = Math.max(0, Math.trunc(Number(r.orders) || 0));
      estimated = !!r.amount_estimated;
    }
    const cur = by.get(price);
    if (cur) {
      cur.amount += amount;
      cur.orders += orders;
      cur.estimated ||= estimated;
      const c = mergeCurve(cur.curve, curve);
      if (c) cur.curve = c;
    } else by.set(price, { price, amount, orders, estimated, ...(curve ? { curve } : {}) });
  }
  return [...by.values()].filter((l) => l.amount > 0n);
}

function accumulate(levels: Level[], max: bigint, scale: bigint): BookRow[] {
  let cumAmount = 0n;
  let cumValue = 0n;
  return levels.map((l) => {
    cumAmount += l.amount;
    cumValue += l.value ?? quoteOf(l.amount, l.price, scale, 'down');
    const depthPct = max > 0n ? Number((cumAmount * 10_000n) / max) / 100 : 0;
    return { ...l, cumAmount, cumValue, depthPct };
  });
}

export interface BookModelOptions {
  /**
   * The market's scale in base units per whole token. Prices are per that many base units. The
   * default is 1.
   */
  scale?: bigint;
  /** Show at most this many levels per side. The default is 15. */
  depth?: number;
  /**
   * Price aggregation step in sompi per whole token. Asks round up and bids down to a multiple of
   * it. 0, 1 or absent means no grouping.
   */
  group?: bigint;
  /**
   * Replaces the native grouping with a grouping in another orientation. The inverted view groups
   * on tokens per KAS (`orientation.ts`).
   */
  regroup?: (levels: Level[], side: 'ask' | 'bid') => Level[];
}

// ------------------------------------------------------------------------------- price aggregation

/**
 * The grouped price of a level. Asks round up and bids down to a multiple of `group`. The grouped
 * row therefore shows the worst price of its orders. A click on an ask group fills the price of a
 * buy that reaches every order in it. A click on a bid group fills the price of a sell that
 * reaches every bid in it.
 */
export function groupPrice(price: bigint, group: bigint, side: 'ask' | 'bid'): bigint {
  if (group <= 1n) return price;
  const q = price / group;
  if (side === 'bid' || price % group === 0n) return q * group;
  return (q + 1n) * group;
}

/** Merges levels onto grouped prices. Amounts and orders add up, and estimates stay marked. */
export function groupLevels<L extends { price: bigint; amount: bigint; orders: number; estimated: boolean; value?: bigint; curve?: number[] }>(levels: readonly L[], group: bigint, side: 'ask' | 'bid'): Level[] {
  const by = new Map<bigint, Level>();
  for (const l of levels) {
    const price = groupPrice(l.price, group, side);
    const cur = by.get(price);
    if (cur) {
      cur.amount += l.amount;
      cur.orders += l.orders;
      cur.estimated ||= l.estimated;
      if (cur.value !== undefined && l.value !== undefined) cur.value += l.value;
      const c = mergeCurve(cur.curve, l.curve);
      if (c) cur.curve = c;
    } else by.set(price, { price, amount: l.amount, orders: l.orders, estimated: l.estimated, ...(l.value !== undefined ? { value: l.value } : {}), ...(l.curve ? { curve: l.curve } : {}) });
  }
  return [...by.values()];
}

const gcd = (a: bigint, b: bigint): bigint => {
  while (b) [a, b] = [b, a % b];
  return a;
};

/**
 * The finest step the visible prices sit on, their gcd. It is 1 without prices. It gives a tick for
 * tokens whose registry entry has none.
 */
export function inferTick(prices: readonly bigint[]): bigint {
  let g = 0n;
  for (const p of prices) if (p > 0n) g = gcd(g, p);
  return g > 0n ? g : 1n;
}

/** Grouping choices: 1x, 10x, 100x and 1000x the tick (sompi per whole token). */
export const GROUP_MULTIPLIERS = [1n, 10n, 100n, 1000n] as const;
export const groupSizes = (tick: bigint): bigint[] => GROUP_MULTIPLIERS.map((m) => m * (tick > 0n ? tick : 1n));

/**
 * Grouping choices that mean something at the market's price. They are the tick, which means no
 * grouping, then the powers of ten near 1/100,000, 1/10,000 and 1/1,000 of `ref`, a reference
 * price in the same unit. Each is a multiple of the tick. A price of 2,165,695.81 KAS per token on
 * a 1-sompi tick offers 10, 100 and 1,000 KAS, not 0.0000001 to 0.00001. Without a reference
 * price use `groupSizes`.
 */
export function groupSizesAt(tick: bigint, ref: bigint | null): bigint[] {
  const t = tick > 0n ? tick : 1n;
  if (ref === null || ref <= 0n) return groupSizes(t);
  let p = 1n;
  while (p * 10n <= ref / 100_000n) p *= 10n;
  const out = [t];
  for (const step of [p, p * 10n, p * 100n]) {
    const s = step % t === 0n ? step : ((step + t - 1n) / t) * t;
    if (s > out[out.length - 1]!) out.push(s);
  }
  while (out.length < GROUP_MULTIPLIERS.length) out.push(out[out.length - 1]! * 10n);
  return out;
}

/**
 * The grouping steps of a token's book on the kit's price rule. They are the display step and the
 * three decades above it, each rounded up to a multiple of the tick. The display step is
 * 10^-`dp` KAS per whole token, with `dp` from `displayPriceDp`. That is 0.001 at 34 KAS and
 * 0.000001 at 0.025 KAS. A step finer than the display decimals would read as the same price
 * twice, so the tick (1 sompi, 0.00000001) is not offered where it is finer than that. `scale` is
 * the base units the book's state prices are per, and `decimals` is the token's.
 */
export function displayGroupSizes(tick: bigint, dp: number, decimals: number, scale: bigint): bigint[] {
  const t = tick > 0n ? tick : 1n;
  const unit = 10n ** BigInt(Math.max(0, decimals));
  // 10^-dp KAS is 10^(8-dp) sompi per whole token. A state price is sompi per `scale` base units.
  const perToken = 10n ** BigInt(Math.max(0, 8 - dp));
  let step = (perToken * scale + unit - 1n) / unit;
  if (step < t) step = t;
  step = ((step + t - 1n) / t) * t;
  return GROUP_MULTIPLIERS.map((m) => m * step);
}

export function buildBookModel(view: { asks: readonly (LevelView | BookOrderView)[]; bids: readonly (LevelView | BookOrderView)[] }, o: BookModelOptions = {}): BookModel {
  const scale = o.scale ?? 1n;
  const depth = Math.max(1, o.depth ?? 15);
  const group = o.group ?? 0n;
  const grouped = (ls: Level[], side: 'ask' | 'bid'): Level[] => (o.regroup ? o.regroup(ls, side) : groupLevels(ls, group, side));
  // The levels as the book holds them. The touch, spread and mid come from these. Grouping, which
  // rounds asks up and bids down, only shapes the rows.
  const withValue = (ls: Level[]): Level[] => ls.map((l) => ({ ...l, value: quoteOf(l.amount, l.price, scale, 'down') }));
  const rawAsks = withValue(levelsFromView(view.asks, scale));
  const rawBids = withValue(levelsFromView(view.bids, scale));
  const askLevels = grouped(rawAsks, 'ask').sort((a, b) => (a.price < b.price ? -1 : a.price > b.price ? 1 : 0)).slice(0, depth);
  const bidLevels = grouped(rawBids, 'bid').sort((a, b) => (a.price > b.price ? -1 : a.price < b.price ? 1 : 0)).slice(0, depth);
  const sum = (ls: Level[]) => ls.reduce((s, l) => s + l.amount, 0n);
  const totalAskAmount = sum(askLevels);
  const totalBidAmount = sum(bidLevels);
  const max = totalAskAmount > totalBidAmount ? totalAskAmount : totalBidAmount;
  const asks = accumulate(askLevels, max, scale);
  const bids = accumulate(bidLevels, max, scale);
  const bestAsk = rawAsks.length ? rawAsks.reduce((m, l) => (l.price < m ? l.price : m), rawAsks[0]!.price) : null;
  const bestBid = rawBids.length ? rawBids.reduce((m, l) => (l.price > m ? l.price : m), rawBids[0]!.price) : null;
  const both = bestAsk !== null && bestBid !== null;
  const mid = both ? (bestAsk + bestBid) / 2n : null;
  const spread = both ? bestAsk - bestBid : null;
  return {
    asks,
    bids,
    asksDisplay: [...asks].reverse(),
    bestAsk,
    bestBid,
    spread,
    spreadBps: both && mid !== null && mid > 0n ? Number(((bestAsk - bestBid) * 10_000n) / mid) : null,
    mid,
    totalAskAmount,
    totalBidAmount,
    anyEstimated: askLevels.some((l) => l.estimated) || bidLevels.some((l) => l.estimated),
    empty: asks.length === 0 && bids.length === 0,
  };
}

// ---------------------------------------------------------------------------- depth chart geometry

export interface DepthChartGeometry {
  width: number;
  height: number;
  /**
   * SVG path data of the bid and ask areas, closed to the baseline. An empty string without data.
   */
  bidPath: string;
  askPath: string;
  hasData: boolean;
  /** Lowest and highest price on the x axis (sompi per whole token). */
  priceMin: bigint | null;
  priceMax: bigint | null;
  /** The x position of the mid price, 0 to width. */
  midX: number | null;
  /** The deeper side's cumulative amount in base units, which is the y axis. */
  maxAmount: bigint;
}

const fx = (n: number): string => (Math.round(n * 100) / 100).toString();

/**
 * Step-area depth chart. The x axis spans the worst bid to the worst ask. The y axis is the deeper
 * side's cumulative amount. Positions are computed from bigint ratios, so no precision is lost for
 * large prices. Bids grow to the left of the spread and asks to the right.
 */
export function depthChartGeometry(m: BookModel, width = 320, height = 120): DepthChartGeometry {
  const empty: DepthChartGeometry = { width, height, bidPath: '', askPath: '', hasData: false, priceMin: null, priceMax: null, midX: null, maxAmount: 0n };
  if (m.empty) return empty;
  const prices = [...m.bids.map((r) => r.price), ...m.asks.map((r) => r.price)];
  let lo = prices.reduce((a, b) => (b < a ? b : a));
  let hi = prices.reduce((a, b) => (b > a ? b : a));
  if (hi === lo) {
    // A single price gets a symmetric window, so the chart is not a line.
    const pad = lo / 100n > 0n ? lo / 100n : 1n;
    lo -= pad;
    hi += pad;
  }
  const span = hi - lo;
  const x = (p: bigint): number => (Number(((p - lo) * 10_000n) / span) / 10_000) * width;
  const maxAmount = m.totalAskAmount > m.totalBidAmount ? m.totalAskAmount : m.totalBidAmount;
  const y = (cum: bigint): number => (maxAmount > 0n ? height - (Number((cum * 10_000n) / maxAmount) / 10_000) * (height - 4) : height);
  const area = (rows: BookRow[]): string => {
    if (!rows.length) return '';
    const pts: string[] = [`M${fx(x(rows[0].price))} ${fx(height)}`];
    let prev = 0n;
    for (const r of rows) {
      pts.push(`L${fx(x(r.price))} ${fx(y(prev))}`, `L${fx(x(r.price))} ${fx(y(r.cumAmount))}`);
      prev = r.cumAmount;
    }
    pts.push(`L${fx(x(rows[rows.length - 1].price))} ${fx(height)}`, 'Z');
    return pts.join(' ');
  };
  return {
    width,
    height,
    bidPath: area(m.bids),
    askPath: area(m.asks),
    hasData: true,
    priceMin: lo,
    priceMax: hi,
    midX: m.mid !== null ? x(m.mid) : null,
    maxAmount,
  };
}
