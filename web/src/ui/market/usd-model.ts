// Token/token pair figures through the KAS books (the pair page `#/market/<base>/<quote>`; with the configured USD quote token as QUOTE, e.g. the TN10 soak's
// TUSD, 1 TUSD = 1 USD worth of KAS, the pair reads as BASE/USD). Pure and DOM-free; exact rationals (kob/pair.ts) for every price, floats only for chart
// coordinates.
//
// Conversion method (the same pair math as the pair book, implied through the two KAS books):
//
//   price(B in Q) = KAS per whole B / KAS per whole Q
//
// Candles are converted bucket by bucket on ONE time grid (the indexer's candle buckets of the chosen interval): both legs are taken from the
// same bucket, a bucket without trades of a leg carries that leg's previous close (gap filling), so the rate used for a bucket is always the
// time-aligned Q/KAS price of that bucket, never a later or an average one. Per bucket, with B = the base leg and Q = the quote leg:
//
//   open  = B.open / Q.open             close = B.close / Q.close
//   high  = max(open, close, B.high / Q.mid, B.mid / Q.low)
//   low   = min(open, close, B.low / Q.mid,  B.mid / Q.high)
//
// where mid is the leg's volume-weighted price of the bucket (quote volume / volume), its close when it did not trade. Each leg's extreme is taken
// at the other leg's typical price of the same bucket. A bucket is "filled" (drawn muted, no trades) only when neither leg traded in it.
import type { StatsView } from '../../data/indexer-types';
import { cmpRational, formatRational, ratToNumber, reduce, significantDp, type Rational } from '../../kob/pair';
import { pow10, SOMPI_PER_KAS } from '../../kob/units';
import { fillCandleGaps, type Candle, type ChartBar, type VolumeBar } from './market-model';

/** A KAS-priced leg: indexer candles of a token (prices = sompi per `basis` base units). */
export interface TokenLeg {
  candles: readonly Candle[];
  decimals: number;
  basis: bigint;
}
export interface RatioCandle {
  /** bucket start, unix ms */
  t: number;
  o: Rational;
  h: Rational;
  l: Rational;
  c: Rational;
  /** volume of the BASE leg in whole units */
  volume: number;
  /** the same volume in whole QUOTE units, converted at the bucket's volume-weighted rate (BASE mid / QUOTE mid) */
  quoteVolume: number;
  /** trades of the BASE leg's KAS market in the bucket */
  trades: number;
  /** neither leg traded in the bucket (both carried) */
  filled: boolean;
}

/** KAS per whole token of a candle price (sompi per `basis` base units), exact. */
export function kasPerToken(price: bigint, decimals: number, basis: bigint): Rational {
  return reduce({ num: price * pow10(decimals), den: basis * SOMPI_PER_KAS });
}

const div = (a: Rational, b: Rational): Rational => reduce({ num: a.num * b.den, den: a.den * b.num });
const maxR = (...rs: Rational[]): Rational => rs.reduce((m, r) => (cmpRational(r, m) > 0 ? r : m));
const minR = (...rs: Rational[]): Rational => rs.reduce((m, r) => (cmpRational(r, m) < 0 ? r : m));

interface LegBucket {
  o: Rational;
  h: Rational;
  l: Rational;
  c: Rational;
  mid: Rational;
  filled: boolean;
  /** the bucket's volume (base units) and KAS turnover (sompi) */
  volume: bigint;
  quote: bigint;
  trades: number;
}

function legBuckets(leg: TokenLeg, intervalMs: number, until: number, maxBars: number): Map<number, LegBucket> {
  const out = new Map<number, LegBucket>();
  const k = (p: bigint) => kasPerToken(p, leg.decimals, leg.basis);
  for (const c of fillCandleGaps(leg.candles, intervalMs, { until, maxBars })) {
    if (c.o <= 0n || c.h <= 0n || c.l <= 0n || c.c <= 0n) continue;
    // volume-weighted price of the bucket: quote (sompi) per volume (base units) -> KAS per whole token
    const mid = !c.filled && c.volume > 0n && c.quote > 0n ? reduce({ num: c.quote * pow10(leg.decimals), den: c.volume * SOMPI_PER_KAS }) : k(c.c);
    out.set(c.t, { o: k(c.o), h: k(c.h), l: k(c.l), c: k(c.c), mid, filled: c.filled, volume: c.volume, quote: c.quote, trades: c.filled ? 0 : c.trades });
  }
  return out;
}

export interface RatioOptions {
  intervalMs: number;
  /** extend both legs with carried buckets up to this time (unix ms); default: the newest candle */
  until?: number;
  maxBars?: number;
}

/**
 * BASE / QUOTE candles on the common bucket grid (see the method at the top). Buckets before either leg's first trade are left out (there is no
 * price to divide by).
 */
export function ratioCandles(base: TokenLeg, quote: TokenLeg, o: RatioOptions): RatioCandle[] {
  const maxBars = Math.max(1, o.maxBars ?? 2000);
  const lastOf = (l: TokenLeg) => l.candles.reduce((m, c) => Math.max(m, c.t), 0);
  const until = o.until ?? Math.max(lastOf(quote), lastOf(base));
  const qb = legBuckets(quote, o.intervalMs, until, maxBars);
  const bb = legBuckets(base, o.intervalMs, until, maxBars);
  const out: RatioCandle[] = [];
  const times = [...qb.keys()].sort((a, b) => a - b);
  for (const t of times) {
    const q = qb.get(t)!;
    const b = bb.get(t);
    if (!b) continue;
    const open = div(b.o, q.o);
    const close = div(b.c, q.c);
    const high = maxR(open, close, div(b.h, q.mid), div(b.mid, q.l));
    const low = minR(open, close, div(b.l, q.mid), div(b.mid, q.h));
    const volume = Number(b.volume) / 10 ** base.decimals;
    const quoteVolume = volume * ratToNumber(div(b.mid, q.mid));
    out.push({ t, o: open, h: high, l: low, c: close, volume, quoteVolume, trades: b.trades, filled: q.filled && b.filled });
  }
  return out.slice(-maxBars);
}

/** Chart series of ratio candles (time in UTC seconds, prices as floats). */
export function ratioSeries(candles: readonly RatioCandle[]): { bars: ChartBar[]; volumes: VolumeBar[] } {
  const bars: ChartBar[] = [];
  const volumes: VolumeBar[] = [];
  for (const c of candles) {
    const time = Math.floor(c.t / 1000);
    bars.push({ time, open: ratToNumber(c.o), high: ratToNumber(c.h), low: ratToNumber(c.l), close: ratToNumber(c.c), filled: c.filled });
    volumes.push({ time, value: c.volume, up: cmpRational(c.c, c.o) >= 0 });
  }
  return { bars, volumes };
}

/** Fraction digits of a USD price (about 5 significant digits, 2 to 10). */
export const usdDp = (ref: Rational | null): number => (ref ? significantDp(ref, 5, 2, 10) : 4);

/** Exact text of a USD price with `dp` digits, thousands grouped. */
export const usdText = (r: Rational, dp: number): string => formatRational(r, dp, 'nearest', dp, ',');

// ------------------------------------------------------------------------------------------------ 24 h figures

export interface UsdStats {
  last: Rational | null;
  /** price 24 h ago (the ratio of both legs' 24 h opens) */
  open: Rational | null;
  /** change in basis points (signed) */
  changeBps: number | null;
}

const statPrice = (s: StatsView | null | undefined, field: 'last' | 'open_24h', decimals: number): Rational | null => {
  const v = s?.[field];
  if (!s || !v || !/^[1-9]\d*$/.test(v) || !/^[1-9]\d*$/.test(s.price_basis)) return null;
  return kasPerToken(BigInt(v), decimals, BigInt(s.price_basis));
};

/**
 * Last price and 24 h change in USD from the two legs' `/v1/stats`. Each leg's last trade is its newest; the ratio of the
 * two is the newest implied price (the legs' last trades can be minutes apart in a thin market: the chart's time-aligned candles are the history).
 */
export function usdStats(base: { stats: StatsView | null; decimals: number }, quote: { stats: StatsView | null; decimals: number }): UsdStats {
  const q = { last: statPrice(quote.stats, 'last', quote.decimals), open: statPrice(quote.stats, 'open_24h', quote.decimals) };
  const b = { last: statPrice(base.stats, 'last', base.decimals), open: statPrice(base.stats, 'open_24h', base.decimals) };
  const last = b.last && q.last ? div(b.last, q.last) : null;
  const open = b.open && q.open ? div(b.open, q.open) : null;
  const changeBps = last && open ? Math.round((ratToNumber(div(last, open)) - 1) * 10_000) : null;
  return { last, open, changeBps };
}

// ------------------------------------------------------------------------------------------------ the USD quote token and the landing screen

/** The app's USD quote token: the first registry token the config marks `USD` (config `quoteTokens`), or null (no USD views). */
export function usdTokenOf<T extends { covenantId: string }>(quoteTokens: Readonly<Record<string, string>>, tokens: readonly T[]): T | null {
  return tokens.find((x) => quoteTokens[x.covenantId.toLowerCase()] === 'USD') ?? null;
}

export type HomeTarget = { name: 'pair'; base: string; quote: string } | { name: 'token'; covenantId: string } | { name: 'market' };

/**
 * The landing screen of config `home`: `list`, `kas-usd`, `usd:<id>`, `market:<id>`, or `auto` = KAS/USD when a USD quote token is configured, else the
 * market page (chart) of the first tradable registry token, else the list. KAS/USD is the USD token's own KAS book shown inverted (a native market with
 * its book, trades and ticket: the token page, whose default orientation for a USD token is KAS/USD), not a derived route. `usd:<id>` is that token in USD:
 * the tradable pair page `<id>/<USD token>` (needs the USD token); `market:<id>` a registry token: otherwise `auto` applies.
 */
export function resolveHome(home: string, usdToken: string | null, tokens: readonly { covenantId: string; tradable: boolean; status: string }[]): HomeTarget {
  const known = (id: string) => tokens.some((x) => x.covenantId === id);
  if (home === 'list') return { name: 'market' };
  if (home === 'kas-usd' && usdToken) return { name: 'token', covenantId: usdToken };
  const m = /^(usd|market):([0-9a-f]{64})$/.exec(home);
  if (m && known(m[2]!)) {
    if (m[1] === 'market') return { name: 'token', covenantId: m[2]! };
    if (usdToken && m[2] !== usdToken) return { name: 'pair', base: m[2]!, quote: usdToken };
  }
  if (usdToken) return { name: 'token', covenantId: usdToken };
  const first = tokens.find((x) => x.tradable && x.status === 'listed') ?? tokens.find((x) => x.tradable);
  return first ? { name: 'token', covenantId: first.covenantId } : { name: 'market' };
}
