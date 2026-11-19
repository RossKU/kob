// How far from the reference the traders let a taker or an auction go.
// There are no `@/` imports, so it is unit-tested with plain `node --test`.
//
// Why the bands exist: trades far from the Binance-derived reference were mostly priced by an
// AUCTION order at its worst bound.
// That covers a stop-market, trailing-stop or OCO stop leg (default band 3 %) and a market order
// (default bound 3 %).
// The executors follow the chain 1 to 2 minutes late and the auctions run 20 to 30 s.
// So every auction is filled at its far end, and the indexer's trade price is the older
// (auctioning) order's price.
// The market maker's trades stay within 0.3 % of the reference at the 90th percentile.
// The bounds below keep every auction within a few tens of bps.
//
// All values are basis points of the price, except `depthFraction` (a share of the visible opposite
// depth).

export interface Bands {
  /** market / close: slippage bound of the IOC auction (protocol default 300) */
  marketBps: number;
  /** streaming (quote-and-execute): tolerance */
  streamingBps: number;
  /**
   * Stop-market, trailing stop, OCO / bracket stop leg: band of the stop's auction (protocol
   * default 300).
   */
  stopBps: number;
  /** stop-limit: distance of the limit from the stop */
  stopLimitBps: number;
  /**
   * A limit placed through the touch (placed as an IOC at that price) and an IOC / FOK: how far
   * past the touch.
   */
  crossBps: number;
  /** a stop entry's auction: how far its limit lies beyond its stop */
  entryBandBps: number;
  /**
   * Dutch auction: start distance range (away from the market) and end distance range (through the
   * market).
   */
  dutchStartBps: [number, number];
  dutchEndBps: [number, number];
  /**
   * A taker takes at most this share of the opposite depth within `depthBandBps` of the touch (KAS
   * books, 0 = no cap).
   */
  depthFraction: number;
  depthBandBps: number;
}

export const DEFAULT_BANDS: Bands = {
  marketBps: 25,
  streamingBps: 25,
  stopBps: 30,
  stopLimitBps: 30,
  crossBps: 15,
  entryBandBps: 30,
  dutchStartBps: [60, 120],
  dutchEndBps: [10, 30],
  depthFraction: 0.25,
  depthBandBps: 30,
};

/** the bands of a config (`bands`, any subset): a missing or invalid value keeps its default */
export function bandsOf(c?: Partial<Bands> | null): Bands {
  const out: Bands = { ...DEFAULT_BANDS };
  if (!c || typeof c !== 'object') return out;
  const num = (v: unknown, lo: number, hi: number): number | undefined => (typeof v === 'number' && Number.isFinite(v) && v >= lo && v <= hi ? v : undefined);
  const range = (v: unknown): [number, number] | undefined =>
    Array.isArray(v) && v.length === 2 && v.every((x) => typeof x === 'number' && Number.isFinite(x) && x >= 0 && x <= 10_000) && v[0] <= v[1] ? [v[0], v[1]] : undefined;
  for (const k of ['marketBps', 'streamingBps', 'stopBps', 'stopLimitBps', 'crossBps', 'entryBandBps', 'depthBandBps'] as const) {
    const n = num(c[k], 1, 10_000);
    if (n !== undefined) out[k] = n;
  }
  out.depthFraction = num(c.depthFraction, 0, 1) ?? out.depthFraction;
  out.dutchStartBps = range(c.dutchStartBps) ?? out.dutchStartBps;
  out.dutchEndBps = range(c.dutchEndBps) ?? out.dutchEndBps;
  return out;
}

/** a book level as the cap reads it: price (sompi per whole token) and base units resting there */
export interface DepthLevel {
  price: bigint;
  amount: bigint;
}

/** the base units resting on `levels` (best first) within `bandBps` of the first level's price */
export function depthWithin(levels: readonly DepthLevel[], bandBps: number): bigint {
  const best = levels[0];
  if (!best) return 0n;
  const reach = (best.price * BigInt(Math.round(bandBps * 100))) / 1_000_000n;
  let sum = 0n;
  for (const l of levels) {
    const far = l.price >= best.price ? l.price - best.price : best.price - l.price;
    if (far > reach) break;
    sum += l.amount;
  }
  return sum;
}

/**
 * The size a taker may take: `amount`, cut to `fraction` of the opposite depth within `bandBps` of
 * the touch,
 * but never below `floor` (the smallest order the trader places) and never above `amount`.
 * An empty opposite side, a zero fraction or a zero depth leave `amount` alone.
 * The planner refuses a taker with nothing to take by itself.
 */
export function capToDepth(amount: bigint, opposite: readonly DepthLevel[], fraction: number, bandBps: number, floor: bigint): bigint {
  if (!(fraction > 0)) return amount;
  const depth = depthWithin(opposite, bandBps);
  if (depth <= 0n) return amount;
  const cap = (depth * BigInt(Math.round(fraction * 10_000))) / 10_000n;
  const lo = floor < amount ? floor : amount;
  return amount <= cap ? amount : cap > lo ? cap : lo;
}
