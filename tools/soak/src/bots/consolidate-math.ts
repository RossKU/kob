// Pure logic of the bots' token-UTXO consolidation (no I/O, so it is unit-tested): which UTXOs to merge, and how often.
//
// Every fill, refund and fan-out leaves a new token UTXO that carries ~10 KAS; left alone a market maker holds hundreds (687 TUSD and 491
// TETH after 1.5 h in the 2026-10-01 soak, ~15,000 KAS of carriers). The fan-out (`fanout.ts`) deliberately keeps `keep` UTXOs per token
// (one token input per sell-side placement), so consolidation merges only the EXCESS, smallest first, and never below `keep`.

/** what the selection needs of a token UTXO (the web `TokenUtxo` satisfies it) */
export interface MergeCandidate {
  transactionId: string;
  index: number;
  state: { amount: string | number | bigint };
}

export interface ConsolidateConfig {
  /** switch the whole step off */
  enabled: boolean;
  /** merge only when a key holds MORE than `keep + slack` plain UTXOs of one token (keep = the token's fan-out target) */
  slack: number;
  /** at most this many merge transactions per `intervalSec`, per key and token */
  maxTxPerInterval: number;
  intervalSec: number;
  /** the traders (and the x402 payer's tokens, if ever wired) run the step too */
  traders: boolean;
}

/**
 * Defaults. KOB's standard KCC-20 program takes 3 token inputs per transaction, so a merge takes 2 UTXOs off the count (8 with the 8 / 8
 * prototype the soak ran before): 8 merge transactions per minute per key and token keep a market maker's fragmentation (about 8 new token
 * UTXOs a minute at the 2026-10-01 rates) in check.
 */
export const DEFAULT_CONSOLIDATE: ConsolidateConfig = { enabled: true, slack: 8, maxTxPerInterval: 8, intervalSec: 60, traders: true };

export function consolidateConfig(raw?: Partial<ConsolidateConfig>): ConsolidateConfig {
  const c = { ...DEFAULT_CONSOLIDATE, ...(raw ?? {}) };
  return { ...c, slack: Math.max(1, Math.floor(c.slack)), maxTxPerInterval: Math.max(0, Math.floor(c.maxTxPerInterval)), intervalSec: Math.max(1, c.intervalSec) };
}

export interface SelectOptions {
  /** the UTXO count to keep (the fan-out target): the visible count never drops below it, even before a merge output is indexed */
  keep: number;
  /** a merge is planned only when the eligible count exceeds `keep + slack` */
  slack: number;
  /** token inputs one transfer of the program takes (KOB's standard KCC20Ref: 3) */
  maxInputs: number;
  /** most batches (merge transactions) to plan */
  maxBatches: number;
  /** outpoints (`txid:index`) in use by the bot (reserved by an unaccepted transaction): never selected */
  reserved?: ReadonlySet<string>;
}

export const outpointKey = (u: { transactionId: string; index: number }): string => `${u.transactionId}:${u.index}`;

/**
 * Batches of UTXOs to merge, one transaction each (one output per batch, so the program's output limit is never the constraint).
 * Rule: the eligible UTXOs (positive amount, not reserved) are sorted smallest first (ties by outpoint, so the plan is deterministic).
 * When their count n is at most `keep + slack` nothing is planned. Otherwise the smallest ones are taken in batches of up to `maxInputs`
 * while the inputs of all batches together stay within n - keep: the count after the merges (outputs included) is then at least `keep`,
 * and even while the merge outputs are not yet visible to the indexer it never drops below `keep`. Batches are disjoint.
 */
export function selectMerges<T extends MergeCandidate>(utxos: readonly T[], o: SelectOptions): T[][] {
  const plan = planOf(utxos, o);
  if (!plan) return [];
  const { sorted, n, keep, maxIn } = plan;
  const batches: T[][] = [];
  let budget = n - keep;
  let at = 0;
  while (batches.length < o.maxBatches) {
    const k = Math.min(maxIn, budget);
    if (k < 2) break;
    batches.push(sorted.slice(at, at + k));
    at += k;
    budget -= k;
  }
  return batches;
}

/**
 * The merges of one pass as a CHAIN of transactions: the first merges up to `maxInputs` of the smallest eligible UTXOs into one output, and
 * every next one spends that (still unaccepted) output together with up to `maxInputs - 1` more, so a pass ends with ONE merged UTXO even on
 * a 3-input program (KOB's standard KCC-20 program: 3 -> 1, then 1 + 2 -> 1, ...). Each link is a transfer of at most `maxInputs` token inputs
 * into one output, valid on its own. Returns the FRESH UTXOs of each link (from the second link on the caller adds the previous link's
 * output). The rule of `selectMerges` holds: the threshold, smallest first, and the fresh inputs of all links within n - keep, so the
 * visible count never drops below `keep` (the chain's one output only adds to it).
 */
export function selectMergeChain<T extends MergeCandidate>(utxos: readonly T[], o: SelectOptions): T[][] {
  const plan = planOf(utxos, o);
  if (!plan) return [];
  const { sorted, n, keep, maxIn } = plan;
  const links: T[][] = [];
  let budget = n - keep;
  let at = 0;
  while (links.length < o.maxBatches) {
    const first = links.length === 0;
    const k = Math.min(first ? maxIn : maxIn - 1, budget);
    if (k < (first ? 2 : 1)) break;
    links.push(sorted.slice(at, at + k));
    at += k;
    budget -= k;
  }
  return links;
}

/** The eligible UTXOs (positive amount, not reserved) sorted smallest first, ties by outpoint; null when nothing is to be merged. */
function planOf<T extends MergeCandidate>(utxos: readonly T[], o: SelectOptions): { sorted: T[]; n: number; keep: number; maxIn: number } | null {
  const reserved = o.reserved;
  const eligible = utxos.filter((u) => BigInt(u.state.amount) > 0n && !(reserved?.has(outpointKey(u)) ?? false));
  const n = eligible.length;
  const keep = Math.max(1, Math.floor(o.keep));
  const maxIn = Math.floor(o.maxInputs);
  if (n <= keep + Math.max(1, Math.floor(o.slack)) || maxIn < 2 || o.maxBatches < 1) return null;
  const sorted = [...eligible].sort((a, b) => {
    const x = BigInt(a.state.amount);
    const y = BigInt(b.state.amount);
    if (x !== y) return x < y ? -1 : 1;
    if (a.transactionId !== b.transactionId) return a.transactionId < b.transactionId ? -1 : 1;
    return a.index - b.index;
  });
  return { sorted, n, keep, maxIn };
}

/** sliding-window rate limit per key: at most `max` events in any `windowMs` */
export class RateLimiter {
  private readonly events = new Map<string, number[]>();
  private readonly max: number;
  private readonly windowMs: number;
  constructor(max: number, windowMs: number) {
    this.max = max;
    this.windowMs = windowMs;
  }

  private live(key: string, now: number): number[] {
    const xs = (this.events.get(key) ?? []).filter((t) => t > now - this.windowMs);
    this.events.set(key, xs);
    return xs;
  }

  /** events still allowed for `key` right now */
  allowance(key: string, now: number): number {
    return Math.max(0, this.max - this.live(key, now).length);
  }

  /** counts `n` events at `now` */
  record(key: string, now: number, n = 1): void {
    const xs = this.live(key, now);
    for (let i = 0; i < n; i++) xs.push(now);
  }
}

/**
 * The fan-out (`fanout.ts`) as a CHAIN of transfers within the program's token outputs: each transaction splits the UTXO it spends into
 * up to `maxOutputs - 1` pieces of `each` base units plus the token change, and the next one spends that change (not yet accepted).
 * Returns the pieces per transaction: together at most `need` (the UTXOs the key lacks) and what `amount` covers, in at most `maxTx`
 * transactions. Pieces that take the whole remainder (no change) end the chain. With KOB's standard program (3 token outputs) that is 2
 * pieces per transaction; the 8 / 8 prototype took 7 in one.
 */
export function fanoutPlan(o: { amount: bigint; each: bigint; need: number; maxOutputs: number; maxTx: number }): number[] {
  const out: number[] = [];
  if (o.each <= 0n || o.maxOutputs < 2) return out;
  let left = o.amount;
  let need = o.need;
  while (need > 0 && out.length < o.maxTx) {
    const k = Math.min(o.maxOutputs - 1, need, Number(left / o.each));
    if (k < 1) break;
    out.push(k);
    left -= BigInt(k) * o.each;
    need -= k;
    if (left === 0n) break;
  }
  return out;
}
