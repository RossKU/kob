// The soak checker: every `checker.intervalSec` it reads the indexer (exec-a, the bots' indexer), the second indexer (exec-b), the node
// and the files the bots / executors write, checks the protocol invariants incrementally (cursors in run/state/checker.json) and appends
// each violation ONCE to run/incidents.jsonl (logged at `error`). The pure rules are in rules.ts (formulas documented there).
//
//  1 custody     (also the amount accounting: filled + left == initial, base units) every live order's current outpoint is in the NODE's UTXO set at P2SH(its proven state) with its covenant id; every
//                live ask-side order with an amount left has exactly one live custody token UTXO of amountLeft base units (indexer), which is in
//                the node's UTXO set at the token program's P2SH of its state. Orders touched in the last 300 DAA are skipped; a problem
//                must persist over two rounds (>= 45 s) with the same outpoints (indexer / node timing is never reported).
//  2 stray       a stray that disappears from /v1/strays must have been spent by a `cancel` event of its order.
//  3 all-in      every fill (paged from /v1/fills): quote within the order's worst bound, maker payout >= the all-in minimum
//                (asks), sell-first entry exits funded with the proceeds.
//  4 ioc-fok     no IOC / FOK live 1200 DAA past its kill time (by the indexer's own cursor); FOK never partial.
//  5 trigger     every arm / trail event backed by a fill (n > 0) in the SAME transaction of a plain resting KobAsk / KobBid of the same
//                token and scale (touch trigger): right side and price vs the stop / trail step, amount (base units) >= minTouch,
//                slope 0, exposed >= minRestDaa DAA before the tx (warn when the exposure cannot be read), tx DAA >= activeFrom.
//  6 repeat      entry amount + booked exit amounts <= N (base units); every rearm pairs with a merged take-profit fill of its exit; cycle profit.
//  7 x402        payer's 200s vs the facilitator ledger (settled once, no double settlement).
//  8 fee         txs.jsonl: fee == feeRate x max(compute, transientNormalized) in relay mode, the recorded feeRate within [100, config fees.maxRate].
//  9 agree       exec-a and exec-b list the same orders and fills up to min(cursors) - 200 DAA, and agree on sampled orders.
// 10 health      follower state / lag / progress of both indexers; node sync, book staleness, step age of both executors.
// 11 pair        every pair order fill (KobPair, KobCondPair, KobIfdPair) carries NO price (founder rule: pair fills are volume only) and
//                honours the order's guarantee at its quote: an ask receives >= ceil(n x q / scale(A)) of B, a bid pays <= floor(...);
//                every pair arm / trail names evidence of one of the two modes (two KAS-book fills, or a resting KobPair filled).
// 12 supply      per soak token: live token UTXOs (indexer B while it follows, else A, + holdings it never saw, on the node) add up to the supply.
// 13 scenarios   the bots' scenario records (run/state/scenarios.jsonl, scenario-rules.ts): a limit ladder's rungs are independent orders of
//                one size (`ladder`), cancelling one rung touches no other; a GTC renewal keeps its terms with a later expiry and a pair
//                amend keeps pair, side and size (`replace`); a cancel-all ends every order (`cancel-all`); a token merge ends in one
//                output of the inputs' total (`merge`); x402 retries: at most one accepted attempt per payment, a lost answer replayed (`x402`).
// Every soak token is checked (TUSD and the asset tokens); fills are read across tokens, so a transaction filling several books and
// pair orders is counted once with every book it touched.
import { closeSync, existsSync, openSync, readdirSync, readFileSync, readSync, rmSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { HttpIndexer } from '@/data/indexer';
import type { EventView, HealthView, OrderView } from '@/data/indexer-types';
import { spkStringToAddress } from '@/data/kaspa-sdk';
import type { AnyState, TokenProgram, TokenState } from '@/kob/types';
import { genesisOf, slotOfToken, type Env } from '../env';
import { ASSET_SLOTS } from '../config';
import { SOAK_PROGRAM } from '../market';
import { errText, logger } from '../log';
import { metricsHealth } from '../indexer-gate';
import { sleep } from '../util';
import { IncidentLog } from './incidents.ts';
import { writeReport } from './report.ts';
import {
  baseKind,
  checkFeeLine,
  DEFAULT_FEE_BOUNDS,
  type FeeBounds,
  checkFill,
  isImportedHistory,
  termsAtFill,
  checkFokEvents,
  checkIfdAskExitFunding,
  checkPairFill,
  checkPairEvidence,
  isPairContract,
  checkAmountBalance,
  checkKill,
  keepersCanAct,
  nextFollowingSince,
  stopMayHaveMoved,
  sweepPages,
  checkSupply,
  big,
  quoteOf,
  checkRearm,
  checkRepeatCycle,
  checkRepeatAmount,
  checkTrigger,
  checkX402,
  diffOrder,
  diffSets,
  type EvidenceFact,
  healthIssues,
  isActive,
  isAskSide,
  metricIssues,
  parseProm,
  partialTerms,
  Persist,
  type Payment,
  type TxLine,
} from './rules.ts';
import { loadState, saveState, type CheckerState, type LegacyTxSums, type OrderMemo, type TxSums } from './store.ts';
import {
  amendedAfter, checkCancelAll, checkLadder, checkLadderCancel, checkMerge, checkMergeRecord, checkPlannedReplacement, checkReplacement, checkX402Retries, repricedRungs,
  SCENARIO_GRACE_MS, type RetryPayment, type Verdict,
} from './scenario-rules.ts';
import { scenariosPath, type MergeRecord, type ScenarioRecord } from '../scenarios';
import { INV, type EventLike, type HealthLike, type OrderLike, type Violation } from './types.ts';

const log = logger('checker');

/** orders touched this recently (DAA, by the indexer's cursor) are not checked against the node yet */
const TOUCH_GRACE_DAA = 300;
const KILL_GRACE_DAA = 1200n;
const REPORT_EVERY_MS = 5 * 60_000;

interface Ctx {
  env: Env;
  A: HttpIndexer;
  B: HttpIndexer | null;
  nameA: string;
  nameB: string;
  /** the primary soak token (TUSD) */
  token: string;
  /** every soak token (primary first) */
  tokens: string[];
  st: CheckerState;
  persist: Persist;
  inc: IncidentLog;
  /** custody lookups by order id, valid while the order's last_daa is unchanged */
  custodyCache: Map<string, { lastDaa: number; items: TokenUtxo[] }>;
  /**
   * indexer A's cursor DAA since which executor A's keepers could act without a break (null while they cannot: see `keepersCanAct`):
   * the IOC / FOK deadline starts no earlier. Sampled every round and every `GATE_MS` between rounds (a minute sample missed lag episodes)
   */
  followingSince: bigint | null;
  /** the fast sweep's own state (see `sweepTerms`) */
  sweep: SweepState;
}

interface SweepState {
  /** a sweep is running: the next tick is skipped (requests slower than the tick stacked up) */
  busy: boolean;
  /** per token: the ids the previous sweep listed (where this one may stop) */
  listed: Map<string, Set<string>>;
  /** per token: live orders the previous sweep listed without their proven state (read again while they are among the newest) */
  unresolved: Map<string, Set<string>>;
}

interface TokenUtxo {
  txid: string;
  index: number;
  amount: string;
  role: string;
  spent: boolean;
  spent_txid: string | null;
  state?: TokenState;
  program?: string;
}

const like = (v: OrderView): OrderLike => v as unknown as OrderLike;
const evLike = (e: EventView): EventLike => e as unknown as EventLike;
/** an order's terms of the moment: its proven state, else (closed: no state) the view's current terms, which follow in-place amends */
function currentTerms(v: OrderView | null): Record<string, string> | null {
  if (!v) return null;
  if (v.state?.state) return v.state.state as unknown as Record<string, string>;
  const o: Record<string, string> = {};
  const set = (k: string, x: unknown) => { if (x !== null && x !== undefined) o[k] = String(x); };
  set('price', v.price);
  set('tip', (v as { tip?: unknown }).tip);
  set('tif', (v as { tif?: unknown }).tif);
  set('expiryDaa', (v as { expiry_daa?: unknown }).expiry_daa);
  set('activeFrom', (v as { active_from?: unknown }).active_from);
  return Object.keys(o).length ? o : null;
}
const detailOf = (e: { detail?: unknown }): Record<string, unknown> => (e.detail && typeof e.detail === 'object' ? (e.detail as Record<string, unknown>) : {});
const stat = (x: Ctx, k: string, by = 1) => (x.st.stats[k] = (x.st.stats[k] ?? 0) + by);

function report(x: Ctx, v: Violation): void {
  x.inc.report(v);
}

async function step<T>(name: string, f: () => Promise<T>): Promise<T | undefined> {
  try {
    return await f();
  } catch (e) {
    log.warn('check step failed', { step: name, error: errText(e), stack: e instanceof Error ? e.stack?.split('\n').slice(1, 4).join(' | ') : undefined });
    return undefined;
  }
}

export async function runChecker(env: Env): Promise<void> {
  const cfg = env.cfg;
  const token = env.state.token?.covenantId;
  if (!token) throw new Error('the soak token is not issued yet (run `soak setup`)');
  // every issued soak token: TUSD, then the asset tokens (TETH, TBTC), then the external tokens the traders trade (a KRON token: no supply check)
  const tokens = [
    token,
    ...ASSET_SLOTS.flatMap((s) => (env.state[s] ? [env.state[s]!.covenantId] : [])),
    ...(cfg.externalTokens ?? []).flatMap((t) => (t.trade && t.trade.bookShare > 0 ? [t.covenantId] : [])),
  ];
  const statePath = join(cfg.runPath, 'state', 'checker.json');
  const st = loadState(statePath);
  // run/recheck-triggers (touch it, restart the checker): re-check every arm / trail / triggered fill of the conditional orders memoized
  const recheck = join(cfg.runPath, 'recheck-triggers');
  if (existsSync(recheck)) {
    for (const [id, m] of Object.entries(st.orders)) {
      if (!/^Kob(Cond|Ifd)/.test(m.c)) continue;
      const n = m.ev.length;
      m.ev = m.ev.filter((k) => !/^(arm|trail|fill):/.test(k));
      if (m.ev.length !== n && !st.retry.includes(id)) st.retry.push(id);
    }
    for (const k of ['triggers', 'triggersOk', 'triggersUnverifiable']) st.stats[k] = 0;
    rmSync(recheck, { force: true });
    log.info('re-checking triggers', { orders: st.retry.length });
  }
  const inc = new IncidentLog(join(cfg.runPath, 'incidents.jsonl'), (i) => log.error('INCIDENT', { invariant: i.invariant, severity: i.severity, subject: i.subject, detail: i.detail }));
  const exName = (url: string | undefined, dflt: string) => cfg.executors.find((e) => url?.includes(e.api))?.name ?? dflt;
  const x: Ctx = {
    env,
    A: env.indexer,
    B: cfg.indexerUrlB ? new HttpIndexer({ baseUrl: cfg.indexerUrlB, timeoutMs: 15_000 }) : null,
    nameA: exName(cfg.indexerUrl, 'A'),
    nameB: exName(cfg.indexerUrlB, 'B'),
    token,
    tokens,
    st,
    persist: new Persist(st.persist),
    inc,
    custodyCache: new Map(),
    followingSince: null,
    sweep: { busy: false, listed: new Map(), unresolved: new Map() },
  };
  log.info('checker starting', { tokens, indexer: cfg.indexerUrl, indexerB: cfg.indexerUrlB, rounds: st.rounds, orders: Object.keys(st.orders).length });
  // the fast sweep runs between rounds (the round's scan then finds the terms in st.early); one at a time: a tick while the previous sweep
  // still waits for the indexer is skipped
  setInterval(() => {
    if (x.sweep.busy) return;
    x.sweep.busy = true;
    void sweepTerms(x)
      .catch((e) => log.debug('sweep failed', { error: errText(e) }))
      .finally(() => (x.sweep.busy = false));
  }, SWEEP_MS);
  // the keepers' gate between rounds: a book that lags the node for a minute between two round samples restarts the kill deadline. Sampled
  // from GET /v1/metrics (the follower's own numbers, no database work; /v1/health counts the whole store on every request), /v1/health
  // when the metrics cannot be read
  let gateBusy = false;
  setInterval(() => {
    if (gateBusy) return;
    gateBusy = true;
    void gateSample(x)
      .then((h) => trackKeepers(x, h))
      .catch((e) => log.debug('keeper gate sample failed', { error: errText(e) }))
      .finally(() => (gateBusy = false));
  }, GATE_MS);
  let lastReport = 0;
  for (;;) {
    const t0 = Date.now();
    try {
      await round(x);
    } catch (e) {
      log.warn('round failed', { error: errText(e) });
    }
    st.persist = x.persist.data;
    try {
      saveState(statePath, st);
    } catch (e) {
      log.warn('state save failed', { error: errText(e) });
    }
    if (Date.now() - lastReport >= REPORT_EVERY_MS) {
      try {
        await writeReport(env);
        lastReport = Date.now();
      } catch (e) {
        log.warn('report failed', { error: errText(e) });
      }
    }
    log.info('round done', { round: st.rounds, ms: Date.now() - t0, orders: Object.keys(st.orders).length, fills: st.fills.count });
    await sleep(Math.max(5_000, cfg.checker.intervalSec * 1000 - (Date.now() - t0)));
  }
}

/**
 * The keepers act only on a book that follows the node: the kill deadline runs from the moment they can (see checkKill). Their gate is
 * indexer A's lag tolerance (/v1/health) and executor A's matcher metrics (book stale against the live node, waiting for the indexer, paused).
 */
function trackKeepers(x: Ctx, hA: HealthLike | null): void {
  // an unanswered health request (a busy API) says nothing about the gate: the sample is skipped
  if (!hA) return;
  const can = keepersCanAct(hA as HealthLike & { within_lag_tolerance?: boolean }, readMetrics(x, x.nameA), Date.now() / 1000);
  x.followingSince = nextFollowingSince(x.followingSince, can, BigInt(hA.cursor_daa));
}

const GATE_MS = 5_000;

/** indexer A's lag tolerance and cursor for the keepers' gate: from its metrics, else from its health (null: no answer, the sample is skipped) */
async function gateSample(x: Ctx): Promise<(HealthLike & { within_lag_tolerance?: boolean }) | null> {
  const m = await metricsHealth(x.env.cfg.indexerUrl, 10_000).catch(() => null);
  if (!m) return health(x.A);
  return { state: m.state ?? 'not within the lag tolerance', cursor_daa: m.cursor_daa, node_daa: null, lag_daa: m.lag_daa, within_lag_tolerance: m.within_lag_tolerance };
}

async function health(ix: HttpIndexer | null): Promise<(HealthView & HealthLike) | null> {
  if (!ix) return null;
  try {
    return (await ix.health({ timeoutMs: 10_000 })) as HealthView & HealthLike;
  } catch {
    return null;
  }
}

async function round(x: Ctx): Promise<void> {
  const now = Date.now();
  x.st.rounds++;
  x.st.stats.rounds = x.st.rounds;
  const [hA, hB] = await Promise.all([health(x.A), health(x.B)]);
  await step('health', async () => checkHealth(x, hA, hB, now));
  trackKeepers(x, hA);
  if (!hA) return;
  const aOk = hA.state === 'following' && (hA.lag_daa ?? 0) <= 100;
  const scan = await step('orders', () => scanOrders(x, hA));
  if (!scan) return;
  await step('kill', async () => {
    for (const v of scan.views.values()) for (const viol of checkKill(like(v), BigInt(hA.cursor_daa), KILL_GRACE_DAA, x.followingSince)) report(x, viol);
  });
  await step('events', () => eventsStep(x, scan));
  await step('repeat', () => repeatStep(x, scan.views, now));
  await step('fills', () => fillsStep(x));
  if (aOk) await step('custody', () => custodyStep(x, scan.views, hA.cursor_daa, now));
  await step('balance', async () => balanceStep(x, scan.views, now));
  await step('strays', () => straysStep(x, now));
  await step('x402', async () => x402Step(x, now));
  await step('fees', async () => feesStep(x));
  // the supply listing (every live token UTXO of every soak token, ~10 pages a round) reads indexer B while it follows (the bots and the web
  // app read A), A while B does not; the two list the same holdings (invariant 9 compares their orders and fills)
  const bOk = !!x.B && !!hB && hB.state === 'following' && (hB.lag_daa ?? 0) <= 100;
  if (bOk || aOk) await step('supply', () => supplyStep(x, now, bOk ? { url: x.env.cfg.indexerUrlB!, name: x.nameB } : { url: x.env.cfg.indexerUrl, name: x.nameA }));
  await step('scenarios', () => scenariosStep(x, now));
  if (x.B && hB) await step('agree', () => agreeStep(x, hA, hB, scan, now));
}

// ------------------------------------------------------------------------------------------------ 10. health

function readMetrics(x: Ctx, name: string): Record<string, number> | null {
  const p = join(x.env.cfg.runPath, name, 'metrics.prom');
  return existsSync(p) ? parseProm(readFileSync(p, 'utf8')) : null;
}

function checkHealth(x: Ctx, hA: HealthLike | null, hB: HealthLike | null, now: number): void {
  const pairs: [string, HealthLike | null][] = [[x.nameA, hA]];
  if (x.B) pairs.push([x.nameB, hB]);
  for (const [name, h] of pairs) {
    const issues = healthIssues(h, now);
    const kinds = issues.map((i) => i.split(' ')[0]).join(',');
    const p = x.persist.observe(`health:${name}`, issues.length > 0, now, 3, 120_000, kinds);
    if (p.fire) {
      report(x, { invariant: INV.health, severity: 'error', subject: `${name}:${kinds}@${new Date(p.since).toISOString()}`, detail: { indexer: name, problem: `indexer unhealthy: ${issues.join('; ')}`, issues, health: h } });
    }
  }
  const nowSec = now / 1000;
  for (const e of x.env.cfg.executors) {
    const m = readMetrics(x, e.name);
    const issues = metricIssues(m, nowSec);
    const kinds = issues.map((i) => i.split(' ').slice(0, 2).join(' ')).join(',');
    const p = x.persist.observe(`metrics:${e.name}`, issues.length > 0, now, 5, 240_000, kinds);
    if (p.fire) report(x, { invariant: INV.health, severity: 'error', subject: `${e.name}:${kinds}@${new Date(p.since).toISOString()}`, detail: { executor: e.name, problem: `executor unhealthy: ${issues.join('; ')}`, issues } });
    const rej = m?.kob_matcher_rejected_total ?? 0;
    if (rej >= 1) {
      const bucket = 2 ** Math.floor(Math.log2(rej));
      report(x, { invariant: INV.health, severity: 'warn', subject: `${e.name}:rejected>=${bucket}`, detail: { executor: e.name, rejected: rej, problem: 'matcher transactions rejected by the node (not a lost race): investigate the executor log' } });
    }
  }
}

// ------------------------------------------------------------------------------------------------ order scan

interface Scan {
  views: Map<string, OrderView>;
  changed: Set<string>;
  cursor: number;
}

const WATCHED = new Set(['KobCondAsk', 'KobCondBid', 'KobIfdBid', 'KobIfdAsk', 'KobCondPair', 'KobIfdPair']);

function trimTerms(s: AnyState | null | undefined): Record<string, string> | null {
  if (!s) return null;
  const { exitState: _x, ...rest } = s.state as unknown as Record<string, string>;
  return rest;
}

function memoize(x: Ctx, v: OrderView): OrderMemo {
  const old = x.st.orders[v.covenant_id];
  const m: OrderMemo = {
    c: v.contract,
    side: v.side,
    tif: v.tif ?? null,
    status: v.status,
    lastDaa: v.last_daa,
    genesisDaa: v.genesis?.daa ?? v.last_daa,
    initialAmount: v.initial_amount ?? old?.initialAmount ?? null,
    terms: trimTerms(v.state) ?? fullTerms(old?.terms) ?? x.st.early?.[v.covenant_id]?.terms ?? old?.terms ?? partialTerms(v),
    ev: old?.ev ?? [],
    active: isActive(v.status),
    watch: v.tif === 2 || WATCHED.has(baseKind(v.contract)),
  };
  if (isImportedHistory(v) || old?.imported) m.imported = true;
  const tk = tokenIxs(x.st, v);
  if (tk.length) m.tk = tk;
  else if (old?.tk) m.tk = old.tk;
  x.st.orders[v.covenant_id] = m;
  if (fullTerms(m.terms) && x.st.early) delete x.st.early[v.covenant_id];
  return m;
}

const fullTerms = (t: Record<string, string> | null | undefined): Record<string, string> | null => (t && t._partial !== '1' ? t : null);

/** an order's token(s) (its token; a pair order also its token B) as indexes into `st.tokenIds`, which grows by the tokens not in it yet */
function tokenIxs(st: CheckerState, v: OrderView): number[] {
  const ids = (st.tokenIds ??= []);
  const out: number[] = [];
  for (const t of [v.token, v.pair?.base, v.pair?.quote]) {
    if (!t) continue;
    const id = t.toLowerCase();
    let i = ids.indexOf(id);
    if (i < 0) i = ids.push(id) - 1;
    if (!out.includes(i)) out.push(i);
  }
  return out;
}

/** the fast sweep's page size (a few orders per token are placed between two sweeps) and how deep it reads at most (the newest orders) */
const SWEEP_PAGE = 25;
const SWEEP_DEPTH = 200;

/**
 * The fast sweep: the full terms of the newest orders of each token, kept until a round memoizes the order (see `CheckerState.early`).
 * Incremental: the listing is newest first (the indexer's block sequence), so the orders below the newest one the previous sweep listed were
 * all looked at before; it reads small pages down to that order, and on (to SWEEP_DEPTH) only while a live order listed without its proven
 * state is still to be read again. Every order is looked at as it was when one 200-order page per token was read every SWEEP_MS.
 */
async function sweepTerms(x: Ctx): Promise<void> {
  const early = (x.st.early ??= {});
  for (const token of x.tokens) {
    const unresolved = new Set<string>();
    const listed = await sweepPages(
      (cursor) => x.A.orders({ token, limit: SWEEP_PAGE, cursor }),
      x.sweep.listed.get(token),
      x.sweep.unresolved.get(token) ?? new Set(),
      SWEEP_DEPTH,
      (v) => {
        const id = v.covenant_id;
        if (early[id] || fullTerms(x.st.orders[id]?.terms)) return;
        const t = trimTerms(v.state);
        if (t) early[id] = { terms: t, daa: v.genesis?.daa ?? v.last_daa };
        else if (isActive(v.status)) unresolved.add(id);
      },
    );
    x.sweep.listed.set(token, listed);
    x.sweep.unresolved.set(token, unresolved);
  }
}

async function memoFor(x: Ctx, id: string): Promise<OrderMemo | null> {
  const m = x.st.orders[id];
  const e = x.st.early?.[id]?.terms;
  if (m && e && !fullTerms(m.terms)) m.terms = e;
  if (m?.terms) return m;
  const v = await x.A.order(id);
  return v ? memoize(x, v) : (m ?? null);
}

async function pageOrders(ix: HttpIndexer, token: string, floorDaa: number, maxPages = 500): Promise<OrderView[]> {
  const out: OrderView[] = [];
  let cursor: string | undefined;
  for (let i = 0; i < maxPages; i++) {
    const p = await ix.orders({ token, limit: 200, cursor });
    out.push(...p.items);
    const last = p.items[p.items.length - 1];
    if (!p.next_cursor || !last) break;
    // keyset order is the indexer's block sequence: stop well below the floor
    if (floorDaa > 0 && last.genesis.daa < floorDaa - 1000) break;
    cursor = p.next_cursor;
  }
  return out;
}

async function scanOrders(x: Ctx, hA: HealthLike): Promise<Scan> {
  const { A, st } = x;
  const views = new Map<string, OrderView>();
  for (const token of x.tokens) {
    const active = await A.allOrders({ token, status: 'active', limit: 200 }, { maxPages: 100 });
    for (const v of active) views.set(v.covenant_id, v);
  }
  // orders created since the last scan (full history on the first round): catches orders created and terminated between rounds; a
  // token added later (the second soak token) starts from its own full history
  for (const token of x.tokens) {
    const floor = st.scanDaaByToken?.[token] === undefined && token !== x.token ? 0 : st.scanDaa;
    for (const v of await pageOrders(A, token, floor)) if (!views.has(v.covenant_id) && v.genesis.daa >= floor - 1000) views.set(v.covenant_id, v);
  }
  // orders that left the active set, and deferred ones
  const refetch = new Set<string>(st.retry);
  for (const [id, m] of Object.entries(st.orders)) if (m.active && !views.has(id)) refetch.add(id);
  for (const id of refetch) {
    if (views.has(id)) continue;
    const v = await A.order(id);
    if (v) views.set(id, v);
    else if (st.orders[id]?.active) delete st.orders[id]; // reorged away
  }
  const changed = new Set<string>();
  for (const [id, v] of views) {
    const m = st.orders[id];
    if (!m) {
      if (v.tif === 1 || v.tif === 2) stat(x, 'killChecked');
    }
    if (!m || m.lastDaa !== v.last_daa || m.status !== v.status || st.retry.includes(id)) changed.add(id);
    memoize(x, v);
  }
  st.retry = [];
  st.scanDaa = Math.max(0, hA.cursor_daa - 600);
  st.scanDaaByToken = Object.fromEntries(x.tokens.map((t) => [t, st.scanDaa]));
  for (const [id, e] of Object.entries(st.early ?? {})) if (hA.cursor_daa - e.daa > 72_000) delete st.early![id];
  // tombstones: forget the terms and event keys of orders terminated more than ~2 h ago
  for (const m of Object.values(st.orders)) {
    if (!m.active && hA.cursor_daa - m.lastDaa > 72_000 && (m.terms || m.ev.length)) {
      m.terms = null;
      m.ev = [];
    }
  }
  return { views, changed, cursor: hA.cursor_daa };
}

async function allEvents(ix: HttpIndexer, id: string): Promise<EventView[]> {
  const out: EventView[] = [];
  let after: string | number | undefined;
  for (let i = 0; i < 100; i++) {
    const p = await ix.orderEvents(id, { after, limit: 200 });
    if (!p) break;
    out.push(...p.items);
    if (!p.next_cursor) break;
    after = p.next_cursor;
  }
  return out.sort((a, b) => a.id - b.id);
}

// ------------------------------------------------------------------------------------------------ 4 (FOK), 5, 6 (rearm): events

async function eventsStep(x: Ctx, scan: Scan): Promise<void> {
  const now = Date.now();
  const retry = new Set<string>();
  for (const id of scan.changed) {
    const m = x.st.orders[id];
    const v = scan.views.get(id);
    if (!m?.watch || !v) continue;
    try {
      if (await processEvents(x, v, now)) retry.add(id);
    } catch (e) {
      retry.add(id);
      log.warn('order events check failed', { order: id, error: errText(e) });
    }
  }
  x.st.retry = [...retry];
}

/** returns true when some event must be re-checked next round */
async function processEvents(x: Ctx, v: OrderView, now: number): Promise<boolean> {
  const id = v.covenant_id;
  const m = x.st.orders[id];
  const evs = await allEvents(x.A, id);
  for (const viol of checkFokEvents(like(v), evs.map(evLike))) report(x, viol);
  const create = evs.find((e) => e.kind === 'create');
  if (create && m.initialAmount === null && create.amount != null) m.initialAmount = String(create.amount);
  const seen = new Set(m.ev);
  const lastTrail = evs.map((e) => e.kind).lastIndexOf('trail');
  let deferred = false;
  for (let i = 0; i < evs.length; i++) {
    const e = evs[i];
    const key = `${e.kind}:${e.txid}`;
    if (seen.has(key)) continue;
    // an event newer than the order view is checked against the next view
    if (e.daa > v.last_daa) {
      deferred = true;
      continue;
    }
    const triggeredFill = e.kind === 'fill' && detailOf(e).evidence != null;
    if (e.kind === 'arm' || e.kind === 'trail' || triggeredFill) {
      // the stop's terms: its proven current state, else (it filled or closed before this round) the full state memoized while it was
      // live; the stop price of a trailing stop may since have moved, so only the latest trail of a live order checks the new stop
      const cur = v.state?.state as unknown as Record<string, string> | undefined;
      const s = cur ?? (m.terms && m.terms._partial !== '1' ? m.terms : undefined);
      if (!s) {
        stat(x, 'triggersUnverifiable');
      } else {
        if (isPairContract(v.contract)) {
          // a pair stop: the evidence the indexer recorded is one of the two modes (the evidence orders' contracts when known)
          const ids = (detailOf(e).evidence as { orders?: (string | null)[] } | undefined)?.orders ?? [];
          const kinds = await Promise.all(ids.map(async (o) => (o ? ((await memoFor(x, o))?.c ?? null) : null)));
          const viol = checkPairEvidence({ covenant_id: id, contract: v.contract }, evLike(e), kinds);
          stat(x, 'pairTriggers');
          const mode = (detailOf(e).evidence as { mode?: number } | undefined)?.mode;
          if (mode === 0 || mode === 1) stat(x, `pairTriggerMode${mode}`);
          if (viol) report(x, viol);
          seen.add(key);
          continue;
        }
        const facts = await evidenceIn(x, e);
        stat(x, 'triggers');
        // a trailing stop's stop price read after a later trail (or from a memo) is not the one this event saw
        // (an arm reads the stop as much as a triggered fill does: a memo older than a trail before the arm holds the stop before it)
        const stopUnknown = (triggeredFill || e.kind === 'arm') && stopMayHaveMoved(s, !!cur, i, lastTrail);
        const r = checkTrigger(like(v), s, evLike(e), facts, { latestStopChange: !!cur && i === lastTrail, stopUnknown });
        if (r.ok) stat(x, 'triggersOk');
        else if (r.violations.every((w) => w.severity === 'warn')) stat(x, 'triggersUnverifiable');
        for (const viol of r.violations) report(x, viol);
      }
    } else if (e.kind === 'rearm') {
      const exit = detailOf(e).exit;
      const exitEvs = typeof exit === 'string' ? await allEvents(x.A, exit) : null;
      const viol = checkRearm(id, evLike(e), exitEvs ? exitEvs.map(evLike) : null);
      const p = x.persist.observe(`rearm:${id}:${e.txid}`, !!viol, now, 2);
      if (viol && !p.fire) {
        deferred = true;
        continue;
      }
      stat(x, 'rearms');
      if (viol) report(x, viol);
    }
    seen.add(key);
  }
  m.ev = [...seen];
  return deferred;
}

/**
 * The fills of the OTHER orders in the transaction of an `arm` / `trail` event (`/v1/fills` has no txid filter: page the token's fills
 * backwards from just above the event's id, event ids of one transaction are adjacent), as evidence candidates with their exposure:
 * `max(UTXO DAA of the order spent by the fill + interval, custody UTXO DAA (asks), activeFrom)`. The DAA of the spent UTXO is that of
 * the order's previous event (or its genesis); when a piece cannot be read, `exposedSince` / `slope` stay null (the rule warns).
 */
const EVIDENCE_ID_WINDOW = 500;
const SWEEP_MS = 5_000;

async function evidenceIn(x: Ctx, e: EventView): Promise<EvidenceFact[]> {
  const fills: EventView[] = [];
  let before: number | undefined = e.id + EVIDENCE_ID_WINDOW;
  // evidence is a fill of the SAME token (the stop's own): the event's token
  const token = e.token ?? x.token;
  for (let i = 0; i < 6; i++) {
    const p = await x.A.fills({ token, limit: 200, before });
    fills.push(...p.items.filter((f) => f.txid === e.txid && f.covenant_id !== e.covenant_id && f.kind === 'fill'));
    const oldest = p.items[p.items.length - 1];
    if (!p.next_cursor || !oldest || oldest.id < e.id - EVIDENCE_ID_WINDOW) break;
    before = oldest.id;
  }
  const out: EvidenceFact[] = [];
  for (const f of fills) {
    const memo = await memoFor(x, f.covenant_id);
    if (!memo?.terms) continue;
    let t = memo.terms;
    const partial = t._partial === '1';
    let exposedSince: number | null = null;
    let exposedFloor: number | null = null;
    // a terminated order's partial terms lack its TWAP `interval`: the start computed without it is only a floor
    if (isPlainKind(memo.c)) {
      const evs = await allEvents(x.A, f.covenant_id);
      // a plain ask or bid amended in place (KOB1 v3) quoted its terms of the moment: the evidence price is the amended one
      if (evs.some((k) => k.kind === 'amend')) {
        t = termsAtFill(t, currentTerms(await x.A.order(f.covenant_id)), evs.map(evLike), f.id);
      }
      const i = evs.findIndex((k) => k.kind === 'fill' && k.txid === e.txid);
      if (i >= 0) {
        const utxoDaa = i > 0 ? evs[i - 1].daa : memo.genesisDaa;
        let since = Math.max(utxoDaa + Number(t.interval ?? 0), Number(t.activeFrom ?? 0));
        let known = true;
        if (baseKind(memo.c) === 'KobAsk') {
          const cs = (await x.A.tokenUtxos({ owner: f.covenant_id, token, spent: true, maxPages: 3 })) ?? [];
          const c = cs.find((u) => u.role === 'custody' && u.spent_txid === e.txid);
          if (c) since = Math.max(since, c.created_daa);
          else known = false;
        }
        if (known) {
          if (partial) exposedFloor = since;
          else exposedSince = since;
        }
      }
    }
    out.push({
      order: f.covenant_id,
      contract: memo.c,
      side: f.side ?? memo.side,
      tokenCovId: t.tokenCovId ?? '',
      scale: t.scale ?? '',
      price: t.price ?? '',
      amount: f.amount ?? 0,
      slope: partial ? null : (t.slope ?? '0'),
      exposedSince,
      exposedFloor,
    });
  }
  return out;
}

const b0s = (v: string | undefined): bigint => {
  try {
    return BigInt(v ?? '0');
  } catch {
    return 0n;
  }
};
const isPlainKind = (c: string): boolean => baseKind(c) === 'KobAsk' || baseKind(c) === 'KobBid';

// ------------------------------------------------------------------------------------------------ 6. repeat positions

async function repeatStep(x: Ctx, views: Map<string, OrderView>, now: number): Promise<void> {
  for (const v of views.values()) {
    const k = baseKind(v.contract);
    if ((k !== 'KobIfdBid' && k !== 'KobIfdAsk') || !isActive(v.status) || !v.state) continue;
    const s = v.state.state as unknown as Record<string, string>;
    if (!(b0s(s.rptAmount) > 0n)) continue;
    const entry = await x.A.order(v.covenant_id);
    if (!entry || !entry.state) continue;
    const exits: OrderView[] = [];
    for (const c of entry.children ?? []) {
      const memo = x.st.orders[c];
      if (memo && !memo.active) continue;
      const cv = await x.A.order(c);
      if (cv) exits.push(cv);
    }
    stat(x, 'repeatEntries');
    const n = entry.initial_amount ?? x.st.orders[v.covenant_id]?.initialAmount ?? null;
    const viol = checkRepeatAmount(like(entry), n, exits.map(like));
    const sig = [entry.current, ...exits.map((e) => e.current)].map((o) => (o ? `${o.txid}:${o.index}` : '-')).join(',');
    const p = x.persist.observe(`rptamount:${v.covenant_id}`, !!viol, now, 2, 45_000, sig);
    if (viol && p.fire) report(x, viol);
  }
}

// ------------------------------------------------------------------------------------------------ 3. fills

async function fillsStep(x: Ctx): Promise<void> {
  const { A, st } = x;
  const f = st.fills;
  migrateFills(x);
  const fresh: EventView[] = [];
  let before: number | undefined;
  // every soak token at once (no token filter): the fills one transaction makes in several books are read together
  for (let i = 0; i < 1000; i++) {
    const p = await A.fills({ limit: 200, before });
    fresh.push(...p.items.filter((e) => !e.token || x.tokens.includes(e.token)));
    const oldest = p.items[p.items.length - 1];
    if (!p.next_cursor || !oldest) break;
    if (oldest.id <= f.maxId - 50 && oldest.daa < f.maxDaa - 600) break;
    before = oldest.id;
  }
  fresh.sort((a, b) => a.id - b.id);
  // event ids only grow (a reorg re-applies a transaction under new ids, which `seen` dedupes): anything at or below the cursor was
  // counted already, even when its `seen` entry has been pruned (counting those again inflated the totals on every round)
  const prevMax = f.maxId;
  const multi = (f.multi ??= { txMultiBook: 0, txPair: 0, pairFills: 0, txMultiPair: 0 });
  multi.txPair ??= 0;
  multi.pairFills ??= 0;
  multi.txMultiPair ??= 0;
  for (const e of fresh) {
    const key = `${e.covenant_id}:${e.txid}`;
    if (e.id <= prevMax || f.seen[key] !== undefined) continue;
    const memo = await memoFor(x, e.covenant_id);
    const d = detailOf(e);
    const pair = !!memo && isPairContract(memo.c);
    if (memo?.imported) stat(x, 'fillsImported');
    else if (memo?.terms) {
      let r = checkFill({ covenant_id: e.covenant_id, contract: memo.c }, memo.terms, evLike(e));
      if (r.violations.length && /^Kob(Ask|Bid)/.test(memo.c)) {
        // a plain ask or bid amended in place (KOB1 v3) keeps its id with new terms: check the fill against the terms it was filled under
        const evs = (await A.orderEvents(e.covenant_id, { limit: 200 }))?.items ?? [];
        if (evs.some((v) => v.kind === 'amend')) {
          r = checkFill({ covenant_id: e.covenant_id, contract: memo.c }, termsAtFill(memo.terms, currentTerms(await A.order(e.covenant_id)), evs.map(evLike), e.id), evLike(e));
          stat(x, 'fillsAmendedTerms');
        }
      }
      stat(x, 'fills');
      if (r.boundChecked) stat(x, 'fillBound');
      if (r.payoutChecked) stat(x, 'fillPayout');
      for (const viol of r.violations) report(x, viol);
      if (pair) pairFill(x, e, memo);
      if (baseKind(memo.c) === 'KobIfdAsk' && typeof d.exit === 'string') {
        const exit = await A.order(d.exit);
        const viol = checkIfdAskExitFunding({ covenant_id: e.covenant_id }, memo.terms, evLike(e), exit ? like(exit) : null);
        if (viol) report(x, viol);
      }
      if (typeof d.merged_into === 'string') {
        const entry = await memoFor(x, d.merged_into);
        if (entry?.terms) {
          stat(x, 'repeatCycles');
          const asLike = (id: string, mm: OrderMemo) => ({ covenant_id: id, contract: mm.c, side: mm.side, status: mm.status, last_daa: mm.lastDaa, state: { kind: mm.c, state: mm.terms! } }) as OrderLike;
          for (const viol of checkRepeatCycle(asLike(d.merged_into, entry), asLike(e.covenant_id, memo), evLike(e))) report(x, viol);
        }
      }
    } else stat(x, 'fillsUnverifiable');
    const token = (e.token ?? memo?.terms?.tokenCovId ?? x.token).toLowerCase();
    const bt = (f.byToken![token] ??= { count: 0, sell: 0, buy: 0, trades: 0, volumeTokens: '0', volumeKas: '0' });
    f.count++;
    bt.count++;
    if (e.side === 1) {
      f.sell++;
      bt.sell++;
    } else {
      f.buy++;
      bt.buy++;
    }
    // per transaction and token: a trade's tokens / KAS are the larger of its ask-side and bid-side sums (a swap-and-pay sells into bids
    // without an ask; a pair order's fill moves base units of its token A and no KAS)
    let t = normTx(f.tx[e.txid], x.token);
    if (!t) {
      t = { d: e.daa, k: {} };
      f.trades++;
    }
    const nBooks = Object.keys(t.k).length;
    let s = t.k[token];
    if (!s) {
      s = t.k[token] = { s: '0', b: '0', ks: '0', kb: '0' };
      bt.trades++;
      if (nBooks === 1) multi.txMultiBook++;
    }
    if (pair) {
      multi.pairFills++;
      t.x = (t.x ?? 0) + 1;
      if (t.x === 1) multi.txPair++;
      if (t.x === 2) multi.txMultiPair++;
    }
    const mx = (a: string, b: string) => (BigInt(a) > BigInt(b) ? BigInt(a) : BigInt(b));
    const [t0, k0] = [mx(s.s, s.b), mx(s.ks, s.kb)];
    if (memo?.terms && e.amount != null) {
      // base units, and the KAS they are worth at the fill's quote, rounded down as the indexer's trades are; a pair order trades a token
      // for another token (no KAS price): no KAS leg
      const tok = BigInt(e.amount);
      const q = e.price ?? memo.terms.price;
      const kas = q && !pair ? (quoteOf(tok, BigInt(q), BigInt(memo.terms.scale ?? '0'), 'down') ?? 0n) : 0n;
      if (e.side === 1) {
        s.s = (BigInt(s.s) + tok).toString();
        s.ks = (BigInt(s.ks) + kas).toString();
      } else {
        s.b = (BigInt(s.b) + tok).toString();
        s.kb = (BigInt(s.kb) + kas).toString();
      }
    }
    bt.volumeTokens = (BigInt(bt.volumeTokens) + mx(s.s, s.b) - t0).toString();
    bt.volumeKas = (BigInt(bt.volumeKas) + mx(s.ks, s.kb) - k0).toString();
    f.tx[e.txid] = t;
    f.seen[key] = e.daa;
    f.maxId = Math.max(f.maxId, e.id);
    f.maxDaa = Math.max(f.maxDaa, e.daa);
  }
  // the primary token's totals stay in the legacy fields (older reports read them)
  const p0 = f.byToken![x.token];
  if (p0) {
    f.volumeTokens = p0.volumeTokens;
    f.volumeKas = p0.volumeKas;
  }
  const floor = f.maxDaa - 72_000;
  for (const [k, daa] of Object.entries(f.seen)) if (daa < floor) delete f.seen[k];
  for (const [k, t] of Object.entries(f.tx)) if ((typeof t === 'object' ? t.d : Number(t)) < floor) delete f.tx[k];
}

/** fill counters of a state written before the second token: the per-token map starts with the primary token's totals */
function migrateFills(x: Ctx): void {
  const f = x.st.fills;
  if (f.byToken) return;
  f.byToken = { [x.token]: { count: f.count, sell: f.sell, buy: f.buy, trades: f.trades, volumeTokens: f.volumeTokens, volumeKas: f.volumeKas } };
}

/** a recent fill transaction in the per-token shape (a legacy entry `{d, s, b, ks, kb}` was the primary token's) */
function normTx(t: TxSums | LegacyTxSums | number | undefined, primary: string): TxSums | null {
  if (!t || typeof t !== 'object') return null;
  if ('k' in t) return t;
  return { d: t.d, k: { [primary]: { s: t.s, b: t.b, ks: t.ks, kb: t.kb } } };
}

/** Invariant 11 for one pair fill: no price, and the order's guarantee in B at its quote (the indexer's `detail.pair`). */
function pairFill(x: Ctx, e: EventView, memo: OrderMemo): void {
  const r = checkPairFill({ covenant_id: e.covenant_id, contract: memo.c }, evLike(e));
  stat(x, 'pairFills');
  if (r.checked) stat(x, 'pairChecked');
  if (r.surplus > 0n) stat(x, 'pairSurplusFills');
  const cp = (detailOf(e).pair as { counterparty?: string } | undefined)?.counterparty;
  if (cp) stat(x, `pairCounterparty:${cp}`);
  for (const v of r.violations) report(x, v);
}

// ------------------------------------------------------------------------------------------------ 1. custody and node UTXOs

async function custodyStep(x: Ctx, views: Map<string, OrderView>, cursor: number, now: number): Promise<void> {
  const { env, A } = x;
  const net = env.cfg.network;
  const addr = (spk: string) => spkStringToAddress(env.sdk, spk, net);
  interface Want {
    order: string;
    what: 'order' | 'custody';
    addr: string;
    txid: string;
    index: number;
    cov: string;
  }
  const want: Want[] = [];
  const problems = new Map<string, string[]>();
  const checked: OrderView[] = [];
  for (const v of views.values()) {
    if (!isActive(v.status) || !v.state_known || !v.state || !v.current) continue;
    const key = `custody:${v.covenant_id}`;
    if (cursor - v.last_daa < TOUCH_GRACE_DAA) {
      x.persist.observe(key, false, now);
      continue;
    }
    checked.push(v);
    const probs: string[] = [];
    problems.set(v.covenant_id, probs);
    try {
      want.push({ order: v.covenant_id, what: 'order', addr: addr(env.kob.scriptPublicKey(v.state)), txid: v.current.txid, index: v.current.index, cov: v.covenant_id });
    } catch (e) {
      probs.push(`order script not derivable: ${errText(e)}`);
    }
    // ask-side kinds hold exactly one custody of their token; a pair order every custody of its state (A and / or B: the view's `pair`)
    const parts = custodyParts(v, x.token);
    if (!parts) continue;
    for (const { token, expected } of parts) {
    stat(x, 'custody');
    const ck = `${v.covenant_id}:${token}`;
    let cached = x.custodyCache.get(ck);
    if (!cached || cached.lastDaa !== v.last_daa) {
      const items = ((await A.tokenUtxos({ owner: v.covenant_id, token, maxPages: 3 })) ?? []) as unknown as TokenUtxo[];
      cached = { lastDaa: v.last_daa, items: items.filter((i) => i.role === 'custody' && !i.spent) };
      x.custodyCache.set(ck, cached);
    }
    const live = cached.items;
    if (expected === 0n) {
      if (live.length) probs.push(`amountLeft 0 but ${live.length} live custody UTXO(s)`);
      continue;
    }
    if (live.length !== 1) {
      probs.push(`${live.length} live custody UTXOs (expected exactly one of ${expected})`);
      continue;
    }
    const c = live[0];
    if (BigInt(c.amount) !== expected) probs.push(`custody ${c.txid}:${c.index} holds ${c.amount} base units, amountLeft = ${expected}`);
    const ts = c.state as unknown as Record<string, unknown> | undefined;
    if (!ts || !c.program) {
      probs.push(`custody ${c.txid}:${c.index} has no proven token state`);
      continue;
    }
    if (String(ts.owner ?? '').toLowerCase() !== v.covenant_id.toLowerCase() || Number(ts.owner_scheme ?? ts.id_type) !== (ts.id_type !== undefined ? 2 : 4)) {
      probs.push(`custody ${c.txid}:${c.index} not owned by the order id (owner ${String(ts.owner)})`);
    }
    try {
      want.push({ order: v.covenant_id, what: 'custody', addr: addr(env.kob.tokenScriptPublicKey(c.program as TokenProgram, c.state!)), txid: c.txid, index: c.index, cov: token });
    } catch (e) {
      probs.push(`custody script not derivable: ${errText(e)}`);
    }
    }
  }
  // the node's UTXO set at every derived address
  const addrs = [...new Set(want.map((w) => w.addr))];
  const onNode = new Map<string, { address: string; covenantId: string | null }>();
  for (let i = 0; i < addrs.length; i += 100) {
    for (const u of await env.node.getUtxosByAddresses(addrs.slice(i, i + 100))) onNode.set(`${u.transactionId}:${u.index}`, { address: u.address, covenantId: u.covenantId });
  }
  for (const w of want) {
    stat(x, 'nodeOutpoints');
    const u = onNode.get(`${w.txid}:${w.index}`);
    const probs = problems.get(w.order)!;
    if (!u) probs.push(`${w.what} outpoint ${w.txid}:${w.index} is not in the node's UTXO set at ${w.addr}`);
    else if (u.covenantId && u.covenantId.toLowerCase() !== w.cov.toLowerCase()) probs.push(`${w.what} outpoint ${w.txid}:${w.index} carries covenant ${u.covenantId}, expected ${w.cov}`);
  }
  for (const v of checked) {
    const probs = problems.get(v.covenant_id) ?? [];
    const sig = `${v.current!.txid}:${v.current!.index}|${probs.join('|')}`;
    const p = x.persist.observe(`custody:${v.covenant_id}`, probs.length > 0, now, 2, 45_000, sig);
    if (p.fire) {
      const s = v.state!.state as unknown as Record<string, string>;
      report(x, {
        invariant: INV.custody,
        severity: 'error',
        subject: `${v.covenant_id}:${v.current!.txid}:${v.current!.index}`,
        detail: { order: v.covenant_id, contract: v.contract, maker: v.maker, status: v.status, amountLeft: s.amountLeft, problem: probs[0], problems: probs, current: v.current, lastDaa: v.last_daa, cursor, since: new Date(p.since).toISOString() },
      });
    }
  }
  // forget custody trackers of orders no longer live
  x.persist.retain((k) => !k.startsWith('custody:') || views.has(k.slice(8)));
  for (const k of x.custodyCache.keys()) {
    const id = k.split(':')[0]!;
    if (!views.has(id) || !isActive(views.get(id)!.status)) x.custodyCache.delete(k);
  }
}

/**
 * The custodies an order view must hold, per token: an ask-side KAS order its `amountLeft` of its token; a pair order each custody of its current
 * state (the view's `pair.custodies`, record order: a sell-first entry's A and its B prefund); null for kinds without custody.
 */
function custodyParts(v: OrderView, primary: string): { token: string; expected: bigint }[] | null {
  const pair = (v as unknown as { pair?: { custodies?: { token: string; expected_amount: string }[] } }).pair;
  if (isPairContract(v.contract)) return pair?.custodies ? pair.custodies.map((c) => ({ token: c.token.toLowerCase(), expected: BigInt(c.expected_amount) })) : null;
  if (!isAskSide(v.contract)) return null;
  const s = v.state!.state as unknown as Record<string, string>;
  return [{ token: (v.token ?? primary).toLowerCase(), expected: BigInt(s.amountLeft ?? '0') }];
}

/** amount accounting (base units) of the live plain asks and KobPair orders: filled + left == initial, over two rounds with the same position */
function balanceStep(x: Ctx, views: Map<string, OrderView>, now: number): void {
  for (const v of views.values()) {
    const key = `balance:${v.covenant_id}`;
    const viol = checkAmountBalance(like(v));
    if (viol || x.persist.data[key]) {
      const p = x.persist.observe(key, !!viol, now, 2, 45_000, `${v.last_daa}`);
      if (viol && p.fire) report(x, viol);
    }
  }
  x.persist.retain((k) => !k.startsWith('balance:') || views.has(k.slice(8)));
}

// ------------------------------------------------------------------------------------------------ 2. strays

async function straysStep(x: Ctx, now: number): Promise<void> {
  const { A, st } = x;
  const live = await A.strays({ limit: 200 });
  const liveKeys = new Set<string>();
  for (const s of live) {
    if (!x.tokens.includes(s.token.toLowerCase())) continue;
    const k = `${s.txid}:${s.index}`;
    liveKeys.add(k);
    st.strays[k] ??= { order: s.owner, maker: s.maker, amount: s.amount, firstSeen: now, token: s.token.toLowerCase() };
  }
  st.stats.straysLive = liveKeys.size;
  st.stats.straysLost = live.filter((s) => s.lost).length;
  if (live.length >= 200) return; // the list is capped: a missing stray may just be beyond it
  for (const [k, t] of Object.entries(st.strays)) {
    if (liveKeys.has(k)) continue;
    const items = ((await A.tokenUtxos({ owner: t.order, token: t.token ?? x.token, spent: true, maxPages: 20 })) ?? []) as unknown as TokenUtxo[];
    const it = items.find((i) => `${i.txid}:${i.index}` === k);
    if (!it) {
      delete st.strays[k]; // reorged away
      continue;
    }
    if (!it.spent || !it.spent_txid) continue;
    const evs = await allEvents(A, t.order);
    const byTx = evs.filter((e) => e.txid === it.spent_txid);
    if (byTx.some((e) => e.kind === 'cancel')) stat(x, 'straysSwept');
    else {
      report(x, {
        invariant: INV.stray,
        severity: 'error',
        subject: k,
        detail: { stray: k, order: t.order, maker: t.maker, amount: t.amount, spentTxid: it.spent_txid, orderEventsInThatTx: byTx.map((e) => e.kind), problem: 'stray token UTXO spent other than by the maker cancel of its order' },
      });
    }
    delete st.strays[k];
  }
}

// ------------------------------------------------------------------------------------------------ 7. x402

function readLines<T>(path: string): T[] {
  if (!existsSync(path)) return [];
  const out: T[] = [];
  for (const l of readFileSync(path, 'utf8').split('\n')) {
    if (!l.trim()) continue;
    try {
      out.push(JSON.parse(l) as T);
    } catch {
      /* torn */
    }
  }
  return out;
}

export function ledgerPath(runPath: string, executors: { name: string; x402?: string }[]): string | null {
  const ex = executors.find((e) => e.x402);
  if (!ex) return null;
  const cfgFile = join(runPath, ex.name, 'x402.json');
  if (existsSync(cfgFile)) {
    try {
      const l = (JSON.parse(readFileSync(cfgFile, 'utf8')) as { ledger?: string }).ledger;
      if (l) return l;
    } catch {
      /* fall back */
    }
  }
  return join(runPath, ex.name, 'x402-ledger.jsonl');
}

function x402Step(x: Ctx, now: number): void {
  const run = x.env.cfg.runPath;
  const lp = ledgerPath(run, x.env.cfg.executors);
  if (!lp) return;
  const payments = readLines<Payment & RetryPayment>(join(run, 'state', 'x402-payments.jsonl'));
  const ledger = existsSync(lp) ? readFileSync(lp, 'utf8') : '';
  const r = checkX402(payments, ledger, now);
  for (const v of r.violations) report(x, v);
  // the retry scenarios (`/retry/*`): at most one accepted attempt per payment, a resent lost answer served from the merchant's memory
  const rr = checkX402Retries(payments, ledger);
  for (const v of rr.violations) report(x, v);
  x.st.stats.x402RetryPayments = rr.stats.retryPayments;
  x.st.stats.x402Resent = rr.stats.resent;
  x.st.stats.x402Rebuilt = rr.stats.rebuilt;
  x.st.stats.x402RetryNotExercised = rr.stats.notExercised;
  x.st.x402 = r.stats as unknown as Record<string, unknown>;
  x.st.stats.x402Payments = r.stats.payments;
  x.st.stats.x402Settled = r.stats.settled;
}

// ------------------------------------------------------------------------------------------------ 8. fees

function feesStep(x: Ctx): void {
  const p = join(x.env.cfg.runPath, 'state', 'txs.jsonl');
  if (!existsSync(p)) return;
  const size = statSync(p).size;
  if (size < x.st.txsOffset) x.st.txsOffset = 0;
  if (size === x.st.txsOffset) return;
  const fd = openSync(p, 'r');
  let text: string;
  try {
    const buf = Buffer.alloc(size - x.st.txsOffset);
    readSync(fd, buf, 0, buf.length, x.st.txsOffset);
    text = buf.toString('utf8');
  } finally {
    closeSync(fd);
  }
  const end = text.lastIndexOf('\n');
  if (end < 0) return;
  // the rates a fee may use: the relay floor up to the configured maximum rate of the dynamic fee policy (config fees.maxRate)
  const bounds: FeeBounds = { floor: DEFAULT_FEE_BOUNDS.floor, maxRate: BigInt(x.env.cfg.feeSettings.maxRate) };
  for (const l of text.slice(0, end).split('\n')) {
    if (!l.trim()) continue;
    let line: TxLine;
    try {
      line = JSON.parse(l) as TxLine;
    } catch {
      continue;
    }
    const r = checkFeeLine(line, bounds);
    stat(x, 'feeLines');
    // transactions paid above the relay floor by the dynamic fee policy: counted (the checker accepts any rate within the bounds)
    if (/^[0-9]+$/.test(String(line.feeRate)) && BigInt(line.feeRate) > bounds.floor) stat(x, 'feeAboveFloor');
    if (r === 'unchecked') stat(x, 'feeUnchecked');
    else if (r === null) continue;
    else if ('kind' in r) {
      // the builder folded a change too small to keep (storage mass) into the fee: visible as an info line and a counter, never an incident
      stat(x, 'feeFoldedDust');
      log.info('fee above its rate: change folded by the builder', { txid: r.txid, bot: r.bot, what: r.what, fee: r.fee, expected: r.expected, excess: r.excess, maxFold: r.maxFold });
    } else report(x, r);
  }
  x.st.txsOffset += Buffer.byteLength(text.slice(0, end + 1), 'utf8');
}

// ------------------------------------------------------------------------------------------------ 13. scenarios

/** records checked per round (the rest wait for the next round) and the most kept waiting */
const SCEN_PER_ROUND = 40;
const SCEN_QUEUE_MAX = 2000;

/** new lines of run/state/scenarios.jsonl since the last round (cursor `scenOffset`) */
function newScenarioRecords(x: Ctx): ScenarioRecord[] {
  const p = scenariosPath(x.env.cfg.runPath);
  if (!existsSync(p)) return [];
  const size = statSync(p).size;
  let off = x.st.scenOffset ?? 0;
  if (size < off) off = 0;
  if (size === off) return [];
  const fd = openSync(p, 'r');
  let text: string;
  try {
    const buf = Buffer.alloc(size - off);
    readSync(fd, buf, 0, buf.length, off);
    text = buf.toString('utf8');
  } finally {
    closeSync(fd);
  }
  const end = text.lastIndexOf('\n');
  if (end < 0) return [];
  x.st.scenOffset = off + Buffer.byteLength(text.slice(0, end + 1), 'utf8');
  const out: ScenarioRecord[] = [];
  for (const l of text.slice(0, end).split('\n')) {
    if (!l.trim()) continue;
    try {
      out.push(JSON.parse(l) as ScenarioRecord);
    } catch {
      /* torn */
    }
  }
  return out;
}

/** the merged output of a merge as the indexer lists it: the owner's live token UTXOs first, its spent ones too once the output may be spent */
async function mergedOutput(x: Ctx, r: MergeRecord, ageMs: number): Promise<{ amount: string; owner: string } | null> {
  const find = (items: { txid: string; index: number; amount: string; owner: string }[] | null) => items?.find((u) => u.txid === r.final.txid && u.index === r.final.index) ?? null;
  const live = find(await x.A.tokenUtxos({ owner: r.maker, token: r.token, spent: false, limit: 200, maxPages: 5 }));
  if (live || ageMs < 120_000) return live;
  return find(await x.A.tokenUtxos({ owner: r.maker, token: r.token, spent: true, limit: 500, maxPages: 20 }));
}

async function scenarioVerdict(x: Ctx, r: ScenarioRecord, ageMs: number): Promise<Verdict> {
  const view = async (id: string) => {
    const v = await x.A.order(id).catch(() => null);
    return v ? like(v) : null;
  };
  const events = async (id: string) => (await allEvents(x.A, id).catch(() => null))?.map(evLike) ?? null;
  switch (r.kind) {
    case 'renew':
    case 'pairAmend': {
      const v = await view(r.id);
      return checkReplacement(r, v, v ? await events(r.id) : null, ageMs);
    }
    case 'ladder': {
      const vs = await Promise.all(r.rungs.map((g) => view(g.id)));
      // a rung off its planned price may have been amended since (a trader's Amend of that order, in place): only those rungs' events are read
      const amended = new Set<string>();
      for (const id of repricedRungs(r, vs)) {
        const evs = await events(id);
        if (evs === null && ageMs < SCENARIO_GRACE_MS) return { done: false };
        if (evs && amendedAfter(evs, r.rungs.find((g) => g.id === id)!.txid)) amended.add(id);
      }
      return checkLadder(r, vs, ageMs, amended);
    }
    case 'ladderCancel': {
      const evs = new Map<string, EventLike[] | null>();
      for (const id of r.rungs) evs.set(id, await events(id));
      return checkLadderCancel(r, await view(r.cancelled.id), evs, ageMs);
    }
    case 'cancelAll': {
      const vs = new Map<string, OrderLike | null>();
      for (const o of r.orders) vs.set(o.id, await view(o.id));
      return checkCancelAll(r, vs, ageMs);
    }
    case 'merge':
      return checkMerge(r, await mergedOutput(x, r, ageMs), ageMs);
    default:
      return { done: true, ok: false, violations: [], note: 'unknown kind' };
  }
}

/**
 * Scenario records of the bots (src/scenarios.ts, rules in scenario-rules.ts): the plan-side promises are checked as soon as a record is read (a
 * renewal keeps its terms and extends the expiry, a pair amend keeps the pair, side and size, a merge's total is its inputs' sum); the chain-side ones
 * once the indexer shows the transactions (up to SCENARIO_GRACE_MS). Counters `scen:<kind>`, `scen:<kind>:ok`, `scen:<kind>:<note>`.
 */
async function scenariosStep(x: Ctx, now: number): Promise<void> {
  const queue = (x.st.scen ??= []);
  for (const r of newScenarioRecords(x)) {
    stat(x, `scen:${r.kind}:recorded`);
    const plan = r.kind === 'renew' || r.kind === 'pairAmend' ? checkPlannedReplacement(r) : r.kind === 'merge' ? checkMergeRecord(r) : null;
    if (plan) report(x, plan);
    queue.push(r);
  }
  if (queue.length > SCEN_QUEUE_MAX) {
    stat(x, 'scen:dropped', queue.length - SCEN_QUEUE_MAX);
    queue.splice(0, queue.length - SCEN_QUEUE_MAX);
  }
  // at most SCEN_PER_ROUND records a round; one still pending goes to the back, so the queue rotates and none starves the others
  const unseen: ScenarioRecord[] = [];
  const waiting: ScenarioRecord[] = [];
  let n = 0;
  for (const r of queue) {
    if (n >= SCEN_PER_ROUND) {
      unseen.push(r);
      continue;
    }
    n++;
    const v = await scenarioVerdict(x, r, now - r.ts);
    if (!v.done) {
      waiting.push(r);
      continue;
    }
    for (const viol of v.violations) report(x, viol);
    stat(x, `scen:${r.kind}`);
    if (v.ok) stat(x, `scen:${r.kind}:ok`);
    if (v.note) stat(x, `scen:${r.kind}:${v.note}`);
  }
  x.st.scen = [...unseen, ...waiting];
}

// ------------------------------------------------------------------------------------------------ 12. supply conservation

interface RawTokenUtxo {
  txid: string;
  index: number;
  amount: string;
}

/** every live token UTXO of `token` the indexer at `indexerUrl` knows (`/v1/token-utxos?token=`, all owners); null when it cannot be listed */
async function liveTokenUtxos(indexerUrl: string, token: string): Promise<RawTokenUtxo[] | null> {
  const out: RawTokenUtxo[] = [];
  let cursor: string | null = null;
  for (let i = 0; i < 200; i++) {
    const u = new URL('/v1/token-utxos', indexerUrl);
    u.searchParams.set('token', token);
    u.searchParams.set('limit', '200');
    if (cursor) u.searchParams.set('cursor', cursor);
    const r = await fetch(u, { signal: AbortSignal.timeout(15_000) });
    if (!r.ok) return null;
    const j = (await r.json()) as { items?: RawTokenUtxo[]; next_cursor?: string | null };
    out.push(...(j.items ?? []));
    if (!j.next_cursor) return out;
    cursor = j.next_cursor;
  }
  return null;
}

interface Holding {
  txid: string;
  index: number;
  program: string;
  state: Record<string, unknown>;
}

/** holdings the bots' trackers know (run/state/tracker-*.json) and the genesis outputs of the soak tokens, for `token` */
function knownHoldings(x: Ctx, token: string): Holding[] {
  const out: Holding[] = [];
  const dir = join(x.env.cfg.runPath, 'state');
  for (const f of readdirSync(dir)) {
    if (!/^tracker-.*\.json$/.test(f)) continue;
    let m: Record<string, string>;
    try {
      m = JSON.parse(readFileSync(join(dir, f), 'utf8')) as Record<string, string>;
    } catch {
      continue;
    }
    for (const v of Object.values(m)) {
      let items: { transactionId: string; index: number; tokenCovId: string; program: string; state: Record<string, unknown> }[] = [];
      try {
        items = (JSON.parse(v) as { items?: typeof items }).items ?? [];
      } catch {
        continue;
      }
      for (const it of items) if (it.tokenCovId?.toLowerCase() === token) out.push({ txid: it.transactionId, index: it.index, program: it.program, state: it.state });
    }
  }
  const st = x.env.state;
  const slot = slotOfToken(st, token);
  const gen = slot ? genesisOf(st, slot) : undefined;
  const ext = slot ? st[slot]?.extensionCommitment : undefined;
  for (const [name, g] of Object.entries(gen ?? {})) {
    const k = x.env.keys[name];
    if (!k) continue;
    out.push({ txid: g.transactionId, index: g.index, program: SOAK_PROGRAM, state: { amount: g.amount, owner: k.publicKey, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: ext ?? '00'.repeat(32) } });
  }
  return out;
}

function supplyOf(x: Ctx, token: string): { ticker: string; supply: bigint; exact: boolean } | null {
  const st = x.env.state;
  if (token === st.token?.covenantId) {
    // TUSD was issued before these indexers started: holdings no tracker knows (executor keys of the earlier run) form a constant offset
    return { ticker: st.token.ticker, supply: BigInt(x.env.cfg.token.supply) * 10n ** BigInt(st.token.decimals), exact: false };
  }
  // an asset token (TETH, TBTC) was issued by this run: its supply is exact
  const slot = slotOfToken(st, token);
  const t = slot && slot !== 'token' ? st[slot] : undefined;
  if (slot && slot !== 'token' && t) {
    const whole = t.supply ?? x.env.cfg[slot]?.supply;
    return whole ? { ticker: t.ticker, supply: BigInt(whole) * 10n ** BigInt(t.decimals), exact: true } : null;
  }
  return null;
}

async function supplyStep(x: Ctx, now: number, ix: { url: string; name: string }): Promise<void> {
  const book = (x.st.supply ??= {});
  for (const token of x.tokens) {
    const sup = supplyOf(x, token);
    if (!sup) continue;
    const indexed = await liveTokenUtxos(ix.url, token);
    if (!indexed) {
      stat(x, 'supplySkipped');
      continue;
    }
    const seen = new Set(indexed.map((u) => `${u.txid}:${u.index}`));
    const indexedSum = indexed.reduce((s, u) => s + BigInt(u.amount), 0n);
    // holdings the indexer never listed, counted only while they are in the node's UTXO set
    const extra = new Map<string, Holding>();
    for (const h of knownHoldings(x, token)) {
      const k = `${h.txid}:${h.index}`;
      if (!seen.has(k)) extra.set(k, h);
    }
    const byAddr = new Map<string, Holding[]>();
    for (const h of extra.values()) {
      try {
        const a = spkStringToAddress(x.env.sdk, x.env.kob.tokenScriptPublicKey(h.program as TokenProgram, h.state as unknown as TokenState), x.env.cfg.network);
        byAddr.set(a, [...(byAddr.get(a) ?? []), h]);
      } catch {
        /* a malformed tracker entry: not a holding */
      }
    }
    let untracked = 0n;
    const addrs = [...byAddr.keys()];
    for (let i = 0; i < addrs.length; i += 100) {
      for (const u of await x.env.node.getUtxosByAddresses(addrs.slice(i, i + 100))) {
        const h = extra.get(`${u.transactionId}:${u.index}`);
        if (h) {
          untracked += BigInt(String(h.state.amount));
          extra.delete(`${u.transactionId}:${u.index}`);
        }
      }
    }
    const live = indexedSum + untracked;
    const delta = live - sup.supply;
    const rec = (book[token] ??= { baseline: sup.exact ? '0' : null, last: delta.toString(), live: live.toString(), untracked: untracked.toString(), ts: now });
    stat(x, 'supplyChecks');
    // a token issued before the indexers: the first offset that holds for 3 rounds (>= 2 min) becomes the accepted baseline
    if (rec.baseline === null) {
      const p = x.persist.observe(`supply-baseline:${token}`, true, now, 3, 120_000, delta.toString());
      if (p.fire) {
        rec.baseline = delta.toString();
        log.info('supply baseline established', { token, ticker: sup.ticker, delta: delta.toString(), note: 'holdings of pre-run keys the trackers do not know' });
      }
    } else {
      const v = checkSupply(token, sup.ticker, sup.supply, live, BigInt(rec.baseline), { indexer: ix.name, indexed: indexedSum.toString(), indexedUtxos: indexed.length, untrackedOnNode: untracked.toString() });
      const p = x.persist.observe(`supply:${token}`, !!v, now, 3, 120_000, delta.toString());
      if (v && p.fire) report(x, v);
      if (!v) stat(x, 'supplyOk');
    }
    Object.assign(rec, { last: delta.toString(), live: live.toString(), untracked: untracked.toString(), ts: now });
  }
}

// ------------------------------------------------------------------------------------------------ 9. the two indexers agree

/**
 * The fills of (fromDaa, toDaa] on one indexer. Fill ids follow insertion order, not chain order: imported fill events (record-log
 * import, older DAA) sit among newer fills at ids of their own on each indexer, so one row below the window says nothing about the rows
 * after it. Paging stops only after two whole pages below the window.
 */
async function fillKeys(ix: HttpIndexer, fromDaa: number, toDaa: number): Promise<string[]> {
  const out: string[] = [];
  let before: number | undefined;
  let below = 0;
  for (let i = 0; i < 1000; i++) {
    const p = await ix.fills({ limit: 200, before });
    for (const e of p.items) if (e.daa > fromDaa && e.daa <= toDaa) out.push(`${e.covenant_id}:${e.txid}:${e.amount}`);
    const oldest = p.items[p.items.length - 1];
    if (!p.next_cursor || !oldest) break;
    below = p.items.every((e) => e.daa <= fromDaa - 1000) ? below + 1 : 0;
    if (below >= 2) break;
    before = oldest.id;
  }
  return out;
}

async function agreeStep(x: Ctx, hA: HealthLike, hB: HealthLike, scan: Scan, now: number): Promise<void> {
  const { A, B, st } = x;
  if (!B || hB.state !== 'following' || Math.abs(hA.cursor_daa - hB.cursor_daa) > 3000) return;
  const D = Math.min(hA.cursor_daa, hB.cursor_daa) - 200;
  const F = st.agreeDaa;
  if (D > F) {
    const inWin = (v: OrderView) => v.genesis.daa > F && v.genesis.daa <= D;
    const both = async (ix: HttpIndexer) => (await Promise.all(x.tokens.map((t) => pageOrders(ix, t, F)))).flat();
    const [oa, ob, fa, fb] = await Promise.all([both(A), both(B), fillKeys(A, F, D), fillKeys(B, F, D)]);
    // a pair order is listed under both its tokens: compare the sets of ids
    const dOrd = diffSets(new Set(oa.filter(inWin).map((v) => v.covenant_id)), new Set(ob.filter(inWin).map((v) => v.covenant_id)));
    const dFill = diffSets(fa, fb);
    const bad = dOrd.onlyA.length + dOrd.onlyB.length + dFill.onlyA.length + dFill.onlyB.length > 0;
    const p = x.persist.observe('agree:window', bad, now, 3, 0, String(F));
    if (!bad) {
      st.agreeDaa = D;
      stat(x, 'agreeWindows');
    } else if (p.fire) {
      report(x, {
        invariant: INV.agree,
        severity: 'error',
        subject: `window:${F}-${D}`,
        detail: { problem: `${x.nameA} and ${x.nameB} disagree on the orders / fills up to DAA ${D}`, from: F, to: D, ordersOnlyA: dOrd.onlyA.slice(0, 20), ordersOnlyB: dOrd.onlyB.slice(0, 20), fillsOnlyA: dFill.onlyA.slice(0, 20), fillsOnlyB: dFill.onlyB.slice(0, 20), cursorA: hA.cursor_daa, cursorB: hB.cursor_daa },
      });
      st.agreeDaa = D;
    }
  }
  // sampled orders (changed ones first): status, filled / left amounts, current outpoint
  const settled = [...scan.views.values()].filter((v) => v.last_daa <= D);
  // suspects (a difference seen last round) first, then changed orders, then a random sample
  const rank = (v: OrderView) => (x.persist.data[`agree:order:${v.covenant_id}`] ? 0 : scan.changed.has(v.covenant_id) ? 1 : 2 + Math.random());
  const pick = settled.map((v) => ({ v, r: rank(v) })).sort((a, b) => a.r - b.r).slice(0, 20).map((e) => e.v);
  for (const v of pick) {
    const b = await B.order(v.covenant_id);
    if (b && b.last_daa > D) continue;
    stat(x, 'agreeOrders');
    const diffs = diffOrder(like(v), b ? like(b) : null);
    const p = x.persist.observe(`agree:order:${v.covenant_id}`, diffs.length > 0, now, 2, 30_000, diffs.join('|'));
    if (p.fire) report(x, { invariant: INV.agree, severity: 'error', subject: `order:${v.covenant_id}`, detail: { order: v.covenant_id, contract: v.contract, problem: `${x.nameA} and ${x.nameB} disagree on the order`, diffs, lastDaa: v.last_daa, D } });
  }
  x.persist.retain((k) => !k.startsWith('agree:order:') || scan.views.has(k.slice(12)));
}
