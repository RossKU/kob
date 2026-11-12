// View models of the market-data endpoints (M5): price-basis conversion, display precision, candles with gap filling, 24 h stats, the trade
// tape and depth series. Pure; bigint arithmetic for every exact figure, floats only for chart coordinates.
//
// Price basis: every market-data price is sompi per `price_basis` token base units (the token's standard scale `10^decimals`). KAS per whole
// token = price / 1e8 * 10^decimals / price_basis. A book price (an order's state price: sompi per `scale` base units) is the same thing with
// basis = scale, so every formatter below takes (price, decimals, basis) and works for both.
import type { CandleInterval, CandleView, DepthView, StatsView, TradeView, TradesView } from '../../data/indexer-types';
import { sompiToUsd, type Rate } from '../../app/fiat';
import { cmpRational, formatRational, type Rational } from '../../kob/pair';
import { formatNumber, t } from '../../i18n';
import { formatPricePerToken, formatUnits, pow10, roundDiv, SOMPI_PER_KAS } from '../../kob/units';
import { displayPriceDp } from '../kit/format';
import type { BookModel } from './book-model';
import type { TradeRow } from './trades-model';

// ------------------------------------------------------------------------------------------------ parsing

/** Unsigned decimal string (or safe integer) -> bigint; anything else -> null (never guessed). */
export function bigOrNull(v: unknown): bigint | null {
  if (typeof v === 'bigint') return v >= 0n ? v : null;
  if (typeof v === 'number') return Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : null;
  if (typeof v === 'string' && /^\d+$/.test(v)) return BigInt(v);
  return null;
}

/** A usable price basis (positive), else null. */
export const basisOf = (v: unknown): bigint | null => {
  const b = bigOrNull(v);
  return b !== null && b > 0n ? b : null;
};

// ------------------------------------------------------------------------------------------------ price basis conversion

/** Sompi per `basis` base units -> sompi per whole token, rounded to nearest. */
export function basisToTokenSompi(price: bigint, decimals: number, basis: bigint): bigint {
  return roundDiv(price * pow10(decimals), basis);
}

/** Sompi per `basis` base units -> KAS per whole token as a float (chart coordinates only; exact text goes through `priceText`). */
export function basisToNumber(price: bigint, decimals: number, basis: bigint): number {
  if (basis <= 0n) return 0;
  // 12 extra digits keep ~15 significant digits for any realistic price
  const scaled = (price * pow10(decimals) * 10n ** 12n) / basis;
  return Number(scaled) / 1e20;
}

/** Base units -> whole tokens as a float (chart coordinates only). */
export const unitsToNumber = (amount: bigint, decimals: number): number => Number(amount) / 10 ** decimals;

// the significant-digit rule is the kit's (ui/kit/format.ts: `displayPriceDp` is the one display precision; `priceDecimals` is its uncapped base)
export { priceDecimals } from '../kit/format';

/** Exact KAS-per-token text with `dp` fraction digits (half up), thousands grouped. */
export function priceText(price: bigint, decimals: number, basis: bigint, dp: number): string {
  // a price DIFFERENCE can be negative: the spread of a momentarily crossed book (a bid above an ask until a matcher crosses them)
  if (price < 0n) return '-' + priceText(-price, decimals, basis, dp);
  if (basis <= 0n) return formatUnits(price, 8, { maxFraction: dp, trim: false, group: ',' });
  return formatPricePerToken(price, decimals, basis, { maxFraction: dp, trim: false, group: ',' });
}

/** Every digit of a price (KAS per whole token; trailing zeros trimmed, thousands grouped): the `title` of a price that is shown rounded to the market's display decimals. */
export function exactPriceText(price: bigint, decimals: number, basis: bigint): string {
  if (price < 0n) return '-' + exactPriceText(-price, decimals, basis);
  if (basis <= 0n) return formatUnits(price, 8, { maxFraction: 8, group: ',' });
  return formatPricePerToken(price, decimals, basis, { maxFraction: 18, group: ',' });
}

const plainNumber = (s: string): string => s.replace(/,/g, '').replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '');

/** `exact` when `shown` (a price cut to the display decimals) is not the whole of it, else null: a title is only worth having when it says more than the cell. */
export function exactIfRounded(shown: string | null, exact: string | null): string | null {
  if (!shown || !exact) return null;
  return plainNumber(shown) === plainNumber(exact) ? null : exact;
}

/** Token amount text with at most `maxFraction` digits (trailing zeros trimmed), thousands grouped. */
export function amountText(amount: bigint, decimals: number, maxFraction = 4): string {
  return formatUnits(amount, decimals, { maxFraction: Math.min(maxFraction, decimals), group: ',' });
}

/** Compact text of a large figure (`1.2M`, `950K`, `12.3`) in the current locale; exact values belong in titles. */
export function compactText(n: number, maxFraction = 2): string {
  if (!Number.isFinite(n)) return '—';
  if (Math.abs(n) < 10_000) return formatNumber(n, { maximumFractionDigits: Math.abs(n) < 10 ? Math.max(maxFraction, 4) : maxFraction });
  return formatNumber(n, { notation: 'compact', maximumFractionDigits: maxFraction });
}

// ------------------------------------------------------------------------------------------------ candles

export const INTERVAL_MS: Record<CandleInterval, number> = { '1m': 60_000, '5m': 300_000, '1h': 3_600_000, '1d': 86_400_000 };

export interface Candle {
  /** bucket start, unix ms */
  t: number;
  o: bigint;
  h: bigint;
  l: bigint;
  c: bigint;
  /** token base units */
  volume: bigint;
  /** sompi */
  quote: bigint;
  trades: number;
  /** a bucket without trades, carried from the previous close */
  filled: boolean;
}

/** Indexer candles -> typed candles, ascending, deduplicated by bucket; malformed items are dropped. */
export function parseCandles(items: readonly CandleView[]): Candle[] {
  const by = new Map<number, Candle>();
  for (const it of items) {
    const o = bigOrNull(it.o), h = bigOrNull(it.h), l = bigOrNull(it.l), c = bigOrNull(it.c);
    if (!Number.isSafeInteger(it.t) || o === null || h === null || l === null || c === null) continue;
    by.set(it.t, { t: it.t, o, h, l, c, volume: bigOrNull(it.volume) ?? 0n, quote: bigOrNull(it.quote_volume) ?? 0n, trades: Number(it.trades) || 0, filled: false });
  }
  return [...by.values()].sort((a, b) => a.t - b.t);
}

export interface GapFillOptions {
  /** extend the series with carried buckets up to (and including) the bucket of this time, unix ms */
  until?: number;
  /** keep at most this many buckets (the newest); default 2000 */
  maxBars?: number;
}

/**
 * The indexer omits buckets without trades; a chart needs them. Every missing bucket between two candles (and after the last one, up to
 * `until`) becomes a flat candle at the previous close with zero volume. Only the newest `maxBars` buckets are produced (a sparse 1m
 * series over months does not allocate millions of bars).
 */
export function fillCandleGaps(candles: readonly Candle[], intervalMs: number, opts: GapFillOptions = {}): Candle[] {
  if (!candles.length || intervalMs <= 0) return [...candles];
  const maxBars = Math.max(1, opts.maxBars ?? 2000);
  const sorted = [...candles].sort((a, b) => a.t - b.t);
  const lastT = sorted[sorted.length - 1]!.t;
  const untilBucket = opts.until !== undefined ? Math.floor(opts.until / intervalMs) * intervalMs : lastT;
  const end = Math.max(lastT, untilBucket);
  const start = Math.max(sorted[0]!.t, end - (maxBars - 1) * intervalMs);
  const out: Candle[] = [];
  let i = 0;
  let prevClose: bigint | null = null;
  // candles before the window only provide the carried close
  while (i < sorted.length && sorted[i]!.t < start) prevClose = sorted[i++]!.c;
  for (let t = start; t <= end; t += intervalMs) {
    // candles off the interval grid (should not happen) are taken as they come
    while (i < sorted.length && sorted[i]!.t < t) {
      out.push(sorted[i]!);
      prevClose = sorted[i++]!.c;
    }
    if (i < sorted.length && sorted[i]!.t === t) {
      out.push(sorted[i]!);
      prevClose = sorted[i++]!.c;
    } else if (prevClose !== null) {
      out.push({ t, o: prevClose, h: prevClose, l: prevClose, c: prevClose, volume: 0n, quote: 0n, trades: 0, filled: true });
    }
  }
  return out.slice(-maxBars);
}

export interface ChartBar { time: number; open: number; high: number; low: number; close: number; /** no trades in the bucket (carried close) */ filled: boolean }
export interface VolumeBar { time: number; value: number; up: boolean }

/** Candles -> chart series (time in UTC seconds, prices in KAS per whole token, volume in whole tokens). */
export function chartSeries(candles: readonly Candle[], decimals: number, basis: bigint): { bars: ChartBar[]; volumes: VolumeBar[] } {
  const bars: ChartBar[] = [];
  const volumes: VolumeBar[] = [];
  for (const c of candles) {
    const time = Math.floor(c.t / 1000);
    bars.push({ time, open: basisToNumber(c.o, decimals, basis), high: basisToNumber(c.h, decimals, basis), low: basisToNumber(c.l, decimals, basis), close: basisToNumber(c.c, decimals, basis), filled: c.filled });
    volumes.push({ time, value: unitsToNumber(c.volume, decimals), up: c.c >= c.o });
  }
  return { bars, volumes };
}

// ------------------------------------------------------------------------------------------------ 24 h stats

export type Tone = 'up' | 'down' | 'flat';

export interface StatsModel {
  /** KAS per token, fixed decimals */
  last: string | null;
  lastSide: 'buy' | 'sell' | null;
  /** `+2.87%` / `-0.40%` / `0.00%` */
  change: string | null;
  /** muted ('flat') while the change rests on a thin window (`changeNote`) */
  changeTone: Tone | null;
  /** the tooltip of the change when it rests on a thin window and / or a thin book: its lines ("Based on N trades in 24 h", "Thin book: ..."), one per line */
  changeNote?: string | null;
  /** fewer than `THIN_TRADES_24H` trades (`data-thin`) */
  changeThin?: boolean;
  /** a thin book (`thinBookUnder`; `data-thin-book`) */
  changeThinBook?: boolean;
  high: string | null;
  low: string | null;
  /** 24 h volume in tokens (compact) and exact */
  volume: string | null;
  volumeExact: string | null;
  /** 24 h volume in KAS (compact) and exact */
  quoteVolume: string | null;
  quoteVolumeExact: string | null;
  trades: number | null;
  /** the trade count as shown when it is not the plain number (a lower bound: `≥ 12`) */
  tradesText?: string | null;
  /** `0.24%` */
  spread: string | null;
  /** the whole price when the shown one is rounded to the display decimals (their tooltips) */
  lastExact?: string | null;
  highExact?: string | null;
  lowExact?: string | null;
  /** decimals used for the prices */
  dp: number;
}

/**
 * A 24 h change rests on a thin window when it counts fewer trades than this: the open is the FIRST trade of the window, so one tiny trade can put the change at +900%.
 * Such a change is shown muted with its trade count in the tooltip (nothing else changes: same text, same cell).
 */
export const THIN_TRADES_24H = 3;

/** The tooltip of a change over a thin window ("Based on 2 trades in 24 h"), or null when it has a normal basis or the indexer did not report the count (an older one: no marker). */
export function thinChangeNote(bps: number | null, trades: number | null | undefined): string | null {
  if (bps === null || typeof trades !== 'number' || !Number.isInteger(trades) || trades < 1 || trades >= THIN_TRADES_24H) return null;
  return t(trades === 1 ? 'market.change.thinOne' : 'market.change.thin', { n: trades });
}

/** A book's thin-book threshold is this share of the token's fully diluted value (FDV)... */
export const THIN_BOOK_FDV_SHARE = 0.001;
/** ...but at least this many US dollars (CoinMarketCap's smallest test order for its liquidity score) */
export const THIN_BOOK_MIN_USD = 100;
/** ...and at most this many (its largest). The depth band is CoinGecko's +/-2 %. */
export const THIN_BOOK_MAX_USD = 10_000;

const FDV_SHARE_DEN = BigInt(Math.round(1 / THIN_BOOK_FDV_SHARE));

/** The thin-book threshold in USD for a token of fully diluted value `fdvUsd`: `THIN_BOOK_FDV_SHARE` of it, clamped to `THIN_BOOK_MIN_USD` .. `THIN_BOOK_MAX_USD`. Exact. */
export function thinBookThresholdUsd(fdvUsd: Rational): Rational {
  const share: Rational = { num: fdvUsd.num, den: fdvUsd.den * FDV_SHARE_DEN };
  const lo: Rational = { num: BigInt(THIN_BOOK_MIN_USD), den: 1n };
  const hi: Rational = { num: BigInt(THIN_BOOK_MAX_USD), den: 1n };
  return cmpRational(share, lo) < 0 ? lo : cmpRational(share, hi) > 0 ? hi : share;
}

/**
 * The thin-book threshold (USD) when the book of a KAS market is thin, else null: the smaller of `depth_2pct_bid_sompi` and `depth_2pct_ask_sompi` (a missing side is 0) is
 * worth less than `thinBookThresholdUsd` of the token's FDV = `supplyUnits` (its fixed total supply, base units) x the reference price (the mid, else the one side, else the
 * last trade) x `rate` (USD per KAS, the app's price feed). Null without the rate, the supply, a price or the indexer's two figures (an older one): never a guess.
 */
export function thinBookUnder(
  v: Pick<StatsView, 'price_basis' | 'mid' | 'best_ask' | 'best_bid' | 'last' | 'depth_2pct_bid_sompi' | 'depth_2pct_ask_sompi'> | null | undefined,
  rate: Rate | null | undefined,
  supplyUnits: bigint | null | undefined,
): Rational | null {
  if (!v || !rate || !supplyUnits || supplyUnits <= 0n) return null;
  const bid = bigOrNull(v.depth_2pct_bid_sompi);
  const ask = bigOrNull(v.depth_2pct_ask_sompi);
  const basis = basisOf(v.price_basis);
  const ref = bigOrNull(v.mid) ?? bigOrNull(v.best_ask) ?? bigOrNull(v.best_bid) ?? bigOrNull(v.last);
  if (bid === null || ask === null || bid < 0n || ask < 0n || !basis || ref === null || ref <= 0n) return null;
  // sompi per `basis` base units x base units / basis = the FDV in sompi (exact), then x the rate
  const fdvUsd = sompiToUsd(ref * supplyUnits, rate);
  const threshold = thinBookThresholdUsd({ num: fdvUsd.num, den: fdvUsd.den * basis });
  const usd = sompiToUsd(bid < ask ? bid : ask, rate);
  return cmpRational(usd, threshold) < 0 ? threshold : null;
}

/** What marks a 24 h change as thin: few trades and / or a thin book. `note` is the tooltip, each applicable line on its own line; null when neither applies or there is no change. */
export interface ChangeMarks {
  note: string;
  thinTrades: boolean;
  thinBook: boolean;
}

/** `thinBookUsd`: the threshold when the book is thin (`thinBookUnder`), else null. */
export function changeMarks(bps: number | null, trades: number | null | undefined, thinBookUsd: Rational | null): ChangeMarks | null {
  if (bps === null) return null;
  const tradesLine = thinChangeNote(bps, trades);
  if (!tradesLine && !thinBookUsd) return null;
  // the book line and, under it, the rule it comes from (the named constants)
  const bookLine = thinBookUsd
    ? [
        t('market.change.thinBook', { usd: '$' + formatRational(thinBookUsd, 0, 'nearest', 0, ',') }),
        t('market.change.thinBookRule', {
          share: formatNumber(THIN_BOOK_FDV_SHARE * 100, { maximumFractionDigits: 4 }) + '%', min: '$' + formatNumber(THIN_BOOK_MIN_USD), max: '$' + formatNumber(THIN_BOOK_MAX_USD),
        }),
      ].join('\n')
    : null;
  const lines = [tradesLine, bookLine].filter((x): x is string => !!x);
  return { note: lines.join('\n'), thinTrades: !!tradesLine, thinBook: !!thinBookUsd };
}

/** The mark fields of a `StatsModel` (none when the change is not thin). */
export function changeMarkFields(m: ChangeMarks | null): Pick<StatsModel, 'changeNote' | 'changeThin' | 'changeThinBook'> {
  return m ? { changeNote: m.note, ...(m.thinTrades ? { changeThin: true } : {}), ...(m.thinBook ? { changeThinBook: true } : {}) } : {};
}

/** Signed percentage text of basis points: 287 -> `+2.87%`. */
export function bpsPercentText(bps: number): string {
  const v = Math.trunc(bps);
  const sign = v > 0 ? '+' : v < 0 ? '-' : '';
  const a = Math.abs(v);
  return `${sign}${Math.floor(a / 100)}.${String(a % 100).padStart(2, '0')}%`;
}

export const toneOf = (bps: number | null): Tone | null => (bps === null ? null : bps > 0 ? 'up' : bps < 0 ? 'down' : 'flat');

/** Stats of `/v1/stats` for display. `null` stats (an indexer without the route) give an all-dash model. */
export function statsModel(v: StatsView | null, decimals: number, dpHint?: number, rate?: Rate | null, supplyUnits?: bigint | null): StatsModel {
  const basis = v ? basisOf(v.price_basis) : null;
  const last = v && basis ? bigOrNull(v.last) : null;
  const ref = last ?? (v && basis ? bigOrNull(v.mid) ?? bigOrNull(v.best_ask) ?? bigOrNull(v.best_bid) : null);
  const dp = dpHint ?? displayPriceDp(ref !== null && basis ? basisToTokenSompi(ref, decimals, basis) : null);
  const p = (x: unknown): string | null => {
    const b = bigOrNull(x);
    return b !== null && basis ? priceText(b, decimals, basis, dp) : null;
  };
  const vol = v ? bigOrNull(v.volume_24h) : null;
  const qv = v ? bigOrNull(v.quote_volume_24h) : null;
  const bps = v && typeof v.change_24h_bps === 'number' && Number.isFinite(v.change_24h_bps) ? v.change_24h_bps : null;
  const exact = (x: unknown): string | null => {
    const b = bigOrNull(x);
    return b !== null && basis ? exactIfRounded(p(x), exactPriceText(b, decimals, basis)) : null;
  };
  const marks = changeMarks(bps, v?.trades_24h, thinBookUnder(v, rate, supplyUnits));
  return {
    last: p(v?.last),
    lastSide: v?.last_side === 'buy' || v?.last_side === 'sell' ? v.last_side : null,
    change: bps === null ? null : bpsPercentText(bps),
    changeTone: marks ? 'flat' : toneOf(bps),
    ...changeMarkFields(marks),
    high: p(v?.high_24h),
    low: p(v?.low_24h),
    lastExact: exact(v?.last),
    highExact: exact(v?.high_24h),
    lowExact: exact(v?.low_24h),
    volume: vol === null ? null : compactText(unitsToNumber(vol, decimals)),
    volumeExact: vol === null ? null : formatUnits(vol, decimals, { group: ',' }),
    quoteVolume: qv === null ? null : compactText(Number(qv) / 1e8),
    quoteVolumeExact: qv === null ? null : formatUnits(qv, 8, { group: ',' }),
    trades: v && typeof v.trades_24h === 'number' ? v.trades_24h : null,
    spread: v && typeof v.spread_bps === 'number' ? bpsPercentText(v.spread_bps).replace('+', '') : null,
    dp,
  };
}

// ------------------------------------------------------------------------------------------------ trade tape

export interface TapeRow {
  id: string;
  /** the transaction of the trade (the explorer link of the row); null when unknown */
  txid?: string | null;
  /** unix ms; null when unknown */
  timeMs: number | null;
  daa: number;
  /** the aggressor's side */
  side: 'buy' | 'sell' | null;
  /** sompi per `basis` base units */
  price: bigint | null;
  /** token base units */
  amount: bigint | null;
  /** sompi */
  quote: bigint | null;
  settled: boolean;
  fills: number;
}

export interface Tape {
  basis: bigint | null;
  rows: TapeRow[];
  /** where the rows came from: the trades endpoint, or the fill events of an older indexer */
  source: 'trades' | 'fills';
}

/** `/v1/trades` -> tape rows (newest first, as served). */
export function tapeFromTrades(v: TradesView, limit = 50): Tape {
  const rows = v.items.slice(0, limit).map((t: TradeView): TapeRow => ({
    id: String(t.id),
    txid: typeof t.txid === 'string' && t.txid ? t.txid : null,
    timeMs: typeof t.ts === 'number' && t.ts > 0 ? t.ts : null,
    daa: t.daa,
    side: t.side === 'buy' || t.side === 'sell' ? t.side : null,
    price: bigOrNull(t.price),
    amount: bigOrNull(t.amount),
    quote: bigOrNull(t.quote),
    settled: !!t.settled,
    fills: Number(t.fills) || 1,
  }));
  return { basis: basisOf(v.price_basis), rows, source: 'trades' };
}

/**
 * Fallback for an indexer without `/v1/trades`: one row per fill event (the orders' state prices, so basis = the market's scale). Without a
 * known scale the prices are left per the orders' own basis.
 */
export function tapeFromFills(rows: readonly TradeRow[], scale: bigint | null): Tape {
  return {
    basis: scale,
    source: 'fills',
    rows: rows.map((r) => ({
      id: `f${r.id}`,
      txid: r.txid || null,
      timeMs: r.timeMs,
      daa: r.daa,
      side: r.takerSide,
      price: r.price,
      amount: r.amount,
      quote: r.value,
      settled: r.settled,
      fills: 1,
    })),
  };
}

/** `HH:MM:SS` in local time (the tape shows today's trades; older ones get the date in the title). */
export function tapeTime(ms: number | null): string {
  if (ms === null) return '--:--:--';
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

// ------------------------------------------------------------------------------------------------ depth

export interface DepthPoint {
  /** KAS per whole token */
  price: number;
  /** cumulative whole tokens from the best price up to this level */
  cum: number;
}

export interface DepthSeries {
  /** best first (highest bid first) */
  bids: DepthPoint[];
  /** best first (lowest ask first) */
  asks: DepthPoint[];
  mid: number | null;
  /** some bid amounts are upper bounds */
  estimated: boolean;
  /** exact cumulative totals (base units) of each side, for the summary */
  bidTotal: bigint;
  askTotal: bigint;
}

/** Cumulates amounts from the best price outwards (the indexer's `cum_*` fields are not trusted for drawing: they are recomputed). */
export function cumulate(levels: readonly { price: bigint; amount: bigint }[]): { price: bigint; amount: bigint; cum: bigint }[] {
  let cum = 0n;
  return levels.map((l) => {
    cum += l.amount;
    return { ...l, cum };
  });
}

/** `/v1/depth` -> depth series in KAS per token and whole tokens. Levels are sorted best first whatever the server order. */
export function depthFromView(v: DepthView, decimals: number): DepthSeries | null {
  const basis = basisOf(v.price_basis);
  if (!basis) return null;
  const parse = (ls: DepthView['bids']) =>
    ls.flatMap((l) => {
      const price = bigOrNull(l.price);
      const amount = bigOrNull(l.amount);
      return price !== null && amount !== null && price > 0n && amount > 0n ? [{ price, amount, estimated: !!l.estimated }] : [];
    });
  const bids = parse(v.bids).sort((a, b) => (a.price > b.price ? -1 : a.price < b.price ? 1 : 0));
  const asks = parse(v.asks).sort((a, b) => (a.price < b.price ? -1 : a.price > b.price ? 1 : 0));
  const pts = (ls: { price: bigint; amount: bigint }[]) => cumulate(ls).map((l) => ({ price: basisToNumber(l.price, decimals, basis), cum: unitsToNumber(l.cum, decimals) }));
  const bestBid = bids[0]?.price ?? null;
  const bestAsk = asks[0]?.price ?? null;
  return {
    bids: pts(bids),
    asks: pts(asks),
    mid: bestBid !== null && bestAsk !== null ? basisToNumber(bestBid + bestAsk, decimals, basis * 2n) : null,
    estimated: bids.some((l) => l.estimated) || asks.some((l) => l.estimated),
    bidTotal: bids.reduce((s, l) => s + l.amount, 0n),
    askTotal: asks.reduce((s, l) => s + l.amount, 0n),
  };
}

/** Fallback without `/v1/depth`: the aggregated book (state prices per `scale` base units, amounts in base units) as a depth series. */
export function depthFromBook(m: BookModel, decimals: number, scale: bigint): DepthSeries {
  const pts = (rows: BookModel['bids']) => rows.map((r) => ({ price: basisToNumber(r.price, decimals, scale), cum: unitsToNumber(r.cumAmount, decimals) }));
  return {
    bids: pts(m.bids),
    asks: pts(m.asks),
    mid: m.mid !== null ? basisToNumber(m.mid, decimals, scale) : null,
    estimated: m.anyEstimated,
    bidTotal: m.totalBidAmount,
    askTotal: m.totalAskAmount,
  };
}

export interface DepthGeometry {
  width: number;
  height: number;
  bidArea: string;
  bidLine: string;
  askArea: string;
  askLine: string;
  /** x of the mid price */
  midX: number | null;
  xTicks: { x: number; value: number }[];
  yTicks: { y: number; value: number }[];
  xMin: number;
  xMax: number;
  yMax: number;
  hasData: boolean;
}

const r2 = (n: number): string => (Math.round(n * 100) / 100).toString();

/** "Nice" tick values covering [lo, hi] (about `count` of them). */
export function niceTicks(lo: number, hi: number, count = 4): number[] {
  if (!(hi > lo) || !Number.isFinite(lo) || !Number.isFinite(hi)) return [];
  const raw = (hi - lo) / Math.max(1, count);
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
  const out: number[] = [];
  for (let v = Math.ceil(lo / step) * step; v <= hi + step * 1e-9; v += step) out.push(Number(v.toPrecision(12)));
  return out;
}

/**
 * Step-area geometry of a depth chart: bids to the left of the mid price, asks to the right, the x range symmetric around the mid (so the
 * two sides are comparable), the y axis the deeper side's cumulative amount. Coordinates in an SVG box of `width` x `height`.
 */
export function depthGeometry(s: DepthSeries, width: number, height: number): DepthGeometry {
  const empty: DepthGeometry = { width, height, bidArea: '', bidLine: '', askArea: '', askLine: '', midX: null, xTicks: [], yTicks: [], xMin: 0, xMax: 0, yMax: 0, hasData: false };
  if (!s.bids.length && !s.asks.length) return empty;
  const prices = [...s.bids, ...s.asks].map((p) => p.price);
  let lo = Math.min(...prices);
  let hi = Math.max(...prices);
  const mid = s.mid ?? (s.bids.length ? s.bids[0]!.price : s.asks[0]!.price);
  const half = Math.max(mid - lo, hi - mid, mid * 0.01, 1e-12);
  lo = mid - half * 1.04;
  hi = mid + half * 1.04;
  const yMax = Math.max(s.bids.at(-1)?.cum ?? 0, s.asks.at(-1)?.cum ?? 0) * 1.1 || 1;
  const x = (p: number) => ((p - lo) / (hi - lo)) * width;
  const y = (c: number) => height - (c / yMax) * height;
  const side = (pts: DepthPoint[], dir: -1 | 1): { area: string; line: string } => {
    if (!pts.length) return { area: '', line: '' };
    const edge = dir < 0 ? 0 : width;
    const line: string[] = [`M${r2(x(pts[0]!.price))} ${r2(height)}`];
    let prev = 0;
    for (const p of pts) {
      line.push(`L${r2(x(p.price))} ${r2(y(prev))}`, `L${r2(x(p.price))} ${r2(y(p.cum))}`);
      prev = p.cum;
    }
    line.push(`L${r2(edge)} ${r2(y(prev))}`);
    const l = line.join(' ');
    return { line: l, area: `${l} L${r2(edge)} ${r2(height)} Z` };
  };
  const b = side(s.bids, -1);
  const a = side(s.asks, 1);
  return {
    width,
    height,
    bidArea: b.area,
    bidLine: b.line,
    askArea: a.area,
    askLine: a.line,
    midX: x(mid),
    xTicks: niceTicks(lo, hi, 4).map((v) => ({ x: x(v), value: v })).filter((t) => t.x >= 0 && t.x <= width),
    yTicks: niceTicks(0, yMax, 3).filter((v) => v > 0).map((v) => ({ y: y(v), value: v })),
    xMin: lo,
    xMax: hi,
    yMax,
    hasData: true,
  };
}
