// Dynamic priority-fee policy. It decides which feerate, in sompi per gram of mass, a transaction
// pays, from the node's `getFeeEstimate` and the urgency of the action. It mirrors the Rust
// executor's policy exactly (docs/ops/executor.md): the same buckets, clamp, total cap and
// fallback.
//
// - Urgency picks the bucket. High uses `priorityBucket`, which confirms in under a second. Normal
//   uses `normalBuckets[0]`, under a minute. Low uses `lowBuckets[0]`, under an hour.
// - The rate is `min(max(ceil(bucketFeerate), floor), max(maxRate, floor))`.
// - Without a usable estimate the rate is the floor. That covers a failed call, a missing method,
//   malformed, non-finite or non-positive values, and an estimate older than `maxAgeMs`. It is
//   never an error.
// - The total cap applies when the built fee exceeds `maxFeeSompi` and the rate is above the
//   floor. The transaction is rebuilt once at `max(floor, floor_div(rate * maxFeeSompi, fee))`.
//   The cap never goes below the floor, so a floor-rate transaction above the cap is still sent
//   and disclosed.
// - `dynamic: false` always pays the floor.
//
// Nothing here touches the network. `FeeOracle` takes the fetch function and the clock as
// parameters.

/**
 * The node's relay floor in sompi per gram, `MIN_FEE_RATE` of kob_protocol. The builders refuse a
 * lower rate.
 */
export const RELAY_FEE_FLOOR = 100n;

/** How urgent an action is. It picks the bucket of the node's estimate. */
export type Urgency = 'high' | 'normal' | 'low';

/** Every urgency, from the most to the least urgent. */
export const URGENCIES: readonly Urgency[] = ['high', 'normal', 'low'];

/**
 * The three feerates the policy reads from `getFeeEstimate`, in sompi per gram.
 * It also holds the node's inclusion-time estimate of each, which is for disclosure only.
 */
export interface FeeEstimate {
  /** The feerate of `priorityBucket`. */
  priority: number;
  /** The feerate of `normalBuckets[0]`. */
  normal: number;
  /** The feerate of `lowBuckets[0]`. */
  low: number;
  /** The estimated inclusion time of each bucket in seconds, as the node reports it. */
  seconds?: { priority?: number; normal?: number; low?: number };
}

/** The fee policy. */
export interface FeePolicy {
  /** When false, the policy always pays the floor. */
  dynamic: boolean;
  /** The lowest rate ever paid in sompi per gram, at least RELAY_FEE_FLOOR. */
  floor: bigint;
  /** The highest rate ever paid in sompi per gram. A value below `floor` means the floor. */
  maxRate: bigint;
  /** The most one transaction pays in total, in sompi. 0 means no total cap. */
  maxFeeSompi: bigint;
  /** The estimate is refreshed at most this often, in ms. */
  refreshMs: number;
  /** An estimate older than this is not used, in ms. */
  maxAgeMs: number;
}

/** The default policy. */
export const DEFAULT_FEE_POLICY: Readonly<FeePolicy> = Object.freeze({
  dynamic: true,
  floor: RELAY_FEE_FLOOR,
  maxRate: 1_000n,
  maxFeeSompi: 100_000_000n,
  refreshMs: 10_000,
  maxAgeMs: 60_000,
});

/**
 * Returns a policy with every field valid. A floor below the relay minimum is raised to it, and a
 * negative cap means none.
 */
export function normalizePolicy(p: Partial<FeePolicy> = {}): FeePolicy {
  const d = DEFAULT_FEE_POLICY;
  const floor = (p.floor ?? d.floor) < RELAY_FEE_FLOOR ? RELAY_FEE_FLOOR : (p.floor ?? d.floor);
  const maxFee = p.maxFeeSompi ?? d.maxFeeSompi;
  return {
    dynamic: p.dynamic ?? d.dynamic,
    floor,
    maxRate: p.maxRate ?? d.maxRate,
    maxFeeSompi: maxFee < 0n ? 0n : maxFee,
    refreshMs: p.refreshMs !== undefined && Number.isFinite(p.refreshMs) && p.refreshMs >= 0 ? p.refreshMs : d.refreshMs,
    maxAgeMs: p.maxAgeMs !== undefined && Number.isFinite(p.maxAgeMs) && p.maxAgeMs >= 0 ? p.maxAgeMs : d.maxAgeMs,
  };
}

/**
 * Returns the policy a wallet or app configuration asks for.
 * `maxRate` is in sompi per gram and `maxFeeKas` is in KAS, where 0 means no total cap. The
 * timings stay at their defaults.
 */
export function policyFromSettings(s: { dynamic: boolean; maxRate: number; maxFeeKas: number }): FeePolicy {
  return normalizePolicy({ dynamic: s.dynamic, maxRate: BigInt(Math.max(0, Math.floor(s.maxRate))), maxFeeSompi: BigInt(Math.max(0, Math.round(s.maxFeeKas * 1e8))) });
}

// --- Settings ---

/**
 * The fee settings of an app configuration. `maxRate` is in sompi per gram and `maxFeeKas` is in
 * KAS, 0 for no total cap.
 */
export interface FeeSettings {
  dynamic: boolean;
  maxRate: number;
  maxFeeKas: number;
}

/** The lowest accepted `maxRate` in sompi per gram, which is the relay minimum. */
export const FEE_MAX_RATE_MIN = 100;
/** The highest accepted `maxRate` in sompi per gram. */
export const FEE_MAX_RATE_MAX = 1_000_000;
/** The highest accepted `maxFeeKas`. */
export const FEE_MAX_KAS_MAX = 1_000;

/** The default fee settings. */
export const DEFAULT_FEES: Readonly<FeeSettings> = Object.freeze({ dynamic: true, maxRate: 1_000, maxFeeKas: 1 });

/**
 * Parses `fees.maxRate`, a whole number of sompi per gram from 100 to 1,000,000, as a number or
 * numeric string. Returns null when invalid.
 */
export function parseMaxRate(v: unknown): number | null {
  const n = typeof v === 'number' ? v : typeof v === 'string' && /^\s*\d+\s*$/.test(v) ? Number(v) : NaN;
  return Number.isInteger(n) && n >= FEE_MAX_RATE_MIN && n <= FEE_MAX_RATE_MAX ? n : null;
}

/**
 * Parses `fees.maxFeeKas`, KAS from 0 to 1000 with at most 8 decimals, as a number or numeric
 * string.
 * 0 means no total cap. It returns null when invalid.
 */
export function parseMaxFeeKas(v: unknown): number | null {
  const n = typeof v === 'number' ? v : typeof v === 'string' && /^\s*\d+(\.\d+)?\s*$/.test(v) ? Number(v) : NaN;
  if (!Number.isFinite(n) || n < 0 || n > FEE_MAX_KAS_MAX) return null;
  return Math.abs(Math.round(n * 1e8) / 1e8 - n) < 1e-12 ? n : null;
}

// --- The estimate ---

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
// A usable feerate is a finite number, numeric string or bigint greater than 0.
function rateOf(v: unknown): number | null {
  const n = typeof v === 'number' ? v : typeof v === 'bigint' ? Number(v) : typeof v === 'string' && v.trim() !== '' ? Number(v) : NaN;
  return Number.isFinite(n) && n > 0 ? n : null;
}
const secondsOf = (v: unknown): number | undefined => {
  const n = typeof v === 'number' ? v : typeof v === 'bigint' ? Number(v) : typeof v === 'string' && v.trim() !== '' ? Number(v) : NaN;
  return Number.isFinite(n) && n >= 0 ? n : undefined;
};
const pick = (o: Record<string, unknown>, ...names: string[]): unknown => {
  for (const n of names) if (n in o) return o[n];
  return undefined;
};

function bucketOf(v: unknown): { feerate: number; seconds?: number } | null {
  if (!isRecord(v)) return null;
  const feerate = rateOf(pick(v, 'feerate', 'feeRate', 'fee_rate'));
  if (feerate === null) return null;
  const seconds = secondsOf(pick(v, 'estimatedSeconds', 'estimated_seconds'));
  return seconds === undefined ? { feerate } : { feerate, seconds };
}

/**
 * Reads the node's answer in the wRPC and SDK shape `{ estimate: { priorityBucket, normalBuckets[],
 * lowBuckets[] } }`. The inner object alone is accepted too. It returns null when any of the three
 * buckets is missing or has a non-finite or non-positive feerate, because a half-readable answer is
 * not trusted.
 */
export function parseFeeEstimate(raw: unknown): FeeEstimate | null {
  if (!isRecord(raw)) return null;
  const e = isRecord(raw.estimate) ? raw.estimate : raw;
  // An estimate this module already parsed, as a NodeApi hands over, is validated again.
  if ('priority' in e && 'normal' in e && 'low' in e) {
    const p0 = rateOf(e.priority);
    const n0 = rateOf(e.normal);
    const l0 = rateOf(e.low);
    if (p0 === null || n0 === null || l0 === null) return null;
    const s0 = isRecord(e.seconds) ? e.seconds : {};
    const seconds: NonNullable<FeeEstimate['seconds']> = {};
    for (const k of ['priority', 'normal', 'low'] as const) {
      const v = secondsOf(s0[k]);
      if (v !== undefined) seconds[k] = v;
    }
    return { priority: p0, normal: n0, low: l0, ...(Object.keys(seconds).length ? { seconds } : {}) };
  }
  const first = (v: unknown): unknown => (Array.isArray(v) ? v[0] : undefined);
  const p = bucketOf(pick(e, 'priorityBucket', 'priority_bucket'));
  const n = bucketOf(first(pick(e, 'normalBuckets', 'normal_buckets')));
  const l = bucketOf(first(pick(e, 'lowBuckets', 'low_buckets')));
  if (!p || !n || !l) return null;
  const seconds: NonNullable<FeeEstimate['seconds']> = {};
  if (p.seconds !== undefined) seconds.priority = p.seconds;
  if (n.seconds !== undefined) seconds.normal = n.seconds;
  if (l.seconds !== undefined) seconds.low = l.seconds;
  return { priority: p.feerate, normal: n.feerate, low: l.feerate, ...(Object.keys(seconds).length ? { seconds } : {}) };
}

/** Maps an urgency to its bucket of the estimate. */
export const BUCKET_OF: Readonly<Record<Urgency, 'priority' | 'normal' | 'low'>> = Object.freeze({ high: 'priority', normal: 'normal', low: 'low' });

// --- Congestion ---

/**
 * How far above the floor the first normal bucket must be for blocks to count as contended, in
 * percent. Same as kob_protocol::fee_estimate::CONTENDED_MARGIN_PERCENT.
 */
export const CONTENDED_MARGIN_PERCENT = 10;

/**
 * Tells whether blocks are contended, as the executor reads the estimate
 * (kob_protocol::fee_estimate::FeeEstimate::contended).
 *
 * Blocks are contended when the rate a normal-urgency transaction would pay is at least
 * {@link CONTENDED_MARGIN_PERCENT} above `floor`, which is at least the relay minimum. That rate is
 * the first normal bucket, rounded up as the policy rounds it. An idle node reports the floor in
 * every bucket. Only the normal bucket counts, because the priority bucket costs a little more
 * even on a quiet network and the low bucket is always the slowest. With no usable estimate the
 * answer is false.
 */
export function isContended(estimate: FeeEstimate | null | undefined, floor: bigint = RELAY_FEE_FLOOR): boolean {
  const normal = estimate ? rateOf(estimate.normal) : null;
  if (normal === null) return false;
  const base = floor < RELAY_FEE_FLOOR ? RELAY_FEE_FLOOR : floor;
  return BigInt(Math.ceil(normal)) * 100n >= base * BigInt(100 + CONTENDED_MARGIN_PERCENT);
}

// --- The rate ---

/** Where a rate came from. */
export type FeeSource = 'estimate' | 'floor';

/** Why the floor was paid: the policy is switched off, or there was no usable estimate. */
export type FloorReason = 'disabled' | 'unavailable' | 'funds';

/** The rate chosen for an urgency, with how it was chosen. */
export interface RatePick {
  urgency: Urgency;
  /** The rate in sompi per gram. */
  rate: bigint;
  source: FeeSource;
  /** Set for source 'floor' only. */
  reason?: FloorReason;
  /**
   * True when the floor or `maxRate` moved the bucket's feerate. Set for source 'estimate' only.
   */
  clamped: boolean;
  /** The bound that moved it. */
  clampedTo?: 'floor' | 'maxRate';
  /** The node's unrounded feerate for the bucket. Set for source 'estimate' only. */
  bucketFeerate?: number;
  /** The node's inclusion-time estimate for the bucket in seconds. */
  estimatedSeconds?: number;
}

/** The lowest rate this policy pays. */
export const floorOf = (p: FeePolicy): bigint => (p.floor < RELAY_FEE_FLOOR ? RELAY_FEE_FLOOR : p.floor);
/**
 * The highest rate a transaction of this policy may declare, which the confirmation checks the
 * builder against.
 * It is `maxRate` when the policy is dynamic, else the floor.
 */
export const declaredRateLimit = (p: Pick<FeePolicy, 'dynamic' | 'floor' | 'maxRate'>): bigint => {
  const floor = p.floor < RELAY_FEE_FLOOR ? RELAY_FEE_FLOOR : p.floor;
  return p.dynamic && p.maxRate > floor ? p.maxRate : floor;
};

/** The highest rate this policy pays (never below the floor). */
export const ceilingOf = (p: FeePolicy): bigint => (p.maxRate < floorOf(p) ? floorOf(p) : p.maxRate);

/**
 * Picks the rate to pay for `urgency` given the node's estimate, which is null when none is usable.
 * It never throws.
 */
export function pickRate(policy: FeePolicy, estimate: FeeEstimate | null, urgency: Urgency): RatePick {
  const floor = floorOf(policy);
  if (!policy.dynamic) return { urgency, rate: floor, source: 'floor', reason: 'disabled', clamped: false };
  const bucket = estimate ? rateOf(estimate[BUCKET_OF[urgency]]) : null;
  if (!estimate || bucket === null) return { urgency, rate: floor, source: 'floor', reason: 'unavailable', clamped: false };
  const wanted = BigInt(Math.ceil(bucket));
  const top = ceilingOf(policy);
  let rate = wanted;
  let clampedTo: 'floor' | 'maxRate' | undefined;
  if (rate < floor) {
    rate = floor;
    clampedTo = 'floor';
  }
  if (rate > top) {
    rate = top;
    clampedTo = 'maxRate';
  }
  const seconds = estimate.seconds?.[BUCKET_OF[urgency]];
  return {
    urgency,
    rate,
    source: 'estimate',
    clamped: clampedTo !== undefined,
    ...(clampedTo ? { clampedTo } : {}),
    bucketFeerate: bucket,
    ...(seconds !== undefined ? { estimatedSeconds: seconds } : {}),
  };
}

/**
 * Applies the total-fee cap to a transaction whose `fee` was built at `rate`.
 * When the fee is above `maxFeeSompi` and the rate is above the floor, it returns the rate to
 * rebuild at once, `max(floor, floor_div(rate * maxFeeSompi, fee))`. A `maxFeeSompi` of 0 means no
 * cap. It returns null to keep the transaction as built, which is when no cap is hit or the rate is
 * already at the floor. The cap never pushes below the floor.
 */
export function capForTotal(policy: FeePolicy, rate: bigint, fee: bigint): bigint | null {
  const floor = floorOf(policy);
  if (policy.maxFeeSompi <= 0n || fee <= policy.maxFeeSompi || rate <= floor || fee <= 0n) return null;
  const lowered = (rate * policy.maxFeeSompi) / fee;
  const next = lowered < floor ? floor : lowered;
  return next < rate ? next : null;
}

/**
 * What happened to one transaction's fee, for the disclosure: the pick, the final rate and the cap.
 */
export interface FeeChoice {
  urgency: Urgency;
  /** The rate the transaction was finally built at, in sompi per gram. */
  rate: bigint;
  /** The fee of the built transaction in sompi. */
  fee: bigint;
  source: FeeSource;
  reason?: FloorReason;
  /** True when the floor or `maxRate` moved the bucket's feerate. */
  clamped: boolean;
  clampedTo?: 'floor' | 'maxRate';
  bucketFeerate?: number;
  estimatedSeconds?: number;
  /** The picked rate, when the total cap lowered the rate from it. */
  cappedFrom?: bigint;
  /**
   * True when the fee is above the total cap and could not be lowered, because the rate is at the
   * floor.
   */
  overCap: boolean;
  /** The total cap in force in sompi. 0 means none. */
  maxFeeSompi: bigint;
  floor: bigint;
}

/**
 * Combines the pick with what was built. `rate` and `fee` are the final ones, after the cap rebuild
 * if any.
 */
export function describeFee(policy: FeePolicy, pick: RatePick, rate: bigint, fee: bigint): FeeChoice {
  const maxFee = policy.maxFeeSompi;
  const { urgency, source, clamped } = pick;
  return {
    urgency,
    rate,
    fee,
    source,
    ...(pick.reason ? { reason: pick.reason } : {}),
    clamped,
    ...(pick.clampedTo ? { clampedTo: pick.clampedTo } : {}),
    ...(pick.bucketFeerate !== undefined ? { bucketFeerate: pick.bucketFeerate } : {}),
    ...(pick.estimatedSeconds !== undefined ? { estimatedSeconds: pick.estimatedSeconds } : {}),
    ...(rate < pick.rate ? { cappedFrom: pick.rate } : {}),
    overCap: maxFee > 0n && fee > maxFee,
    maxFeeSompi: maxFee,
    floor: floorOf(policy),
  };
}

// --- Build with the cap ---

/** The slice of a built transaction the cap looks at. */
export interface BuiltFeeLike {
  fee: { fee: string; feeRate?: string };
}

/**
 * A request that names its feerate in `fee.feeRate` as a decimal string, like every kob-wasm build
 * request.
 */
export interface FeeRequestLike {
  fee?: { feeRate?: string | null; feeMode?: unknown } | null;
}

/** The transaction and request returned by `buildWithCap`. */
export interface CappedBuild<Req, B> {
  built: B;
  request: Req;
  /** The rate of the returned transaction. It is the floor when the request named none. */
  rate: bigint;
}

/**
 * Builds `request` and applies the total cap.
 * When the fee comes out above `policy.maxFeeSompi` at a rate above the floor, the request is
 * rebuilt once at the rate of `capForTotal`. A failing rebuild keeps the first transaction. The
 * build errors of the first call propagate unchanged.
 */
export function buildWithCap<Req extends FeeRequestLike, B extends BuiltFeeLike>(policy: FeePolicy | null | undefined, request: Req, build: (r: Req) => B): CappedBuild<Req, B> {
  const floor = policy ? floorOf(policy) : RELAY_FEE_FLOOR;
  const asked = request.fee?.feeRate;
  const rate = asked !== undefined && asked !== null && /^\d+$/.test(asked) ? BigInt(asked) : floor;
  const built = build(request);
  if (!policy) return { built, request, rate };
  const next = capForTotal(policy, rate, BigInt(built.fee.fee));
  if (next === null) return { built, request, rate };
  const again = { ...request, fee: { ...(request.fee ?? {}), feeRate: next.toString() } } as Req;
  try {
    return { built: build(again), request: again, rate: next };
  } catch {
    return { built, request, rate };
  }
}

// --- What the planners carry ---

/**
 * What a planner needs to choose a rate, the policy and the newest usable estimate or null. It is
 * read once per planning environment.
 */
export interface FeeContext {
  policy: FeePolicy;
  estimate: FeeEstimate | null;
}

/** Returns the pick of a context, or the floor pick when the environment carries no context. */
export function pickFor(ctx: FeeContext | undefined, urgency: Urgency): RatePick {
  return pickRate(ctx?.policy ?? normalizePolicy({ dynamic: false }), ctx?.estimate ?? null, urgency);
}

/**
 * The fee fields every planning environment carries. `feeRate` is an explicit override, `fees` is
 * the policy input and `feePick` is what the planner picked.
 */
export interface FeeEnvFields {
  feeRate?: bigint;
  fees?: FeeContext;
  feePick?: RatePick;
}

/**
 * Returns the environment with `feeRate` and `feePick` set to the rate of `urgency`.
 * An environment that already names a `feeRate`, as a caller's override, or carries no policy is
 * returned unchanged. It pays the builder default, which is the floor.
 */
export function withUrgency<E extends FeeEnvFields>(env: E, urgency: Urgency): E {
  if (!env.fees || env.feeRate !== undefined) return env;
  const pick = pickRate(env.fees.policy, env.fees.estimate, urgency);
  return { ...env, feeRate: pick.rate, feePick: pick };
}

/**
 * Returns the same environment at the floor, for the retry of a plan the wallet cannot fund at the
 * picked rate. An estimate must never make a payable transaction impossible. It returns null when
 * the environment is already at the floor or has no policy.
 */
export function atFloor<E extends FeeEnvFields>(env: E): E | null {
  if (!env.fees || !env.feePick || env.feeRate === undefined) return null;
  const floor = floorOf(env.fees.policy);
  if (env.feeRate <= floor) return null;
  return { ...env, feeRate: floor, feePick: { urgency: env.feePick.urgency, rate: floor, source: 'floor', reason: 'funds', clamped: false } };
}

/**
 * Remembers what the built transaction pays for the fee disclosure. It does nothing without a
 * policy and pick.
 */
export function recordFee(env: FeeEnvFields, built: BuiltFeeLike, rate: bigint): void {
  if (!env.fees || !env.feePick) return;
  rememberFeeChoice(built, describeFee(env.fees.policy, env.feePick, rate, BigInt(built.fee.fee)));
}

// The fee choices of built transactions, for the disclosure. A WeakMap keeps them, because no
// plan type carries one and a rebuilt object simply has none.
const choices = new WeakMap<object, FeeChoice>();
/** Remembers the fee choice of a built transaction. */
export function rememberFeeChoice(built: object, choice: FeeChoice): void {
  choices.set(built, choice);
}
/** Returns the fee choice remembered for a built transaction, or null. */
export function feeChoiceOf(built: object): FeeChoice | null {
  return choices.get(built) ?? null;
}

/**
 * What the app holds, the policy and, when it is dynamic, the oracle that reads the node's
 * estimate.
 */
export interface FeeService {
  policy: FeePolicy;
  oracle: FeeOracle | null;
}

// --- The oracle ---

/** Options of `FeeOracle`. */
export interface FeeOracleOptions {
  /**
   * Reads the node's raw answer, which `parseFeeEstimate` interprets. It may reject or return null.
   */
  fetch: () => Promise<unknown>;
  /** The refresh limit in ms. Defaults to the policy default. */
  refreshMs?: number;
  /** The age limit in ms. Defaults to the policy default. */
  maxAgeMs?: number;
  /** A call that does not answer within this many ms counts as failed. Defaults to 4000. */
  timeoutMs?: number;
  /** The clock in ms, injectable for tests. */
  now?: () => number;
}

/**
 * A cached reader of the fee estimate.
 * `get()` asks the node at most once per `refreshMs`, and concurrent callers share one call. It
 * keeps the last good estimate and serves it until it is `maxAgeMs` old. It never rejects. With no
 * usable estimate it returns null, and the policy then pays the floor.
 */
export class FeeOracle {
  private readonly f: () => Promise<unknown>;
  private readonly refreshMs: number;
  private readonly maxAgeMs: number;
  private readonly timeoutMs: number;
  private readonly now: () => number;
  private good: { estimate: FeeEstimate; at: number } | null = null;
  private lastTry = Number.NEGATIVE_INFINITY;
  private inflight: Promise<void> | null = null;

  constructor(o: FeeOracleOptions) {
    this.f = o.fetch;
    this.refreshMs = o.refreshMs ?? DEFAULT_FEE_POLICY.refreshMs;
    this.maxAgeMs = o.maxAgeMs ?? DEFAULT_FEE_POLICY.maxAgeMs;
    this.timeoutMs = o.timeoutMs ?? 4_000;
    this.now = o.now ?? Date.now;
  }

  private fresh(): FeeEstimate | null {
    if (!this.good) return null;
    const age = this.now() - this.good.at;
    return age >= 0 && age <= this.maxAgeMs ? this.good.estimate : null;
  }

  private async refresh(): Promise<void> {
    this.lastTry = this.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const timeout = new Promise<never>((_, rej) => {
        timer = setTimeout(() => rej(new Error('getFeeEstimate timed out')), this.timeoutMs);
      });
      const raw = await Promise.race([Promise.resolve().then(this.f), timeout]);
      const estimate = parseFeeEstimate(raw);
      if (estimate) this.good = { estimate, at: this.now() };
    } catch {
      /* a failed read keeps the last good estimate until it ages out */
    } finally {
      if (timer !== undefined) clearTimeout(timer);
    }
  }

  /**
   * Returns the newest usable estimate, refreshed when the last try is `refreshMs` old, or null.
   */
  async get(): Promise<FeeEstimate | null> {
    if (this.now() - this.lastTry >= this.refreshMs || this.now() < this.lastTry) {
      this.inflight ??= this.refresh().finally(() => {
        this.inflight = null;
      });
    }
    if (this.inflight) await this.inflight;
    return this.fresh();
  }

  /** Returns the cached estimate without any call, or null when there is none or it is too old. */
  peek(): FeeEstimate | null {
    return this.fresh();
  }
}

/**
 * Creates the policy of `settings` and, when it is dynamic, an oracle over the node's
 * `getFeeEstimate`. A node or transport without that method answers null, which means the floor.
 * The wallet uses it through app/services.ts.
 */
export function createFeeService(settings: { dynamic: boolean; maxRate: number; maxFeeKas: number }, node: { getFeeEstimate?: () => Promise<FeeEstimate | null> }, now?: () => number): FeeService {
  const policy = policyFromSettings(settings);
  const oracle = policy.dynamic
    ? new FeeOracle({ fetch: async () => (node.getFeeEstimate ? await node.getFeeEstimate() : null), refreshMs: policy.refreshMs, maxAgeMs: policy.maxAgeMs, ...(now ? { now } : {}) })
    : null;
  return { policy, oracle };
}

/**
 * Reads the planners' fee context of one moment, the policy and, only when it is dynamic, the
 * oracle's estimate.
 */
export async function readFeeContext(policy: FeePolicy, oracle: Pick<FeeOracle, 'get'> | null | undefined): Promise<FeeContext> {
  if (!policy.dynamic || !oracle) return { policy, estimate: null };
  return { policy, estimate: await oracle.get() };
}
