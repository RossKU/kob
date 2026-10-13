// Wallet-side guards (docs/spec/matcher.md section 10): the rules the covenants cannot check for the user.
//   * self-trade prevention (10.12)
//   * FOK pre-check against the visible book (10.4, spec 9.9)
//   * marketable-limit detection (10.5) and price-band sanity
//   * tick / amount / minimum fill / value / time-range checks
// Every function is pure and returns PlanIssue[] (never throws for user-input problems). Amounts are token base units, prices sompi per
// whole token (`scale` base units), all bigint.
import type { BookLevel, BookOrder, BookView, Clock, OwnOrderRef, PlanIssue, TokenMarket } from './plan-types';
import { issue } from './orders/common-issues';
import { MAX_IDLE_DAA } from './daa';
import { BPS, fitsI64, formatUnits, isOnTick, quoteOf, roundToTick } from './units';
import { KRON_MAX_OUTPUT_AMOUNT } from './token-state';

/** Most token inputs any transaction may carry (`MAX_TOK_IN` of every KOB covenant). */
export const MAX_TOKEN_INPUTS = 8;

// ------------------------------------------------------------------------------------------------ crossing

/** The two limits of a potential trade, all-in per whole token: a sell receives `price - tip` at least, a buy pays `price + tip` at most. */
export interface Quote { price: bigint; tip: bigint }

/** True when a buy and a sell can trade: the buyer's all-in ceiling reaches the seller's all-in floor (equality crosses). */
export const crosses = (sell: Quote, buy: Quote): boolean => buy.price + buy.tip >= sell.price - sell.tip;

/** Best opposite price of the book for an order on `side` (sell -> best bid, buy -> best ask), or null when that side is empty. */
export function touchPrice(book: BookView, side: 'sell' | 'buy'): bigint | null {
  const lv = side === 'sell' ? book.bids[0] : book.asks[0];
  return lv ? lv.price : null;
}

/** Midpoint of the touch when both sides exist, else the one side that does; the reference of the price-band warnings. */
export function referencePrice(book: BookView): bigint | null {
  const b = book.bids[0]?.price;
  const a = book.asks[0]?.price;
  if (b !== undefined && a !== undefined) return (a + b) / 2n;
  return b ?? a ?? null;
}

/** KAS (sompi) the bids at or above the firm reference must be worth together: one default minimum fill (10 KAS). */
export const FIRM_DEPTH_SOMPI = 1_000_000_000n;

/**
 * A KAS value of a token that one far-off quote cannot move (the default minimum fill of a pair order is the amount worth 10 KAS at it):
 * a bid is a firm offer (anyone can sell into it), an ask nobody has to take is not. With the token's `scale` the bid is the price at which
 * the bids, best first, are worth `depth` (10 KAS) together, so a tiny bid at an absurd price above the others does not set it; without it
 * the best bid. The midpoint with the best ask when that ask is at most twice the bid, the bid alone when the spread is wider or there is
 * no ask, null when only asks exist (or the bids are not worth `depth` together).
 */
export function firmReferencePrice(book: BookView, scale?: bigint, depth: bigint = FIRM_DEPTH_SOMPI): bigint | null {
  let b: bigint | undefined;
  if (scale === undefined || scale <= 0n) {
    b = book.bids[0]?.price;
  } else {
    let value = 0n;
    for (const l of book.bids) {
      value += (l.amount * l.price) / scale;
      if (value >= depth) {
        b = l.price;
        break;
      }
    }
  }
  const a = book.asks[0]?.price;
  if (b === undefined || b <= 0n) return null;
  if (a !== undefined && a >= b && a <= 2n * b) return (a + b) / 2n;
  return b;
}

// ------------------------------------------------------------------------------------------------ self-trade (10.12)

/**
 * A new order must not cross the user's own resting order (a self-trade pays fees for nothing and can be filled by a matcher against
 * yourself). `cand.price` is the most aggressive price the new order can trade at: its limit, or the worst bound of an auction / IOC.
 * Own orders are compared at their own worst reach (OwnOrderRef.price), all-in with tips, on the opposite side only.
 */
export function checkSelfTrade(own: readonly OwnOrderRef[], cand: { side: 'sell' | 'buy'; price: bigint; tip: bigint }): PlanIssue[] {
  const out: PlanIssue[] = [];
  for (const o of own) {
    if (!o.active || o.side === cand.side || o.amountLeft <= 0n) continue;
    const sell: Quote = cand.side === 'sell' ? cand : { price: o.price, tip: o.tip };
    const buy: Quote = cand.side === 'buy' ? cand : { price: o.price, tip: o.tip };
    if (crosses(sell, buy)) out.push(issue('SELF_TRADE', { ownCovenantId: o.covenantId, ownPrice: o.price }));
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ FOK pre-check (10.4)

export interface FokInput {
  side: 'sell' | 'buy';
  /** base units the FOK must fill */
  amount: bigint;
  /** the order's most aggressive price (limit, or the worst bound of a streaming FOK), per whole token */
  limit: bigint;
  tip: bigint;
  slots: { inputs: number; outputs: number };
  book: BookView;
}

export interface FokAssessment {
  /** base units available from counterparties that cross the limit */
  available: bigint;
  /** counterparties needed to fill `amount` (an upper bound when `exact` is false) */
  counterparties: number;
  /** true when computed from per-order entries; false = conservative bound from the aggregated levels' order counts */
  exact: boolean;
  /** the book can supply `amount` at all */
  enough: boolean;
  /** the counterparty limit of this transaction: FOK ask -> token output slots; FOK bid -> min(input slots, 8) */
  maxCounterparties: number;
  /**
   * Per-order entries only: no set of at most `maxCounterparties` crossing orders can take exactly `amount` because of their minimum fills (the
   * last order of the fewest would need a partial fill below its minimum fill, and no other choice of orders avoids it). From the fewest: `left` =
   * what is left of the FOK for the last order, `minFill` = the least that order takes, `below` = what the orders before it take in full (never 0:
   * one order that holds the whole amount fills it), `above` = the amount that fills with that last order at its minimum. Absent when a fill exists
   * or the search could not decide.
   */
  minFillBlock?: { left: bigint; minFill: bigint; below: bigint; above: bigint };
}

/** A crossing order of the per-order book: what it can give (`amount`) and the least fill it takes (`lo`: its minimum fill, or all it has left). */
interface FokCand { amount: bigint; lo: bigint }

/** Search budget of the exact FOK feasibility check (nodes): beyond it the check gives up and refuses nothing. */
const FOK_SEARCH_BUDGET = 50_000;

/**
 * Whether at most `max` of `cands` (sorted by amount, largest first) can fill exactly `need`: some set S with sum(lo) <= need <= sum(amount) (every
 * order of S then takes between its least fill and all it has). null when the search ran out of budget (undecided).
 */
function fokFillable(cands: readonly FokCand[], need: bigint, max: number): boolean | null {
  const n = cands.length;
  // prefix sums of the amounts: the largest `k` amounts from index i on are cands[i .. i+k) (sorted)
  const prefix: bigint[] = [0n];
  for (const c of cands) prefix.push(prefix[prefix.length - 1]! + c.amount);
  let budget = FOK_SEARCH_BUDGET;
  const go = (i: number, count: number, sumAmt: bigint, sumLo: bigint): boolean | null => {
    if (sumLo > need) return false;
    if (sumAmt >= need) return true;
    if (count >= max || i >= n) return false;
    const room = Math.min(max - count, n - i);
    if (sumAmt + (prefix[i + room]! - prefix[i]!) < need) return false;
    if (--budget < 0) return null;
    const c = cands[i]!;
    const take = go(i + 1, count + 1, sumAmt + c.amount, sumLo + c.lo);
    if (take !== false) return take;
    return go(i + 1, count, sumAmt, sumLo);
  };
  return go(0, 0, 0n, 0n);
}

/**
 * How many counterparties a FOK needs. With per-order entries the answer is exact: the FEWEST crossing orders that hold `amount` (a matcher may
 * pick any crossing subset; matcher.md 3.2 repairs a FOK from unallocated liquidity), and whether their minimum fills let at most the slots'
 * counterparties take exactly `amount` (`minFillBlock`): an order fills at least its minimum fill unless it gives all it has left, so a FOK whose
 * last counterparty would need a smaller partial fill cannot complete unless another choice of orders avoids it. An order that cannot take part at
 * all (its least fill exceeds the whole FOK) is left out. With aggregated levels it is a conservative bound: levels are taken best-first and every
 * order of a touched level counts, except on a pair route level (`routeOrders` KAS orders of `routeRows` route levels of the indexer): every
 * route level combines at least one KAS order of each token and only the traded token's orders use its slots, so a route level of n orders counts
 * n - 1.
 */
export function assessFok(f: FokInput): FokAssessment {
  const sellSide = f.side === 'sell';
  // counterparties: a FOK sell fills against bids (each delivers one token output); a FOK buy against asks (each is a token input)
  const detail = sellSide ? f.book.bidOrders : f.book.askOrders;
  const levels = sellSide ? f.book.bids : f.book.asks;
  const maxCounterparties = sellSide ? f.slots.outputs : Math.min(f.slots.inputs, MAX_TOKEN_INPUTS);
  const okPrice = (price: bigint, tip: bigint | undefined): boolean =>
    sellSide ? price + (tip ?? 0n) >= f.limit - f.tip : price - (tip ?? 0n) <= f.limit + f.tip;

  if (detail) {
    const cross: FokCand[] = detail
      .filter((o: BookOrder) => o.amount > 0n && okPrice(o.price, o.tip))
      .map((o) => {
        const min = o.minFill ?? 1n;
        return { amount: o.amount, lo: min < o.amount ? min : o.amount };
      })
      .filter((c) => c.lo <= f.amount)
      .sort((a, b) => (a.amount === b.amount ? (a.lo === b.lo ? 0 : a.lo < b.lo ? -1 : 1) : a.amount > b.amount ? -1 : 1));
    const available = cross.reduce((s, o) => s + o.amount, 0n);
    let need = f.amount;
    let count = 0;
    let before = 0n;
    let last: FokCand | null = null;
    for (const o of cross) {
      if (need <= 0n) break;
      if (need <= o.amount) last = o;
      else before += o.amount;
      need -= o.amount;
      count++;
    }
    const out: FokAssessment = { available, counterparties: count, exact: true, enough: available >= f.amount, maxCounterparties };
    if (out.enough && count <= maxCounterparties && last !== null && f.amount - before < last.lo && fokFillable(cross, f.amount, maxCounterparties) === false) {
      out.minFillBlock = { left: f.amount - before, minFill: last.lo, below: before, above: before + last.lo };
    }
    return out;
  }
  const cross = levels.filter((l: BookLevel) => l.amount > 0n && okPrice(l.price, l.tip));
  const available = cross.reduce((s, l) => s + l.amount, 0n);
  let need = f.amount;
  let count = 0;
  for (const l of cross) {
    if (need <= 0n) break;
    need -= l.amount;
    const route = Math.min(l.routeOrders ?? 0, l.orders);
    const rows = Math.min(l.routeRows ?? 0, route);
    count += l.orders - route + (route - rows);
  }
  return { available, counterparties: count, exact: false, enough: available >= f.amount, maxCounterparties };
}

/** Rejects a FOK the visible book cannot fill in ONE transaction (matcher.md 10.4): depth, counterparty slots, 8 token inputs, minimum fills. */
export function checkFok(f: FokInput): PlanIssue[] {
  const a = assessFok(f);
  if (!a.enough) return [issue('FOK_INSUFFICIENT_DEPTH', { amount: f.amount, available: a.available })];
  if (a.counterparties > a.maxCounterparties) {
    if (f.side === 'buy' && f.slots.inputs > MAX_TOKEN_INPUTS && a.counterparties > MAX_TOKEN_INPUTS) {
      return [issue('FOK_TOO_MANY_TOKEN_INPUTS', { count: a.counterparties })];
    }
    return [issue('FOK_TOO_MANY_COUNTERPARTIES', { count: a.counterparties, max: a.maxCounterparties, exact: a.exact ? 1 : 0 })];
  }
  const m = a.minFillBlock;
  return m ? [issue('FOK_MIN_FILL_BLOCKS', { left: m.left, minFill: m.minFill, below: m.below, above: m.above }, 'amount')] : [];
}

// ------------------------------------------------------------------------------------------------ marketable limit (10.5)

/** Whether a limit on `side` at `price` crosses the visible book right now, and the touch it would cross. */
export function crossingTouch(book: BookView, side: 'sell' | 'buy', price: bigint, tip: bigint): { crossing: boolean; touch: bigint | null } {
  const lv = side === 'sell' ? book.bids[0] : book.asks[0];
  if (!lv) return { crossing: false, touch: null };
  const mine: Quote = { price, tip };
  const theirs: Quote = { price: lv.price, tip: lv.tip ?? 0n };
  return { crossing: side === 'sell' ? crosses(mine, theirs) : crosses(theirs, mine), touch: lv.price };
}

/** Base units the visible book offers to an order on `side` down/up to `bound` (all-in with tips), for market-depth warnings. */
export function depthWithin(book: BookView, side: 'sell' | 'buy', bound: bigint, tip: bigint): bigint {
  const levels = side === 'sell' ? book.bids : book.asks;
  let amount = 0n;
  for (const l of levels) {
    const t = l.tip ?? 0n;
    const ok = side === 'sell' ? l.price + t >= bound - tip : l.price - t <= bound + tip;
    if (ok) amount += l.amount;
  }
  return amount;
}

// ------------------------------------------------------------------------------------------------ price, amount, value, time

/** The protocol bound of every quote: the full fill of an order at any rate it carries is worth less than 2^62 (kob-protocol `check_quote`). */
export const QUOTE_LIMIT = 1n << 62n;

/**
 * Amount in base units: positive, fits the 64-bit state fields and, for a token-holding order (`custody`), the token program's output limit
 * (a KRON token output holds at most KRON_MAX_OUTPUT_AMOUNT base units).
 */
export function checkAmount(amount: bigint, market: TokenMarket, field = 'amount', custody = true): PlanIssue[] {
  if (amount <= 0n) return [issue('AMOUNT_NOT_POSITIVE', undefined, field)];
  if (!fitsI64(amount)) return [issue('AMOUNT_TOO_LARGE', undefined, field)];
  if (custody && market.family === 'kron' && amount > KRON_MAX_OUTPUT_AMOUNT) return [issue('AMOUNT_TOO_LARGE', undefined, field)];
  return [];
}

/** The minimum fill: at least one base unit and at most the order amount (an order's `minFill`, the covenant's anti-dust rule). */
export function checkMinFill(minFill: bigint, amount: bigint, field = 'minFill'): PlanIssue[] {
  return minFill < 1n || minFill > amount ? [issue('MIN_FILL_INVALID', undefined, field)] : [];
}

/** The 2^62 bound of the order's value at `rate` (kob-protocol `check_quote`; the builders refuse beyond it). */
export function checkNotional(amount: bigint, rate: bigint, scale: bigint, field = 'price'): PlanIssue[] {
  if (amount <= 0n || rate <= 0n) return [];
  return quoteOf(amount, rate, scale, 'up') >= QUOTE_LIMIT ? [issue('NOTIONAL_TOO_LARGE', undefined, field)] : [];
}

/** Positive, fits 64 bits, and (when the token defines a tick) on the tick. Off-tick is REFUSED with the two nearest valid prices. */
export function checkPrice(market: TokenMarket, price: bigint, field = 'price'): PlanIssue[] {
  if (price <= 0n) return [issue('PRICE_NOT_POSITIVE', undefined, field)];
  if (!fitsI64(price)) return [issue('PRICE_TOO_LARGE', undefined, field)];
  if (!isOnTick(price, market.tick)) {
    const below = roundToTick(price, market.tick, 'down');
    return [issue('PRICE_NOT_ON_TICK', { tick: market.tick, below, above: roundToTick(price, market.tick, 'up') }, field)];
  }
  return [];
}

/** Tip >= 0 and, for a sell, below the price (the maker must receive something). */
export function checkTip(side: 'sell' | 'buy', tip: bigint, worstPrice: bigint, field = 'tip'): PlanIssue[] {
  if (tip < 0n) return [issue('TIP_NEGATIVE', undefined, field)];
  if (side === 'sell' && tip >= worstPrice) return [issue('TIP_EXCEEDS_PRICE', { tip }, field)];
  return [];
}

/** Time-range checks of a planned order: activation and expiry within 90 days, expiry after activation. */
export function checkTimes(clock: Clock, t: { activeFrom: bigint; expiryDaa: bigint; requestedActiveFrom?: bigint | null }): PlanIssue[] {
  const out: PlanIssue[] = [];
  const horizon = clock.daa + MAX_IDLE_DAA;
  if (t.requestedActiveFrom != null && t.requestedActiveFrom > 0n && t.requestedActiveFrom <= clock.daa) out.push(issue('ACTIVE_IN_PAST', undefined, 'activeFrom'));
  if (t.activeFrom > horizon) out.push(issue('ACTIVE_TOO_FAR', undefined, 'activeFrom'));
  if (t.expiryDaa > horizon) out.push(issue('EXPIRY_TOO_FAR', undefined, 'expiry'));
  if (t.expiryDaa <= clock.daa) out.push(issue('EXPIRY_TOO_SOON', undefined, 'expiry'));
  else if (t.expiryDaa <= t.activeFrom) out.push(issue('ACTIVE_AFTER_EXPIRY', undefined, 'expiry'));
  return out;
}

/**
 * Sanity warnings against the market: a limit far THROUGH the market (sell well below / buy well above the reference) is probably a
 * typing error; one far on the passive side will not fill soon. Thresholds: 10% through, 50% away (in basis points below).
 */
export const PRICE_THROUGH_BPS = 1_000n;
export const PRICE_AWAY_BPS = 5_000n;

export function checkPriceBand(reference: bigint | null, side: 'sell' | 'buy', price: bigint, field = 'price'): PlanIssue[] {
  if (reference === null || reference <= 0n) return [];
  const through = side === 'sell' ? reference - price : price - reference;
  const pct = (bps: bigint): string => formatUnits(bps, 2, { maxFraction: 1 });
  if (through > 0n) {
    const bps = (through * BPS) / reference;
    return bps >= PRICE_THROUGH_BPS ? [issue('PRICE_AGGRESSIVE_VS_MARKET', { percent: pct(bps), reference }, field)] : [];
  }
  const bps = (-through * BPS) / reference;
  return bps >= PRICE_AWAY_BPS ? [issue('PRICE_FAR_FROM_MARKET', { percent: pct(bps), reference }, field)] : [];
}

/** KAS locked as carriers dwarfing the order: warns when the carriers exceed the order's notional value. */
export function checkCarrierRatio(locked: bigint, notional: bigint | null): PlanIssue[] {
  return notional !== null && locked > notional ? [issue('CARRIERS_DOMINATE', { locked })] : [];
}
