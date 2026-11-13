// Shared plumbing of the conditional and if-done planners (cond.ts, cond-legs.ts, cond-ifd.ts).
//
// It holds defaults, the issue log, expiry resolution, price checks, guards, summaries and the
// final `buildCreate` plus disclosure step every order type ends in.
//
// Coin selection, the `createOrder` build with an adaptive fee reserve and the recover-and-compare
// self-check are the shared layer of common.ts (`buildCreate`). Nothing here re-implements protocol
// logic. States are only assembled, and the byte encoding, the signing plan, the fees and the
// placement record all come from kob-wasm.
import type { CondExpiry, CondIntent } from '../intent-cond';
import type { Activation } from '../intent-simple';
import type { CarrierLine, Disclosure, OrderPlan, PlanEnv, PlanIssue } from '../plan-types';
import { baseKind } from '../order-facts';
import type { OrderState } from '../types';
import { daaToUnix, expiryFor, unixToDaaBound } from '../daa';
import { checkPrice, checkSelfTrade as guardSelfTrade, checkTimes } from '../guards';
import { ceilDiv } from '../units';
import { issue, type IssueCode, type IssueParams } from './common-issues';
import { buildCreate } from './common';
import { condIssue, type CondIssueCode } from './cond-issues';

// ---- defaults (matcher.md 10, order-types.md "Defaults")

/**
 * `expiryDaa` of an if-done exit with the default GTC.
 *
 * The exit's expiry is committed inside the entry when it is placed, long before the exit exists,
 * so it cannot be "placement + 90 days". The covenants refund at `min(expiryDaa, UTXO DAA + 90
 * days)`, so a huge value means "90 days idle from the exit's creation" (matcher.md 10.8).
 */
export const GTC_EXIT_EXPIRY_DAA = 1n << 62n;
/** Default trailing update interval in DAA (10 minutes). */
export const DEFAULT_TRAIL_WAIT_DAA = 6_000n;
/** Shortest accepted trailing update interval in DAA (60 s). */
export const MIN_TRAIL_WAIT_DAA = 600n;
/** Trailing updates a keeper reserve is funded for by default. */
export const DEFAULT_EXPECTED_UPDATES = 20;
/** Most trailing updates a keeper reserve can be funded for. */
export const MAX_EXPECTED_UPDATES = 1_000;
/** Repeat count meaning "unlimited": large enough that only the 90-day bound ends it. */
export const UNLIMITED_REPEAT_COUNT = 10_000_000n;
/** Extra KAS (sompi) one merge costs a buy-first repeat take-profit, rounded up. */
export const MERGE_COST_BUY_FIRST = 1_000_000n;
/**
 * Extra KAS (sompi) one merge costs a sell-first repeat take-profit, rounded up.
 *
 * A merge adds the entry input (buy-first, about 3.6 KB) or the entry custody (sell-first, about
 * 7.6 KB) to the take-profit. See matcher.md 6.1 rule 7 and 10.13. `mergeTipRate` spreads the cost
 * over the smallest take-profit fill.
 */
export const MERGE_COST_SELL_FIRST = 1_500_000n;

/**
 * The merge cost as a tip rate (sompi per whole token of `scale` base units).
 *
 * The rate is `ceil(cost * scale / minFill)`, so `floor(minFill * rate / scale) >= cost` and even
 * the smallest take-profit fill (`minFill` base units of the exit) pays `cost`.
 */
export const mergeTipRate = (cost: bigint, scale: bigint, minFill: bigint): bigint => ceilDiv(cost * scale, minFill > 0n ? minFill : 1n);
/** 32 zero bytes as hex. */
export const ZERO32 = '0'.repeat(64);

// ---- issues

/** Collects issues of a plan. `failed` is true when at least one is an error. */
export class IssueLog {
  readonly issues: PlanIssue[] = [];
  add(...i: PlanIssue[]): void { this.issues.push(...i); }
  /** Adds an issue of the shared catalogue (common-issues.ts). */
  shared(code: IssueCode, params?: IssueParams, field?: string, severity?: PlanIssue['severity']): void { this.issues.push(issue(code, params, field, severity)); }
  /** Adds an issue of the conditional catalogue (cond-issues.ts). */
  cond(code: CondIssueCode, params?: IssueParams, field?: string, severity?: PlanIssue['severity']): void { this.issues.push(condIssue(code, params, field, severity)); }
  get failed(): boolean { return this.issues.some((i) => i.severity === 'error'); }
}

// ---- what a plan additionally tells the UI

/** Prices of the legs of a conditional order, as the UI shows them. */
export interface LegSummary {
  /** limit leg (sompi per whole token), null when absent */
  takeProfit: bigint | null;
  /** stop trigger (sompi per whole token), null when absent */
  stop: bigint | null;
  /** worst price of the stop leg per whole token, null without a stop leg */
  stopWorst: bigint | null;
  slipBps: number;
  bandDaa: bigint;
  /** the stop leg's limit as the user gave it (stop-limit) */
  limit: bigint | null;
  /** trigger rule: `minTouch` base units resting at or beyond the stop for `minRestDaa` DAA */
  minTouch: bigint;
  minRestDaa: bigint;
}
/** The trailing numbers of a stop. */
export interface TrailSummary {
  step: bigint;
  gap: bigint;
  waitDaa: bigint;
  waitSeconds: bigint;
  /** the covenant allows at most one update per `waitDaa` (CSV) */
  maxUpdatesPerDay: bigint;
  expectedUpdates: number;
}
/** The keeper reserve of an order that a keeper arms or trails. */
export interface KeeperSummary {
  /** most a keeper takes per arm / trail update */
  tip: bigint;
  /** updates pre-funded (1 arm for a plain stop) */
  expectedUpdates: number;
  /** tip * expectedUpdates */
  reserve: bigint;
  /** where the reserve sits: the carrier (asks, sell-first) or the escrow (bids, buy-first) */
  fundedFrom: 'carrier' | 'escrow';
}
/** The committed exit of an if-done entry, as the UI shows it. */
export interface ExitSummary {
  side: 'sell' | 'buy';
  legs: LegSummary;
  /** the exit's tip, sompi per whole token (a repeat's includes the merge cost) */
  tip: bigint;
  /** the exit's minimum fill (base units) */
  minFill: bigint;
  /** all-in per whole token of the take-profit and of the stop worst price (sell: receives) */
  takeProfitAllIn: bigint | null;
  stopWorstAllIn: bigint | null;
  expiry: 'gtc' | 'gtd';
  /** gtd only: the date every exit ends (DAA and unix seconds), null for gtc */
  expiryDaa: bigint | null;
  expiryUnixSeconds: bigint | null;
  deliveryCarrier: bigint;
  exitCarrier: bigint;
  trail: TrailSummary | null;
  keeper: KeeperSummary | null;
  /** sell-first only: KAS prefunded per whole token beyond the entry proceeds */
  prefund: bigint | null;
}
/** The repeat numbers of a repeating if-done entry. */
export interface RepeatSummary {
  /** K; null = unlimited within the 90-day bound */
  count: bigint | null;
  /** rptAmount = 1 + K * N (N = base units per cycle) */
  rptAmount: bigint;
  /** N: base units per cycle */
  cycleAmount: bigint;
  /** the merge cost added to the exit's tip (sompi per whole token) */
  mergeTip: bigint;
  /** profit per whole token and cycle at the limits (all-in take-profit minus all-in entry) */
  profitPerToken: bigint;
  /** the repeat ends at this DAA at the latest (entry expiry, at most 90 days from placement) */
  untilDaa: bigint;
}
/** Extra structured numbers of a conditional plan, which the UI turns into text with the notes. */
export interface CondSummary {
  type: CondIntent['type'];
  legs: LegSummary | null;
  trail: TrailSummary | null;
  keeper: KeeperSummary | null;
  /** `minFill` is the entry's smallest fill. `minTouch` and `minRestDaa` are the stop's trigger. */
  entry: { price: bigint; stop: bigint | null; bandDaa: bigint; minFill: bigint; maxFills: bigint; minTouch: bigint; minRestDaa: bigint } | null;
  exit: ExitSummary | null;
  repeat: RepeatSummary | null;
}

/** An OrderPlan plus the conditional summary. */
export interface CondPlan extends OrderPlan {
  cond: CondSummary | null;
}

/** A plan that failed before anything was built. */
export const failedPlan = (log: IssueLog): CondPlan => ({ ok: false, issues: log.issues, states: [], request: null, built: null, disclosure: null, cond: null });

// ---- small helpers

/** A price per whole token as the state price. Logs issues and returns null if it cannot be one. */
export function toStatePrice(env: PlanEnv, log: IssueLog, price: bigint, field: string): bigint | null {
  const bad = checkPrice(env.token, price, field);
  if (bad.length > 0) {
    log.add(...bad);
    return null;
  }
  return price;
}

// ---- activation

/** The state's activation and the moment shown to the user. */
export interface ResolvedActivation {
  /** the state's `activeFrom` (0 = active at once) */
  activeFrom: bigint;
  /** disclosure: the moment the order wakes up (null = at once) */
  activatesAt: bigint | null;
}

/**
 * Timed activation exactly like the plain kinds (simple.ts `activationDaa` and `checkTimes`).
 *
 * A moment already past is an info (the order is active at once). One beyond the 90-day horizon is
 * an error. Expiry is not shifted by the activation (GTC is placement + 90 days, as for plain
 * orders). `resolveExpiry({ activeFrom })` refuses an expiry at or before the activation.
 */
export function resolveActivation(env: PlanEnv, log: IssueLog, a: Activation | undefined): ResolvedActivation | null {
  if (!a) return { activeFrom: 0n, activatesAt: null };
  const requested = 'daa' in a ? a.daa : unixToDaaBound(env.clock, a.unixSeconds, 'start');
  const future = requested > env.clock.daa;
  const activeFrom = future ? requested : 0n;
  const bad = checkTimes(env.clock, { activeFrom, expiryDaa: env.clock.daa + 1n, requestedActiveFrom: requested }).filter((i) => i.field === 'activeFrom');
  log.add(...bad);
  if (bad.some((i) => i.severity === 'error')) return null;
  return { activeFrom, activatesAt: future ? requested : null };
}

// ---- expiry

/** The state's expiry, the day-order deadline and the expiry shown to the user. */
export interface ResolvedExpiry {
  /** the state's `expiryDaa` */
  expiryDaa: bigint;
  /** placement-record deadline (day orders) */
  deadline: bigint | null;
  disclosure: Disclosure['expiry'];
}

/**
 * Turns the user's expiry into `expiryDaa` and the disclosure.
 *
 * Placed orders follow the shared rules: GTC is placement + 90 days, nothing goes beyond 90 days
 * and day orders use `kob.dayOrder`. An `exit` is committed inside the entry long before it
 * exists, so its GTC is `GTC_EXIT_EXPIRY_DAA` and a day order is refused. A repeat entry must end
 * within 90 days (matcher.md 10.13), which the 90-day ceiling of every order guarantees.
 */
export function resolveExpiry(env: PlanEnv, log: IssueLog, e: CondExpiry | undefined, o: { field: string; exit?: boolean; activeFrom?: bigint }): ResolvedExpiry | null {
  const spec: CondExpiry = e ?? { kind: 'gtc' };
  const clock = env.clock;
  let expiryDaa: bigint;
  let deadline: bigint | null = null;
  let kind: Disclosure['expiry']['kind'];
  switch (spec.kind) {
    case 'gtc':
      kind = 'gtc';
      expiryDaa = expiryFor('gtc', clock).expiryDaa;
      break;
    case 'gtdDaa':
      kind = 'gtd';
      expiryDaa = spec.daa;
      break;
    case 'gtdUnix':
      kind = 'gtd';
      expiryDaa = unixToDaaBound(clock, spec.atUnixSeconds, 'end');
      break;
    case 'day': {
      if (o.exit) {
        log.cond('COND_EXIT_EXPIRY_DAY', undefined, o.field);
        return null;
      }
      const d = expiryFor('day', clock, { kob: env.kob });
      kind = 'day';
      expiryDaa = d.expiryDaa;
      deadline = d.deadline;
      break;
    }
  }
  const bad = checkTimes(clock, { activeFrom: o.activeFrom ?? 0n, expiryDaa }).filter((i) => i.field === undefined || i.field === 'expiry');
  if (bad.length > 0) {
    log.add(...bad.map((i) => ({ ...i, field: o.field })));
    return null;
  }
  const shown = expiryDaa;
  const committed = o.exit && spec.kind === 'gtc' ? GTC_EXIT_EXPIRY_DAA : expiryDaa;
  return {
    expiryDaa: committed,
    deadline,
    disclosure: {
      kind,
      daa: shown,
      approxUnixSeconds: spec.kind === 'gtdUnix' ? spec.atUnixSeconds : deadline ?? daaToUnix(clock, shown),
      deadlineUnixSeconds: deadline,
    },
  };
}

// ---- guards

/**
 * Self-trade prevention (matcher.md 10.12).
 *
 * `worstAllIn` is the least a sell accepts or the most a buy pays per whole token over every leg
 * that can trade, all-in. It goes to the shared guard with a zero tip. An own active order of the
 * opposite side that could match is an error for the placed order and only a warning for an exit,
 * which is created later.
 */
export function checkSelfTrade(env: PlanEnv, log: IssueLog, side: 'sell' | 'buy', worstAllIn: bigint, o: { exit?: boolean; field: string }): void {
  const found = guardSelfTrade(env.ownOrders, { side, price: worstAllIn, tip: 0n });
  for (const f of found) log.add({ ...f, field: o.field, ...(o.exit ? { severity: 'warning' as const } : {}) });
}

// ---- the placement pipeline

/** Everything an order type decides. `finishPlan` builds it and completes the disclosure. */
export interface CondSpec {
  order: OrderState;
  /** committed exit of an if-done entry (listed last in `states`) */
  exit: OrderState | null;
  /**
   * KAS on the order UTXO, itemised as carriers plus budget lines ('escrow', 'prefund').
   * The order UTXO value is their sum. `finishPlan` adds the custody token carrier and the token
   * change of ask kinds.
   */
  lines: CarrierLine[];
  carrier: bigint;
  side: 'sell' | 'buy';
  /** base units of the placed order (an if-done entry: of one cycle) */
  amount: bigint;
  expiry: ResolvedExpiry;
  /** timed activation of the placed order (disclosure); default none */
  activatesAt?: bigint | null;
  disclosure: Pick<Disclosure, 'limitPrice' | 'allInPrice' | 'allInTotal' | 'expectedPrice' | 'worstPrice' | 'tip' | 'minTouch'>;
  notes: string[];
  cond: CondSummary;
}

const holdsTokens = (o: OrderState): boolean => baseKind(o.kind) === 'KobCondAsk' || baseKind(o.kind) === 'KobIfdAsk';
const linesTotal = (lines: readonly CarrierLine[]): bigint => lines.reduce((s, l) => s + l.amount * BigInt(l.count), 0n);

/** Builds the transaction through the shared `buildCreate` and completes the disclosure. */
export function finishPlan(env: PlanEnv, log: IssueLog, spec: CondSpec): CondPlan {
  const states: OrderState[] = spec.exit ? [spec.order, spec.exit] : [spec.order];
  const value = linesTotal(spec.lines);
  const asks = holdsTokens(spec.order);
  const tokenAmount = asks ? spec.amount : 0n;
  // Every covenant UTXO of this order (custody, delivery, exit) uses the order's carrier.
  const res = buildCreate({ ...env, carrier: spec.carrier }, { order: spec.order, value, tokenAmount, deadline: spec.expiry.deadline });
  log.add(...res.issues);
  if (res.built === null || res.request === null || log.failed) {
    return { ok: false, issues: log.issues, states, request: null, built: null, disclosure: null, cond: spec.cond };
  }

  const carriers: CarrierLine[] = [...spec.lines];
  if (asks) {
    carriers.push({ kind: 'tokenCarrier', amount: res.tokenCarrier, count: 1 });
    if (res.tokenChange > 0n) carriers.push({ kind: 'tokenChangeCarrier', amount: res.tokenCarrier, count: 1, kept: true });
  }
  const kasLocked = linesTotal(carriers.filter((c) => !c.kept));
  const st = spec.order.state;
  const d = spec.disclosure;
  const disclosure: Disclosure = {
    side: spec.side,
    tokenAmount: spec.amount,
    scale: env.token.scale,
    minFill: BigInt(st.minFill),
    minTouch: d.minTouch,
    limitPrice: d.limitPrice,
    allInPrice: d.allInPrice,
    allInTotal: d.allInTotal,
    expectedPrice: d.expectedPrice,
    worstPrice: d.worstPrice,
    tip: d.tip,
    carriers,
    kasLocked,
    tokensEscrowed: tokenAmount,
    fee: BigInt(res.built.fee.fee),
    refundTip: BigInt(st.refundTip),
    keeperTip: 'keeperTip' in st ? BigInt(st.keeperTip) : 0n,
    expiry: spec.expiry.disclosure,
    activatesAt: spec.activatesAt != null ? { daa: spec.activatesAt, approxUnixSeconds: daaToUnix(env.clock, spec.activatesAt) } : null,
    notes: [...new Set([...spec.notes, 'carrierReturned'])],
  };
  return { ok: true, issues: log.issues, states, request: res.request, built: res.built, disclosure, cond: spec.cond };
}
