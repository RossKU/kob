// Legs of a conditional covenant (`KobCondAsk` / `KobCondBid`, also the exits of if-done orders): take-profit (limit) leg, stop leg with
// its band, trailing, trigger and keeper parameters. Pure: validates the user's numbers, converts them to state units and builds the
// state. The covenants enforce the rules on chain; this only refuses inputs that would make a dud order (matcher.md 10.6, 10.7).
import { MIN_REST_DAA, SLIPPAGE_BPS, STOP_BAND_DAA, clampRate } from '../daa';
import type { StopSpec, TrailSpec } from '../intent-cond';
import type { PlanEnv } from '../plan-types';
import type { CondAskState, CondBidState } from '../types';
import { str } from './common';
import {
  DEFAULT_EXPECTED_UPDATES, DEFAULT_TRAIL_WAIT_DAA, MAX_EXPECTED_UPDATES, MIN_TRAIL_WAIT_DAA, ZERO32, toStatePrice,
  type IssueLog, type KeeperSummary, type LegSummary, type TrailSummary,
} from './cond-common';

/** The user's legs as typed (prices per whole token). `tp` is the limit leg, `stop` the trigger. */
export interface LegInput extends Partial<Omit<StopSpec, 'stop'>> {
  tp?: bigint;
  stop?: bigint;
}

/** Validated legs in STATE units (sompi per whole token of `scale` base units; `minTouch` base units). */
export interface Legs {
  tpPrice: bigint;
  stopPrice: bigint;
  slipBps: bigint;
  bandDaa: bigint;
  keeperTip: bigint;
  /** smallest trigger evidence, base units of a plain order of the same scale */
  minTouch: bigint;
  minRestDaa: bigint;
  trailStep: bigint;
  trailGap: bigint;
  trailWait: bigint;
  /** the stop leg's worst price in state units (0 without a stop leg) */
  stopWorst: bigint;
  expectedUpdates: number;
  summary: LegSummary;
  trail: TrailSummary | null;
  keeper: KeeperSummary | null;
}

/** Largest stop price (state units) the covenants accept on a stop leg: `stopPrice * slipBps` must fit in an i64 (`MAX_STOP`). */
export const MAX_STOP_PRICE = 922_337_203_685_477n;

/**
 * The largest `slipBps` with `stop -/+ floor(stop*slipBps/10^4)` not beyond `limit` (10.6). Works in state units and multiplies first,
 * exactly like the covenants (`stopPrice * bps / 10000`; dividing first loses the band for low stops and could put the worst price past
 * the user's limit). The largest `bps` with `floor(stop*bps/10^4) <= dist` is `floor(((dist+1)*10^4 - 1) / stop)`, capped at 10 000.
 */
export function slipForLimit(side: 'sell' | 'buy', stop: bigint, limit: bigint): bigint {
  if (stop <= 0n) return 0n;
  const dist = side === 'sell' ? stop - limit : limit - stop;
  if (dist <= 0n) return 0n; // a limit at the stop leaves no band to spend
  const slip = ((dist + 1n) * 10_000n - 1n) / stop;
  return slip > 10_000n ? 10_000n : slip;
}

/** The stop leg's worst price: sell `stop - floor(stop*bps/10^4)` (never below 0), buy `stop + ...` (state units); multiply first like the covenants. */
export const stopWorstPrice = (side: 'sell' | 'buy', stop: bigint, slipBps: bigint): bigint => {
  const move = (stop * slipBps) / 10_000n;
  return side === 'sell' ? stop - move : stop + move;
};

/**
 * Validates and converts the legs of an order of `side` (the side of the covenant that will trade: 'sell' = KobCondAsk).
 * Returns null when there are errors (already logged). `field` prefixes the issue fields (e.g. "exit."). `minFill` is the order's own
 * minimum fill and `amount` the amount the stop sells or buys (0n: unknown): the default trigger threshold is the larger of the minimum
 * fill and a quarter of the amount (kob-wasm `defaultMinTouch`).
 */
export function resolveLegs(env: PlanEnv, log: IssueLog, side: 'sell' | 'buy', input: LegInput, f = '', minFill = 1n, amount = 0n): Legs | null {
  const hasTp = input.tp !== undefined;
  const hasStop = input.stop !== undefined;
  if (!hasTp && !hasStop) {
    log.cond('COND_LEGS_MISSING', undefined, `${f}takeProfit`);
    return null;
  }
  const tp = hasTp ? toStatePrice(env, log, input.tp!, `${f}takeProfit`) : 0n;
  const stop = hasStop ? toStatePrice(env, log, input.stop!, `${f}stop`) : 0n;
  if (tp === null || stop === null) return null;
  if (hasStop && stop > MAX_STOP_PRICE) {
    log.shared('PRICE_TOO_LARGE', undefined, `${f}stop`);
    return null;
  }

  if (hasTp && hasStop && (side === 'sell' ? tp <= stop : tp >= stop)) {
    log.cond('COND_TP_STOP_ORDER', { direction: side === 'sell' ? 'above' : 'below', takeProfit: input.tp!, stop: input.stop! }, `${f}takeProfit`);
    return null;
  }

  // band
  let slip = input.slipBps !== undefined ? BigInt(Math.trunc(input.slipBps)) : SLIPPAGE_BPS;
  if (input.slipBps !== undefined && (!Number.isInteger(input.slipBps) || input.slipBps < 0 || input.slipBps > 10_000)) {
    log.cond('COND_SLIP_INVALID', undefined, `${f}slipBps`);
    return null;
  }
  let limitPrice: bigint | null = null;
  if (hasStop && input.limit !== undefined) {
    const lim = input.limit;
    if (lim <= 0n) {
      log.shared('PRICE_NOT_POSITIVE', undefined, `${f}limit`);
      return null;
    }
    if (side === 'sell' ? lim > input.stop! : lim < input.stop!) {
      log.cond('COND_STOP_LIMIT_BEYOND_STOP', { side, direction: side === 'sell' ? 'above' : 'below', limit: lim, stop: input.stop! }, `${f}limit`);
      return null;
    }
    // the band ends at the user's limit or inside it (the covenant's stop band is floor(stop * slipBps / 10^4))
    slip = slipForLimit(side, stop, lim);
    limitPrice = lim;
  }

  let band = input.bandDaa ?? STOP_BAND_DAA;
  if (band < 0n) {
    log.cond('COND_BAND_INVALID', undefined, `${f}bandDaa`);
    return null;
  }
  // a band of zero basis points is a single price: no auction to run
  if (hasStop && slip === 0n) band = 0n;

  const stopWorst = hasStop ? stopWorstPrice(side, stop, slip) : 0n;
  if (hasStop && stopWorst <= 0n) {
    log.cond('COND_WORST_PRICE_INVALID', undefined, `${f}slipBps`);
    return null;
  }

  // trigger
  const minRestDaa = input.minRestDaa ?? MIN_REST_DAA;
  if (minRestDaa < 0n) {
    log.cond('COND_MIN_REST_INVALID', undefined, `${f}minRestDaa`);
    return null;
  }
  const minTouch = input.minTouch ?? env.kob.defaultMinTouch(minFill, amount);
  if (minTouch <= 0n) {
    log.cond('COND_MIN_TOUCH_INVALID', undefined, `${f}minTouch`);
    return null;
  }

  // keeper
  const keeperTip = hasStop ? input.keeperTip ?? env.token.keeperTip : 0n;
  if (keeperTip < 0n) {
    log.cond('COND_KEEPER_TIP_INVALID', undefined, `${f}keeperTip`);
    return null;
  }

  // trailing
  let trailStep = 0n;
  let trailGap = 0n;
  let trailWait = 0n;
  let trail: TrailSummary | null = null;
  let expected = hasStop ? 1 : 0;
  const t: TrailSpec | undefined = input.trail;
  if (t !== undefined) {
    if (!hasStop) {
      log.cond('COND_TRAIL_NEEDS_STOP', undefined, `${f}stop`);
      return null;
    }
    if (t.step <= 0n) {
      log.cond('COND_TRAIL_STEP_INVALID', undefined, `${f}trail.step`);
      return null;
    }
    if (env.token.tick > 0n && t.step % env.token.tick !== 0n) {
      log.shared('PRICE_NOT_ON_TICK', { tick: env.token.tick, below: (t.step / env.token.tick) * env.token.tick, above: (t.step / env.token.tick + 1n) * env.token.tick }, `${f}trail.step`);
      return null;
    }
    if (t.gap < 0n) {
      log.cond('COND_TRAIL_GAP_INVALID', undefined, `${f}trail.gap`);
      return null;
    }
    const wait = t.wait ?? DEFAULT_TRAIL_WAIT_DAA;
    if (wait < MIN_TRAIL_WAIT_DAA) {
      log.cond('COND_TRAIL_WAIT_TOO_SHORT', { min: MIN_TRAIL_WAIT_DAA }, `${f}trail.wait`);
      return null;
    }
    const upd = t.expectedUpdates ?? DEFAULT_EXPECTED_UPDATES;
    if (!Number.isInteger(upd) || upd < 0 || upd > MAX_EXPECTED_UPDATES) {
      log.cond('COND_TRAIL_UPDATES_INVALID', { max: MAX_EXPECTED_UPDATES }, `${f}trail.expectedUpdates`);
      return null;
    }
    trailStep = t.step;
    trailGap = t.gap;
    trailWait = wait;
    // the arm that follows a trail is one more update
    expected = upd + 1;
    const r = BigInt(clampRate(env.clock.rateMilli));
    trail = {
      step: t.step,
      gap: t.gap,
      waitDaa: wait,
      waitSeconds: (wait * 1000n) / r,
      maxUpdatesPerDay: (86_400n * r) / 1000n / wait,
      expectedUpdates: upd,
    };
  }

  const summary: LegSummary = {
    takeProfit: hasTp ? input.tp! : null,
    stop: hasStop ? input.stop! : null,
    stopWorst: hasStop ? stopWorst : null,
    slipBps: Number(slip),
    bandDaa: band,
    limit: limitPrice,
    minTouch,
    minRestDaa,
  };
  const keeper: KeeperSummary | null = hasStop
    ? { tip: keeperTip, expectedUpdates: expected, reserve: keeperTip * BigInt(expected), fundedFrom: side === 'sell' ? 'carrier' : 'escrow' }
    : null;
  return {
    tpPrice: tp,
    stopPrice: stop,
    slipBps: hasStop ? slip : 0n,
    bandDaa: hasStop ? band : 0n,
    keeperTip,
    minTouch,
    minRestDaa,
    trailStep,
    trailGap,
    trailWait,
    stopWorst,
    expectedUpdates: expected,
    summary,
    trail,
    keeper,
  };
}

interface CondBase {
  /** base units */
  amount: bigint;
  /** smallest fill (base units) unless a fill takes everything left */
  minFill: bigint;
  /** sompi per whole token */
  tip: bigint;
  expiryDaa: bigint;
  /** timed activation (state `activeFrom`); default 0 */
  activeFrom?: bigint;
}

/** `KobCondAsk` state of a sell-side conditional: a new order (no repeat fields, not armed). */
export function condAskState(env: PlanEnv, b: CondBase, l: Legs): CondAskState {
  const tk = env.token;
  return {
    maker: env.maker,
    tokenCovId: tk.covenantId,
    tokenTplHash: tk.templateHash,
    tplPrefixLen: String(tk.prefixLen),
    tplSuffixLen: String(tk.suffixLen),
    scale: str(tk.scale),
    minFill: str(b.minFill),
    tip: str(b.tip),
    activeFrom: str(b.activeFrom ?? 0n),
    expiryDaa: str(b.expiryDaa),
    refundTip: str(tk.refundTip),
    tpPrice: str(l.tpPrice),
    stopPrice: str(l.stopPrice),
    slipBps: str(l.slipBps),
    trailStep: str(l.trailStep),
    trailGap: str(l.trailGap),
    trailWait: str(l.trailWait),
    minTouch: str(l.minTouch),
    minRestDaa: str(l.minRestDaa),
    armed: '0',
    bandDaa: str(l.bandDaa),
    keeperTip: str(l.keeperTip),
    amountLeft: str(b.amount),
    parent: ZERO32,
    rptPrice: '0',
    rptUntil: '0',
  };
}

/** `KobCondBid` state of a buy-side conditional: a new order (no repeat fields, not armed). */
export function condBidState(env: PlanEnv, b: CondBase & { deliveryCarrier: bigint }, l: Legs): CondBidState {
  const tk = env.token;
  return {
    maker: env.maker,
    tokenCovId: tk.covenantId,
    tokenTplHash: tk.templateHash,
    tplPrefixLen: String(tk.prefixLen),
    tplSuffixLen: String(tk.suffixLen),
    extensionCommitment: tk.extensionCommitment,
    scale: str(tk.scale),
    minFill: str(b.minFill),
    tip: str(b.tip),
    activeFrom: str(b.activeFrom ?? 0n),
    expiryDaa: str(b.expiryDaa),
    refundTip: str(tk.refundTip),
    deliveryCarrier: str(b.deliveryCarrier),
    tpPrice: str(l.tpPrice),
    stopPrice: str(l.stopPrice),
    slipBps: str(l.slipBps),
    trailStep: str(l.trailStep),
    trailGap: str(l.trailGap),
    trailWait: str(l.trailWait),
    minTouch: str(l.minTouch),
    minRestDaa: str(l.minRestDaa),
    amountLeft: str(b.amount),
    armed: '0',
    bandDaa: str(l.bandDaa),
    keeperTip: str(l.keeperTip),
    parent: ZERO32,
    rptPrice: '0',
    rptPre: '0',
    rptUntil: '0',
  };
}

/** The worst state price a buy-side conditional can pay (the covenant's `worst()`): the larger of the limit leg and the stop ceiling. */
export const bidWorst = (l: Legs): bigint => (l.stopPrice > 0n && l.stopWorst > l.tpPrice ? l.stopWorst : l.tpPrice);
/** The lowest state price a sell-side conditional accepts over its legs. */
export const askWorst = (l: Legs): bigint => {
  const cands: bigint[] = [];
  if (l.tpPrice > 0n) cands.push(l.tpPrice);
  if (l.stopPrice > 0n) cands.push(l.stopWorst);
  return cands.reduce((a, b) => (a < b ? a : b));
};
