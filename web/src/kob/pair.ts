// Exact arithmetic of token/token pairs A/B (BASE A, QUOTE B, e.g. BTC/USDT) and the pair book / own-order helpers of the pair planner.
// DOM-free, bigint only.
//
// Prices of a pair are exact rationals. The indexer quotes `price_num / price_den` QUOTE base units per BASE base unit
// (`GET /v1/pairs/{base}/{quote}/book`, docs/ops/executor.md 5.4); a pair order's state price is QUOTE base units per WHOLE BASE
// (`scale(A)` base units of A): per whole A = num x scale(A) / den, rounded UP for what one pays (an ask level: the price a buyer meets) and
// DOWN for what one receives (a bid level). The UI shows QUOTE per WHOLE BASE token = num/den x 10^(baseDecimals - quoteDecimals).
//
// Exported API (planner side):
//   pairBookToBookView(view, scaleA)          the pair book as the planners' BookView (B per whole A, asks up, bids down; every source)
//   wholePriceOf(num, den, scaleA, round)     one level price per whole A
//   ownPairOrderRefs(orders, base, quote)     the wallet's live pair orders of the pair A/B as OwnOrderRef (self-trade prevention)
//   impliedPairRate(kasA, kasB, scaleB)       the rate two KAS prices imply, floor(kasA x scale(B) / kasB) B per whole A
//   isPairOrderView(o)                        whether an indexer order view is a pair order
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

/** Reduced form (den > 0). */
export function reduce(r: Rational): Rational {
  if (r.den === 0n) throw new RangeError('zero denominator');
  const g = gcd(r.num, r.den) || 1n;
  const s = r.den < 0n ? -1n : 1n;
  return { num: (s * r.num) / g, den: (s * r.den) / g };
}

/** a < b: -1, a == b: 0, a > b: 1 (cross multiplication, exact). */
export const cmpRational = (a: Rational, b: Rational): number => {
  const l = a.num * b.den;
  const r = b.num * a.den;
  return l < r ? -1 : l > r ? 1 : 0;
};

export const mulRational = (a: Rational, b: Rational): Rational => reduce({ num: a.num * b.num, den: a.den * b.den });

export const ceilDivBig = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;

/** Parses a plain non-negative decimal ("0.052", "12", "1e3" is refused) into an exact rational; null when it is not one. */
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
 * Decimal text of a rational with at most `dp` fraction digits, rounded `up` or `down` (the side that is conservative for the reader)
 * or to `nearest`. Trailing zeros are trimmed down to `minDp` digits.
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

/** Fraction digits that show `r` with about `sig` significant digits (at least `min`, at most `max`). */
export function significantDp(r: Rational, sig = 6, min = 2, max = 18): number {
  if (r.num <= 0n) return min;
  const int = r.num / r.den;
  if (int > 0n) return Math.max(min, Math.min(max, sig - int.toString().length));
  // leading zeros after the point
  let zeros = 0;
  let x = r.num * 10n;
  while (x < r.den && zeros < max) {
    x *= 10n;
    zeros++;
  }
  return Math.max(min, Math.min(max, zeros + sig));
}

/** QUOTE base units per BASE base unit -> QUOTE per WHOLE BASE token (whole QUOTE units): x 10^(baseDecimals - quoteDecimals). */
export function wholePrice(perBaseUnit: Rational, baseDecimals: number, quoteDecimals: number): Rational {
  const d = baseDecimals - quoteDecimals;
  return d >= 0 ? reduce({ num: perBaseUnit.num * pow10(d), den: perBaseUnit.den }) : reduce({ num: perBaseUnit.num, den: perBaseUnit.den * pow10(-d) });
}

/** Inverse of `wholePrice`: QUOTE per whole BASE -> QUOTE base units per BASE base unit. */
export function unitPrice(whole: Rational, baseDecimals: number, quoteDecimals: number): Rational {
  return wholePrice(whole, quoteDecimals, baseDecimals);
}

/** Float of a rational, for chart coordinates and depth bars only (never for amounts). */
export const ratToNumber = (r: Rational): number => (r.den === 0n ? NaN : Number(r.num * 1_000_000_000_000n / r.den) / 1e12);

// ------------------------------------------------------------------------------------------------ pair identity

/** The two sides of a pair a user can trade: sell BASE for QUOTE, or buy BASE with QUOTE. */
export type PairSide = 'sell' | 'buy';

/** What the conversions need to know about a token of a pair. */
export interface PairToken {
  covenantId: Hex;
  ticker: string;
  decimals: number;
  /** the order scale: base units per whole token (`10^decimals`, at most 10^9): the denominator of a pair order's price */
  scale: bigint;
}

/** Whole-unit amount text of `units` base units (exact, trailing zeros trimmed). */
export function unitsText(units: bigint, decimals: number, group = ''): string {
  return formatRational({ num: units, den: pow10(decimals) }, decimals, 'down', 0, group);
}

// ------------------------------------------------------------------------------------------------ the pair book of the planners

/** A level price per WHOLE A: `num x scaleA / den` B base units, rounded `up` (asks) or `down` (bids). Null for a malformed level. */
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
 * The pair book (indexer 5.4: `direct` KobPair levels, `entry` KobIfdPair levels, `route` levels implied by the two KAS books) as the planners'
 * BookView: prices B base units per whole A (asks rounded UP: never better than the book; bids DOWN), amounts base units of A, no tips (a pair
 * order's tip is KAS and never part of its B price). Every source is liquidity a filler can use (option 2: route, netting, inventory); levels
 * that round to one price merge (orders add up). Malformed levels are skipped.
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
      const last = out[out.length - 1];
      if (last && last.price === price) {
        last.amount += amount;
        last.orders += orders;
      } else out.push({ price, amount, orders });
    }
    // keep the order the planners assume even if the server's order and the rounding disagree: asks ascending, bids descending
    return out.sort((a, b) => (a.price === b.price ? 0 : (round === 'up' ? a.price < b.price : a.price > b.price) ? -1 : 1));
  };
  return { asks: side(view.asks ?? [], 'up'), bids: side(view.bids ?? [], 'down') };
}

const PAIR_CONTRACTS = new Set(['KobPair', 'KobCondPair', 'KobIfdPair']);

/** An indexer order view of a pair order (by its `pair` object or its contract). */
export const isPairOrderView = (o: Pick<OrderView, 'contract'> & { pair?: unknown }): boolean => !!o.pair || PAIR_CONTRACTS.has(o.contract);

const isLive = (o: OrderView): boolean => o.status === 'open' || o.status === 'partial';

/**
 * The wallet's live pair orders of the pair `base`/`quote` (oriented: an order of B/A is not listed) for self-trade prevention: an ASK (sells A;
 * a sell-first entry) is a sell, a BID a buy, at its quote now (`pair.quote_now`, else `pair.price`: B per whole A), tip 0 (the KAS tip never
 * changes a B price). Orders without a price (a stop-only conditional: it trades only after its trigger) are left out, as are malformed views.
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

/** The rate two KAS prices imply (B base units per whole A): `floor(kasPerWholeA x scale(B) / kasPerWholeB)`; null without both. */
export function impliedPairRate(kasPerWholeA: bigint | null, kasPerWholeB: bigint | null, scaleB: bigint): bigint | null {
  if (kasPerWholeA === null || kasPerWholeB === null || kasPerWholeA <= 0n || kasPerWholeB <= 0n || scaleB <= 0n) return null;
  return (kasPerWholeA * scaleB) / kasPerWholeB;
}
