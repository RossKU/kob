// Planner of IFD, IFO, bracket and the repeat forms (matcher.md 6, 6.1, 10.8 and 10.13,
// order-types.md "Repeat IFD in detail").
//
// - Buy-first is a KobIfdBid (escrow is budget plus carriers). Every fill of n base units creates a
//   KobCondAsk exit for exactly n.
// - Sell-first is a KobIfdAsk (custody, prefund and carriers). Every fill of n base units creates a
//   KobCondBid exit for exactly n.
//
// The exit is committed in the entry's `exitState`, the first 279 or 321 bytes of the exit state
// including its `minFill`. The entry itself writes the exit's `amountLeft` and repeat fields,
// so this planner commits the plain exit with `amountLeft = amount`.
import { MAX_IDLE_DAA, MIN_REST_DAA, STOP_BAND_DAA } from '../daa';
import type { EntrySpec, IfdLikeIntent } from '../intent-cond';
import { isRepeat } from '../intent-cond';
import { kindFor } from '../order-facts';
import type { CarrierLine, PlanEnv } from '../plan-types';
import type { CondAskState, CondBidState, Hex, IfdAskState, IfdBidState, OrderState } from '../types';
import { I64_MAX, ceilDiv } from '../units';
import { checkMinFill, checkNotional, touchPrice } from '../guards';
import { defaultMinFillFor, str } from './common';
import {
  MERGE_COST_BUY_FIRST, MERGE_COST_SELL_FIRST, UNLIMITED_REPEAT_COUNT, checkSelfTrade, finishPlan, mergeTipRate, resolveActivation, resolveExpiry,
  toStatePrice, failedPlan, type CondPlan, type CondSpec, type ExitSummary, type IssueLog, type KeeperSummary, type RepeatSummary,
} from './cond-common';
import { askWorst, bidWorst, condAskState, condBidState, resolveLegs, type LegInput } from './cond-legs';

/** Bytes of the committed exit prefix of a KobIfdBid (`IFD_BID_EXIT_COMMIT` in state.rs). */
const IFD_BID_EXIT_COMMIT_BYTES = 279;
/** The KRON sell-first exit has no extension commitment, 33 bytes less than the kcc20 prefix. */
const IFD_ASK_EXIT_COMMIT_BYTES_KRON = 288;
/** Bytes of the committed exit prefix of a KobIfdAsk (`IFD_ASK_EXIT_COMMIT` in state.rs). */
const IFD_ASK_EXIT_COMMIT_BYTES = 321;
/** A repeating stop entry re-arms each cycle and prefunds at most this many arming updates. */
const MAX_ENTRY_ARM_UPDATES = 10n;

/** The leg input of the resolver for an if-done exit. */
export const exitLegInput = (e: IfdLikeIntent['exit']): LegInput => {
  const l: LegInput = {};
  if (e.takeProfit !== undefined) l.tp = e.takeProfit;
  if (e.stop !== undefined) l.stop = e.stop;
  if (e.stopLimit !== undefined) l.limit = e.stopLimit;
  if (e.slipBps !== undefined) l.slipBps = e.slipBps;
  if (e.bandDaa !== undefined) l.bandDaa = e.bandDaa;
  if (e.keeperTip !== undefined) l.keeperTip = e.keeperTip;
  if (e.minTouch !== undefined) l.minTouch = e.minTouch;
  if (e.minRestDaa !== undefined) l.minRestDaa = e.minRestDaa;
  if (e.trail !== undefined) l.trail = e.trail;
  return l;
};

/**
 * Checks which exit legs each type demands. Logs an issue and returns false on a bad shape.
 *
 * An IFD takes one leg, an IFO both, and a repeat needs the take-profit to re-arm (order-types.md).
 */
export function checkExitShape(log: IssueLog, intent: IfdLikeIntent): boolean {
  const e = intent.exit;
  const tp = e.takeProfit !== undefined;
  const sl = e.stop !== undefined;
  if (e.stopLimit !== undefined && !sl) {
    log.cond('COND_EXIT_LIMIT_NEEDS_STOP', undefined, 'exit.stop');
    return false;
  }
  switch (intent.type) {
    case 'ifd':
      if (tp === sl) {
        log.cond('COND_EXIT_NEEDS_ONE_LEG', undefined, tp ? 'exit.stop' : 'exit.takeProfit');
        return false;
      }
      return true;
    case 'repeatIfd':
      if (!tp || sl) {
        log.cond('COND_EXIT_NEEDS_TP_ONLY', undefined, tp ? 'exit.stop' : 'exit.takeProfit');
        return false;
      }
      return true;
    case 'ifo':
    case 'repeatIfo':
      if (!tp || !sl) {
        log.cond('COND_EXIT_NEEDS_BOTH', undefined, tp ? 'exit.stop' : 'exit.takeProfit');
        return false;
      }
      return true;
  }
}

/** Resolves the entry's price and stop into state units and checks their order. */
export function resolveEntry(env: PlanEnv, log: IssueLog, side: 'sell' | 'buy', e: EntrySpec): { price: bigint; stop: bigint } | null {
  const price = toStatePrice(env, log, e.price, 'entry.price');
  const stopState = e.stop === undefined ? 0n : toStatePrice(env, log, e.stop, 'entry.stop');
  if (price === null || stopState === null) return null;
  const stop = stopState;
  if (stop > 0n && (side === 'buy' ? stop > price : stop < price)) {
    log.cond('COND_ENTRY_STOP_BEYOND_LIMIT', { side, direction: side === 'buy' ? 'below' : 'above', stop: e.stop!, price: e.price }, 'entry.stop');
    return null;
  }
  return { price, stop };
}

/** The committed exit prefix: the first bytes of the encoded exit state. */
function commitExit(env: PlanEnv, exit: { kind: 'KobCondAsk' | 'KobCondAskKron'; state: CondAskState } | { kind: 'KobCondBid' | 'KobCondBidKron'; state: CondBidState }): Hex {
  // A KRON sell-first entry commits 288 bytes because its exit has no extension commitment.
  const bytes = exit.kind === 'KobCondAsk' || exit.kind === 'KobCondAskKron' ? IFD_BID_EXIT_COMMIT_BYTES : env.token.family === 'kron' ? IFD_ASK_EXIT_COMMIT_BYTES_KRON : IFD_ASK_EXIT_COMMIT_BYTES;
  return env.kob.encodeState(exit).slice(0, 2 * bytes);
}

/** The exit leg price its default minimum fill is worth 10 KAS at (a sell exit: the lower leg). */
export function exitLegPrice(side: 'sell' | 'buy', l: LegInput): bigint {
  const cands = [l.tp, l.stop].filter((p): p is bigint => p !== undefined && p > 0n);
  if (cands.length === 0) return 0n;
  return cands.reduce((a, b) => (side === 'sell' ? (a < b ? a : b) : a > b ? a : b));
}

/** Plans an if-done entry with its committed exit. Never throws for user-input problems. */
export function planIfd(env: PlanEnv, log: IssueLog, intent: IfdLikeIntent, tip: bigint, carrier: bigint): CondPlan {
  const tk = env.token;
  const buyFirst = intent.side === 'buy';
  const exitSide = buyFirst ? 'sell' : 'buy';
  const repeat = isRepeat(intent);
  const amount = intent.amount;

  if (!checkExitShape(log, intent)) return failedPlan(log);

  // ---- entry
  const entry = resolveEntry(env, log, intent.side, intent.entry);
  const entryTip = tip;
  if (entry === null) return failedPlan(log);
  const entryAllIn = buyFirst ? entry.price + entryTip : entry.price - entryTip;
  // A sell-first entry's price must exceed its tip, or the covenant refuses every fill.
  if (entryAllIn <= 0n) {
    log.shared('TIP_EXCEEDS_PRICE', { tip: entryTip }, 'tip');
    return failedPlan(log);
  }

  // ---- the entry's minimum fill (matcher.md 10.8, default ceil(amount / 4))
  const minFill = intent.minFill ?? env.kob.defaultMinFillIfd(amount);
  if (minFill < 1n || minFill > amount) {
    log.cond('COND_MIN_FILL_INVALID', { amount }, 'minFill');
    return failedPlan(log);
  }
  const fills = ceilDiv(amount, minFill);

  // ---- repeat: rptAmount = 1 + K * N (N < 2^53) and at most an i64
  let count: bigint | null = null;
  let rptAmount = 0n;
  if (repeat) {
    const k = intent.repeat?.count;
    if (k !== undefined && k < 1n) {
      log.cond('COND_REPEAT_COUNT_INVALID', undefined, 'repeat.count');
      return failedPlan(log);
    }
    if (amount >= 1n << 53n) {
      log.shared('AMOUNT_TOO_LARGE', undefined, 'amount');
      return failedPlan(log);
    }
    count = k ?? null;
    const kMax = (I64_MAX - 1n) / amount;
    const kk = k ?? (UNLIMITED_REPEAT_COUNT < kMax ? UNLIMITED_REPEAT_COUNT : kMax);
    if (kk > kMax) {
      log.cond('COND_REPEAT_COUNT_INVALID', undefined, 'repeat.count');
      return failedPlan(log);
    }
    rptAmount = 1n + kk * amount;
  }

  // ---- exit (built first because the entry commits to it, its minimum fill included)
  const exitIn = intent.exit;
  const exitTipBase = exitIn.tip ?? 0n;
  if (exitTipBase < 0n) {
    log.shared('TIP_NEGATIVE', undefined, 'exit.tip');
    return failedPlan(log);
  }
  const exitInput = exitLegInput(exitIn);
  // Every exit holds one entry fill, at least the entry's minimum fill except the last. Its own
  // minimum fill is at most that.
  const exitMinDefault = defaultMinFillFor(env, amount, exitLegPrice(exitSide, exitInput));
  const exitMinFill = exitIn.minFill ?? (exitMinDefault < minFill ? exitMinDefault : minFill);
  log.add(...checkMinFill(exitMinFill, amount, 'exit.minFill'));
  if (log.failed) return failedPlan(log);
  // A repeat's exit tip carries the merge cost, paid even by the smallest take-profit fill.
  const mergeTip = repeat ? mergeTipRate(buyFirst ? MERGE_COST_BUY_FIRST : MERGE_COST_SELL_FIRST, tk.scale, exitMinFill) : 0n;
  const exitTip = exitTipBase + mergeTip;
  // The exit's trigger threshold scales with what it sells or buys. That is at most the entry's
  // amount, which is known even though its fills are not.
  const legs = resolveLegs(env, log, exitSide, exitInput, 'exit.', exitMinFill, amount);
  const act = resolveActivation(env, log, intent.activeFrom);
  const entryExpiry = act === null ? null : resolveExpiry(env, log, intent.expiry, { field: 'expiry', activeFrom: act.activeFrom });
  const exitExpiry = resolveExpiry(env, log, exitIn.expiry, { field: 'exit.expiry', exit: true });
  if (legs === null || act === null || entryExpiry === null || exitExpiry === null || log.failed) return failedPlan(log);

  // ---- economics (matcher.md 10.13)
  // The take-profit must beat the entry all-in per whole token. For a plain IFD it is a warning.
  const tpAllIn = legs.tpPrice > 0n ? (buyFirst ? legs.tpPrice - exitTip : legs.tpPrice + exitTip) : null;
  let profitPerToken = 0n;
  if (tpAllIn !== null) {
    profitPerToken = buyFirst ? tpAllIn - entryAllIn : entryAllIn - tpAllIn;
    if (profitPerToken <= 0n) {
      // A repeat re-arms only on a profitable take-profit. A plain IFD may exit at a loss.
      log.cond('COND_TP_NOT_PROFITABLE', { profitPerToken }, 'exit.takeProfit', repeat ? 'error' : 'warning');
      if (repeat) return failedPlan(log);
    }
  }
  // Every rate the entry and its exit carry stays below 2^62 (the builders refuse more).
  log.add(...checkNotional(amount, buyFirst ? entry.price + entryTip : entry.price, tk.scale, 'entry.price'));
  const exitTop = legs.stopWorst > legs.tpPrice ? legs.stopWorst : legs.tpPrice;
  log.add(...checkNotional(amount, buyFirst ? exitTop : exitTop + exitTip, tk.scale, 'exit.takeProfit'));
  if (log.failed) return failedPlan(log);

  // ---- self-trade (the entry is a real order, the exit is created later so it only warns)
  checkSelfTrade(env, log, intent.side, entryAllIn, { field: 'entry.price' });
  const exitWorstAllIn = buyFirst ? askWorst(legs) - exitTip : bidWorst(legs) + exitTip;
  checkSelfTrade(env, log, exitSide, exitWorstAllIn, { exit: true, field: 'exit' });

  // The entry crosses the book at placement and fills right away at its limit.
  const touch = touchPrice(env.book, intent.side);
  if (entry.stop === 0n && touch !== null && (buyFirst ? touch <= intent.entry.price : touch >= intent.entry.price)) {
    log.cond('COND_ENTRY_CROSSES', undefined, 'entry.price');
  }
  if (log.failed) return failedPlan(log);

  // ---- entry stop parameters
  const stopEntry = entry.stop > 0n;
  const entryBand = stopEntry && entry.stop !== entry.price ? intent.entry.bandDaa ?? STOP_BAND_DAA : 0n;
  const entryKeeperTip = stopEntry ? intent.entry.keeperTip ?? tk.keeperTip : 0n;
  // The stop entry's trigger threshold defaults to the larger of its minimum fill and a quarter of
  // its amount (kob-wasm defaultMinTouch). A limit entry has none.
  const entryTouch = stopEntry ? (intent.entry.minTouch ?? env.kob.defaultMinTouch(minFill, amount)) : 0n;
  const entryRestDaa = intent.entry.minRestDaa ?? MIN_REST_DAA;
  if (entryBand < 0n) log.cond('COND_BAND_INVALID', undefined, 'entry.bandDaa');
  if (entryKeeperTip < 0n) log.cond('COND_KEEPER_TIP_INVALID', undefined, 'entry.keeperTip');
  if (stopEntry && entryTouch < 1n) log.cond('COND_MIN_TOUCH_INVALID', undefined, 'entry.minTouch');
  if (entryRestDaa < 0n) log.cond('COND_MIN_REST_INVALID', undefined, 'entry.minRestDaa');
  if (log.failed) return failedPlan(log);
  const armUpdates = !stopEntry ? 0n : repeat ? (count === null || count + 1n > MAX_ENTRY_ARM_UPDATES ? MAX_ENTRY_ARM_UPDATES : count + 1n) : 1n;
  const keeperReserve = entryKeeperTip * armUpdates;

  // ---- states
  const base = {
    maker: env.maker,
    tokenCovId: tk.covenantId,
    tokenTplHash: tk.templateHash,
    tplPrefixLen: String(tk.prefixLen),
    tplSuffixLen: String(tk.suffixLen),
    scale: str(tk.scale),
    tip: str(entryTip),
    activeFrom: str(act.activeFrom),
    expiryDaa: str(entryExpiry.expiryDaa),
    refundTip: str(tk.refundTip),
    minFill: str(minFill),
    entryStop: str(entry.stop),
    bandDaa: str(entryBand),
    minTouch: str(entryTouch),
    minRestDaa: str(entryRestDaa),
    keeperTip: str(entryKeeperTip),
    armed: '0',
    rptAmount: str(rptAmount),
  };

  let order: OrderState;
  let exitState: OrderState;
  let prefund: bigint | null = null;
  // KAS on the order UTXO, itemised. The total is the state's own escrow (kob-wasm ifdBidEscrow or
  // ifdAskEscrow), and the builder refuses less.
  const lines: CarrierLine[] = [];

  const exitBase = { amount, minFill: exitMinFill, tip: exitTip, expiryDaa: exitExpiry.expiryDaa };
  if (buyFirst) {
    const ask = condAskState(env, exitBase, legs);
    exitState = { kind: kindFor('KobCondAsk', tk.family), state: ask } as OrderState;
    const bid: IfdBidState = {
      ...base,
      extensionCommitment: tk.extensionCommitment,
      amountLeft: str(amount),
      price: str(entry.price),
      deliveryCarrier: str(carrier),
      exitCarrier: str(carrier),
      exitState: commitExit(env, { kind: kindFor('KobCondAsk', tk.family), state: ask } as { kind: 'KobCondAsk'; state: CondAskState }),
    };
    order = { kind: kindFor('KobIfdBid', tk.family), state: bid } as OrderState;
    // A repeating entry outlives its last fill, so it carries one more exit carrier (10.13).
    const exitCarriers = fills + (repeat ? 1n : 0n);
    const escrow = env.kob.ifdBidEscrow(order);
    if (escrow === null) {
      log.shared('AMOUNT_TOO_LARGE', undefined, 'amount');
      return failedPlan(log);
    }
    lines.push({ kind: 'escrow', amount: escrow - carrier * fills - carrier * exitCarriers, count: 1 });
    lines.push({ kind: 'deliveryCarrier', amount: carrier, count: Number(fills) });
    lines.push({ kind: 'exitCarrier', amount: carrier, count: Number(exitCarriers) });
    if (keeperReserve > 0n) lines.push({ kind: 'keeperReserve', amount: entryKeeperTip, count: Number(armUpdates) });
  } else {
    const bidExit = condBidState(env, { ...exitBase, deliveryCarrier: carrier }, legs);
    exitState = { kind: kindFor('KobCondBid', tk.family), state: bidExit } as OrderState;
    // The exit holds the proceeds plus the prefund. That covers its worst buy-back per whole token,
    // which is what the exit's covenant spends at most.
    const proceeds = entry.price - entryTip;
    const worstBuyBack = bidWorst(legs) + exitTip;
    const minPrefund = worstBuyBack > proceeds ? worstBuyBack - proceeds : 0n;
    prefund = intent.prefund ?? minPrefund;
    if (prefund < 0n) {
      log.cond('COND_PREFUND_INVALID', undefined, 'prefund');
      return failedPlan(log);
    }
    if (prefund < minPrefund) {
      log.cond('COND_PREFUND_SHORT', { needed: minPrefund, given: prefund }, 'prefund');
      return failedPlan(log);
    }
    const ask: IfdAskState = {
      ...base,
      amountLeft: str(amount),
      price: str(entry.price),
      prefund: str(prefund),
      exitCarrier: str(carrier),
      exitState: commitExit(env, { kind: kindFor('KobCondBid', tk.family), state: bidExit } as { kind: 'KobCondBid'; state: CondBidState }),
    };
    order = { kind: kindFor('KobIfdAsk', tk.family), state: ask } as OrderState;
    const value = env.kob.ifdAskEscrow(order, carrier);
    if (value === null) {
      log.shared('AMOUNT_TOO_LARGE', undefined, 'amount');
      return failedPlan(log);
    }
    lines.push({ kind: 'orderCarrier', amount: carrier, count: 1 });
    // The prefund of the whole amount is ceil(amount * prefund / scale), plus a sompi of rounding
    // per extra fill because each fill's prefund rounds up.
    const prefundKas = value - carrier - carrier * fills;
    if (prefundKas > 0n) lines.push({ kind: 'prefund', amount: prefundKas, count: 1 });
    lines.push({ kind: 'exitCarrier', amount: carrier, count: Number(fills) });
    // The arming keeper is paid from the entry's carrier, which must stay a carrier because the
    // exit and the refund need it too.
    if (keeperReserve * 2n > carrier) {
      log.cond('COND_KEEPER_FUNDING_TOO_LARGE', { reserve: keeperReserve }, 'carrier');
      return failedPlan(log);
    }
  }

  // Exit keepers are paid from the exit's own carrier (buy-first exit: the order value, sell-first
  // exit: exitCarrier plus proceeds).
  if (legs.keeper !== null && legs.keeper.reserve * 2n > carrier) {
    log.cond('COND_KEEPER_FUNDING_TOO_LARGE', { reserve: legs.keeper.reserve }, 'exit.trail');
    return failedPlan(log);
  }

  // ---- summaries
  const keeper: KeeperSummary | null = stopEntry ? { tip: entryKeeperTip, expectedUpdates: Number(armUpdates), reserve: keeperReserve, fundedFrom: buyFirst ? 'escrow' : 'carrier' } : null;
  const exitSummary: ExitSummary = {
    side: exitSide,
    legs: legs.summary,
    tip: exitTip,
    minFill: exitMinFill,
    takeProfitAllIn: tpAllIn,
    stopWorstAllIn: legs.stopPrice > 0n ? (buyFirst ? legs.stopWorst - exitTip : legs.stopWorst + exitTip) : null,
    expiry: exitExpiry.disclosure.kind === 'gtc' ? 'gtc' : 'gtd',
    expiryDaa: exitExpiry.disclosure.kind === 'gtc' ? null : exitExpiry.disclosure.daa,
    expiryUnixSeconds: exitExpiry.disclosure.kind === 'gtc' ? null : exitExpiry.disclosure.approxUnixSeconds,
    deliveryCarrier: carrier,
    exitCarrier: carrier,
    trail: legs.trail,
    keeper: legs.keeper,
    prefund,
  };
  const repeatSummary: RepeatSummary | null = repeat
    ? {
        count,
        rptAmount,
        cycleAmount: amount,
        mergeTip,
        profitPerToken,
        untilDaa: entryExpiry.expiryDaa < env.clock.daa + MAX_IDLE_DAA ? entryExpiry.expiryDaa : env.clock.daa + MAX_IDLE_DAA,
      }
    : null;

  const notes: string[] = [intent.type === 'ifd' || intent.type === 'repeatIfd' ? 'ifd' : 'ifo', 'position'];
  notes.push(buyFirst ? 'buyFirst' : 'sellFirst', 'minFill', exitExpiry.disclosure.kind === 'gtc' ? 'exitGtc' : 'exitGtd');
  if (stopEntry) notes.push('stopEntry', 'stopEntryTrigger', 'stopEntryAuction', 'triggerExposure', 'keeperReserve');
  if (legs.stopPrice > 0n) notes.push('exitStop', 'triggerExposure');
  if (legs.stopPrice > 0n && intent.exit.stopLimit !== undefined) notes.push('stopLimitMayNotFill');
  if (legs.trail !== null) notes.push('trailing');
  if (repeat) {
    notes.push('repeat', count === null ? 'repeatUnlimited' : 'repeatCounted', buyFirst ? 'repeatReBuys' : 'repeatReSells', 'repeatStopLossEnds', 'mergeTip', 'cancelPosition');
  }
  if (!buyFirst) notes.push('prefund');
  const uniq = [...new Set(notes)];

  // The entry's all-in total by the covenant's quote rule. A buy-first entry pays at most
  // floor(amount (price + tip) / scale). A sell-first entry receives at least
  // ceil(amount (price - tip) / scale).
  const allInTotal = buyFirst ? env.kob.ifdBidSpend(order, amount, entry.price) : env.kob.ifdAskProceeds(order, amount, entry.price);
  const spec: CondSpec = {
    order,
    exit: exitState,
    lines,
    carrier,
    side: intent.side,
    amount,
    expiry: entryExpiry,
    activatesAt: act.activatesAt,
    disclosure: {
      limitPrice: entry.price,
      allInPrice: entryAllIn,
      allInTotal,
      expectedPrice: stopEntry ? entry.stop : null,
      worstPrice: entry.price,
      tip: entryTip,
      minTouch: stopEntry ? entryTouch : null,
    },
    notes: uniq,
    cond: {
      type: intent.type,
      legs: null,
      trail: null,
      keeper,
      entry: { price: intent.entry.price, stop: stopEntry ? intent.entry.stop ?? null : null, bandDaa: entryBand, minFill, maxFills: fills, minTouch: entryTouch, minRestDaa: entryRestDaa },
      exit: exitSummary,
      repeat: repeatSummary,
    },
  };
  return finishPlan(env, log, spec);
}
