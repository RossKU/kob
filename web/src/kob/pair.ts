// Exact arithmetic of token/token pairs A/B, with BASE A and QUOTE B such as BTC/USDT. It also
// holds the pair book and own-order helpers of the pair planner. It is DOM-free and bigint only.
//
// Prices of a pair are exact rationals. The indexer quotes `price_num / price_den` QUOTE base units
// per BASE base unit (`GET /v1/pairs/{base}/{quote}/book`, docs/ops/executor.md 5.4). A pair
// order's state price is QUOTE base units per whole BASE, meaning per `scale(A)` base units of A.
// Per whole A it is `num x scale(A) / den`. It rounds up for what one pays, which is an ask level
// and the price a buyer meets. It rounds down for what one receives, which is a bid level. The UI
// shows QUOTE per whole BASE token as `num/den x 10^(baseDecimals - quoteDecimals)`.
//
// Planner-side entry points:
// - `pairBookToBookView(view, scaleA)` turns the pair book into the planners' BookView, with B per
//   whole A, asks up, bids down and every source included.
// - `wholePriceOf(num, den, scaleA, round)` gives one level price per whole A.
// - `ownPairOrderRefs(orders, base, quote)` lists the wallet's live pair orders of the pair A/B as
//   OwnOrderRef, for self-trade prevention.
// - `impliedPairRate(kasA, kasB, scaleB)` gives the rate two KAS prices imply, in B per whole A.
// - `isPairOrderView(o)` tells whether an indexer order view is a pair order.
import type { OrderView, PairBookView, PairLevelView } from '../data/indexer-types';
import type { BookLevel, BookView, OwnOrderRef } from './plan-types';
import type { Hex } from './types';
import { pow10 } from './units';

/** A non-negative exact rational. `den` > 0. */
export interface Rational { num: bigint; den: bigint }

const gcd = (a: bigint, b: bigint): bigint => {
  let x = a < 0n ? -a : a;
  let y = b < 0n ? -b : b;
  while (y) [x, y] = [y, x % y];
  return x;
};

/**
 * Returns the reduced form of `r` with a positive denominator. It throws for a zero denominator.
 */
export function reduce(r: Rational): Rational {
  if (r.den === 0n) throw new RangeError('zero denominator');
  const g = gcd(r.num, r.den) || 1n;
  const s = r.den < 0n ? -1n : 1n;
  return { num: (s * r.num) / g, den: (s * r.den) / g };
}

/**
 * Compares two rationals exactly by cross multiplication. It returns -1, 0 or 1 for a below, equal
 * to or above b.
 */
export const cmpRational = (a: Rational, b: Rational): number => {
  const l = a.num * b.den;
  const r = b.num * a.den;
  return l < r ? -1 : l > r ? 1 : 0;
};

/** Multiplies two rationals and reduces the result. */
export const mulRational = (a: Rational, b: Rational): Rational => reduce({ num: a.num * b.num, den: a.den * b.den });

/** Integer division of positive bigints, rounded up. */
export const ceilDivBig = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;

/**
 * Parses a plain non-negative decimal such as "0.052" or "12" into an exact rational. Exponents
 * like "1e3" and other text give null.
 */
export function parseDecimal(text: string): Rational | null {
  const s = (text ?? '').trim().replace(/,/g, '');
  const m = /^(\d*)(?:\.(\d*))?$/.exec(s);
  if (!m || (m[1] === '' && (m[2] ?? '') === '')) return null;
  const frac = m[2] ?? '';
  if (frac.length > 36) return null;
  const num = BigInt((m[1] || '0') + frac);
  return reduce({ num, den: pow10(frac.length) });
}

/**
 * Formats a rational as decimal text with at most `dp` fraction digits.
 * It rounds `up` or `down`, whichever is conservative for the reader, or to `nearest`. Trailing
 * zeros are trimmed down to `minDp` digits.
 */
export function formatRational(r: Rational, dp: number, mode: 'up' | 'down' | 'nearest' = 'nearest', minDp = 0, group = ''): string {
  const scale = pow10(dp);
  const n = r.num * scale;
  let q = n / r.den;
  const rem = n % r.den;
  if (rem !== 0n) {
    if (mode === 'up') q += 1n;
    else if (mode === 'nearest' && rem * 2n >= r.den) q += 1n;
  }
  const int = q / scale;
  let frac = dp > 0 ? (q % scale).toString().padStart(dp, '0') : '';
  while (frac.length > minDp && frac.endsWith('0')) frac = frac.slice(0, -1);
  const intText = group ? int.toString().replace(/\B(?=(\d{3})+(?!\d))/g, group) : int.toString();
  return frac ? `${intText}.${frac}` : intText;
}

/**
 * Fraction digits that show `r` with about `sig` significant digits (at least `min`, at most
 * `max`).
 */
export function significantDp(r: Rational, sig = 6, min = 2, max = 18): number {
  if (r.num <= 0n) return min;
  const int = r.num / r.den;
  if (int > 0n) return Math.max(min, Math.min(max, sig - int.toString().length));
  // Count the leading zeros after the point.
  let zeros = 0;
  let x = r.num * 10n;
  while (x < r.den && zeros < max) {
    x *= 10n;
    zeros++;
  }
  return Math.max(min, Math.min(max, zeros + sig));
}

/**
 * Converts QUOTE base units per BASE base unit to whole QUOTE units per whole BASE token, times
 * 10^(baseDecimals - quoteDecimals).
 */
export function wholePrice(perBaseUnit: Rational, baseDecimals: number, quoteDecimals: number): Rational {
  const d = baseDecimals - quoteDecimals;
  return d >= 0 ? reduce({ num: perBaseUnit.num * pow10(d), den: perBaseUnit.den }) : reduce({ num: perBaseUnit.num, den: perBaseUnit.den * pow10(-d) });
}

/**
 * Converts whole QUOTE units per whole BASE to QUOTE base units per BASE base unit, the inverse of
 * `wholePrice`.
 */
export function unitPrice(whole: Rational, baseDecimals: number, quoteDecimals: number): Rational {
  return wholePrice(whole, quoteDecimals, baseDecimals);
}

/**
 * Converts a rational to a float, for chart coordinates and depth bars only and never for amounts.
 */
export const ratToNumber = (r: Rational): number => (r.den === 0n ? NaN : Number(r.num * 1_000_000_000_000n / r.den) / 1e12);

// --- Pair identity ---

/** The two sides of a pair a user can trade: sell BASE for QUOTE, or buy BASE with QUOTE. */
export type PairSide = 'sell' | 'buy';

/** What the conversions need to know about a token of a pair. */
export interface PairToken {
  covenantId: Hex;
  ticker: string;
  decimals: number;
  /**
   * The order scale, base units per whole token. It is `10^decimals`, at most 10^9, and the
   * denominator of a pair order's price.
   */
  scale: bigint;
}

/** Formats `units` base units as a whole-unit amount, exactly and with trailing zeros trimmed. */
export function unitsText(units: bigint, decimals: number, group = ''): string {
  return formatRational({ num: units, den: pow10(decimals) }, decimals, 'down', 0, group);
}

// --- The pair book of the planners ---

/**
 * Returns a level price per whole A, `num x scaleA / den` B base units, rounded `up` for asks or
 * `down` for bids. A malformed level gives null.
 */
export function wholePriceOf(num: bigint, den: bigint, scaleA: bigint, round: 'up' | 'down'): bigint | null {
  if (den <= 0n || num <= 0n || scaleA <= 0n) return null;
  const n = num * scaleA;
  return round === 'up' ? ceilDivBig(n, den) : n / den;
}

const big = (v: unknown): bigint | null => {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number' && Number.isSafeInteger(v)) return BigInt(v);
  if (typeof v === 'string' && /^\d{1,40}$/.test(v)) return BigInt(v);
  return null;
};

/**
 * Converts the pair book to the planners' BookView.
 *
 * The book comes from indexer 5.4. It has `direct` KobPair levels, `entry` KobIfdPair levels and
 * `route` levels implied by the two KAS books. Prices are B base units per whole A. Asks round up,
 * so they are never better than the book, and bids round down. Amounts are base units of A. There
 * are no tips, because a pair order's tip is KAS and never part of its B price.
 *
 * Every source is liquidity a filler can use. Levels that round to one price merge, and their
 * orders add up. Malformed levels are skipped.
 */
export function pairBookToBookView(view: Pick<PairBookView, 'asks' | 'bids'>, scaleA: bigint): BookView {
  const side = (rows: readonly PairLevelView[], round: 'up' | 'down'): BookLevel[] => {
    const out: BookLevel[] = [];
    for (const l of rows) {
      const num = big(l.price_num);
      const den = big(l.price_den);
      const amount = big(l.amount);
      if (num === null || den === null || amount === null || amount <= 0n) continue;
      const price = wholePriceOf(num, den, scaleA, round);
      if (price === null || price <= 0n) continue;
      const orders = Number.isSafeInteger(l.orders) && l.orders > 0 ? l.orders : 1;
      // A route level's KAS orders are of both tokens. The FOK pre-check counts only the traded
      // token's.
      const route = l.source === 'route' && orders > 1 ? { routeOrders: orders, routeRows: 1 } : null;
      const last = out[out.length - 1];
      if (last && last.price === price) {
        last.amount += amount;
        last.orders += orders;
        if (route) {
          last.routeOrders = (last.routeOrders ?? 0) + route.routeOrders;
          last.routeRows = (last.routeRows ?? 0) + 1;
        }
      } else out.push({ price, amount, orders, ...(route ?? {}) });
    }
    // Keep the order the planners assume, asks ascending and bids descending, even if the
    // server's order and the rounding disagree.
    return out.sort((a, b) => (a.price === b.price ? 0 : (round === 'up' ? a.price < b.price : a.price > b.price) ? -1 : 1));
  };
  return { asks: side(view.asks ?? [], 'up'), bids: side(view.bids ?? [], 'down') };
}

const PAIR_CONTRACTS = new Set(['KobPair', 'KobCondPair', 'KobIfdPair']);

/** Tells whether an indexer order view is a pair order, by its `pair` object or its contract. */
export const isPairOrderView = (o: Pick<OrderView, 'contract'> & { pair?: unknown }): boolean => !!o.pair || PAIR_CONTRACTS.has(o.contract);

const isLive = (o: OrderView): boolean => o.status === 'open' || o.status === 'partial';

/**
 * Lists the wallet's live pair orders of the pair `base`/`quote` for self-trade prevention.
 *
 * The pair is oriented, so an order of B/A is not listed. An ask, which sells A, is a sell. A bid
 * is a buy. Each is taken at its quote now, `pair.quote_now` else `pair.price`, in B per whole A,
 * with a tip of 0 because the KAS tip never changes a B price. Orders without a price are left out,
 * such as a stop-only conditional, which trades only after its trigger. So are malformed views.
 */
export function ownPairOrderRefs(orders: readonly OrderView[], base: Hex, quote: Hex): OwnOrderRef[] {
  const out: OwnOrderRef[] = [];
  for (const o of orders) {
    const p = o.pair;
    if (!p || !isLive(o) || p.base !== base || p.quote !== quote) continue;
    const price = big(p.quote_now ?? p.price);
    if (price === null || price <= 0n) continue;
    const left = big(p.amount_left ?? o.amount_left ?? o.initial_amount ?? null);
    out.push({ covenantId: o.covenant_id, side: p.side === 'ask' ? 'sell' : 'buy', price, tip: 0n, amountLeft: left ?? 0n, active: !o.expired });
  }
  return out;
}

/**
 * Returns the rate two KAS prices imply, `floor(kasPerWholeA x scale(B) / kasPerWholeB)` B base
 * units per whole A. It is null without both prices.
 */
export function impliedPairRate(kasPerWholeA: bigint | null, kasPerWholeB: bigint | null, scaleB: bigint): bigint | null {
  if (kasPerWholeA === null || kasPerWholeB === null || kasPerWholeA <= 0n || kasPerWholeB <= 0n || scaleB <= 0n) return null;
  return (kasPerWholeA * scaleB) / kasPerWholeB;
}
