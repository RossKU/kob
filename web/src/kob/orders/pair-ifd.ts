// IFD / IFO / bracket and repeat IFD / IFO on a pair A/B (`KobIfdPair`, contracts/v2/KobIfdPair.sil; docs/spec/order-types.md "Pair if-done
// entries"), from the same intents as cond-ifd.ts.
//
//   buy-first (side BID):  holds a B escrow (the spend of its whole amount at its limit, kob-wasm `ifdPairBCustodyNeeded`) and buys A; every
//                          fill of n creates a fresh `KobCondPair` ASK exit holding exactly those n of A
//   sell-first (side ASK): holds the A amount and a B PREFUND (`prefund` B per whole A over the whole amount, plus one base unit per possible fill
//                          but the last) and sells A; every fill creates a `KobCondPair` BID exit holding the B proceeds plus the prefund of n
//
// The exit is COMMITTED in the entry's `exitState` (kob-wasm `ifdPairCommitExit`: the KobCondPair state up to `armed`, 432 bytes); the entry
// writes the exit's amount, custody and repeat fields. KAS: the entry UTXO holds per possible fill a delivery carrier (on the exit's custody) and
// an exit carrier (on the exit UTXO), the tip of the whole amount, a repeating entry one more exit carrier (kob-wasm `pairKasValue`), and the
// stop entry's keeper reserve. Each exit carrier funds its exit: `DEFAULT_EXIT_FILLS` delivery carriers, the exit's KAS tip of the whole amount
// and the exit's keeper reserve. Prices are B per whole A; tips (the repeat's merge cost included) KAS per whole A.
import { MAX_IDLE_DAA, MIN_REST_DAA, STOP_BAND_DAA } from '../daa';
import type { IfdLikeIntent } from '../intent-cond';
import { isRepeat } from '../intent-cond';
import type { CarrierLine, PairOrderPlan, PairPlanEnv } from '../plan-types';
import type { OrderState } from '../types';
import { I64_MAX, ceilDiv, minBig } from '../units';
import { checkMinFill, touchPrice } from '../guards';
import {
  UNLIMITED_REPEAT_COUNT, checkSelfTrade, mergeTipRate, resolveActivation, resolveExpiry, type CondSummary, type ExitSummary, type IssueLog,
  type KeeperSummary, type RepeatSummary,
} from './cond-common';
import { checkExitShape, exitLegInput, resolveEntry } from './cond-ifd';
import { askWorst, bidWorst, resolveLegs } from './cond-legs';
import {
  DEFAULT_EXIT_FILLS, checkPairNotional, checkPairTip, failedPairPlan, legEnvOf, makeCondPairState, makeIfdPairState, pairTipsOf, placePair, withCustody,
} from './pair-common';
import { pairIssue } from './pair-issues';

/** stop entries re-arm unarmed each cycle: pre-fund at most this many arming updates of a repeating stop entry (as the KAS kinds) */
const MAX_ENTRY_ARM_UPDATES = 10n;
/**
 * The KAS one repeat merge adds to the exit's take-profit fill, measured (docs/spec/matcher.md 6.1 rule 7): a buy-first merge +11,862 B / 0.0237 KAS over
 * a plain take-profit, a sell-first merge into new custodies +8,428 B / 0.0169 KAS. `mergeTipRate` spreads it over the smallest take-profit fill.
 */
export const PAIR_MERGE_COST_BUY_FIRST = 2_370_000n;
export const PAIR_MERGE_COST_SELL_FIRST = 1_690_000n;

export function planPairIfd(env: PairPlanEnv, log: IssueLog, intent: IfdLikeIntent, tip: bigint, carrier: bigint): PairOrderPlan {
  const a = env.token;
  const b = env.pair.quote;
  const lenv = legEnvOf(env);
  const tips = pairTipsOf(env);
  const buyFirst = intent.side === 'buy';
  const exitSide = buyFirst ? 'sell' : 'buy';
  const repeat = isRepeat(intent);
  const amount = intent.amount;
  const fail = (cond: CondSummary | null = null): PairOrderPlan => failedPairPlan(log.issues, [], cond);

  if (!checkExitShape(log, intent)) return fail();
  const entry = resolveEntry(lenv, log, intent.side, intent.entry);
  if (entry === null) return fail();
  log.add(...checkPairTip(env, amount, tip));

  // the entry's minimum fill (matcher.md 10.8: default ceil(amount / 4), kob-wasm defaultMinFillIfd)
  const minFill = intent.minFill ?? env.kob.defaultMinFillIfd(amount);
  if (minFill < 1n || minFill > amount) {
    log.cond('COND_MIN_FILL_INVALID', { amount }, 'minFill');
    return fail();
  }
  const fills = ceilDiv(amount, minFill);

  // repeat: rptAmount = 1 + K * N (N < 2^53), at most an i64
  let count: bigint | null = null;
  let kk = 0n;
  let rptAmount = 0n;
  if (repeat) {
    const k = intent.repeat?.count;
    if (k !== undefined && k < 1n) {
      log.cond('COND_REPEAT_COUNT_INVALID', undefined, 'repeat.count');
      return fail();
    }
    if (amount >= 1n << 53n) {
      log.shared('AMOUNT_TOO_LARGE', undefined, 'amount');
      return fail();
    }
    count = k ?? null;
    const kMax = (I64_MAX - 1n) / amount;
    kk = k ?? (UNLIMITED_REPEAT_COUNT < kMax ? UNLIMITED_REPEAT_COUNT : kMax);
    if (kk > kMax) {
      log.cond('COND_REPEAT_COUNT_INVALID', undefined, 'repeat.count');
      return fail();
    }
    rptAmount = 1n + kk * amount;
  }

  // ---- exit (built first: the entry commits to it, its minimum fill included)
  const exitIn = intent.exit;
  const exitTipBase = exitIn.tip ?? 0n;
  if (exitTipBase < 0n) {
    log.shared('TIP_NEGATIVE', undefined, 'exit.tip');
    return fail();
  }
  const exitInput = exitLegInput(exitIn);
  // every exit holds one entry fill: its own default minimum fill is the pair default, at most the entry's minimum fill
  const exitMinDefault = env.kob.defaultMinFillPair(amount, env.pair.kasPerWholeA, a.scale);
  const exitMinFill = exitIn.minFill ?? (exitMinDefault < minFill ? exitMinDefault : minFill);
  log.add(...checkMinFill(exitMinFill, amount, 'exit.minFill'));
  if (log.failed) return fail();
  // a repeat's exit tip (KAS) carries the merge cost, paid even by the smallest take-profit fill
  const mergeTip = repeat ? mergeTipRate(buyFirst ? PAIR_MERGE_COST_BUY_FIRST : PAIR_MERGE_COST_SELL_FIRST, a.scale, exitMinFill) : 0n;
  const exitTip = exitTipBase + mergeTip;
  log.add(...checkPairTip(env, amount, exitTip, 'exit.tip'));
  // the exit's trigger threshold scales with what it will sell or buy: at most the entry's amount, known now (its fills are not)
  const legs = resolveLegs(lenv, log, exitSide, exitInput, 'exit.', exitMinFill, amount);
  const act = resolveActivation(lenv, log, intent.activeFrom);
  const entryExpiry = act === null ? null : resolveExpiry(lenv, log, intent.expiry, { field: 'expiry', activeFrom: act.activeFrom });
  const exitExpiry = resolveExpiry(lenv, log, exitIn.expiry, { field: 'exit.expiry', exit: true });
  if (legs === null || act === null || entryExpiry === null || exitExpiry === null || log.failed) return fail();

  // ---- economics in B (the tips are KAS, outside the rates): the take-profit must beat the entry price (a repeat: an error)
  let profitPerToken = 0n;
  if (legs.tpPrice > 0n) {
    profitPerToken = buyFirst ? legs.tpPrice - entry.price : entry.price - legs.tpPrice;
    if (profitPerToken <= 0n) {
      log.add(pairIssue('PAIR_TP_NOT_PROFITABLE', { profitPerToken, ticker: b.ticker }, 'exit.takeProfit', repeat ? 'error' : 'warning'));
      if (repeat) return fail();
    }
  }
  const exitWorst = buyFirst ? askWorst(legs) : bidWorst(legs);
  // sell-first: the B prefund (per whole A) beyond the entry price that covers the exit's worst buy-back
  let prefund = 0n;
  if (!buyFirst) {
    const minPrefund = exitWorst > entry.price ? exitWorst - entry.price : 0n;
    prefund = intent.prefund ?? minPrefund;
    if (prefund < 0n) {
      log.cond('COND_PREFUND_INVALID', undefined, 'prefund');
      return fail();
    }
    if (prefund < minPrefund) {
      log.add(pairIssue('PAIR_PREFUND_SHORT', { needed: minPrefund, given: prefund, ticker: b.ticker }, 'prefund'));
      return fail();
    }
  }
  // the 2^62 bound of every rate the entry and its exit carry (the builders refuse beyond it)
  const entryTop = entry.price > entry.stop ? entry.price : entry.stop;
  log.add(...checkPairNotional(env, amount, buyFirst ? entryTop : entryTop + prefund, 'entry.price'));
  const exitTop = [legs.tpPrice, legs.stopPrice, legs.stopWorst].reduce((x, y) => (x > y ? x : y));
  log.add(...checkPairNotional(env, amount, exitTop, 'exit.takeProfit'));
  if (log.failed) return fail();

  // ---- self-trade: the entry is a real order now; the exit only later (warning)
  checkSelfTrade(env, log, intent.side, entry.price, { field: 'entry.price' });
  checkSelfTrade(env, log, exitSide, exitWorst, { exit: true, field: 'exit' });
  const touch = touchPrice(env.book, intent.side);
  if (entry.stop === 0n && touch !== null && (buyFirst ? touch <= entry.price : touch >= entry.price)) log.cond('COND_ENTRY_CROSSES', undefined, 'entry.price');
  if (log.failed) return fail();

  // ---- entry stop parameters (as the KAS kinds; the keeper tip defaults to the pair's)
  const stopEntry = entry.stop > 0n;
  const entryBand = stopEntry && entry.stop !== entry.price ? intent.entry.bandDaa ?? STOP_BAND_DAA : 0n;
  const entryKeeperTip = stopEntry ? intent.entry.keeperTip ?? tips.keeperTip : 0n;
  const entryTouch = stopEntry ? (intent.entry.minTouch ?? env.kob.defaultMinTouch(minFill, amount)) : 0n;
  const entryRestDaa = intent.entry.minRestDaa ?? MIN_REST_DAA;
  if (entryBand < 0n) log.cond('COND_BAND_INVALID', undefined, 'entry.bandDaa');
  if (entryKeeperTip < 0n) log.cond('COND_KEEPER_TIP_INVALID', undefined, 'entry.keeperTip');
  if (stopEntry && entryTouch < 1n) log.cond('COND_MIN_TOUCH_INVALID', undefined, 'entry.minTouch');
  if (entryRestDaa < 0n) log.cond('COND_MIN_REST_INVALID', undefined, 'entry.minRestDaa');
  if (log.failed) return fail();
  const armUpdates = !stopEntry ? 0n : repeat ? (count === null || count + 1n > MAX_ENTRY_ARM_UPDATES ? MAX_ENTRY_ARM_UPDATES : count + 1n) : 1n;
  const keeperReserve = entryKeeperTip * armUpdates;

  // ---- the exit, committed: its custody and amount are written by the entry per fill
  const exitDraft = makeCondPairState(env, {
    side: exitSide, amount, minFill: exitMinFill, tip: exitTip, activeFrom: 0n, expiryDaa: exitExpiry.expiryDaa, refundTip: tips.refundTip, deliveryCarrier: carrier,
    custody: 0n,
  }, legs);
  const exitTipKas = env.kob.pairTipKas(exitDraft, amount);
  if (exitTipKas === null) {
    log.shared('TIP_TOO_LARGE', undefined, 'exit.tip');
    return fail();
  }
  // the exit UTXO funds its own fills (a delivery carrier each, up to DEFAULT_EXIT_FILLS), its KAS tip and its keeper's updates
  const exitFills = minBig(DEFAULT_EXIT_FILLS, ceilDiv(amount, exitMinFill));
  const exitKeeperReserve = legs.keeper?.reserve ?? 0n;
  const exitPlanned = carrier * exitFills + exitTipKas + exitKeeperReserve;
  const exitState = env.kob.ifdPairCommitExit(exitDraft);

  const draftWith = (exitCarrier: bigint) =>
    makeIfdPairState(env, {
      side: intent.side, amount, price: entry.price, prefund, tip, activeFrom: act.activeFrom, expiryDaa: entryExpiry.expiryDaa, refundTip: tips.refundTip,
      deliveryCarrier: carrier, exitCarrier, minFill, entryStop: entry.stop, bandDaa: entryBand, minTouch: entryTouch, minRestDaa: entryRestDaa,
      keeperTip: entryKeeperTip, custody: 0n, rptAmount, exitState,
    });
  // never below what the protocol requires of every exit (kob-wasm ifdPairExitCarrierNeeded: the keeper reserve of an exit stop, then the exit's
  // own deliveries and the tip of the whole amount, or its refund tip)
  const needed = env.kob.ifdPairExitCarrierNeeded(draftWith(exitPlanned));
  const exitCarrier = needed !== null && needed > exitPlanned ? needed : exitPlanned;
  const draft = draftWith(exitCarrier);
  // the B custody the entry needs (buy-first: its escrow; sell-first: its prefund), exactly kob-wasm ifdPairBCustodyNeeded
  const bCustody = env.kob.ifdPairBCustodyNeeded(draft);
  if (bCustody === null) {
    log.add(pairIssue('PAIR_NOTIONAL_TOO_LARGE', { ticker: b.ticker }, buyFirst ? 'entry.price' : 'prefund'));
    return fail();
  }
  const order = withCustody(draft, bCustody);
  const kasValue = env.kob.pairKasValue(order, 0n);
  const tipKasTotal = env.kob.pairTipKas(order, amount);
  if (kasValue === null || tipKasTotal === null) {
    log.shared('TIP_TOO_LARGE', undefined, 'tip');
    return fail();
  }
  const exitCarriers = fills + (repeat ? 1n : 0n);
  const lines: CarrierLine[] = [
    { kind: 'deliveryCarrier', amount: carrier, count: Number(fills) },
    { kind: 'exitCarrier', amount: exitCarrier, count: Number(exitCarriers) },
  ];
  if (tipKasTotal > 0n) lines.push({ kind: 'tipPrefund', amount: tipKasTotal, count: 1 });
  if (keeperReserve > 0n) lines.push({ kind: 'keeperReserve', amount: entryKeeperTip, count: Number(armUpdates) });
  if (carrier * fills + exitCarrier * exitCarriers + tipKasTotal !== kasValue) {
    log.shared('PLAN_MISMATCH', { reason: 'the entry carriers differ from kob-wasm pairKasValue' });
    return fail();
  }

  // the exit a fill of the whole amount creates (listed in `states`): buy-first n of A; sell-first the proceeds plus the prefund of n, in B
  const full = env.kob.ifdPairAmounts(order, amount, entry.price);
  const exitCustody = buyFirst ? amount : full.proceeds !== null && full.pre !== null ? full.proceeds + full.pre : null;
  if (exitCustody === null) {
    log.add(pairIssue('PAIR_NOTIONAL_TOO_LARGE', { ticker: b.ticker }, 'entry.price'));
    return fail();
  }
  const exitOrder: OrderState = env.kob.ifdPairExitFor(order, amount, exitCustody);
  const n1 = minFill < amount ? minFill : amount;
  const one = env.kob.ifdPairAmounts(order, n1, entry.price);

  // ---- summaries (prices B per whole A; the KAS tip is outside them, so the all-in prices are the leg prices)
  const keeper: KeeperSummary | null = stopEntry ? { tip: entryKeeperTip, expectedUpdates: Number(armUpdates), reserve: keeperReserve, fundedFrom: 'escrow' } : null;
  const exitSummary: ExitSummary = {
    side: exitSide,
    legs: legs.summary,
    tip: exitTip,
    minFill: exitMinFill,
    takeProfitAllIn: legs.tpPrice > 0n ? legs.tpPrice : null,
    stopWorstAllIn: legs.stopPrice > 0n ? legs.stopWorst : null,
    expiry: exitExpiry.disclosure.kind === 'gtc' ? 'gtc' : 'gtd',
    expiryDaa: exitExpiry.disclosure.kind === 'gtc' ? null : exitExpiry.disclosure.daa,
    expiryUnixSeconds: exitExpiry.disclosure.kind === 'gtc' ? null : exitExpiry.disclosure.approxUnixSeconds,
    deliveryCarrier: carrier,
    exitCarrier,
    trail: legs.trail,
    keeper: legs.keeper === null ? null : { ...legs.keeper, fundedFrom: 'carrier' },
    prefund: buyFirst ? null : prefund,
  };
  const untilDaa = entryExpiry.expiryDaa < env.clock.daa + MAX_IDLE_DAA ? entryExpiry.expiryDaa : env.clock.daa + MAX_IDLE_DAA;
  const repeatSummary: RepeatSummary | null = repeat ? { count, rptAmount, cycleAmount: amount, mergeTip, profitPerToken, untilDaa } : null;
  const cond: CondSummary = {
    type: intent.type,
    legs: null,
    trail: null,
    keeper,
    entry: { price: entry.price, stop: stopEntry ? entry.stop : null, bandDaa: entryBand, minFill, maxFills: fills, minTouch: entryTouch, minRestDaa: entryRestDaa },
    exit: exitSummary,
    repeat: repeatSummary,
  };

  const notes: string[] = [intent.type === 'ifd' || intent.type === 'repeatIfd' ? 'ifd' : 'ifo', 'position'];
  notes.push(buyFirst ? 'buyFirst' : 'sellFirst', 'minFill', exitExpiry.disclosure.kind === 'gtc' ? 'exitGtc' : 'exitGtd');
  if (stopEntry) notes.push('stopEntry', 'stopEntryTrigger', 'stopEntryAuction', 'triggerExposure', 'keeperReserve');
  if (legs.stopPrice > 0n) notes.push('exitStop', 'triggerExposure');
  if (legs.stopPrice > 0n && intent.exit.stopLimit !== undefined) notes.push('stopLimitMayNotFill');
  if (legs.trail !== null) notes.push('trailing');
  if (repeat) notes.push('repeat', count === null ? 'repeatUnlimited' : 'repeatCounted', buyFirst ? 'repeatReBuys' : 'repeatReSells', 'repeatStopLossEnds', 'mergeTip', 'cancelPosition');
  if (!buyFirst) notes.push('prefund');
  const pairNotes = ['pairExitCustody'];
  if (stopEntry || legs.stopPrice > 0n) pairNotes.push('pairTrigger');

  const sell = !buyFirst;
  const exitKind = legs.tpPrice > 0n && legs.stopPrice > 0n ? 'oco' : legs.tpPrice > 0n ? 'takeProfit' : 'stop';
  return placePair(env, log.issues, {
    order,
    exit: exitOrder,
    lines,
    deadline: entryExpiry.deadline,
    side: intent.side,
    amount,
    expiry: entryExpiry.disclosure,
    activatesAt: act.activatesAt,
    limitPrice: entry.price,
    expectedPrice: stopEntry ? entry.stop : null,
    worstPrice: entry.price,
    tip,
    minTouch: stopEntry ? entryTouch : null,
    keeperTip: entryKeeperTip,
    allInTotal: sell ? full.proceeds : full.spend,
    receiveMinB: sell ? full.proceeds : null,
    payMaxB: sell ? null : full.spend,
    expectedB: stopEntry ? (sell ? env.kob.ifdPairAmounts(order, amount, entry.stop).proceeds : env.kob.ifdPairAmounts(order, amount, entry.stop).spend) : null,
    minFillB: sell ? one.proceeds : one.spend,
    tipKasTotal,
    deliveries: fills,
    deliveryCarrier: carrier,
    exitCarrier,
    // what one minimum fill puts into its exit's custody: buy-first n of A, sell-first its proceeds plus its prefund in B
    delivery: buyFirst ? { market: a, amount: n1 } : { market: b, amount: (one.proceeds ?? 0n) + (one.pre ?? 0n) },
    exitFacts: { kind: exitKind, takeProfit: legs.tpPrice > 0n ? legs.tpPrice : null, stop: legs.stopPrice > 0n ? legs.stopPrice : null, custodyPerFill: buyFirst ? 'baseAmount' : 'proceedsPlusPrefund' },
    repeat: repeat ? { count: kk, levels: 1n, rptAmount, unlimited: count === null } : null,
    notes: [...new Set(notes)],
    pairNotes,
    cond,
    carrier,
  });
}
