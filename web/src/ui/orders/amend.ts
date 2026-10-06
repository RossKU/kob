// Amend = cancel + a new order in ONE atomic transaction (`planCancelReplace`). This module derives the ReplacementSpec from the CURRENT
// order state: it edits only the fields the user may change (price, amount, tip; a conditional order's take-profit and stop) and leaves every
// other term as it is. It never decides validity: kob-wasm's builder (run by planCancelReplace) is the reference and its refusals come back as issues.
//
// Amounts are token base units, prices / tips the state's (sompi per whole token of the order's `scale` base units). Pair orders (KobPair,
// KobCondPair, KobIfdPair) have no in-place amend in the protocol: they are replaced from the full ticket form (cancel-replace). KAS on the new
// order UTXO:
//   * asks: unchanged (the order UTXO is a carrier; custody and top-up tokens are handled by planCancelReplace);
//   * plain bids: the KAS budget stays unless the user tops it up (`addKas`, taken from the wallet's free KAS by planCancelReplace); the amount
//     follows from the new price and the budget. A partly filled bid whose escrow no longer funds ONE minimum fill can only be replaced with a
//     top-up (the builder refuses a replacement below `bidEscrow(minFill, 1)`, kob-protocol `min_order_value`);
//   * conditional bids: the escrow follows the new worst price and tip: `value' = value + escrow'(amount') - escrow(amount)` where escrow is the
//     spend of the whole amount at the worst leg (kob-wasm `condBidEscrow`; the delivery carriers and the keeper reserve stay as they are).
import { bidWorst, slipForLimit, stopWorstPrice, type Legs } from '../../kob/orders/cond-legs';
import { baseKind } from '../../kob/order-facts';
import type { PlanIssue } from '../../kob/plan-types';
import type { OrderSnapshot, ReplacementSpec } from '../../kob/cancel';
import { MAX_IDLE_DAA } from '../../kob/daa';
import { formatKas, quoteOf } from '../../kob/units';
import type { CondAskState, CondBidState, AskState, BidState, OrderState } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';

/** limit: plain ask / bid (in place where possible); cond: stop / take-profit / OCO legs. */
export type AmendKind = 'limit' | 'cond';

/** New values as typed by the user (state prices: sompi per whole token; amounts: base units); a missing field keeps the current value. */
export interface AmendInput {
  /** limit price (limit orders) or take-profit price (conditional orders with a take-profit leg) */
  price?: bigint;
  /** trigger price of a conditional order's stop leg */
  stop?: bigint;
  /**
   * the stop leg's worst price (stop-limit: the limit). Without it a moved stop keeps its band in PERCENT, so its worst price moves with it
   * (the preview shows the new worst price); with it the band is re-derived so the worst price stays at this limit (matcher.md 10.6).
   */
  stopLimit?: bigint;
  /** base units to keep resting (asks and conditional orders; a plain bid keeps its KAS budget) */
  amount?: bigint;
  /** priority tip, sompi per whole token */
  tip?: bigint;
  /** plain bids only: additional KAS (sompi) for the new order's escrow, on top of what the old order still holds */
  addKas?: bigint;
  /**
   * GTC renewal: the new order gets a fresh on-chain expiry (now + 90 days). A GTC ends 90 days after PLACEMENT whatever its fills
   * (refund due at min(expiryDaa, utxoDaa + 90 days), expiryDaa = placement + 90 days), so renewing is a replacement with a new expiryDaa.
   */
  renew?: boolean;
}

export interface AmendOptions {
  /** the token's tick (sompi per whole token): prices must be multiples of it */
  tick?: bigint;
  /** current DAA score: needed to renew (the new expiry is nowDaa + 90 days) */
  nowDaa?: bigint;
  /** kob-wasm: the escrows and the buying power come from its exact helpers (the covenant's arithmetic); without it the same quote rule in bigint */
  kob?: KobWasm;
}

/** The renewed expiry, or an issue when the clock is unknown. */
function renewedExpiry(input: AmendInput, o: AmendOptions, issues: PlanIssue[]): { expiryDaa: string } | Record<string, never> {
  if (!input.renew) return {};
  if (o.nowDaa === undefined) {
    issues.push(issue('renew-no-clock', 'error', 'the current DAA score is needed to renew the order'));
    return {};
  }
  issues.push(issue('renewed', 'info', 'the new order is good for another 90 days from now'));
  return { expiryDaa: str(o.nowDaa + MAX_IDLE_DAA) };
}

export interface AmendResult {
  ok: boolean;
  issues: PlanIssue[];
  spec: ReplacementSpec | null;
  /** the values the new order will have (state prices, base units), for the preview line */
  next: {
    price: bigint | null; stop: bigint | null; amount: bigint | null; tip: bigint;
    /** plain bids: the escrow of the new order (sompi) and the most base units it can fund (its buying power) */
    value?: bigint; maxAmount?: bigint;
    /** conditional orders with a stop: the worst price after the trigger (the band's end) */
    stopWorst?: bigint;
  } | null;
}

const issue = (code: string, severity: PlanIssue['severity'], message: string, field?: string, params?: PlanIssue['params']): PlanIssue => ({
  code, severity, message, ...(field ? { field } : {}), ...(params ? { params } : {}),
});
const big = (v: string | number | bigint): bigint => BigInt(v);
const str = (v: bigint): string => v.toString();

function fail(issues: PlanIssue[]): AmendResult {
  return { ok: false, issues, spec: null, next: null };
}

function checkPrice(field: string, price: bigint, tick: bigint | undefined, issues: PlanIssue[]): bigint | null {
  if (price <= 0n) {
    issues.push(issue('price-not-positive', 'error', 'the price must be greater than zero', field));
    return null;
  }
  if (tick !== undefined && tick > 0n && price % tick !== 0n) {
    issues.push(issue('price-off-tick', 'error', `the price must be a multiple of the tick ${tick} sompi`, field, { tick }));
    return null;
  }
  return price;
}

/** Escrow of a plain bid for `amount` in `fills` fills (kob-protocol `BidState::escrow`). */
function bidEscrowOf(o: AmendOptions, order: OrderState, amount: bigint, fills: bigint): bigint | null {
  if (o.kob) return o.kob.bidEscrow(order, amount, fills);
  const s = order.state as BidState;
  const pMax = big(s.slope) !== 0n && big(s.priceEnd) > big(s.price) ? big(s.priceEnd) : big(s.price);
  const f = fills > 1n ? fills : 1n;
  return quoteOf(amount, pMax + big(s.tip), big(s.scale), 'up') + (f - 1n) + f * big(s.deliveryCarrier) + big(s.reserve);
}

/** Buying power of a plain bid escrow of `value` sompi (kob-protocol `BidState::buying_power`). */
function buyingPowerOf(o: AmendOptions, order: OrderState, value: bigint): bigint {
  if (o.kob) return o.kob.bidBuyingPower(order, value);
  const s = order.state as BidState;
  const pMax = big(s.slope) !== 0n && big(s.priceEnd) > big(s.price) ? big(s.priceEnd) : big(s.price);
  const rate = pMax + big(s.tip);
  const budget = value - big(s.deliveryCarrier) - big(s.reserve);
  return rate > 0n && budget > 0n ? (budget * big(s.scale)) / rate : 0n;
}

/** Builds the replacement of a plain limit order (KobAsk / KobBid). */
function limitReplacement(snap: OrderSnapshot, input: AmendInput, o: AmendOptions): AmendResult {
  const st = snap.order.state;
  if (baseKind(st.kind) !== 'KobAsk' && baseKind(st.kind) !== 'KobBid') return fail([issue('not-amendable', 'error', 'this order type cannot be amended in place')]);
  const s = st.state as AskState | BidState;
  const issues: PlanIssue[] = [];
  const sell = baseKind(st.kind) === 'KobAsk';

  const want = input.price ?? big(s.price);
  const price = checkPrice('price', want, o.tick, issues);
  const tip = input.tip ?? big(s.tip);
  if (tip < 0n) issues.push(issue('tip-negative', 'error', 'the tip cannot be negative', 'tip'));
  if (sell && tip >= want && want > 0n) issues.push(issue('tip-exceeds-price', 'error', 'a sell must receive something after the tip', 'tip'));
  const amount = sell ? (input.amount ?? big((s as AskState).amountLeft)) : null;
  if (amount !== null && amount < 1n) issues.push(issue('amount-not-positive', 'error', 'the amount must be at least one base unit', 'amount'));
  if (!sell && input.amount !== undefined) issues.push(issue('bid-amount-fixed', 'info', 'a buy order keeps its KAS budget: the amount follows from the price', 'amount'));
  const renewed = renewedExpiry(input, o, issues);
  const add = input.addKas ?? 0n;
  if (add < 0n) issues.push(issue('add-kas-negative', 'error', 'the KAS to add cannot be negative', 'addKas'));
  if (sell && add !== 0n) issues.push(issue('add-kas-bid-only', 'error', 'only a buy order takes additional KAS', 'addKas'));
  if (issues.some((i) => i.severity === 'error') || price === null) return fail(issues);

  // an ask's minimum fill never exceeds what is left (a smaller amount keeps a valid minimum)
  const minFill = big(s.minFill);
  const fit = sell && amount !== null && minFill > amount ? { minFill: str(amount) } : {};
  const next = { ...s, price: str(price), tip: str(tip), ...(sell ? { amountLeft: str(amount!) } : {}), ...fit, ...renewed };
  const order = { kind: st.kind, state: next } as OrderState;
  let value = big(snap.order.amount);
  let bidNext: { value: bigint; maxAmount: bigint } | undefined;
  if (!sell) {
    value += add;
    // the builder refuses a replacement that cannot fund one minimum fill (kob-protocol min_order_value = bidEscrow(minFill, 1))
    const minValue = bidEscrowOf(o, order, minFill, 1n);
    if (minValue === null || value < minValue) {
      const need = minValue ?? 0n;
      return fail([...issues, issue('bid-below-min-fill', 'error', 'the escrow does not fund one minimum fill at this price: add KAS', 'addKas', { missing: `${formatKas(need > value ? need - value : 0n)} KAS`, needed: `${formatKas(need)} KAS` })]);
    }
    bidNext = { value, maxAmount: buyingPowerOf(o, order, value) };
    issues.push(issue(add > 0n ? 'bid-budget-topped-up' : 'bid-budget-kept', 'info', add > 0n ? 'the KAS budget of the buy order grows by the amount added' : 'the KAS budget of the buy order stays the same', undefined, add > 0n ? { added: `${formatKas(add)} KAS` } : undefined));
  }
  return {
    ok: true,
    issues,
    spec: { order, value, ...(snap.deadline !== null ? { deadline: snap.deadline } : {}) },
    next: { price, stop: null, amount, tip, ...(bidNext ?? {}) },
  };
}

/** Builds the replacement of a conditional order (take-profit / stop / OCO). */
function condReplacement(snap: OrderSnapshot, input: AmendInput, o: AmendOptions): AmendResult {
  const st = snap.order.state;
  if (baseKind(st.kind) !== 'KobCondAsk' && baseKind(st.kind) !== 'KobCondBid') return fail([issue('not-amendable', 'error', 'this order type cannot be amended in place')]);
  const s = st.state as CondAskState | CondBidState;
  const sell = baseKind(st.kind) === 'KobCondAsk';
  const issues: PlanIssue[] = [];

  const hasTp = big(s.tpPrice) > 0n;
  const hasStop = big(s.stopPrice) > 0n;
  if (big(s.trailStep) > 0n) return fail([issue('trailing-not-amendable', 'error', 'a trailing stop cannot be amended: cancel it and place a new one')]);
  if (s.parent !== '0'.repeat(64)) return fail([issue('exit-not-amendable', 'error', 'an exit of a position cannot be amended on its own: cancel the position instead')]);
  if (input.price !== undefined && !hasTp) issues.push(issue('no-take-profit-leg', 'error', 'this order has no take-profit leg', 'price'));
  if (input.stop !== undefined && !hasStop) issues.push(issue('no-stop-leg', 'error', 'this order has no stop leg', 'stop'));

  const tpWant = input.price ?? big(s.tpPrice);
  const stopWant = input.stop ?? big(s.stopPrice);
  const tp = hasTp ? checkPrice('price', tpWant, o.tick, issues) : 0n;
  const stop = hasStop ? checkPrice('stop', stopWant, o.tick, issues) : 0n;
  if (input.stopLimit !== undefined && !hasStop) issues.push(issue('no-stop-leg', 'error', 'this order has no stop leg', 'stopLimit'));
  const tip = input.tip ?? big(s.tip);
  if (tip < 0n) issues.push(issue('tip-negative', 'error', 'the tip cannot be negative', 'tip'));
  const oldAmount = big(s.amountLeft);
  const amount = input.amount ?? oldAmount;
  if (amount < 1n) issues.push(issue('amount-not-positive', 'error', 'the amount must be at least one base unit', 'amount'));
  if (tp !== null && stop !== null && hasTp && hasStop && (sell ? tp <= stop : tp >= stop)) {
    issues.push(issue('legs-order', 'error', sell ? 'the take-profit must be above the stop' : 'the take-profit must be below the stop', 'price', { direction: sell ? 'above' : 'below' }));
  }
  const renewed = renewedExpiry(input, o, issues);
  let slip = big(s.slipBps);
  if (hasStop && stop !== null && input.stopLimit !== undefined) {
    // stop-limit: the band ends at the user's limit or inside it
    const lim = input.stopLimit;
    if (lim <= 0n) issues.push(issue('price-not-positive', 'error', 'the price must be greater than zero', 'stopLimit'));
    else if (sell ? lim > stopWant : lim < stopWant) {
      issues.push(issue('stop-limit-beyond-stop', 'error', sell ? 'the limit of a sell stop must be at or below the stop' : 'the limit of a buy stop must be at or above the stop', 'stopLimit', { direction: sell ? 'below' : 'above' }));
    } else slip = slipForLimit(sell ? 'sell' : 'buy', stop, lim);
  }
  if (hasStop && stop !== null && stopWorstPrice(sell ? 'sell' : 'buy', stop, slip) <= 0n) issues.push(issue('stop-worst-invalid', 'error', 'the stop is too low for its price band', 'stop'));
  if (issues.some((i) => i.severity === 'error') || tp === null || stop === null) return fail(issues);

  const side = sell ? 'sell' : 'buy';
  const minFill = big(s.minFill);
  const fit = minFill > amount ? { minFill: str(amount) } : {};
  const next = { ...s, tpPrice: str(tp), stopPrice: str(stop), slipBps: str(slip), tip: str(tip), amountLeft: str(amount), ...fit, ...renewed };
  const order = { kind: st.kind, state: next } as OrderState;
  let value = big(snap.order.amount);
  if (!sell) {
    // the escrow follows the worst price the buy can now pay and the new tip: the spend of the whole amount at the worst leg (rounded down per
    // fill, so it funds any split), the delivery carriers and the keeper reserve staying as they are
    const legsOf = (tpS: bigint, stopS: bigint, slipS: bigint): Legs => ({ tpPrice: tpS, stopPrice: stopS, stopWorst: stopS > 0n ? stopWorstPrice(side, stopS, slipS) : 0n }) as Legs;
    const spend = (state: CondBidState, legs: Legs): bigint | null => {
      const worst = bidWorst(legs);
      if (o.kob) {
        const e = o.kob.condBidEscrow({ kind: st.kind, state } as OrderState, 1);
        return e === null ? null : e - big(state.deliveryCarrier);
      }
      return quoteOf(big(state.amountLeft), worst + big(state.tip), big(state.scale), 'down');
    };
    const oldSpend = spend(s as CondBidState, legsOf(big(s.tpPrice), big(s.stopPrice), big(s.slipBps)));
    const newSpend = spend(next as CondBidState, legsOf(tp, stop, slip));
    if (oldSpend === null || newSpend === null) return fail([issue('escrow-invalid', 'error', 'the new terms do not fit the protocol\'s numbers')]);
    value = value + newSpend - oldSpend;
    if (value <= 0n) return fail([issue('escrow-invalid', 'error', 'the new terms leave no KAS for the order')]);
  }
  return {
    ok: true,
    issues,
    spec: { order, value, ...(snap.deadline !== null ? { deadline: snap.deadline } : {}) },
    next: { price: hasTp ? tp : null, stop: hasStop ? stop : null, amount, tip, ...(hasStop ? { stopWorst: stopWorstPrice(side, stop, slip) } : {}) },
  };
}

/** The replacement spec for an amendment, or the reasons it cannot be built. `kind` comes from `amendKind` (orders-model.ts). */
export function buildReplacement(snap: OrderSnapshot, kind: AmendKind, input: AmendInput, o: AmendOptions = {}): AmendResult {
  return kind === 'limit' ? limitReplacement(snap, input, o) : condReplacement(snap, input, o);
}
