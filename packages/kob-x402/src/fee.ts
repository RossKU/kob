// Fee rates of the payer's transactions: the same dynamic policy as the executor's (crates/kob-executor/src/fee.rs, docs/ops/
// executor.md "Fee policy"). The node (rusty-kaspa `getFeeEstimate`) answers sompi-per-gram buckets: a priority bucket
// (sub-second inclusion), normal buckets (the first: sub-minute) and low buckets (the first: sub-hour). A payment pays the
// bucket of its urgency, `ceil(bucket)` clamped to `[floor, maxRate]` (the floor wins over a lower maxRate); without an
// estimate it pays the floor. A transaction that cannot be built at the rate is rebuilt once at the floor: the policy prices
// a payment, it never makes a payable one impossible.

/** The relay floor: the least fee rate a node relays, sompi per gram (kob-protocol `MIN_FEE_RATE`). */
export const MIN_FEE_RATE = 100;

/** How soon a transaction should be accepted (the executor's `Urgency`). */
export type FeeUrgency = 'low' | 'normal' | 'high';

/** The node's estimate, sompi per gram: the priority bucket, the first normal and the first low bucket. */
export interface NodeFeeEstimate {
  priority: number;
  normal: number;
  low: number;
}

/** A fixed rate, or a function asked before each transaction (undefined: the builders' floor). */
export type FeeRateSource = number | undefined | (() => number | undefined | Promise<number | undefined>);

/**
 * Parses a `getFeeEstimate` answer (`{ estimate: { priorityBucket: { feerate }, normalBuckets: [...], lowBuckets: [...] } }`,
 * or the inner `estimate`). An empty normal or low list takes the next faster bucket. Every rate must be finite and positive.
 */
export function parseFeeEstimate(v: unknown): NodeFeeEstimate {
  const o = (v ?? {}) as Record<string, unknown>;
  const e = (o.estimate ?? o) as Record<string, unknown>;
  const rate = (b: unknown): number | undefined => {
    const r = (b as { feerate?: unknown } | undefined)?.feerate;
    return typeof r === 'number' ? r : undefined;
  };
  const first = (k: string): number | undefined => (Array.isArray(e[k]) ? rate((e[k] as unknown[])[0]) : undefined);
  const priority = rate(e.priorityBucket);
  if (priority === undefined) throw new Error('getFeeEstimate: no priorityBucket.feerate');
  const normal = first('normalBuckets') ?? priority;
  const low = first('lowBuckets') ?? normal;
  for (const [k, r] of [['priority', priority], ['normal', normal], ['low', low]] as const) {
    if (!Number.isFinite(r) || r <= 0) throw new Error(`getFeeEstimate: ${k} feerate ${r} is not a positive number`);
  }
  return { priority, normal, low };
}

/**
 * The rate of an urgency from an estimate: `ceil(bucket)` clamped to `[floor, max(floor, maxRate)]` (defaults: the relay floor,
 * no cap). Without an estimate (`undefined`) the floor. Accepts a parsed estimate or a raw `getFeeEstimate` answer.
 */
export function feeRateFromEstimate(
  estimate: NodeFeeEstimate | unknown | undefined,
  urgency: FeeUrgency = 'high',
  opts: { floor?: number; maxRate?: number } = {},
): number {
  const floor = opts.floor ?? MIN_FEE_RATE;
  if (!Number.isInteger(floor) || floor < MIN_FEE_RATE) throw new Error(`fee floor ${floor} is below the relay floor ${MIN_FEE_RATE}`);
  if (estimate === undefined || estimate === null) return floor;
  const e = isParsed(estimate) ? estimate : parseFeeEstimate(estimate);
  const bucket = urgency === 'high' ? e.priority : urgency === 'normal' ? e.normal : e.low;
  const max = Math.max(floor, opts.maxRate ?? Number.MAX_SAFE_INTEGER);
  return Math.min(Math.max(Math.ceil(bucket), floor), max);
}

function isParsed(v: unknown): v is NodeFeeEstimate {
  const o = v as Record<string, unknown>;
  return typeof o?.priority === 'number' && typeof o?.normal === 'number' && typeof o?.low === 'number';
}

/** The rate a source names now (a function is asked; undefined = the builders' floor). Must be a positive integer. */
export async function resolveFeeRate(src: FeeRateSource): Promise<number | undefined> {
  const r = typeof src === 'function' ? await src() : src;
  if (r === undefined) return undefined;
  if (!Number.isInteger(r) || r < MIN_FEE_RATE) throw new Error(`fee rate ${r} must be an integer of at least ${MIN_FEE_RATE} sompi per gram`);
  return r;
}

/** Builds at `rate`; when that throws and the rate is above the floor, builds once more at the floor (the floor's error stands). */
export function withFeeFloor<T>(rate: number | undefined, floor: number | undefined, build: (rate: number | undefined) => T): T {
  const f = floor ?? MIN_FEE_RATE;
  try {
    return build(rate);
  } catch (e) {
    if (rate === undefined || rate <= f) throw e;
    return build(f);
  }
}

/** [`withFeeFloor`] for an async build. */
export async function withFeeFloorAsync<T>(rate: number | undefined, floor: number | undefined, build: (rate: number | undefined) => Promise<T>): Promise<T> {
  const f = floor ?? MIN_FEE_RATE;
  try {
    return await build(rate);
  } catch (e) {
    if (rate === undefined || rate <= f) throw e;
    return await build(f);
  }
}
