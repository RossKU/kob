// planCond(env, intent): stop-market, stop-limit, trailing stop, take-profit, OCO (one covenant, both legs) and, via cond-ifd.ts, IFD /
// IFO / bracket and repeat IFD / IFO. Same contract as planSimple: never throws for user-input problems, they come back as `issues`.
//
// Mapping (docs/spec/order-types.md):
//   sell side -> KobCondAsk (custody + carrier)     buy side -> KobCondBid (KAS escrow at the worst leg + delivery carriers)
//   take-profit = leg 0 (`tpPrice`), stop = leg 1 (`stopPrice`, `slipBps`, `bandDaa`), trailing = `trailStep/Gap/Wait`, all `armed = 0`.
import { checkAmount, checkMinFill, checkNotional, checkTip, crossingTouch } from '../guards';
import type { CondIntent, OcoIntent, StopLimitIntent, StopMarketIntent, TakeProfitIntent, TrailingStopIntent } from '../intent-cond';
import { COND_TYPES, isIfdLike } from '../intent-cond';
import { kindFor } from '../order-facts';
import type { CarrierLine, PlanEnv } from '../plan-types';
import { ceilDiv, minBig } from '../units';
import { DEFAULT_BID_FILLS, carrierOf, defaultMinFillFor } from './common';
import { checkSelfTrade, failedPlan, finishPlan, resolveActivation, resolveExpiry, IssueLog, type CondPlan, type CondSummary } from './cond-common';
import { planIfd } from './cond-ifd';
import { askWorst, bidWorst, condAskState, condBidState, resolveLegs, type LegInput } from './cond-legs';

export { COND_TYPES };

/** Stop, take-profit and OCO intents (one covenant, no entry). */
export type LegsIntent = StopMarketIntent | StopLimitIntent | TrailingStopIntent | TakeProfitIntent | OcoIntent;

/** The optional stop-leg knobs, copied only when present (exactOptionalPropertyTypes-safe). */
function knobs(i: { slipBps?: number; bandDaa?: bigint; keeperTip?: bigint; minTouch?: bigint; minRestDaa?: bigint }): LegInput {
  const l: LegInput = {};
  if (i.slipBps !== undefined) l.slipBps = i.slipBps;
  if (i.bandDaa !== undefined) l.bandDaa = i.bandDaa;
  if (i.keeperTip !== undefined) l.keeperTip = i.keeperTip;
  if (i.minTouch !== undefined) l.minTouch = i.minTouch;
  if (i.minRestDaa !== undefined) l.minRestDaa = i.minRestDaa;
  return l;
}

export const legInputOf = (i: LegsIntent): LegInput => {
  switch (i.type) {
    case 'takeProfit':
      return { tp: i.price };
    case 'stopMarket':
      return { stop: i.stop, ...knobs(i) };
    case 'stopLimit':
      return { stop: i.stop, limit: i.limit, ...knobs(i) };
    case 'trailingStop': {
      const l: LegInput = { stop: i.stop, trail: i.trail, ...knobs(i) };
      if (i.takeProfit !== undefined) l.tp = i.takeProfit;
      return l;
    }
    case 'oco':
      return { stop: i.stop, tp: i.takeProfit, ...(i.limit !== undefined ? { limit: i.limit } : {}), ...knobs(i) };
  }
};

/** UI sentence tags of `Disclosure.notes` per type (the UI owns the wording; see the i18n keys `plan.note.<tag>`). */
export const NOTES_BY_TYPE: Record<LegsIntent['type'], string[]> = {
  stopMarket: ['stopTrigger', 'stopAuction', 'triggerExposure'],
  stopLimit: ['stopTrigger', 'stopAuction', 'triggerExposure', 'stopLimitMayNotFill'],
  trailingStop: ['stopTrigger', 'stopAuction', 'triggerExposure', 'trailing'],
  takeProfit: ['takeProfitLeg'],
  oco: ['oco', 'partialFillsKeepLegs', 'stopTrigger', 'stopAuction', 'triggerExposure'],
};

/**
 * The leg price the default minimum fill is worth 10 KAS at: the take-profit, else the stop trigger; with both legs the lower one for a sell
 * (the larger default) and the higher one for a buy.
 */
function minFillPrice(side: 'sell' | 'buy', l: LegInput): bigint {
  const cands = [l.tp, l.stop].filter((p): p is bigint => p !== undefined && p > 0n);
  if (cands.length === 0) return 0n;
  return cands.reduce((a, b) => (side === 'sell' ? (a < b ? a : b) : a > b ? a : b));
}

function planLegs(env: PlanEnv, log: IssueLog, intent: LegsIntent, tip: bigint, carrier: bigint): CondPlan {
  const tk = env.token;
  const sell = intent.side === 'sell';
  const amount = intent.amount;
  const input = legInputOf(intent);
  // the wallet default: the amount worth 10 KAS at the leg price (kob-wasm defaultMinFill)
  const minFill = intent.minFill ?? defaultMinFillFor(env, amount, minFillPrice(intent.side, input));
  log.add(...checkMinFill(minFill, amount));
  if (log.failed) return failedPlan(log);

  const legs = resolveLegs(env, log, intent.side, input, '', minFill);
  const act = resolveActivation(env, log, intent.activeFrom);
  const expiry = act === null ? null : resolveExpiry(env, log, intent.expiry, { field: 'expiry', activeFrom: act.activeFrom });
  if (legs === null || act === null || expiry === null) return failedPlan(log);

  const hasStop = legs.stopPrice > 0n;
  const hasTp = legs.tpPrice > 0n;
  const allIn = (price: bigint): bigint => (sell ? price - tip : price + tip);
  const worstState = sell ? askWorst(legs) : bidWorst(legs);

  // a sell must leave the maker something on its lowest leg; a buy has no such bound
  log.add(...checkTip(intent.side, tip, worstState));
  const topLeg = legs.tpPrice > legs.stopPrice ? legs.tpPrice : legs.stopPrice;
  log.add(...checkNotional(amount, sell ? topLeg : (legs.stopWorst > topLeg ? legs.stopWorst : topLeg) + tip, tk.scale));
  if (log.failed) return failedPlan(log);
  checkSelfTrade(env, log, intent.side, allIn(worstState), { field: 'amount' });

  // market checks that only warn: the order is still valid
  const bestAsk = env.book.asks[0]?.price;
  const bestBid = env.book.bids[0]?.price;
  if (hasStop) {
    const s = legs.stopPrice;
    if (sell && bestAsk !== undefined && bestAsk <= s) log.cond('COND_STOP_ALREADY_REACHED', { direction: 'at or below' }, 'stop');
    if (!sell && bestBid !== undefined && bestBid >= s) log.cond('COND_STOP_ALREADY_REACHED', { direction: 'at or above' }, 'stop');
  }
  if (hasTp) {
    const c = crossingTouch(env.book, intent.side, legs.tpPrice, tip);
    if (c.crossing && c.touch !== null) log.cond('COND_TP_CROSSES', { touch: c.touch }, 'takeProfit');
  }

  const keeper = legs.keeper;
  const base = { amount, minFill, tip, expiryDaa: expiry.expiryDaa, activeFrom: act.activeFrom };
  const lines: CarrierLine[] = [];
  let order: CondSpecOrder;
  if (sell) {
    order = { kind: kindFor('KobCondAsk', tk.family), state: condAskState(env, base, legs) } as CondSpecOrder;
    lines.push({ kind: 'orderCarrier', amount: carrier, count: 1 });
    // arm / trail updates are paid from the carrier, next to the refund tip
    if (keeper !== null && keeper.reserve + tk.refundTip > carrier) {
      log.cond('COND_KEEPER_FUNDING_TOO_LARGE', { reserve: keeper.reserve }, 'carrier');
      return failedPlan(log);
    }
  } else {
    // each partial fill pays one delivery carrier out of the escrow (KobCondBid.settle): budget them, at most one per possible fill
    const maxFills = ceilDiv(amount, minFill);
    const fills = intent.maxFills ?? minBig(DEFAULT_BID_FILLS, maxFills);
    if (fills < 1n || fills > maxFills) {
      log.shared('MAX_FILLS_INVALID', undefined, 'maxFills');
      return failedPlan(log);
    }
    order = { kind: kindFor('KobCondBid', tk.family), state: condBidState(env, { ...base, deliveryCarrier: carrier }, legs) } as CondSpecOrder;
    // the escrow of the whole amount at the worst leg plus one carrier per fill, exactly kob-wasm condBidEscrow (each fill's spend rounds down)
    const escrow = env.kob.condBidEscrow(order, fills);
    if (escrow === null) {
      log.shared('AMOUNT_TOO_LARGE', undefined, 'amount');
      return failedPlan(log);
    }
    lines.push({ kind: 'escrow', amount: escrow - carrier * fills, count: 1 });
    lines.push({ kind: 'deliveryCarrier', amount: carrier, count: Number(fills) });
    // an arming / trailing keeper takes its tip out of the escrow: fund it on top so the budget stays whole
    if (keeper !== null && keeper.reserve > 0n) lines.push({ kind: 'keeperReserve', amount: keeper.tip, count: keeper.expectedUpdates });
  }

  // disclosure numbers (sompi per whole token; the total by the covenant's quote rule, kob-wasm condAskProceeds / condBidSpend)
  const stopWorst = hasStop ? legs.stopWorst : null;
  const tp = hasTp ? legs.tpPrice : null;
  const limitPrice = hasTp ? tp! : stopWorst!;
  const expected = hasTp ? (hasStop ? null : tp) : legs.stopPrice;
  const worst = hasTp && hasStop ? (sell ? minBigOf(tp!, stopWorst!) : maxBigOf(tp!, stopWorst!)) : limitPrice;
  const allInLimit = sell ? limitPrice - tip : limitPrice + tip;
  const allInTotal = sell ? env.kob.condAskProceeds(order, amount, limitPrice) : env.kob.condBidSpend(order, amount, limitPrice);

  const notes = [...NOTES_BY_TYPE[intent.type]];
  if (keeper !== null) notes.push('keeperReserve');
  const cond: CondSummary = { type: intent.type, legs: legs.summary, trail: legs.trail, keeper, entry: null, exit: null, repeat: null };
  return finishPlan(env, log, {
    order,
    exit: null,
    lines,
    carrier,
    side: intent.side,
    amount,
    expiry,
    activatesAt: act.activatesAt,
    disclosure: {
      limitPrice, allInPrice: allInLimit, allInTotal, expectedPrice: expected, worstPrice: worst, tip, minTouch: hasStop ? legs.minTouch : null,
    },
    notes,
    cond,
  });
}

type CondSpecOrder = Parameters<typeof finishPlan>[2]['order'];
const minBigOf = (a: bigint, b: bigint): bigint => (a < b ? a : b);
const maxBigOf = (a: bigint, b: bigint): bigint => (a > b ? a : b);

/** Plans any conditional / if-done intent. */
export function planCond(env: PlanEnv, intent: CondIntent): CondPlan {
  const log = new IssueLog();
  const raw = intent as { type?: unknown; side?: unknown; amount?: unknown };
  if (typeof raw.type !== 'string' || !COND_TYPES.includes(raw.type)) {
    log.shared('INTENT_UNKNOWN_TYPE', { type: String(raw.type) });
    return failedPlan(log);
  }
  if (raw.side !== 'sell' && raw.side !== 'buy') {
    log.shared('SIDE_INVALID', { side: String(raw.side) }, 'side');
    return failedPlan(log);
  }
  if (typeof raw.amount !== 'bigint') {
    log.shared('AMOUNT_NOT_POSITIVE', undefined, 'amount');
    return failedPlan(log);
  }
  log.add(...checkAmount(raw.amount, env.token, 'amount', raw.side === 'sell' || isIfdLike(intent)));
  const tip = intent.tip ?? 0n;
  if (tip < 0n) log.shared('TIP_NEGATIVE', undefined, 'tip');
  const carrier = intent.carrier ?? carrierOf(env);
  if (carrier <= 0n) log.cond('COND_CARRIER_INVALID', undefined, 'carrier');
  if (log.failed) return failedPlan(log);
  return isIfdLike(intent) ? planIfd(env, log, intent, tip, carrier) : planLegs(env, log, intent, tip, carrier);
}
