// View model of the order book: indexer levels -> display rows with cumulative depth, spread and mid price. Pure, bigint arithmetic only.
//
// Price convention (protocol v3, docs/ops/executor.md 5.2): an indexer level's `price` is sompi per `scale` base units of the token (its `scale`,
// a number), its `amount` base units. The book shows prices per whole token of the token's standard scale (`10^decimals`, the wallet's scale);
// a row of another scale (an open-list token's orders may differ) is converted to it for display (rounded to the nearest sompi).
import type { BookOrderView, LevelView } from '../../data/indexer-types';
import type { Rational } from '../../kob/pair';
import { quoteOf, roundDiv } from '../../kob/units';

export interface BookRow {
  /** sompi per whole token (the market's scale) */
  price: bigint;
  /** base units */
  amount: bigint;
  orders: number;
  /** the level's amount is an estimate (bid budgets, unproven states): shown with a marker */
  estimated: boolean;
  /** base units from the best price up to and including this level */
  cumAmount: bigint;
  /** sompi of KAS value from the best price up to and including this level (each level's `amount x price / scale`, rounded down) */
  cumValue: bigint;
  /** cumulative depth relative to the deeper side, 0..100 (bar width) */
  depthPct: number;
  /** the INVERTED price (tokens per KAS) of a level grouped in the inverted view: shown instead of the inverse of `price` (see `orientation.ts`) */
  inv?: Rational;
}

export interface BookModel {
  /** best (lowest) price first: the order of the depth accumulation */
  asks: BookRow[];
  /** best (highest) price first */
  bids: BookRow[];
  /** asks in display order: highest price at the top, best ask directly above the spread */
  asksDisplay: BookRow[];
  bestAsk: bigint | null;
  bestBid: bigint | null;
  spread: bigint | null;
  /** spread relative to the mid price in basis points, rounded down */
  spreadBps: number | null;
  mid: bigint | null;
  /** best bid at or above best ask: the indexer view is in flux (a crossing that is about to fill) */
  crossed: boolean;
  totalAskAmount: bigint;
  totalBidAmount: bigint;
  anyEstimated: boolean;
  empty: boolean;
}

/** One price level: `price` sompi per whole token of the market's scale, `amount` base units. */
export interface Level { price: bigint; amount: bigint; orders: number; estimated: boolean; inv?: Rational }

const isOrder = (r: LevelView | BookOrderView): r is BookOrderView => 'covenant_id' in r;

const parseBig = (v: unknown): bigint | null => {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number') return Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : null;
  if (typeof v === 'string' && /^\d+$/.test(v)) return BigInt(v);
  return null;
};

/**
 * Aggregated levels (or per-order entries, grouped by price) -> levels in sompi per whole token of `scale` base units (the market's scale; a row
 * of another scale is converted). Malformed rows are dropped, never guessed, and so are levels of amount 0 (a bid whose escrow no longer funds a
 * base unit: it is not liquidity, it stays in its maker's orders).
 */
export function levelsFromView(rows: readonly (LevelView | BookOrderView)[], scale: bigint): Level[] {
  const by = new Map<bigint, Level>();
  for (const r of rows) {
    const p = parseBig(r.price);
    if (p === null || p <= 0n) continue;
    // a row quotes per its own scale: the same whole token unless an order chose another (converted to the market's scale)
    const rowScale = parseBig(r.scale) ?? scale;
    const price = rowScale === scale || rowScale <= 0n ? p : roundDiv(p * scale, rowScale);
    if (price <= 0n) continue;
    let amount: bigint;
    let orders: number;
    let estimated: boolean;
    if (isOrder(r)) {
      const left = parseBig(r.amount_left);
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
    } else by.set(price, { price, amount, orders, estimated });
  }
  return [...by.values()].filter((l) => l.amount > 0n);
}

function accumulate(levels: Level[], max: bigint, scale: bigint): BookRow[] {
  let cumAmount = 0n;
  let cumValue = 0n;
  return levels.map((l) => {
    cumAmount += l.amount;
    cumValue += quoteOf(l.amount, l.price, scale, 'down');
    const depthPct = max > 0n ? Number((cumAmount * 10_000n) / max) / 100 : 0;
    return { ...l, cumAmount, cumValue, depthPct };
  });
}

export interface BookModelOptions {
  /** the market's scale: base units per whole token (prices are per that many base units; default 1) */
  scale?: bigint;
  /** show at most this many levels per side (default 15) */
  depth?: number;
  /** price aggregation step (sompi per whole token): asks round UP, bids DOWN to a multiple of it; 0 / 1 / absent = no grouping */
  group?: bigint;
  /** replaces the native grouping: a grouping in another orientation (the inverted view groups on tokens per KAS; `orientation.ts`) */
  regroup?: (levels: Level[], side: 'ask' | 'bid') => Level[];
}

// ------------------------------------------------------------------------------------------------ price aggregation (tick grouping)

/**
 * The grouped price of a level: asks round UP, bids DOWN to a multiple of `group`. The grouped row therefore shows the worst price of its
 * orders (a click on an ask group prefills a buy that reaches every order in it; a bid group a sell that reaches every bid in it).
 */
export function groupPrice(price: bigint, group: bigint, side: 'ask' | 'bid'): bigint {
  if (group <= 1n) return price;
  const q = price / group;
  if (side === 'bid' || price % group === 0n) return q * group;
  return (q + 1n) * group;
}

/** Merges levels onto grouped prices (amounts and orders add up, estimates stay marked). */
export function groupLevels<L extends { price: bigint; amount: bigint; orders: number; estimated: boolean }>(levels: readonly L[], group: bigint, side: 'ask' | 'bid'): Level[] {
  const by = new Map<bigint, Level>();
  for (const l of levels) {
    const price = groupPrice(l.price, group, side);
    const cur = by.get(price);
    if (cur) {
      cur.amount += l.amount;
      cur.orders += l.orders;
      cur.estimated ||= l.estimated;
    } else by.set(price, { price, amount: l.amount, orders: l.orders, estimated: l.estimated });
  }
  return [...by.values()];
}

const gcd = (a: bigint, b: bigint): bigint => {
  while (b) [a, b] = [b, a % b];
  return a;
};

/** The finest step the visible prices sit on (their gcd), 1 without prices: a tick for tokens whose registry entry has none. */
export function inferTick(prices: readonly bigint[]): bigint {
  let g = 0n;
  for (const p of prices) if (p > 0n) g = gcd(g, p);
  return g > 0n ? g : 1n;
}

/** Grouping choices: 1x, 10x, 100x, 1000x the tick (sompi per whole token). */
export const GROUP_MULTIPLIERS = [1n, 10n, 100n, 1000n] as const;
export const groupSizes = (tick: bigint): bigint[] => GROUP_MULTIPLIERS.map((m) => m * (tick > 0n ? tick : 1n));

export function buildBookModel(view: { asks: readonly (LevelView | BookOrderView)[]; bids: readonly (LevelView | BookOrderView)[] }, o: BookModelOptions = {}): BookModel {
  const scale = o.scale ?? 1n;
  const depth = Math.max(1, o.depth ?? 15);
  const group = o.group ?? 0n;
  const grouped = (ls: Level[], side: 'ask' | 'bid'): Level[] => (o.regroup ? o.regroup(ls, side) : groupLevels(ls, group, side));
  const askLevels = grouped(levelsFromView(view.asks, scale), 'ask').sort((a, b) => (a.price < b.price ? -1 : a.price > b.price ? 1 : 0)).slice(0, depth);
  const bidLevels = grouped(levelsFromView(view.bids, scale), 'bid').sort((a, b) => (a.price > b.price ? -1 : a.price < b.price ? 1 : 0)).slice(0, depth);
  const sum = (ls: Level[]) => ls.reduce((s, l) => s + l.amount, 0n);
  const totalAskAmount = sum(askLevels);
  const totalBidAmount = sum(bidLevels);
  const max = totalAskAmount > totalBidAmount ? totalAskAmount : totalBidAmount;
  const asks = accumulate(askLevels, max, scale);
  const bids = accumulate(bidLevels, max, scale);
  const bestAsk = asks.length ? asks[0].price : null;
  const bestBid = bids.length ? bids[0].price : null;
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
    crossed: both && bestBid >= bestAsk,
    totalAskAmount,
    totalBidAmount,
    anyEstimated: askLevels.some((l) => l.estimated) || bidLevels.some((l) => l.estimated),
    empty: asks.length === 0 && bids.length === 0,
  };
}

// ------------------------------------------------------------------------------------------------ depth chart geometry

export interface DepthChartGeometry {
  width: number;
  height: number;
  /** SVG path data of the bid / ask areas (closed to the baseline); empty string without data */
  bidPath: string;
  askPath: string;
  hasData: boolean;
  /** lowest / highest price on the x axis (sompi per whole token) */
  priceMin: bigint | null;
  priceMax: bigint | null;
  /** x position of the mid price, 0..width */
  midX: number | null;
  /** the deeper side's cumulative amount (base units): the y axis */
  maxAmount: bigint;
}

const fx = (n: number): string => (Math.round(n * 100) / 100).toString();

/**
 * Step-area depth chart. The x axis spans the worst bid to the worst ask, the y axis the deeper side's cumulative amount; positions are
 * computed from bigint ratios so no precision is lost for large prices. Bids grow to the left of the spread, asks to the right.
 */
export function depthChartGeometry(m: BookModel, width = 320, height = 120): DepthChartGeometry {
  const empty: DepthChartGeometry = { width, height, bidPath: '', askPath: '', hasData: false, priceMin: null, priceMax: null, midX: null, maxAmount: 0n };
  if (m.empty) return empty;
  const prices = [...m.bids.map((r) => r.price), ...m.asks.map((r) => r.price)];
  let lo = prices.reduce((a, b) => (b < a ? b : a));
  let hi = prices.reduce((a, b) => (b > a ? b : a));
  if (hi === lo) {
    // a single price: give it a symmetric window so the chart is not a line
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
