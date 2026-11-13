// Planner of the pair conditionals (`KobCondPair`, contracts/orders/KobCondPair.sil).
//
// It covers stop-market, stop-limit, trailing stop (optionally with a take-profit), take-profit
// and OCO on a pair A/B, from the same intents as planCond (intent-cond.ts). The if-done types are
// dispatched to pair-ifd.ts.
//
// Mapping:
// - A sell is side ASK. It holds exactly the amount of A, with the stop below the market and the
//   take-profit above.
// - A buy is side BID. It holds a B escrow for the whole amount at its worst leg, rounded down,
//   plus one base unit (kob-wasm `condPairBidEscrow`), with the stop above and the limit below.
//
// Leg prices (`stop`, `limit`, `takeProfit`, `trail.step`, `trail.gap`) are B base units per whole
// A. `tip` and `keeperTip` are KAS. The stop arms on pair trigger evidence (kob-wasm
// `pairTriggerRule`): two KAS-book fills implying the rate, or a fill of a resting KobPair of the
// pair. The presets are those of the KAS kinds: slipBps 300, bandDaa 300, minRestDaa 50 and
// trailWait 6000. The default minTouch is the larger of the order's minimum fill and a quarter of
// its amount.
//
// The order UTXO holds:
// - One delivery carrier per budgeted fill (default one per possible fill, at most 64, or
//   `maxFills`).
// - The tip of the whole amount (kob-wasm `pairKasValue`).
// - The keeper reserve, which is keeperTip times the expected arm and trail updates. The keeper
//   takes it from the order UTXO.
import type { CondIntent } from '../intent-cond';
import { COND_TYPES, isIfdLike } from '../intent-cond';
import type { CarrierLine, PairOrderPlan, PairPlanEnv } from '../plan-types';
import { checkAmount, checkMinFill, crossingTouch } from '../guards';
import { ceilDiv, maxBig } from '../units';
import { issue } from './common-issues';
import { IssueLog, checkSelfTrade, resolveActivation, resolveExpiry, type CondSummary } from './cond-common';
import { NOTES_BY_TYPE, legInputOf, type LegsIntent } from './cond';
import { askWorst, bidWorst, resolveLegs } from './cond-legs';
import {
  checkPairNotional, defaultPairFills, defaultPairTip, checkPairTip, failedPairPlan, legEnvOf, makeCondPairState, pairCarrierOf, pairKasLines, pairTipsOf, placePair,
  withCustody,
} from './pair-common';
import { planPairIfd } from './pair-ifd';
import { pairIssue } from './pair-issues';

const failed = (log: IssueLog, cond: CondSummary | null = null): PairOrderPlan => failedPairPlan(log.issues, [], cond);

/** Plans a stop, take-profit, trailing or OCO order on a pair. */
function planPairLegs(env: PairPlanEnv, log: IssueLog, intent: LegsIntent, tip: bigint, carrier: bigint): PairOrderPlan {
  const a = env.token;
  const b = env.pair.quote;
  const lenv = legEnvOf(env);
  const sell = intent.side === 'sell';
  const amount = intent.amount;
  const input = legInputOf(intent);
  // A resting pair order uses the amount of A worth 10 KAS on its KAS book (defaultMinFillPair).
  const minFill = intent.minFill ?? env.kob.defaultMinFillPair(amount, env.pair.kasPerWholeA, a.scale);
  if (intent.minFill === undefined && env.pair.kasPerWholeA === null) log.add(pairIssue('PAIR_KAS_REFERENCE_MISSING', { ticker: a.ticker }));
  log.add(...checkMinFill(minFill, amount));
  log.add(...checkPairTip(env, amount, tip));
  if (log.failed) return failed(log);

  const legs = resolveLegs(lenv, log, intent.side, input, '', minFill, amount);
  const act = resolveActivation(lenv, log, intent.activeFrom);
  const expiry = act === null ? null : resolveExpiry(lenv, log, intent.expiry, { field: 'expiry', activeFrom: act.activeFrom });
  if (legs === null || act === null || expiry === null) return failed(log);
  const hasStop = legs.stopPrice > 0n;
  const hasTp = legs.tpPrice > 0n;
  const top = [legs.tpPrice, legs.stopPrice, legs.stopWorst].reduce((x, y) => (x > y ? x : y));
  log.add(...checkPairNotional(env, amount, top));
  if (log.failed) return failed(log);
  // Self-trade is checked on the B prices. A KAS tip never makes a B price cross.
  checkSelfTrade(env, log, intent.side, sell ? askWorst(legs) : bidWorst(legs), { field: 'amount' });

  // Market checks only warn.
  const bestAsk = env.book.asks[0]?.price;
  const bestBid = env.book.bids[0]?.price;
  if (hasStop) {
    if (sell && bestAsk !== undefined && bestAsk <= legs.stopPrice) log.cond('COND_STOP_ALREADY_REACHED', { direction: 'at or below' }, 'stop');
    if (!sell && bestBid !== undefined && bestBid >= legs.stopPrice) log.cond('COND_STOP_ALREADY_REACHED', { direction: 'at or above' }, 'stop');
  }
  if (hasTp) {
    const c = crossingTouch(env.book, intent.side, legs.tpPrice, 0n);
    if (c.crossing && c.touch !== null) log.add(pairIssue('PAIR_TP_CROSSES', { touch: c.touch, ticker: b.ticker }, 'takeProfit'));
  }
  // KAS delivery carriers: one per budgeted fill, at most one per possible fill.
  const possible = ceilDiv(amount, minFill);
  const fills = intent.maxFills ?? defaultPairFills(possible);
  if (fills < 1n || fills > possible) {
    log.shared('MAX_FILLS_INVALID', undefined, 'maxFills');
    return failed(log);
  }
  // With fewer carriers than possible fills, the rest fills only in full after fills - 1 partials.
  if (fills < possible) log.add(pairIssue('PAIR_FILLS_LIMITED', { fills, partials: fills - 1n }));
  if (log.failed) return failed(log);

  const tips = pairTipsOf(env);
  const draft = makeCondPairState(env, {
    side: intent.side, amount, minFill, tip, activeFrom: act.activeFrom, expiryDaa: expiry.expiryDaa, refundTip: tips.refundTip, deliveryCarrier: carrier, custody: amount,
  }, legs);
  let order = draft;
  if (!sell) {
    // The B escrow is the whole amount at the worst leg rounded down plus one base unit (kob-wasm
    // condPairBidEscrow). Exact floors are subadditive.
    const escrow = env.kob.condPairBidEscrow(draft, env.kob.pairMaxFills(draft));
    if (escrow === null) {
      log.add(pairIssue('PAIR_NOTIONAL_TOO_LARGE', { ticker: b.ticker }, 'stop'));
      return failed(log);
    }
    order = withCustody(draft, escrow);
  }
  // At least the deliveries the protocol requires a new order to fund itself (kob-wasm
  // pairFundedFills): for a resting order a partial fill and the fill of its rest.
  const funded = maxBig(fills, env.kob.pairFundedFills(order));
  const kas = pairKasLines(env, order, funded, carrier);
  if (kas === null) {
    log.shared('TIP_TOO_LARGE', undefined, 'tip');
    return failed(log);
  }
  const lines: CarrierLine[] = [...kas.lines];
  const keeper = legs.keeper === null ? null : { ...legs.keeper, fundedFrom: 'escrow' as const };
  // An arming or trailing keeper takes its tip from the order UTXO, so fund it on top.
  if (keeper !== null && keeper.reserve > 0n) lines.push({ kind: 'keeperReserve', amount: keeper.tip, count: keeper.expectedUpdates });

  // Disclosure prices (B per whole A) and amounts. A sell receives at least the ceil and a buy pays
  // at most the floor (kob-wasm).
  const bounds = env.kob.condPairBounds(order);
  const stopWorst = hasStop ? bounds.stopWorst : null;
  const tp = hasTp ? legs.tpPrice : null;
  const limitPrice = (hasTp ? tp : stopWorst) as bigint;
  const expected = hasTp ? (hasStop ? null : tp) : legs.stopPrice;
  const worst = sell ? bounds.lowest : bounds.worst;
  const bOf = (n: bigint, p: bigint | null): bigint | null => (p === null ? null : sell ? env.kob.pairTOutMin(order, n, p) : env.kob.pairSOut(order, n, p));
  const n1 = minFill < amount ? minFill : amount;
  const notes = [...NOTES_BY_TYPE[intent.type]];
  if (keeper !== null) notes.push('keeperReserve');
  const cond: CondSummary = { type: intent.type, legs: legs.summary, trail: legs.trail, keeper, entry: null, exit: null, repeat: null };
  return placePair(env, log.issues, {
    order,
    lines,
    deadline: expiry.deadline,
    side: intent.side,
    amount,
    expiry: expiry.disclosure,
    activatesAt: act.activatesAt,
    limitPrice,
    expectedPrice: expected,
    worstPrice: worst,
    tip,
    minTouch: hasStop ? legs.minTouch : null,
    keeperTip: legs.keeperTip,
    allInTotal: bOf(amount, limitPrice),
    receiveMinB: sell ? bOf(amount, worst) : null,
    payMaxB: sell ? null : bOf(amount, worst),
    expectedB: bOf(amount, expected),
    minFillB: bOf(n1, limitPrice),
    tipKasTotal: kas.tipKasTotal,
    deliveries: funded,
    deliveryCarrier: carrier,
    exitCarrier: null,
    delivery: sell ? { market: b, amount: env.kob.pairTOutMin(order, n1, legs.tpPrice > legs.stopPrice ? legs.tpPrice : legs.stopPrice) ?? 0n } : { market: a, amount: n1 },
    exitFacts: null,
    repeat: null,
    notes,
    pairNotes: hasStop ? ['pairTrigger'] : [],
    cond,
    carrier,
  });
}

/**
 * Plans any conditional or if-done intent on a pair (`KobCondPair`, `KobIfdPair`).
 *
 * It never throws for user-input problems. The plan carries the `cond` summary of the KAS planners
 * in the same shape, with prices in B per whole A, amounts in base units of A and KAS in sompi.
 */
export function planPairCond(env: PairPlanEnv, intent: CondIntent): PairOrderPlan {
  const log = new IssueLog();
  const raw = intent as { type?: unknown; side?: unknown; amount?: unknown };
  if (typeof raw.type !== 'string' || !COND_TYPES.includes(raw.type)) {
    log.shared('INTENT_UNKNOWN_TYPE', { type: String(raw.type) });
    return failed(log);
  }
  if (raw.side !== 'sell' && raw.side !== 'buy') {
    log.shared('SIDE_INVALID', { side: String(raw.side) }, 'side');
    return failed(log);
  }
  if (typeof raw.amount !== 'bigint') {
    log.shared('AMOUNT_NOT_POSITIVE', undefined, 'amount');
    return failed(log);
  }
  // The A amount is the custody of a sell, the delivery of a buy and every exit's custody of an
  // if-done order (KRON: at most 1e9).
  log.add(...checkAmount(raw.amount, env.token, 'amount', raw.side === 'sell' || isIfdLike(intent)));
  const tip = intent.tip ?? defaultPairTip(env, raw.amount);
  if (tip < 0n) log.add(issue('TIP_NEGATIVE', undefined, 'tip'));
  const carrier = pairCarrierOf(env, intent.carrier);
  if (carrier <= 0n) log.cond('COND_CARRIER_INVALID', undefined, 'carrier');
  if (log.failed) return failed(log);
  return isIfdLike(intent) ? planPairIfd(env, log, intent, tip, carrier) : planPairLegs(env, log, intent, tip, carrier);
}
