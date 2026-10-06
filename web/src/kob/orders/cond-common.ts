// Shared plumbing of the conditional / if-done planners (cond.ts, cond-legs.ts, cond-ifd.ts): defaults, issue log, expiry resolution,
// price checks, guards, summaries, and the final `buildCreate` + disclosure step every order type ends in.
//
// Coin selection, the `createOrder` build with adaptive fee reserve and the recover-and-compare self-check are the shared layer of
// common.ts (`buildCreate`); nothing here re-implements protocol logic: states are only ASSEMBLED, the byte encoding, the signing
// plan, fees and the placement record all come from kob-wasm.
import type { CondExpiry, CondIntent } from '../intent-cond';
import type { Activation } from '../intent-simple';
import type { CarrierLine, Disclosure, OrderPlan, PlanEnv, PlanIssue } from '../plan-types';
import { baseKind } from '../order-facts';
import type { OrderState } from '../types';
import { daaToUnix, expiryFor, unixToDaa } from '../daa';
import { checkPrice, checkSelfTrade as guardSelfTrade, checkTimes } from '../guards';
import { ceilDiv } from '../units';
import { issue, type IssueCode, type IssueParams } from './common-issues';
import { buildCreate } from './common';
import { condIssue, type CondIssueCode } from './cond-issues';

// ------------------------------------------------------------------------------------------------ defaults (matcher.md 10, order-types.md "Defaults")

/**
 * `expiryDaa` of an if-done EXIT with the default GTC: the exit's expiry is committed inside the entry when it is placed, long before
 * the exit exists, so it cannot be "placement + 90 days". The covenants refund at `min(expiryDaa, UTXO DAA + 90 days)`: a huge value
 * is exactly "90 days idle from the exit's creation" (matcher.md 10.8).
 */
export const GTC_EXIT_EXPIRY_DAA = 1n << 62n;
/** trailing: default update interval (10 minutes) and the shortest accepted (60 s) */
export const DEFAULT_TRAIL_WAIT_DAA = 6_000n;
export const MIN_TRAIL_WAIT_DAA = 600n;
// The default trigger threshold `minTouch` is the order's own minimum fill (kob-wasm `defaultMinTouch`, matcher.md §10 item 6, founder
// 2026-10-03); the ticket also offers 25% / 50% / 100% of the order's amount and a custom amount, resolved to base units before the intent.
export const DEFAULT_EXPECTED_UPDATES = 20;
export const MAX_EXPECTED_UPDATES = 1_000;
/** repeat: "unlimited" is K large enough that only the 90-day bound ends it (each cycle takes at least two transactions) */
export const UNLIMITED_REPEAT_COUNT = 10_000_000n;
/**
 * matcher.md 6.1 rule 7 / 10.13: a merge adds the entry input (buy-first: about +3.6 KB, +0.007 KAS) and, sell-first, the entry custody
 * (about +7.6 KB, +0.015 KAS) to the take-profit (protocol v2.6 measurements, to be re-measured for v3). Rounded up: the KAS one merge costs;
 * `mergeTipRate` spreads it over the smallest take-profit fill.
 */
export const MERGE_COST_BUY_FIRST = 1_000_000n;
export const MERGE_COST_SELL_FIRST = 1_500_000n;

/**
 * The merge cost as a tip rate (sompi per whole token of `scale` base units) that pays `cost` even on the smallest take-profit fill
 * (`minFill` base units of the exit): `ceil(cost * scale / minFill)`, so `floor(minFill * rate / scale) >= cost`.
 */
export const mergeTipRate = (cost: bigint, scale: bigint, minFill: bigint): bigint => ceilDiv(cost * scale, minFill > 0n ? minFill : 1n);
export const ZERO32 = '0'.repeat(64);

// ------------------------------------------------------------------------------------------------ issues

/** Collects issues; `failed` = at least one error. */
export class IssueLog {
  readonly issues: PlanIssue[] = [];
  add(...i: PlanIssue[]): void { this.issues.push(...i); }
  /** issue of the shared catalogue (common-issues.ts) */
  shared(code: IssueCode, params?: IssueParams, field?: string, severity?: PlanIssue['severity']): void { this.issues.push(issue(code, params, field, severity)); }
  /** issue of the conditional catalogue (cond-issues.ts) */
  cond(code: CondIssueCode, params?: IssueParams, field?: string, severity?: PlanIssue['severity']): void { this.issues.push(condIssue(code, params, field, severity)); }
  get failed(): boolean { return this.issues.some((i) => i.severity === 'error'); }
}

// ------------------------------------------------------------------------------------------------ what a plan additionally tells the UI

export interface LegSummary {
  /** limit leg (sompi per whole token), null when absent */
  takeProfit: bigint | null;
  /** stop trigger (sompi per whole token), null when absent */
  stop: bigint | null;
  /** worst price of the stop leg (per whole token): sell `stop - floor(stop*slipBps/10^4)`, buy `stop + ...`; null without a stop leg */
  stopWorst: bigint | null;
  slipBps: number;
  bandDaa: bigint;
  /** the stop leg's limit as the user gave it (stop-limit) */
  limit: bigint | null;
  /** trigger rule (touch): a fill of at least `minTouch` base units of a resting order at or beyond the stop, exposed for `minRestDaa` DAA */
  minTouch: bigint;
  minRestDaa: bigint;
}
export interface TrailSummary {
  step: bigint;
  gap: bigint;
  waitDaa: bigint;
  waitSeconds: bigint;
  /** the covenant allows at most one update per `waitDaa` (CSV) */
  maxUpdatesPerDay: bigint;
  expectedUpdates: number;
}
export interface KeeperSummary {
  /** most a keeper takes per arm / trail update */
  tip: bigint;
  /** updates pre-funded (1 arm for a plain stop) */
  expectedUpdates: number;
  /** tip * expectedUpdates */
  reserve: bigint;
  /** where the reserve sits: inside the order's carrier (asks, sell-first entries) or added to the escrow (bids, buy-first entries) */
  fundedFrom: 'carrier' | 'escrow';
}
export interface ExitSummary {
  side: 'sell' | 'buy';
  legs: LegSummary;
  /** the exit's tip, sompi per whole token (a repeat's includes the merge cost) */
  tip: bigint;
  /** the exit's minimum fill (base units) */
  minFill: bigint;
  /** all-in per whole token of the take-profit / of the stop leg's worst price (sell: receives, buy: pays) */
  takeProfitAllIn: bigint | null;
  stopWorstAllIn: bigint | null;
  expiry: 'gtc' | 'gtd';
  /** gtd only: the date every exit ends (DAA and approximate unix seconds); null for gtc (90 days idle from each exit's creation) */
  expiryDaa: bigint | null;
  expiryUnixSeconds: bigint | null;
  deliveryCarrier: bigint;
  exitCarrier: bigint;
  trail: TrailSummary | null;
  keeper: KeeperSummary | null;
  /** sell-first only: KAS prefunded per whole token beyond the entry proceeds */
  prefund: bigint | null;
}
export interface RepeatSummary {
  /** K; null = unlimited within the 90-day bound */
  count: bigint | null;
  /** rptAmount = 1 + K * N (N = base units per cycle) */
  rptAmount: bigint;
  /** N: base units per cycle */
  cycleAmount: bigint;
  /** the merge cost added to the exit's tip (sompi per whole token) */
  mergeTip: bigint;
  /** profit per whole token and cycle at the limits (all-in take-profit minus all-in entry, buy-first; mirrored sell-first) */
  profitPerToken: bigint;
  /** the repeat ends at this DAA at the latest (entry expiry, at most 90 days from placement) */
  untilDaa: bigint;
}
/** Extra structured numbers of a conditional plan (the UI turns them and `disclosure.notes` into sentences). */
export interface CondSummary {
  type: CondIntent['type'];
  legs: LegSummary | null;
  trail: TrailSummary | null;
  keeper: KeeperSummary | null;
  /** `minFill` the entry's smallest fill (base units); `minTouch` / `minRestDaa`: the stop entry's trigger rule (touch), as for a stop leg */
  entry: { price: bigint; stop: bigint | null; bandDaa: bigint; minFill: bigint; maxFills: bigint; minTouch: bigint; minRestDaa: bigint } | null;
  exit: ExitSummary | null;
  repeat: RepeatSummary | null;
}

export interface CondPlan extends OrderPlan {
  cond: CondSummary | null;
}

export const failedPlan = (log: IssueLog): CondPlan => ({ ok: false, issues: log.issues, states: [], request: null, built: null, disclosure: null, cond: null });

// ------------------------------------------------------------------------------------------------ small helpers

/** A price per whole token as the state price (tick and range checked); null (issues logged) when it cannot be one. */
export function toStatePrice(env: PlanEnv, log: IssueLog, price: bigint, field: string): bigint | null {
  const bad = checkPrice(env.token, price, field);
  if (bad.length > 0) {
    log.add(...bad);
    return null;
  }
  return price;
}

// ------------------------------------------------------------------------------------------------ activation

export interface ResolvedActivation {
  /** the state's `activeFrom` (0 = active at once) */
  activeFrom: bigint;
  /** disclosure: the moment the order wakes up (null = now) */
  activatesAt: bigint | null;
}

/**
 * Timed activation exactly like the plain kinds (simple.ts `activationDaa` + `checkTimes`): a moment already past is an info (the order is active at
 * once), one beyond the 90-day horizon an error. Expiry is NOT shifted by it (GTC = placement + 90 days, as for plain orders); expiry <= activation is refused by
 * `resolveExpiry({ activeFrom })`.
 */
export function resolveActivation(env: PlanEnv, log: IssueLog, a: Activation | undefined): ResolvedActivation | null {
  if (!a) return { activeFrom: 0n, activatesAt: null };
  const requested = 'daa' in a ? a.daa : unixToDaa(env.clock, a.unixSeconds);
  const future = requested > env.clock.daa;
  const activeFrom = future ? requested : 0n;
  const bad = checkTimes(env.clock, { activeFrom, expiryDaa: env.clock.daa + 1n, requestedActiveFrom: requested }).filter((i) => i.field === 'activeFrom');
  log.add(...bad);
  if (bad.some((i) => i.severity === 'error')) return null;
  return { activeFrom, activatesAt: future ? requested : null };
}

// ------------------------------------------------------------------------------------------------ expiry

export interface ResolvedExpiry {
  /** the state's `expiryDaa` */
  expiryDaa: bigint;
  /** placement-record deadline (day orders) */
  deadline: bigint | null;
  disclosure: Disclosure['expiry'];
}

/**
 * Turns the user's expiry into `expiryDaa` and the disclosure. Placed orders follow the shared rules (GTC = placement + 90 days,
 * nothing beyond 90 days, day orders by `kob.dayOrder`). `exit`: committed inside the entry long before it exists, so its GTC is the
 * "never" of `GTC_EXIT_EXPIRY_DAA` and a day order is refused; a repeat entry must end within 90 days (matcher.md 10.13), which the
 * 90-day ceiling of every order already guarantees.
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
      expiryDaa = unixToDaa(clock, spec.atUnixSeconds);
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

// ------------------------------------------------------------------------------------------------ guards

/**
 * Self-trade prevention (matcher.md 10.12): `worstAllIn` is the least a sell accepts / the most a buy pays per whole token over every leg that
 * can trade, all-in (tip included; passed to the shared guard with a zero tip). An own ACTIVE order of the opposite side that could
 * match it is an error for the placed order and only a warning for an exit (which is created later).
 */
export function checkSelfTrade(env: PlanEnv, log: IssueLog, side: 'sell' | 'buy', worstAllIn: bigint, o: { exit?: boolean; field: string }): void {
  const found = guardSelfTrade(env.ownOrders, { side, price: worstAllIn, tip: 0n });
  for (const f of found) log.add({ ...f, field: o.field, ...(o.exit ? { severity: 'warning' as const } : {}) });
}

// ------------------------------------------------------------------------------------------------ the placement pipeline

/** Everything an order type decides; `finishPlan` builds it and completes the disclosure. */
export interface CondSpec {
  order: OrderState;
  /** committed exit of an if-done entry (listed last in `states`) */
  exit: OrderState | null;
  /**
   * KAS on the order UTXO, itemised: carriers plus budget lines ('escrow', 'prefund'). The order UTXO value is their sum; the custody
   * token carrier and token change (ask kinds) are added by `finishPlan`.
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

/** Builds the transaction (shared `buildCreate`) and completes the disclosure. Never throws for user-input problems. */
export function finishPlan(env: PlanEnv, log: IssueLog, spec: CondSpec): CondPlan {
  const states: OrderState[] = spec.exit ? [spec.order, spec.exit] : [spec.order];
  const value = linesTotal(spec.lines);
  const asks = holdsTokens(spec.order);
  const tokenAmount = asks ? spec.amount : 0n;
  // every covenant UTXO of this order (custody, delivery, exit) uses the order's carrier
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
