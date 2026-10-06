// Market orientation: every token book is a TOKEN/KAS market natively (price = KAS per token); the same book can be shown inverted, KAS/TOKEN
// (price = tokens per KAS). A token the config marks as a USD reference (`quoteTokens`) is shown inverted by default: stablecoins are the quote
// (KAS/TUSD, never TUSD/KAS; the ticker is never renamed). The choice is remembered per market in localStorage.
//
// THIS FILE IS A PURE DISPLAY TRANSFORM. Nothing here touches an order: the ticket keeps its form in native terms (KAS per token, token amounts) and
// only converts what the user TYPES (an inverted price) into the native text with a rounding that never makes the limit worse (see
// `invertedToNativePriceText`); the intent, the plan and the transaction are built from the native form exactly as before. Everything else here maps
// what the indexer reports onto the other orientation, exactly (rationals, no float except chart coordinates):
//
//   price        p KAS/token  ->  1/p tokens/KAS
//   candle       o h l c      ->  1/o 1/l 1/h 1/c     (high and low swap), volume in KAS instead of tokens
//   book         asks <-> bids   (a native ask, a seller of the token, is a buyer of KAS), cumulative size in KAS, total in tokens
//   trade        side flips (the taker of the token bought it = sold KAS), size in KAS
//   24 h stats   last 1/last, high 1/low, low 1/high, change 1/(1+c)-1, volumes swap
import type { DepthView, StatsView } from '../../data/indexer-types';
import { formatRational, parseDecimal, ratToNumber, reduce, significantDp, type Rational } from '../../kob/pair';
import { formatUnits, pow10, quoteOf, SOMPI_PER_KAS } from '../../kob/units';
import { columnFraction, fixedUnits, tickKasFraction } from '../kit/format';
import type { BookModel, BookRow, Level } from './book-model';
import {
  amountText, basisOf, basisToNumber, bigOrNull, bpsPercentText, compactText, priceText, toneOf, unitsToNumber,
  type Candle, type ChartBar, type DepthPoint, type DepthSeries, type StatsModel, type Tape, type VolumeBar,
} from './market-model';

// ------------------------------------------------------------------------------------------------ remembered choice

const KEY_PREFIX = 'kob.flip.v1:';

/** The remembered orientation of a market (`true` = inverted), or null when the user never chose (the convention applies). Never throws. */
export function readInverted(market: string): boolean | null {
  try {
    const v = localStorage.getItem(KEY_PREFIX + market);
    return v === '1' ? true : v === '0' ? false : null;
  } catch {
    return null;
  }
}

export function writeInverted(market: string, inverted: boolean): void {
  try {
    localStorage.setItem(KEY_PREFIX + market, inverted ? '1' : '0');
  } catch {
    /* no storage: the choice lasts until the page is closed */
  }
}

/** The storage key of a market: `token:<covenantId>` (the token's own KAS book; the pair page has no stored flip, its swap is a route). */
export const marketKey = (kind: 'token', id: string): string => `${kind}:${id.toLowerCase()}`;

/** The convention: a USD reference token is shown inverted (KAS/<its ticker>); every other token as TOKEN/KAS. */
export const defaultInverted = (usdRef: boolean): boolean => usdRef;

/** `TOKEN/KAS`, or `KAS/TOKEN` when inverted. */
export const pairLabel = (name: string, inverted: boolean): string => (inverted ? `KAS/${name}` : `${name}/KAS`);

// ------------------------------------------------------------------------------------------------ exact price arithmetic

const ZERO: Rational = { num: 0n, den: 1n };

/** KAS per whole token of a price in sompi per `basis` base units (exact). */
export function kasPerToken(price: bigint, decimals: number, basis: bigint): Rational {
  return reduce({ num: price * pow10(decimals), den: basis * SOMPI_PER_KAS });
}

/** 1/r; null for zero. */
export const invert = (r: Rational): Rational | null => (r.num > 0n ? reduce({ num: r.den, den: r.num }) : null);

/** Tokens per KAS of a price in sompi per `basis` base units; null when the price is not positive. */
export function tokensPerKas(price: bigint, decimals: number, basis: bigint): Rational | null {
  return price > 0n && basis > 0n ? invert(kasPerToken(price, decimals, basis)) : null;
}

const add = (a: Rational, b: Rational): Rational => reduce({ num: a.num * b.den + b.num * a.den, den: a.den * b.den });
const sub = (a: Rational, b: Rational): Rational => reduce({ num: a.num * b.den - b.num * a.den, den: a.den * b.den });
const half = (a: Rational): Rational => reduce({ num: a.num, den: a.den * 2n });

/** Fraction digits of an inverted price (about 5 significant digits, 2 to 10), chosen once per market. */
export const flipDp = (ref: Rational | null): number => (ref ? significantDp(ref, 5, 2, 10) : 4);

/** Exact text of an inverted price with `dp` digits, thousands grouped; negative values (a spread of a crossed book) keep their sign. */
export function ratioText(r: Rational, dp: number, group = ','): string {
  if (r.num < 0n) return '-' + formatRational({ num: -r.num, den: r.den }, dp, 'nearest', dp, group);
  return formatRational(r, dp, 'nearest', dp, group);
}

/** Tokens per KAS text of a price in sompi per `basis` base units ('—' when there is none). */
export function flippedPriceText(price: bigint, decimals: number, basis: bigint, dp: number): string {
  const r = tokensPerKas(price, decimals, basis);
  return r ? ratioText(r, dp) : '—';
}

// ------------------------------------------------------------------------------------------------ the ticket's price input

/**
 * What the user types as an inverted price (tokens per KAS) -> the native text the form keeps (KAS per token, 8 decimals). `rounding` is the
 * direction that never makes the limit worse for the NATIVE side of the order (a sell rounds up, a buy down: kob/units.ts `safeRounding`); the
 * form then ticks it the same way. Text that is not a positive decimal passes through unchanged so the form reports its own format error.
 */
export function invertedToNativePriceText(text: string, rounding: 'up' | 'down'): string {
  const r = parseDecimal(text);
  if (!r) return text;
  if (r.num === 0n) return '0';
  return formatRational(reduce({ num: r.den, den: r.num }), 8, rounding, 0);
}

/** The native price text of the form (KAS per token) -> the inverted text shown in the input (about 8 significant digits, no grouping). */
export function nativeToInvertedPriceText(text: string): string {
  const r = parseDecimal(text);
  if (!r || r.num === 0n) return text;
  const inv = reduce({ num: r.den, den: r.num });
  return formatRational(inv, significantDp(inv, 8, 2, 12), 'nearest', 0);
}

/** The opposite side: buying the quote asset of the inverted pair is selling the token. */
export const oppositeSide = (s: 'buy' | 'sell'): 'buy' | 'sell' => (s === 'buy' ? 'sell' : 'buy');

// ------------------------------------------------------------------------------------------------ order book

/** Grouping choices of the inverted book: 1x, 10x, 100x, 1000x the displayed price step 10^-dp (tokens per KAS). */
export const INVERTED_GROUP_MULTIPLIERS = [1n, 10n, 100n, 1000n] as const;

const ceilDiv = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;

/**
 * Grouping on the INVERTED prices (tokens per KAS), as a `regroup` of `buildBookModel`: each native level goes to the bucket of its inverted price,
 * a multiple `mult` x 10^-dp. The displayed bids (native asks) round DOWN and the displayed asks (native bids) round UP, like the native grouping, so a
 * bucket shows the worst price of its orders. A bucket keeps its exact inverted price (`inv`) for the display and a NATIVE price (sompi per whole
 * token of the market's `scale`) for a click: the native price that still reaches every order of the bucket (a buy rounds up, a sell down). `mult`
 * <= 1 returns undefined (no grouping).
 */
export function invertedRegrouper(c: { decimals: number; scale: bigint; dp: number; mult: bigint }): ((levels: Level[], side: 'ask' | 'bid') => Level[]) | undefined {
  if (c.mult <= 1n || c.scale <= 0n) return undefined;
  const scale = pow10(c.dp);
  const basisKas = c.scale * SOMPI_PER_KAS; // native price P (sompi per `scale` base units) <-> q tokens per KAS: q = basisKas / (P x 10^decimals)
  const dec = pow10(c.decimals);
  return (levels, side) => {
    const by = new Map<bigint, Level>();
    for (const l of levels) {
      // q = basisKas / (P dec); q / step = q scale / mult = basisKas scale / (P dec mult)
      const num = basisKas * scale;
      const den = l.price * dec * c.mult;
      const k = side === 'ask' ? num / den : ceilDiv(num, den); // native ask -> displayed bid: down; native bid -> displayed ask: up
      if (k <= 0n) {
        // a price below one step: left as it is (never rounded to zero)
        const key = -l.price;
        const cur0 = by.get(key);
        if (cur0) { cur0.amount += l.amount; cur0.orders += l.orders; cur0.estimated ||= l.estimated; } else by.set(key, { ...l });
        continue;
      }
      const cur = by.get(k);
      if (cur) {
        cur.amount += l.amount;
        cur.orders += l.orders;
        cur.estimated ||= l.estimated;
        continue;
      }
      // the bucket's native price: P = basisKas scale / (dec k mult), rounded so that the order it prefills reaches the whole bucket
      const pn = basisKas * scale;
      const pd = dec * k * c.mult;
      const price = side === 'ask' ? ceilDiv(pn, pd) : pn / pd > 0n ? pn / pd : 1n;
      by.set(k, { price, amount: l.amount, orders: l.orders, estimated: l.estimated, inv: reduce({ num: k * c.mult, den: scale }) });
    }
    return [...by.values()];
  };
}

/** One level of the book as drawn (strings and the figures the tests read). */
export interface BookRowView {
  key: string;
  priceText: string;
  /** the price as plain decimal text (no grouping), for tests and tooltips */
  priceValue: string;
  sizeText: string;
  totalText: string;
  totalTitle: string;
  estimated: boolean;
  depthPct: number;
  orders: number;
  /** base units at this level (native) */
  amount: bigint;
  /** the level's NATIVE price (sompi per whole token): the identity of the level and what a click prefills */
  nativePrice: bigint;
  /** the side a click prefills in the ticket, in NATIVE terms: an ask level means "buy the token at this price", a bid level "sell" */
  pick: 'buy' | 'sell';
}

export interface BookView {
  /** display order: highest price at the top, the best ask directly above the spread */
  asks: BookRowView[];
  /** best first */
  bids: BookRowView[];
  head: { price: string; size: string; total: string };
  mid: { text: string; value: string } | null;
  spread: { text: string; pct: string | null } | null;
  anyEstimated: boolean;
}

export interface BookViewCtx {
  /** display name of the token */
  name: string;
  decimals: number;
  /** the market's scale: base units per whole token (the prices' denominator) */
  scale: bigint;
  /** price decimals of the shown orientation */
  dp: number;
  /** the market's tick (sompi per whole token; the registry's, else the step the visible prices sit on): fixes the decimals of the KAS columns */
  tick?: bigint | null;
  /** i18n of the header cells */
  labels: { price: string; size: string; total: string };
}

const plain = (r: Rational, dp: number): string => ratioText(r, dp, '');

/** KAS columns (size of an inverted book, totals) show at most this many fraction digits in the cumulative total. */
const TOTAL_KAS_MAX_FRACTION = 4;

/**
 * The fixed decimals of every numeric column of a book view, derived ONCE per view (never per row): the price column's `dp`, token amounts from
 * the finest amount shown (`amounts`; amounts are any base units, so the column takes what its rows need), KAS amounts from the tick; the
 * cumulative KAS total is capped at 4 digits.
 */
export function bookColumnFractions(c: Pick<BookViewCtx, 'decimals' | 'tick'>, amounts: Iterable<bigint> = []): { tokens: number; kas: number; totalKas: number } {
  const kas = tickKasFraction(c.tick);
  return { tokens: columnFraction(amounts, c.decimals), kas, totalKas: Math.min(kas, TOTAL_KAS_MAX_FRACTION) };
}

/** The book as the exchange convention draws it: TOKEN/KAS, price in KAS per whole token, sizes in tokens. */
export function nativeBookView(m: BookModel, c: BookViewCtx): BookView {
  const price = (v: bigint) => priceText(v, c.decimals, c.scale, c.dp);
  const fr = bookColumnFractions(c, [...m.asks, ...m.bids].map((r) => r.amount));
  const row = (r: BookRow, side: 'ask' | 'bid'): BookRowView => ({
    key: r.price.toString(),
    priceText: price(r.price),
    priceValue: r.price.toString(),
    sizeText: fixedUnits(r.amount, c.decimals, fr.tokens),
    totalText: fixedUnits(r.cumValue, 8, fr.totalKas),
    totalTitle: `${formatUnits(r.cumValue, 8, { group: ',' })} KAS`,
    estimated: r.estimated,
    depthPct: r.depthPct,
    orders: r.orders,
    amount: r.amount,
    nativePrice: r.price,
    pick: side === 'ask' ? 'buy' : 'sell',
  });
  return {
    asks: m.asksDisplay.map((r) => row(r, 'ask')),
    bids: m.bids.map((r) => row(r, 'bid')),
    head: c.labels,
    mid: m.mid !== null ? { text: price(m.mid), value: m.mid.toString() } : null,
    spread: m.spread !== null ? { text: price(m.spread), pct: m.spreadBps !== null ? (m.spreadBps / 100).toFixed(2) : null } : null,
    anyEstimated: m.anyEstimated,
  };
}

/**
 * The same book as KAS/TOKEN. A native ask (a seller of the token) is a bid of KAS and the other way round; the best native ask is the best
 * inverted bid. Size is KAS (the level's value, `amount x price / scale` rounded down), total the cumulative tokens; the depth bars are relative
 * to the deeper side in KAS.
 */
export function flippedBookView(m: BookModel, c: BookViewCtx): BookView {
  const scale = c.scale;
  const fr = bookColumnFractions(c, [...m.asks, ...m.bids].map((r) => r.cumAmount));
  const inv = (p: bigint): Rational => invert(kasPerToken(p, c.decimals, scale)) ?? ZERO;
  const invOf = (r: BookRow): Rational => r.inv ?? inv(r.price);
  const totalValue = (rows: BookRow[]): bigint => rows.at(-1)?.cumValue ?? 0n;
  const maxValue = totalValue(m.asks) > totalValue(m.bids) ? totalValue(m.asks) : totalValue(m.bids);
  const row = (r: BookRow, side: 'ask' | 'bid'): BookRowView => {
    const p = invOf(r);
    const tokens = r.cumAmount;
    return {
      key: r.price.toString(),
      priceText: ratioText(p, c.dp),
      priceValue: plain(p, c.dp),
      sizeText: fixedUnits(quoteOf(r.amount, r.price, scale, 'down'), 8, fr.kas),
      totalText: fixedUnits(tokens, c.decimals, fr.tokens),
      totalTitle: `${amountText(tokens, c.decimals, c.decimals)} ${c.name}`,
      estimated: r.estimated,
      depthPct: maxValue > 0n ? Number((r.cumValue * 10_000n) / maxValue) / 100 : 0,
      orders: r.orders,
      amount: r.amount,
      nativePrice: r.price,
      // the level a click reaches is native: a native ask level = buy the token (displayed as a BID of KAS), a native bid level = sell the token
      pick: side === 'ask' ? 'buy' : 'sell',
    };
  };
  // displayed bids = native asks (best = lowest native price = highest inverted), displayed asks = native bids; asks are listed best-last
  const bids = m.asks.map((r) => row(r, 'ask'));
  const asks = m.bids.map((r) => row(r, 'bid')).reverse();
  let mid: BookView['mid'] = null;
  let spread: BookView['spread'] = null;
  if (m.bestAsk !== null && m.bestBid !== null) {
    const bestBid = invOf(m.asks[0]!);
    const bestAsk = invOf(m.bids[0]!);
    const midR = half(add(bestBid, bestAsk));
    const spreadR = sub(bestAsk, bestBid);
    mid = { text: ratioText(midR, c.dp), value: plain(midR, 12) };
    const pct = midR.num > 0n ? Number((spreadR.num * 10_000n * midR.den) / (spreadR.den * midR.num)) : null;
    spread = { text: ratioText(spreadR, c.dp), pct: pct !== null ? (pct / 100).toFixed(2) : null };
  }
  return { asks, bids, head: c.labels, mid, spread, anyEstimated: m.anyEstimated };
}

// ------------------------------------------------------------------------------------------------ trade tape columns

export interface TapeColumnsCtx {
  decimals: number;
  /** price decimals of the shown orientation */
  dp: number;
  inverted: boolean;
  /** the market's tick, sompi per whole token (fixes the decimals of the KAS size column of the inverted view) */
  tick?: bigint | null;
}

/**
 * The price and size texts of every tape row with ONE number of decimals per column (chosen once for the whole tape: price `dp`, size from the
 * finest row / the tick), trailing zeros kept, so the decimal points line up. '—' for a value the row does not have.
 */
export function tapeColumns(tape: Tape, c: TapeColumnsCtx): { price: string; size: string }[] {
  const basis = tape.basis;
  const sizeFr = c.inverted
    ? c.tick
      ? tickKasFraction(c.tick) // the market's tick fixes the KAS column (like the book's size column); a finer row is rounded, its exact amount is in the row title
      : columnFraction(tape.rows.map((r) => r.quote), 8)
    : columnFraction(tape.rows.map((r) => r.amount), c.decimals);
  return tape.rows.map((r) => ({
    price: r.price === null ? '—' : c.inverted ? (basis ? flippedPriceText(r.price, c.decimals, basis, c.dp) : '—') : priceText(r.price, c.decimals, basis ?? 0n, c.dp),
    size: c.inverted ? (r.quote !== null ? fixedUnits(r.quote, 8, sizeFr) : '—') : r.amount !== null ? fixedUnits(r.amount, c.decimals, sizeFr) : '—',
  }));
}

// ------------------------------------------------------------------------------------------------ 24 h stats

/** `/v1/stats` shown inverted. The shape of `StatsModel` is kept: `volume` is the BASE asset's (KAS) and `quoteVolume` the QUOTE's (the token). */
export function statsModelFlipped(v: StatsView | null, decimals: number, dp: number): StatsModel {
  const basis = v ? basisOf(v.price_basis) : null;
  const p = (x: unknown): string | null => {
    const b = bigOrNull(x);
    const r = b !== null && basis ? tokensPerKas(b, decimals, basis) : null;
    return r ? ratioText(r, dp) : null;
  };
  const vol = v ? bigOrNull(v.volume_24h) : null;
  const qv = v ? bigOrNull(v.quote_volume_24h) : null;
  const nativeBps = v && typeof v.change_24h_bps === 'number' && Number.isFinite(v.change_24h_bps) ? Math.trunc(v.change_24h_bps) : null;
  // price p1 = p0 (1 + c)  ->  1/p1 = (1/p0) / (1 + c): the inverted change is 1/(1 + c) - 1
  const bps = nativeBps !== null && nativeBps > -10_000 ? Math.round(100_000_000 / (10_000 + nativeBps) - 10_000) : null;
  return {
    last: p(v?.last),
    lastSide: v?.last_side === 'buy' ? 'sell' : v?.last_side === 'sell' ? 'buy' : null,
    change: bps === null ? null : bpsPercentText(bps),
    changeTone: toneOf(bps),
    high: p(v?.low_24h),
    low: p(v?.high_24h),
    volume: qv === null ? null : compactText(Number(qv) / 1e8),
    volumeExact: qv === null ? null : formatUnits(qv, 8, { group: ',' }),
    quoteVolume: vol === null ? null : compactText(unitsToNumber(vol, decimals)),
    quoteVolumeExact: vol === null ? null : formatUnits(vol, decimals, { group: ',' }),
    trades: v && typeof v.trades_24h === 'number' ? v.trades_24h : null,
    spread: v && typeof v.spread_bps === 'number' ? bpsPercentText(v.spread_bps).replace('+', '') : null,
    dp,
  };
}

// ------------------------------------------------------------------------------------------------ candles

export interface FlippedSeries {
  bars: ChartBar[];
  volumes: VolumeBar[];
  /** exact OHLC texts of bar i */
  text: (i: number) => { o: string; h: string; l: string; c: string } | null;
}

/**
 * Indexer candles (gap-filled, native) as KAS/TOKEN candles: open and close invert, high and low swap (the native low is the inverted high),
 * the volume is the KAS turnover of the bucket, and a bar is "up" when the inverted close is at or above the inverted open.
 */
export function flippedChartSeries(candles: readonly Candle[], decimals: number, basis: bigint, dp: number): FlippedSeries {
  const bars: ChartBar[] = [];
  const volumes: VolumeBar[] = [];
  const texts: { o: string; h: string; l: string; c: string }[] = [];
  const num = (price: bigint): { n: number; t: string } => {
    const r = tokensPerKas(price, decimals, basis);
    return r ? { n: ratToNumber(r), t: ratioText(r, dp) } : { n: 0, t: '—' };
  };
  for (const c of candles) {
    const time = Math.floor(c.t / 1000);
    const o = num(c.o), h = num(c.l), l = num(c.h), cl = num(c.c);
    bars.push({ time, open: o.n, high: h.n, low: l.n, close: cl.n, filled: c.filled });
    volumes.push({ time, value: Number(c.quote) / 1e8, up: cl.n >= o.n });
    texts.push({ o: o.t, h: h.t, l: l.t, c: cl.t });
  }
  return { bars, volumes, text: (i) => texts[i] ?? null };
}

// ------------------------------------------------------------------------------------------------ depth

const pointsKas = (levels: { price: bigint; amount: bigint }[], decimals: number, basis: bigint): { pts: DepthPoint[]; totalSompi: bigint } => {
  let cumKas = 0;
  let total = 0n;
  const pts: DepthPoint[] = [];
  for (const l of levels) {
    const kasPerTok = basisToNumber(l.price, decimals, basis);
    if (!(kasPerTok > 0)) continue;
    cumKas += kasPerTok * unitsToNumber(l.amount, decimals);
    total += (l.price * l.amount) / basis;
    pts.push({ price: 1 / kasPerTok, cum: cumKas });
  }
  return { pts, totalSompi: total };
};

/** `/v1/depth` as KAS/TOKEN: native asks become the bids (price 1/p), the cumulative amount is KAS. */
export function depthFromViewFlipped(v: DepthView, decimals: number): DepthSeries | null {
  const basis = basisOf(v.price_basis);
  if (!basis) return null;
  const parse = (ls: DepthView['bids']) =>
    ls.flatMap((l) => {
      const price = bigOrNull(l.price);
      const amount = bigOrNull(l.amount);
      return price !== null && amount !== null && price > 0n && amount > 0n ? [{ price, amount, estimated: !!l.estimated }] : [];
    });
  const nativeBids = parse(v.bids).sort((a, b) => (a.price > b.price ? -1 : a.price < b.price ? 1 : 0));
  const nativeAsks = parse(v.asks).sort((a, b) => (a.price < b.price ? -1 : a.price > b.price ? 1 : 0));
  const bids = pointsKas(nativeAsks, decimals, basis);
  const asks = pointsKas(nativeBids, decimals, basis);
  const bestBid = bids.pts[0]?.price ?? null;
  const bestAsk = asks.pts[0]?.price ?? null;
  return {
    bids: bids.pts,
    asks: asks.pts,
    mid: bestBid !== null && bestAsk !== null ? (bestBid + bestAsk) / 2 : null,
    estimated: nativeBids.some((l) => l.estimated) || nativeAsks.some((l) => l.estimated),
    bidTotal: bids.totalSompi,
    askTotal: asks.totalSompi,
  };
}

/** Fallback without `/v1/depth`: the aggregated book (state prices per `scale` base units) as a KAS/TOKEN depth series; the cumulative value is already in KAS. */
export function depthFromBookFlipped(m: BookModel, decimals: number, scale: bigint): DepthSeries {
  const pts = (rows: BookRow[]): DepthPoint[] => rows.map((r) => ({ price: 1 / basisToNumber(r.price, decimals, scale), cum: Number(r.cumValue) / 1e8 }));
  const bids = pts(m.asks);
  const asks = pts(m.bids);
  return {
    bids,
    asks,
    mid: bids.length && asks.length ? (bids[0]!.price + asks[0]!.price) / 2 : null,
    estimated: m.anyEstimated,
    bidTotal: m.asks.at(-1)?.cumValue ?? 0n,
    askTotal: m.bids.at(-1)?.cumValue ?? 0n,
  };
}
