// The chain's present, as the web signs times from it (R9FB3-1). Every signed time (expiries,
// activations, auction origins, take and refund lock times, GTC renewals) is computed from one DAA.
// That DAA came from one node's `virtualDaaScore`, checked against nothing. Here no single party
// decides it:
//
// - Witnesses that cost no new node load: the node's `virtualDaaScore` (the one call the web makes
//   at plan time already); the indexer's present, its verified cursor advanced by the local wall
//   clock since the cursor block's verified header time at the nominal rate, at most 90 s ahead (the
//   same rule kobd's public reads use, docs/api-reference.md 2.6; never the node DAA the indexer
//   relays); the highest `blockDaaScore` of the UTXOs the plan read, a floor the present is not
//   below.
// - Indexer healthy: the node and the indexer must agree within PRESENT_MARGIN_DAA, else nothing
//   timed is built.
// - No indexer witness (indexer down, standalone): the last verified present this browser saw (the
//   anchor), carried forward by the wall clock, is the witness, with a tolerance that grows with its
//   age; an order is built when the node agrees with it and the tolerance is small against the
//   order's own time scale (kob/plan.ts). A node the user named in the settings is trusted for the
//   time by that choice. With neither, orders with a short life or a schedule are refused and
//   long-lived ones carry a "chain time unverified" mark.
//
// A cancel, a sweep, a claim and a refund need no present: their planners never read this verdict.
import type { HealthView } from '../data/indexer-types';

/** The nominal DAA rate (10 per second since Crescendo). */
export const DAA_PER_SECOND = 10n;
/** How far kobd advances its present past the cursor block by the wall clock (PRESENT_AHEAD_MAX_SECS). */
export const INDEXER_AHEAD_MAX_SECONDS = 90n;
/** The lag at which the web's health still calls the indexer healthy (ui/shell/health.ts LAG_WARN_SECONDS). */
export const INDEXER_LAG_OK_SECONDS = 120n;
/**
 * How far the node and the indexer may differ: the part of a healthy lag the indexer's present does
 * not advance (120 s - 90 s = 30 s), plus 30 s of difference between the browser's clock and a
 * miner's header time, at 10 DAA a second: 600 DAA, one minute. Within it a lie moves an expiry or
 * an auction's origin by at most a minute.
 */
export const PRESENT_MARGIN_DAA = (INDEXER_LAG_OK_SECONDS - INDEXER_AHEAD_MAX_SECONDS + 30n) * DAA_PER_SECOND;

/**
 * `agreed`: the indexer's verified present within the margin. `anchored`: no indexer now, but the
 * last verified present this browser saw, carried forward by its wall clock, agrees within a
 * tolerance that grows with its age. `own-node`: the user named this node in the settings and trusts
 * its time by that choice. `unverified`: no second witness. `disagree`: nothing timed is built.
 */
export type PresentVerdict = 'agreed' | 'anchored' | 'own-node' | 'unverified' | 'disagree';

export interface ChainPresent {
  /** The DAA the planners use: the node's, when it is not refused. */
  daa: bigint;
  verdict: PresentVerdict;
  /** The indexer's present, when it was a witness. */
  indexerDaa: bigint | null;
  /** How far the node may be from the true present under this verdict (DAA); null when unbounded. */
  toleranceDaa: bigint | null;
  /** Why the present is not agreed, in plain words. */
  reason: string | null;
}

/** The indexer's present from its health, or null when it is no witness (unhealthy, no header time). */
export function indexerPresent(h: HealthView | null | undefined, nowMs: number): bigint | null {
  if (!h || h.ok === false || h.stale === true) return null;
  const at = (h as { cursor_time_unix_ms?: number | null }).cursor_time_unix_ms;
  if (typeof at !== 'number' || !Number.isFinite(at) || typeof h.cursor_daa !== 'number') return null;
  if ((h as { chain_proofs_off?: boolean }).chain_proofs_off === true) return null;
  const age = BigInt(Math.max(0, Math.floor((nowMs - at) / 1000)));
  // an indexer behind by more than a healthy lag gives no present to judge the node by
  if (age > INDEXER_LAG_OK_SECONDS) return null;
  const ahead = age < INDEXER_AHEAD_MAX_SECONDS ? age : INDEXER_AHEAD_MAX_SECONDS;
  return BigInt(h.cursor_daa) + ahead * DAA_PER_SECOND;
}

// ------------------------------------------------------------------------------------ the anchor

/**
 * The last verified present this browser saw: the indexer's present while it was at the tip, and
 * the wall clock (Unix ms) when it was read. In memory and in localStorage; one read back from
 * storage is data, bounded (never in the future, never older than ANCHOR_MAX_AGE_MS).
 */
export interface PresentAnchor { daa: bigint; atMs: number }
/** An anchor older than this is no witness. */
export const ANCHOR_MAX_AGE_MS = 7 * 86_400_000;
/** The lag (s) up to which an indexer's present is fresh enough to anchor on. */
export const ANCHOR_LAG_MAX_SECONDS = 10;

/**
 * The tolerance of an anchor in DAA: 5 s (a miner's header time against the browser's clock) plus
 * 2 % of its age (the DAA rate's drift from the nominal 10 per second, and the browser clock's).
 * Minutes after an outage it is seconds; after a day about half an hour.
 */
export function anchorTolerance(ageMs: number): bigint {
  return (5n + BigInt(Math.floor(Math.max(0, ageMs) / 50_000))) * DAA_PER_SECOND;
}

const ANCHOR_KEY = 'kob.chainAnchor.v1';
let memoryAnchor: PresentAnchor | null = null;
let userNamedNode = false;

/** The user named the node in the settings (a stored setting, not a link or the resolvers). */
export function setUserNamedNode(v: boolean): void {
  userNamedNode = v;
}
export const isUserNamedNode = (): boolean => userNamedNode;

const storage = (): Storage | null => {
  try {
    return (globalThis as { localStorage?: Storage }).localStorage ?? null;
  } catch {
    return null;
  }
};

/** Keeps an indexer's health as the anchor when it is at the tip. */
export function recordAnchor(h: HealthView | null | undefined, nowMs: number): void {
  if (!h || typeof h.lag_seconds !== 'number' || h.lag_seconds > ANCHOR_LAG_MAX_SECONDS) return;
  const daa = indexerPresent(h, nowMs);
  if (daa === null) return;
  memoryAnchor = { daa, atMs: nowMs };
  try {
    storage()?.setItem(ANCHOR_KEY, JSON.stringify({ daa: daa.toString(), atMs: nowMs }));
  } catch {
    /* storage blocked: the anchor lives in memory only */
  }
}

/** The anchor held (memory first, then storage), bounded; null when none or out of bounds. */
export function currentAnchor(nowMs: number): PresentAnchor | null {
  let a = memoryAnchor;
  if (!a) {
    try {
      const raw = storage()?.getItem(ANCHOR_KEY);
      const j = raw ? (JSON.parse(raw) as { daa?: unknown; atMs?: unknown }) : null;
      if (j && typeof j.daa === 'string' && /^[0-9]{1,20}$/.test(j.daa) && typeof j.atMs === 'number' && Number.isFinite(j.atMs)) a = { daa: BigInt(j.daa), atMs: j.atMs };
    } catch {
      a = null;
    }
  }
  if (!a || a.atMs > nowMs || nowMs - a.atMs > ANCHOR_MAX_AGE_MS) return null;
  return a;
}

/** Forgets the anchor (tests). */
export function clearAnchor(): void {
  memoryAnchor = null;
  try {
    storage()?.removeItem(ANCHOR_KEY);
  } catch {
    /* nothing stored */
  }
}

// --------------------------------------------------------------------------------- the verdict

const off = (a: bigint, b: bigint): bigint => (a > b ? a - b : b - a);

/** Judges the node's DAA against the other witnesses. */
export function judgePresent(nodeDaa: bigint, o: { indexer: bigint | null; floor: bigint | null; anchor?: PresentAnchor | null; nowMs?: number; userNode?: boolean }): ChainPresent {
  if (o.floor !== null && nodeDaa < o.floor) {
    return { daa: nodeDaa, verdict: 'disagree', indexerDaa: o.indexer, toleranceDaa: null, reason: `the node's DAA ${nodeDaa} is below ${o.floor}, the DAA of a coin it reported itself` };
  }
  if (o.indexer !== null) {
    const d = off(nodeDaa, o.indexer);
    if (d > PRESENT_MARGIN_DAA) {
      return { daa: nodeDaa, verdict: 'disagree', indexerDaa: o.indexer, toleranceDaa: null, reason: `the node says the chain is at DAA ${nodeDaa}, the indexer at ${o.indexer}: ${d / DAA_PER_SECOND} s apart, more than ${PRESENT_MARGIN_DAA / DAA_PER_SECOND} s` };
    }
    return { daa: nodeDaa, verdict: 'agreed', indexerDaa: o.indexer, toleranceDaa: PRESENT_MARGIN_DAA, reason: null };
  }
  if (o.anchor && o.nowMs !== undefined) {
    const age = o.nowMs - o.anchor.atMs;
    const expected = o.anchor.daa + BigInt(Math.floor(age / 1000)) * DAA_PER_SECOND;
    const tol = anchorTolerance(age);
    const d = off(nodeDaa, expected);
    const mins = Math.round(age / 60_000);
    if (d > tol) {
      return { daa: nodeDaa, verdict: 'disagree', indexerDaa: null, toleranceDaa: null, reason: `the node says the chain is at DAA ${nodeDaa}, ${d / DAA_PER_SECOND} s from the last verified time this browser saw ${mins} min ago, more than its tolerance of ${tol / DAA_PER_SECOND} s` };
    }
    return { daa: nodeDaa, verdict: 'anchored', indexerDaa: null, toleranceDaa: tol, reason: `checked against the last verified chain time this browser saw ${mins} min ago, within ${tol / DAA_PER_SECOND} s` };
  }
  if (o.userNode) return { daa: nodeDaa, verdict: 'own-node', indexerDaa: null, toleranceDaa: null, reason: 'the chain time is from the node you named in the settings' };
  return { daa: nodeDaa, verdict: 'unverified', indexerDaa: null, toleranceDaa: null, reason: "no indexer could confirm the node's chain time" };
}

/**
 * A clock read later from the same node carries the verdict of the judged one it follows, and is
 * itself judged against it: projected at the nominal rate by the browser's clock, the two must stay
 * within PRESENT_MARGIN_DAA, or the later one is refused.
 */
export function followWitness<C extends { daa: bigint; unixSeconds: bigint; witness?: PresentVerdict; witnessReason?: string | null; toleranceDaa?: bigint | null }>(later: C, judged: { daa: bigint; unixSeconds: bigint; witness?: PresentVerdict; witnessReason?: string | null; toleranceDaa?: bigint | null }): C {
  if (!judged.witness) return later;
  const expected = judged.daa + (later.unixSeconds - judged.unixSeconds) * DAA_PER_SECOND;
  const d = off(later.daa, expected);
  if (d > PRESENT_MARGIN_DAA) return { ...later, witness: 'disagree', witnessReason: `the node's chain time moved ${d / DAA_PER_SECOND} s away from the time it gave when the order was planned` };
  return { ...later, witness: judged.witness, witnessReason: judged.witnessReason ?? null, toleranceDaa: judged.toleranceDaa ?? null };
}
