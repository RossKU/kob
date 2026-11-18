// Market orientation. Every token book is natively a TOKEN/KAS market, with price in KAS per token.
// The same book can be shown inverted as KAS/TOKEN, with price in tokens per KAS. Every market
// opens as TOKEN/KAS, a USD reference token (`quoteTokens`) included. For TUSD/KAS the token is on
// the left and amounts are in the token. The flip shows KAS/TOKEN with amounts in KAS. The ticker
// is never renamed. The choice is remembered per market in localStorage.
//
// This file is a pure display transform. Nothing here touches an order. The ticket keeps its form
// in native terms, KAS per token and token amounts. It only converts what the user types, an
// inverted price, into the native text. The rounding never makes the limit worse (see
// `invertedToNativePriceText`). The intent, the plan and the transaction are built from the native
// form. Everything else here maps what the indexer reports onto the other orientation. The maps
// are exact, with rationals and no float except for chart coordinates.
// - Price: `p` KAS/token becomes `1/p` tokens/KAS.
// - Candle: `o h l c` becomes `1/o 1/l 1/h 1/c`, so high and low swap. Volume is in KAS instead of
//   tokens.
// - Book: asks and bids swap, because a native ask, a seller of the token, is a buyer of KAS.
//   The cumulative size is in KAS and the total in tokens.
// - Trade: the side flips, because the taker of the token bought it and so sold KAS. The size is
//   in KAS.
// - 24 h stats: last becomes `1/last`, high becomes `1/low`, low becomes `1/high`, change becomes
//   `1/(1+c)-1`, and the volumes swap.
import type { DepthView, StatsView } from '../../data/indexer-types';
import type { Rate } from '../../app/fiat';
import { formatRational, parseDecimal, ratToNumber, reduce, significantDp, type Rational } from '../../kob/pair';
import { formatUnits, pow10, quoteOf, SOMPI_PER_KAS } from '../../kob/units';
import { columnFraction, fitAmountColumn, fitDecimalTexts, PRICE_DP_CAP, PRICE_SIGNIFICANT_DIGITS, tickKasFraction, type AmountCell } from '../kit/format';
import type { BookModel, BookRow, Level } from './book-model';
import { mergeCurve } from './book-model';
import {
  amountText, basisOf, basisToNumber, bigOrNull, bpsPercentText, changeMarkFields, changeMarks, compactText, exactIfRounded, exactPriceText, priceText, thinBookUnder, toneOf, unitsToNumber,
  type Candle, type ChartBar, type DepthPoint, type DepthSeries, type StatsModel, type VolumeBar,
} from './market-model';

// ------------------------------------------------------------------------------- remembered choice

// The prefix carries a version. The keys of the earlier prefix are removed once, so a stored
// choice always reads in the current labelling.
const KEY_PREFIX = 'kob.flip.v2:';
const LEGACY_PREFIX = 'kob.flip.v1:';

function purgeLegacy(): void {
  try {
    const stale: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k !== null && k.startsWith(LEGACY_PREFIX)) stale.push(k);
    }
    for (const k of stale) localStorage.removeItem(k);
  } catch {
    /* No storage. */
  }
}
purgeLegacy();

/**
 * The remembered orientation of a market (`true` means inverted). It is null when the user never
 * chose, and then the convention applies. It never throws.
 */
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
    /* No storage. The choice lasts until the page is closed. */
  }
}

/**
 * The storage key of a market, `token:<covenantId>`, for the token's own KAS book. The pair page
 * has no stored flip, because its swap is a route.
 */
export const marketKey = (kind: 'token', id: string): string => `${kind}:${id.toLowerCase()}`;

/**
 * The convention. Every token's KAS market opens as TOKEN/KAS, a USD reference token too, such as
 * TUSD/KAS with the token on the left.
 */
export const defaultInverted = (_usdRef: boolean): boolean => false;

/** `TOKEN/KAS`, or `KAS/TOKEN` when inverted. */
export const pairLabel = (name: string, inverted: boolean): string => (inverted ? `KAS/${name}` : `${name}/KAS`);

// -------------------------------------------------------------------------- exact price arithmetic

const ZERO: Rational = { num: 0n, den: 1n };

/** KAS per whole token of a price in sompi per `basis` base units, exact. */
export function kasPerToken(price: bigint, decimals: number, basis: bigint): Rational {
  return reduce({ num: price * pow10(decimals), den: basis * SOMPI_PER_KAS });
}

/** `1/r`, or null for zero. */
export const invert = (r: Rational): Rational | null => (r.num > 0n ? reduce({ num: r.den, den: r.num }) : null);

/**
 * Tokens per KAS of a price in sompi per `basis` base units. Null when the price is not positive.
 */
export function tokensPerKas(price: bigint, decimals: number, basis: bigint): Rational | null {
  return price > 0n && basis > 0n ? invert(kasPerToken(price, decimals, basis)) : null;
}

const add = (a: Rational, b: Rational): Rational => reduce({ num: a.num * b.den + b.num * a.den, den: a.den * b.den });
const sub = (a: Rational, b: Rational): Rational => reduce({ num: a.num * b.den - b.num * a.den, den: a.den * b.den });
const half = (a: Rational): Rational => reduce({ num: a.num, den: a.den * 2n });

/**
 * Fraction digits of an inverted price, chosen once per market. It follows the kit's display rule
 * of about 5 significant digits, at most `PRICE_DP_CAP` and at least 2.
 */
export const flipDp = (ref: Rational | null): number => (ref ? Math.min(PRICE_DP_CAP, significantDp(ref, PRICE_SIGNIFICANT_DIGITS, 2, 10)) : 4);

/**
 * Exact text of an inverted price with `dp` digits, thousands grouped. Negative values, such as the
 * spread of a crossed book, keep their sign.
 */
export function ratioText(r: Rational, dp: number, group = ','): string {
  if (r.num < 0n) return '-' + formatRational({ num: -r.num, den: r.den }, dp, 'nearest', dp, group);
  return formatRational(r, dp, 'nearest', dp, group);
}

/** Tokens per KAS text of a price in sompi per `basis` base units, or '—' when there is none. */
export function flippedPriceText(price: bigint, decimals: number, basis: bigint, dp: number): string {
  const r = tokensPerKas(price, decimals, basis);
  return r ? ratioText(r, dp) : '—';
}

// ------------------------------------------------------------------------ the ticket's price input

/**
 * Turns what the user types as an inverted price, in tokens per KAS, into the native text the form
 * keeps, in KAS per token with 8 decimals. `rounding` is the direction that never makes the limit
 * worse for the native side of the order. A sell rounds up and a buy down (kob/units.ts
 * `safeRounding`), and the form then ticks it the same way. Text that is not a positive decimal
 * passes through unchanged, so the form reports its own format error.
 */
export function invertedToNativePriceText(text: string, rounding: 'up' | 'down'): string {
  const r = parseDecimal(text);
  if (!r) return text;
  if (r.num === 0n) return '0';
  return formatRational(reduce({ num: r.den, den: r.num }), 8, rounding, 0);
}

/**
 * Turns the native price text of the form (KAS per token) into the inverted text shown in the
 * input, with about 8 significant digits and no grouping.
 */
export function nativeToInvertedPriceText(text: string): string {
  const r = parseDecimal(text);
  if (!r || r.num === 0n) return text;
  const inv = reduce({ num: r.den, den: r.num });
  return formatRational(inv, significantDp(inv, 8, 2, 12), 'nearest', 0);
}

/** The opposite side. Buying the quote asset of the inverted pair is selling the token. */
export const oppositeSide = (s: 'buy' | 'sell'): 'buy' | 'sell' => (s === 'buy' ? 'sell' : 'buy');

// -------------------------------------------------------------------------------------- order book

/**
 * Grouping choices of the inverted book: 1x, 10x, 100x and 1000x the displayed price step 10^-dp
 * (tokens per KAS).
 */
export const INVERTED_GROUP_MULTIPLIERS = [1n, 10n, 100n, 1000n] as const;

const ceilDiv = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;

/**
 * Grouping on the inverted prices (tokens per KAS), as a `regroup` of `buildBookModel`. Each
 * native level goes to the bucket of its inverted price, a multiple `mult` x 10^-dp. The displayed
 * bids, which are native asks, round down. The displayed asks, which are native bids, round up.
 * This is like the native grouping, so a bucket shows the worst price of its orders.
 *
 * A bucket keeps its exact inverted price (`inv`) for the display. It also keeps a native price,
 * in sompi per whole token of the market's `scale`, for a click. That is the native price that
 * still reaches every order of the bucket, so a buy rounds up and a sell down. A `mult` of 1 or
 * less returns undefined, meaning no grouping.
 */
export function invertedRegrouper(c: { decimals: number; scale: bigint; dp: number; mult: bigint }): ((levels: Level[], side: 'ask' | 'bid') => Level[]) | undefined {
  if (c.mult <= 1n || c.scale <= 0n) return undefined;
  const scale = pow10(c.dp);
  const basisKas = c.scale * SOMPI_PER_KAS; // native price P (sompi per `scale` base units) <-> q tokens per KAS: q = basisKas / (P x 10^decimals)
  const dec = pow10(c.decimals);
  return (levels, side) => {
    const by = new Map<bigint, Level>();
    for (const l of levels) {
      // q = basisKas / (P dec), and q / step = q scale / mult = basisKas scale / (P dec mult).
      const num = basisKas * scale;
      const den = l.price * dec * c.mult;
      const k = side === 'ask' ? num / den : ceilDiv(num, den); // native ask -> displayed bid: down; native bid -> displayed ask: up
      if (k <= 0n) {
        // A price below one step is left as it is. It is never rounded to zero.
        const key = -l.price;
        const cur0 = by.get(key);
        if (cur0) { cur0.amount += l.amount; cur0.orders += l.orders; cur0.estimated ||= l.estimated; if (cur0.value !== undefined && l.value !== undefined) cur0.value += l.value; } else by.set(key, { ...l });
        continue;
      }
      const cur = by.get(k);
      if (cur) {
        cur.amount += l.amount;
        cur.orders += l.orders;
        cur.estimated ||= l.estimated;
        if (cur.value !== undefined && l.value !== undefined) cur.value += l.value;
        const cv = mergeCurve(cur.curve, l.curve);
        if (cv) cur.curve = cv;
        continue;
      }
      // The bucket's native price is P = basisKas scale / (dec k mult). It is rounded so that the
      // order it prefills reaches the whole bucket.
      const pn = basisKas * scale;
      const pd = dec * k * c.mult;
      const price = side === 'ask' ? ceilDiv(pn, pd) : pn / pd > 0n ? pn / pd : 1n;
      by.set(k, { price, amount: l.amount, orders: l.orders, estimated: l.estimated, ...(l.value !== undefined ? { value: l.value } : {}), ...(l.curve ? { curve: l.curve } : {}), inv: reduce({ num: k * c.mult, den: scale }) });
    }
    return [...by.values()];
  };
}

/** One level of the book as drawn, with strings and the figures the tests read. */
export interface BookRowView {
  key: string;
  /** The price at the column's fixed decimals. It is exact, and labels and tests read it. */
  priceText: string;
  /**
   * The price as the cell shows it. It is `priceText`, or fewer decimals or K, M, B when the
   * column is too wide for its cells. A digit is never cut.
   */
  priceShown: string;
  /** `priceText` when `priceShown` is not exact. */
  priceTitle: string | null;
  /** The price as plain decimal text with no grouping, for tests and tooltips. */
  priceValue: string;
  /**
   * The size and the cumulative total. A digit is never cut. A column that is too wide shows
   * fewer decimals or K, M, B, and the exact amount is in the title.
   */
  sizeText: string;
  /** The exact size when `sizeText` is rounded or compact. */
  sizeTitle: string | null;
  totalText: string;
  totalTitle: string;
  estimated: boolean;
  depthPct: number;
  orders: number;
  /** Base units at this level, native. */
  amount: bigint;
  /**
   * The level's native price in sompi per whole token. It is the identity of the level and what a
   * click prefills.
   */
  nativePrice: bigint;
  /**
   * The side a click would take, in native terms. An ask level means "buy the token at this
   * price" and a bid level "sell". It picks the rounding of the price. The ticket keeps its own
   * side.
   */
  pick: 'buy' | 'sell';
  /**
   * The curve levels (table indexes) at this level, absent when none. The row is marked as part of
   * a curve order.
   */
  curve?: number[];
  /**
   * The hover read-out from the best price to this level. It has the average price, one digit
   * finer than the column, the amount of the token and its value in KAS. In a flipped book the
   * order is KAS first, then the token.
   */
  cum: { avg: string; amount: string; total: string };
}

export interface BookView {
  /**
   * Display order. The highest price is at the top, and the best ask is directly above the spread.
   */
  asks: BookRowView[];
  /** Best first. */
  bids: BookRowView[];
  head: { price: string; size: string; total: string };
  /** `title` is the whole price when `text` is rounded to the display decimals. */
  mid: { text: string; value: string; title?: string | null } | null;
  spread: { text: string; pct: string | null; title?: string | null } | null;
  anyEstimated: boolean;
}

export interface BookViewCtx {
  /** Display name of the token. */
  name: string;
  decimals: number;
  /** The market's scale in base units per whole token, the denominator of the prices. */
  scale: bigint;
  /** Price decimals of the shown orientation. */
  dp: number;
  /**
   * The market's tick in sompi per whole token. It is the registry's, else the step the visible
   * prices sit on. It fixes the decimals of the KAS columns.
   */
  tick?: bigint | null;
  /** i18n of the header cells. */
  labels: { price: string; size: string; total: string };
}

const plain = (r: Rational, dp: number): string => ratioText(r, dp, '');

/**
 * KAS columns, meaning the size of an inverted book and the totals, show at most this many fraction
 * digits in the cumulative total.
 */
const TOTAL_KAS_MAX_FRACTION = 4;

/**
 * The fixed decimals of every numeric column of a book view. They are derived once per view and
 * never per row. The price column uses `dp`. Token amounts use the finest amount shown
 * (`amounts`), because amounts are any base units and the column takes what its rows need. KAS
 * amounts use the tick. The cumulative KAS total is capped at 4 digits.
 */
export function bookColumnFractions(c: Pick<BookViewCtx, 'decimals' | 'tick'>, amounts: Iterable<bigint> = []): { tokens: number; kas: number; totalKas: number } {
  const kas = tickKasFraction(c.tick);
  return { tokens: columnFraction(amounts, c.decimals), kas, totalKas: Math.min(kas, TOTAL_KAS_MAX_FRACTION) };
}

/**
 * A row before its price is fitted. `priceExact` is the whole price when `priceText` is rounded to
 * the display decimals, used as the cell's title.
 */
type UnfittedRow = Omit<BookRowView, 'priceShown' | 'priceTitle'> & { priceExact: string | null };

/**
 * The price column of a book view as its cells show it (see `BookRowView.priceShown`). The title is
 * the whole price wherever the cell shows less of it.
 */
function fitPrices(rows: UnfittedRow[]): BookRowView[] {
  const cells = fitDecimalTexts(rows.map((r) => r.priceText));
  return rows.map(({ priceExact, ...r }, i) => ({ ...r, priceShown: cells[i]!.text, priceTitle: cells[i]!.exact ?? priceExact }));
}

/**
 * `ratioText` with every digit (12) and no trailing zeros, the whole inverted price for a title.
 */
const exactRatio = (r: Rational): string => ratioText(r, 12).replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '');

/** The cells of a fitted column by row. */
const columnOf = <R,>(rows: readonly R[], cells: readonly AmountCell[]): Map<R, AmountCell> => new Map(rows.map((r, i) => [r, cells[i]!]));

/**
 * The book as the exchange convention draws it: TOKEN/KAS, price in KAS per whole token, sizes in
 * tokens.
 */
export function nativeBookView(m: BookModel, c: BookViewCtx): BookView {
  const price = (v: bigint) => priceText(v, c.decimals, c.scale, c.dp);
  const fr = bookColumnFractions(c, [...m.asks, ...m.bids].map((r) => r.amount));
  const shown = [...m.asksDisplay, ...m.bids];
  const sizes = columnOf(shown, fitAmountColumn(shown.map((r) => r.amount), c.decimals, { fraction: fr.tokens, unit: c.name }));
  const totals = columnOf(shown, fitAmountColumn(shown.map((r) => r.cumValue), 8, { fraction: fr.totalKas, unit: 'KAS' }));
  const row = (r: BookRow, side: 'ask' | 'bid'): UnfittedRow => ({
    cum: {
      avg: r.cumAmount > 0n ? priceText((r.cumValue * c.scale) / r.cumAmount, c.decimals, c.scale, c.dp + 1) : '—',
      amount: amountText(r.cumAmount, c.decimals, 4),
      total: formatUnits(r.cumValue, 8, { maxFraction: 4, group: ',' }),
    },
    key: r.price.toString(),
    priceText: price(r.price),
    priceExact: exactIfRounded(price(r.price), exactPriceText(r.price, c.decimals, c.scale)),
    priceValue: r.price.toString(),
    sizeText: sizes.get(r)!.text,
    sizeTitle: sizes.get(r)!.exact,
    totalText: totals.get(r)!.text,
    totalTitle: `${formatUnits(r.cumValue, 8, { group: ',' })} KAS`,
    estimated: r.estimated,
    depthPct: r.depthPct,
    orders: r.orders,
    amount: r.amount,
    nativePrice: r.price,
    pick: side === 'ask' ? 'buy' : 'sell',
    ...(r.curve ? { curve: r.curve } : {}),
  });
  const priced = fitPrices([...m.asksDisplay.map((r) => row(r, 'ask')), ...m.bids.map((r) => row(r, 'bid'))]);
  return {
    asks: priced.slice(0, m.asksDisplay.length),
    bids: priced.slice(m.asksDisplay.length),
    head: c.labels,
    mid: m.mid !== null ? { text: price(m.mid), value: m.mid.toString(), title: exactIfRounded(price(m.mid), exactPriceText(m.mid, c.decimals, c.scale)) } : null,
    spread: m.spread !== null ? { text: price(m.spread), pct: m.spreadBps !== null ? (m.spreadBps / 100).toFixed(2) : null, title: exactIfRounded(price(m.spread), exactPriceText(m.spread, c.decimals, c.scale)) } : null,
    anyEstimated: m.anyEstimated,
  };
}

/**
 * The same book as KAS/TOKEN. A native ask, a seller of the token, is a bid of KAS and the other
 * way round. The best native ask is the best inverted bid. Size is KAS, the level's value
 * `amount x price / scale` rounded down. Total is the cumulative tokens. The depth bars are
 * relative to the deeper side in KAS.
 */
export function flippedBookView(m: BookModel, c: BookViewCtx): BookView {
  const scale = c.scale;
  const fr = bookColumnFractions(c, [...m.asks, ...m.bids].map((r) => r.cumAmount));
  const shown = [...m.asks, ...m.bids];
  const sizes = columnOf(shown, fitAmountColumn(shown.map((r) => quoteOf(r.amount, r.price, scale, 'down')), 8, { fraction: fr.kas, unit: 'KAS' }));
  const totals = columnOf(shown, fitAmountColumn(shown.map((r) => r.cumAmount), c.decimals, { fraction: fr.tokens, unit: c.name }));
  const inv = (p: bigint): Rational => invert(kasPerToken(p, c.decimals, scale)) ?? ZERO;
  const invOf = (r: BookRow): Rational => r.inv ?? inv(r.price);
  const totalValue = (rows: BookRow[]): bigint => rows.at(-1)?.cumValue ?? 0n;
  const maxValue = totalValue(m.asks) > totalValue(m.bids) ? totalValue(m.asks) : totalValue(m.bids);
  const row = (r: BookRow, side: 'ask' | 'bid'): UnfittedRow => {
    const p = invOf(r);
    const tokens = r.cumAmount;
    return {
      cum: {
        // Tokens per KAS over the whole stretch: (tokens / 10^decimals) / (KAS sompi / 1e8).
        avg: r.cumValue > 0n && tokens > 0n ? ratioText(reduce({ num: tokens * SOMPI_PER_KAS, den: r.cumValue * pow10(c.decimals) }), c.dp + 1) : '—',
        amount: formatUnits(r.cumValue, 8, { maxFraction: 4, group: ',' }),
        total: amountText(tokens, c.decimals, 4),
      },
      key: r.price.toString(),
      priceText: ratioText(p, c.dp),
      priceExact: exactIfRounded(ratioText(p, c.dp), exactRatio(p)),
      priceValue: plain(p, c.dp),
      sizeText: sizes.get(r)!.text,
      sizeTitle: sizes.get(r)!.exact,
      totalText: totals.get(r)!.text,
      totalTitle: `${amountText(tokens, c.decimals, c.decimals)} ${c.name}`,
      estimated: r.estimated,
      depthPct: maxValue > 0n ? Number((r.cumValue * 10_000n) / maxValue) / 100 : 0,
      orders: r.orders,
      amount: r.amount,
      nativePrice: r.price,
      // The level a click reaches is native. A native ask level means buy the token, displayed as a
      // bid of KAS. A native bid level means sell the token.
      pick: side === 'ask' ? 'buy' : 'sell',
      ...(r.curve ? { curve: r.curve } : {}),
    };
  };
  // Displayed bids are native asks. The best is the lowest native price, which is the highest
  // inverted. Displayed asks are native bids. Asks are listed best-last.
  const priced = fitPrices([...m.bids.map((r) => row(r, 'bid')).reverse(), ...m.asks.map((r) => row(r, 'ask'))]);
  const asks = priced.slice(0, m.bids.length);
  const bids = priced.slice(m.bids.length);
  let mid: BookView['mid'] = null;
  let spread: BookView['spread'] = null;
  if (m.bestAsk !== null && m.bestBid !== null) {
    // The touch of the ungrouped book, inverted. A bucket's inverted price is its worst price. The
    // best native ask is the best displayed bid.
    const bestBid = inv(m.bestAsk);
    const bestAsk = inv(m.bestBid);
    const midR = half(add(bestBid, bestAsk));
    const spreadR = sub(bestAsk, bestBid);
    mid = { text: ratioText(midR, c.dp), value: plain(midR, 12), title: exactIfRounded(ratioText(midR, c.dp), exactRatio(midR)) };
    const pct = midR.num > 0n ? Number((spreadR.num * 10_000n * midR.den) / (spreadR.den * midR.num)) : null;
    spread = { text: ratioText(spreadR, c.dp), pct: pct !== null ? (pct / 100).toFixed(2) : null, title: exactIfRounded(ratioText(spreadR, c.dp), exactRatio(spreadR)) };
  }
  return { asks, bids, head: c.labels, mid, spread, anyEstimated: m.anyEstimated };
}

// -------------------------------------------------------------------------------------- 24 h stats

/**
 * `/v1/stats` shown inverted. The shape of `StatsModel` is kept. `volume` is the BASE asset's (KAS)
 * and `quoteVolume` the QUOTE's (the token).
 */
export function statsModelFlipped(v: StatsView | null, decimals: number, dp: number, rate?: Rate | null, supplyUnits?: bigint | null): StatsModel {
  const basis = v ? basisOf(v.price_basis) : null;
  const p = (x: unknown): string | null => {
    const b = bigOrNull(x);
    const r = b !== null && basis ? tokensPerKas(b, decimals, basis) : null;
    return r ? ratioText(r, dp) : null;
  };
  // The whole inverted price for a tooltip, when the shown one is rounded.
  const exact = (x: unknown): string | null => {
    const b = bigOrNull(x);
    const r = b !== null && basis ? tokensPerKas(b, decimals, basis) : null;
    return r ? exactIfRounded(p(x), exactRatio(r)) : null;
  };
  const vol = v ? bigOrNull(v.volume_24h) : null;
  const qv = v ? bigOrNull(v.quote_volume_24h) : null;
  const nativeBps = v && typeof v.change_24h_bps === 'number' && Number.isFinite(v.change_24h_bps) ? Math.trunc(v.change_24h_bps) : null;
  // A price `p1 = p0 (1 + c)` inverts to `1/p1 = (1/p0) / (1 + c)`. The inverted change is
  // `1/(1 + c) - 1`.
  const bps = nativeBps !== null && nativeBps > -10_000 ? Math.round(100_000_000 / (10_000 + nativeBps) - 10_000) : null;
  const marks = changeMarks(bps, v?.trades_24h, thinBookUnder(v, rate, supplyUnits));
  return {
    last: p(v?.last),
    lastSide: v?.last_side === 'buy' ? 'sell' : v?.last_side === 'sell' ? 'buy' : null,
    change: bps === null ? null : bpsPercentText(bps),
    changeTone: marks ? 'flat' : toneOf(bps),
    ...changeMarkFields(marks),
    high: p(v?.low_24h),
    low: p(v?.high_24h),
    lastExact: exact(v?.last),
    highExact: exact(v?.low_24h),
    lowExact: exact(v?.high_24h),
    volume: qv === null ? null : compactText(Number(qv) / 1e8),
    volumeExact: qv === null ? null : formatUnits(qv, 8, { group: ',' }),
    quoteVolume: vol === null ? null : compactText(unitsToNumber(vol, decimals)),
    quoteVolumeExact: vol === null ? null : formatUnits(vol, decimals, { group: ',' }),
    trades: v && typeof v.trades_24h === 'number' ? v.trades_24h : null,
    spread: v && typeof v.spread_bps === 'number' ? bpsPercentText(v.spread_bps).replace('+', '') : null,
    dp,
  };
}

// ----------------------------------------------------------------------------------------- candles

export interface FlippedSeries {
  bars: ChartBar[];
  volumes: VolumeBar[];
  /** Exact OHLC texts of bar i. */
  text: (i: number) => { o: string; h: string; l: string; c: string } | null;
}

/**
 * Turns the native, gap-filled indexer candles into KAS/TOKEN candles. Open and close invert, and
 * high and low swap, because the native low is the inverted high. The volume is the KAS turnover
 * of the bucket. A bar is "up" when the inverted close is at or above the inverted open.
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

// ------------------------------------------------------------------------------------------- depth

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

/**
 * `/v1/depth` as KAS/TOKEN. Native asks become the bids with price `1/p`, and the cumulative amount
 * is KAS.
 */
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

/**
 * Fallback without `/v1/depth`. It turns the aggregated book, with state prices per `scale` base
 * units, into a KAS/TOKEN depth series. The cumulative value is already in KAS.
 */
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
