// IFD / IFO / bracket and repeat IFD / IFO (matcher.md 6, 6.1, 10.8, 10.13; order-types.md "Repeat IFD in detail").
//
//   buy-first : KobIfdBid  (escrow = budget + carriers)      -> every fill of n base units creates a KobCondAsk exit for exactly n
//   sell-first: KobIfdAsk  (custody + prefund + carriers)    -> every fill of n base units creates a KobCondBid exit for exactly n
//
// The exit is COMMITTED in the entry's `exitState` (the first 279 / 321 bytes of the exit state span, its `minFill` included). The entry itself
// writes the exit's `amountLeft` and repeat fields, so what this planner commits is the plain exit with `amountLeft = amount`.
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

/** Bytes of the committed exit prefix (`IFD_BID_EXIT_COMMIT` / `IFD_ASK_EXIT_COMMIT` of kob-protocol/state.rs). */
const IFD_BID_EXIT_COMMIT_BYTES = 279;
/** the KRON sell-first exit has no extension commitment: 33 bytes less than `IFD_ASK_EXIT_COMMIT_BYTES` (state.rs `IFD_ASK_EXIT_COMMIT_KRON`) */
const IFD_ASK_EXIT_COMMIT_BYTES_KRON = 288;
const IFD_ASK_EXIT_COMMIT_BYTES = 321;
/** stop entries re-arm unarmed each cycle: pre-fund at most this many arming updates of a repeating stop entry */
const MAX_ENTRY_ARM_UPDATES = 10n;

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

/** Which exit legs each type demands (order-types.md: IFD one leg, IFO both; a repeat needs the take-profit to re-arm). */
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

/** The committed exit prefix: first bytes of the encoded exit state (the wasm encoder owns the layout; the entry replaces the rest). */
function commitExit(env: PlanEnv, exit: { kind: 'KobCondAsk' | 'KobCondAskKron'; state: CondAskState } | { kind: 'KobCondBid' | 'KobCondBidKron'; state: CondBidState }): Hex {
  // a KRON sell-first entry commits 288 bytes: its exit has no extension commitment (33 bytes less)
  const bytes = exit.kind === 'KobCondAsk' || exit.kind === 'KobCondAskKron' ? IFD_BID_EXIT_COMMIT_BYTES : env.token.family === 'kron' ? IFD_ASK_EXIT_COMMIT_BYTES_KRON : IFD_ASK_EXIT_COMMIT_BYTES;
  return env.kob.encodeState(exit).slice(0, 2 * bytes);
}

/** The exit leg price its default minimum fill is worth 10 KAS at: the take-profit, else the stop (a sell exit: the lower of the two). */
export function exitLegPrice(side: 'sell' | 'buy', l: LegInput): bigint {
  const cands = [l.tp, l.stop].filter((p): p is bigint => p !== undefined && p > 0n);
  if (cands.length === 0) return 0n;
  return cands.reduce((a, b) => (side === 'sell' ? (a < b ? a : b) : a > b ? a : b));
}

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
  // a sell-first entry's price must not be below its tip (the covenant refuses every fill otherwise)
  if (entryAllIn <= 0n) {
    log.shared('TIP_EXCEEDS_PRICE', { tip: entryTip }, 'tip');
    return failedPlan(log);
  }

  // ---- the entry's minimum fill (matcher.md 10.8: default ceil(amount / 4), kob-wasm defaultMinFillIfd)
  const minFill = intent.minFill ?? env.kob.defaultMinFillIfd(amount);
  if (minFill < 1n || minFill > amount) {
    log.cond('COND_MIN_FILL_INVALID', { amount }, 'minFill');
    return failedPlan(log);
  }
  const fills = ceilDiv(amount, minFill);

  // ---- repeat: rptAmount = 1 + K * N (N < 2^53), at most an i64
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

  // ---- exit (built first: the entry commits to it, its minimum fill included)
  const exitIn = intent.exit;
  const exitTipBase = exitIn.tip ?? 0n;
  if (exitTipBase < 0n) {
    log.shared('TIP_NEGATIVE', undefined, 'exit.tip');
    return failedPlan(log);
  }
  const exitInput = exitLegInput(exitIn);
  // every exit holds one entry fill (at least the entry's minimum fill, except the last): its own minimum fill is at most that
  const exitMinDefault = defaultMinFillFor(env, amount, exitLegPrice(exitSide, exitInput));
  const exitMinFill = exitIn.minFill ?? (exitMinDefault < minFill ? exitMinDefault : minFill);
  log.add(...checkMinFill(exitMinFill, amount, 'exit.minFill'));
  if (log.failed) return failedPlan(log);
  // a repeat's exit tip carries the merge cost, paid even by the smallest take-profit fill
  const mergeTip = repeat ? mergeTipRate(buyFirst ? MERGE_COST_BUY_FIRST : MERGE_COST_SELL_FIRST, tk.scale, exitMinFill) : 0n;
  const exitTip = exitTipBase + mergeTip;
  const legs = resolveLegs(env, log, exitSide, exitInput, 'exit.', exitMinFill);
  const act = resolveActivation(env, log, intent.activeFrom);
  const entryExpiry = act === null ? null : resolveExpiry(env, log, intent.expiry, { field: 'expiry', activeFrom: act.activeFrom });
  const exitExpiry = resolveExpiry(env, log, exitIn.expiry, { field: 'exit.expiry', exit: true });
  if (legs === null || act === null || entryExpiry === null || exitExpiry === null || log.failed) return failedPlan(log);

  // ---- economics: the take-profit must beat the entry all-in per whole token (10.13); for a plain IFD it is only a warning
  const tpAllIn = legs.tpPrice > 0n ? (buyFirst ? legs.tpPrice - exitTip : legs.tpPrice + exitTip) : null;
  let profitPerToken = 0n;
  if (tpAllIn !== null) {
    profitPerToken = buyFirst ? tpAllIn - entryAllIn : entryAllIn - tpAllIn;
    if (profitPerToken <= 0n) {
      // a repeat re-arms only on a profitable take-profit (10.13); a plain IFD may knowingly exit at a loss
      log.cond('COND_TP_NOT_PROFITABLE', { profitPerToken }, 'exit.takeProfit', repeat ? 'error' : 'warning');
      if (repeat) return failedPlan(log);
    }
  }
  // the 2^62 bound of every rate the entry and its exit carry (the builders refuse beyond it)
  log.add(...checkNotional(amount, buyFirst ? entry.price + entryTip : entry.price, tk.scale, 'entry.price'));
  const exitTop = legs.stopWorst > legs.tpPrice ? legs.stopWorst : legs.tpPrice;
  log.add(...checkNotional(amount, buyFirst ? exitTop : exitTop + exitTip, tk.scale, 'exit.takeProfit'));
  if (log.failed) return failedPlan(log);

  // ---- self-trade: the entry is a real order now; the exit only later (warning)
  checkSelfTrade(env, log, intent.side, entryAllIn, { field: 'entry.price' });
  const exitWorstAllIn = buyFirst ? askWorst(legs) - exitTip : bidWorst(legs) + exitTip;
  checkSelfTrade(env, log, exitSide, exitWorstAllIn, { exit: true, field: 'exit' });

  // the entry crosses the book at placement: it fills right away at its limit
  const touch = touchPrice(env.book, intent.side);
  if (entry.stop === 0n && touch !== null && (buyFirst ? touch <= intent.entry.price : touch >= intent.entry.price)) {
    log.cond('COND_ENTRY_CROSSES', undefined, 'entry.price');
  }
  if (log.failed) return failedPlan(log);

  // ---- entry stop parameters
  const stopEntry = entry.stop > 0n;
  const entryBand = stopEntry && entry.stop !== entry.price ? intent.entry.bandDaa ?? STOP_BAND_DAA : 0n;
  const entryKeeperTip = stopEntry ? intent.entry.keeperTip ?? tk.keeperTip : 0n;
  // the stop entry's trigger threshold defaults to the entry's own minimum fill (kob-wasm defaultMinTouch); a limit entry has none
  const entryTouch = stopEntry ? (intent.entry.minTouch ?? env.kob.defaultMinTouch(minFill)) : 0n;
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
  // KAS on the order UTXO, itemised: the total is exactly the state's own escrow (kob-wasm ifdBidEscrow / ifdAskEscrow; the builder refuses less)
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
    // a repeating entry outlives its last fill: one more exit carrier of its own (10.13)
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
    // the exit holds the proceeds plus prefund: enough for its worst buy-back (the covenant of the exit spends up to that), per whole token
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
    // the prefund of the whole amount, ceil(amount * prefund / scale), plus a sompi of rounding per extra fill (each fill's prefund rounds up)
    const prefundKas = value - carrier - carrier * fills;
    if (prefundKas > 0n) lines.push({ kind: 'prefund', amount: prefundKas, count: 1 });
    lines.push({ kind: 'exitCarrier', amount: carrier, count: Number(fills) });
    // the arming keeper is paid from the entry's carrier: it must stay a carrier (the exit and refund need it too)
    if (keeperReserve * 2n > carrier) {
      log.cond('COND_KEEPER_FUNDING_TOO_LARGE', { reserve: keeperReserve }, 'carrier');
      return failedPlan(log);
    }
  }

  // exit keepers are paid from the exit's own carrier (buy-first exit: order value; sell-first exit: exitCarrier + proceeds)
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

  // the entry's all-in total by the covenant's quote rule: a buy-first entry PAYS at most floor(amount (price + tip) / scale), a sell-first
  // entry RECEIVES at least ceil(amount (price - tip) / scale)
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
