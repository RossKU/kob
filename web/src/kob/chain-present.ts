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
// - No indexer witness (indexer down, standalone): the node's DAA must not be below the floor; the
//   present is then unverified. Orders with a short life or a schedule are refused, long-lived ones
//   carry a "chain time unverified" mark.
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

export type PresentVerdict = 'agreed' | 'unverified' | 'disagree';

export interface ChainPresent {
  /** The DAA the planners use: the node's, when it is not refused. */
  daa: bigint;
  verdict: PresentVerdict;
  /** The indexer's present, when it was a witness. */
  indexerDaa: bigint | null;
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

/** Judges the node's DAA against the other witnesses. */
export function judgePresent(nodeDaa: bigint, o: { indexer: bigint | null; floor: bigint | null }): ChainPresent {
  if (o.floor !== null && nodeDaa < o.floor) {
    return { daa: nodeDaa, verdict: 'disagree', indexerDaa: o.indexer, reason: `the node's DAA ${nodeDaa} is below ${o.floor}, the DAA of a coin it reported itself` };
  }
  if (o.indexer !== null) {
    const d = nodeDaa > o.indexer ? nodeDaa - o.indexer : o.indexer - nodeDaa;
    if (d > PRESENT_MARGIN_DAA) {
      return { daa: nodeDaa, verdict: 'disagree', indexerDaa: o.indexer, reason: `the node says the chain is at DAA ${nodeDaa}, the indexer at ${o.indexer}: ${d / DAA_PER_SECOND} s apart, more than ${PRESENT_MARGIN_DAA / DAA_PER_SECOND} s` };
    }
    return { daa: nodeDaa, verdict: 'agreed', indexerDaa: o.indexer, reason: null };
  }
  return { daa: nodeDaa, verdict: 'unverified', indexerDaa: null, reason: 'no indexer could confirm the node\'s chain time' };
}

/**
 * A clock read later from the same node carries the verdict of the judged one it follows, and is
 * itself judged against it: projected at the nominal rate by the browser's clock, the two must stay
 * within PRESENT_MARGIN_DAA, or the later one is refused.
 */
export function followWitness<C extends { daa: bigint; unixSeconds: bigint; witness?: PresentVerdict; witnessReason?: string | null }>(later: C, judged: { daa: bigint; unixSeconds: bigint; witness?: PresentVerdict; witnessReason?: string | null }): C {
  if (!judged.witness) return later;
  const expected = judged.daa + (later.unixSeconds - judged.unixSeconds) * DAA_PER_SECOND;
  const d = later.daa > expected ? later.daa - expected : expected - later.daa;
  if (d > PRESENT_MARGIN_DAA) return { ...later, witness: 'disagree', witnessReason: `the node's chain time moved ${d / DAA_PER_SECOND} s away from the time it gave when the order was planned` };
  return { ...later, witness: judged.witness, witnessReason: judged.witnessReason ?? null };
}
