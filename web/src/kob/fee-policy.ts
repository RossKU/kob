// Dynamic priority-fee policy (pure): which feerate (sompi per gram of mass) a transaction pays, from the node's `getFeeEstimate` and the urgency of the
// action. It mirrors the Rust executor's policy exactly (docs/ops/executor.md): the same buckets, the same clamp, the same total cap, the same fallback.
//
//   urgency -> bucket       high -> priorityBucket (sub-second), normal -> normalBuckets[0] (sub-minute), low -> lowBuckets[0] (sub-hour)
//   rate                    min(max(ceil(bucketFeerate), floor), max(maxRate, floor))
//   no usable estimate      the floor (call failed, method missing, malformed / non-finite / <= 0 values, older than maxAgeMs). NEVER an error.
//   total cap               built fee > maxFeeSompi and rate > floor: rebuild once at max(floor, floor_div(rate * maxFeeSompi, fee)). The cap never goes below
//                           the floor: a floor-rate transaction above the cap is still sent (and disclosed).
//   `dynamic: false`        always the floor (the behaviour before this policy existed).
//
// Nothing here touches the network: `FeeOracle` takes the fetch function and the clock as parameters.

/** kob_protocol MIN_FEE_RATE: the node's relay floor in sompi per gram. The builders refuse a lower rate. */
export const RELAY_FEE_FLOOR = 100n;

/** How urgent an action is: it picks the bucket of the node's estimate. */
export type Urgency = 'high' | 'normal' | 'low';

export const URGENCIES: readonly Urgency[] = ['high', 'normal', 'low'];

/** The three feerates the policy reads from `getFeeEstimate` (sompi per gram), plus the node's inclusion-time estimate of each (disclosure only). */
export interface FeeEstimate {
  /** priorityBucket.feerate */
  priority: number;
  /** normalBuckets[0].feerate */
  normal: number;
  /** lowBuckets[0].feerate */
  low: number;
  /** estimated inclusion time of each bucket, seconds (as the node reports it) */
  seconds?: { priority?: number; normal?: number; low?: number };
}

export interface FeePolicy {
  /** false: always pay the floor */
  dynamic: boolean;
  /** lowest rate ever paid, sompi per gram (>= RELAY_FEE_FLOOR) */
  floor: bigint;
  /** highest rate ever paid, sompi per gram (a value below `floor` means "the floor") */
  maxRate: bigint;
  /** most one transaction pays in total, sompi; 0 = no total cap */
  maxFeeSompi: bigint;
  /** the estimate is refreshed at most this often, ms */
  refreshMs: number;
  /** an estimate older than this is not used, ms */
  maxAgeMs: number;
}

export const DEFAULT_FEE_POLICY: Readonly<FeePolicy> = Object.freeze({
  dynamic: true,
  floor: RELAY_FEE_FLOOR,
  maxRate: 1_000n,
  maxFeeSompi: 100_000_000n,
  refreshMs: 10_000,
  maxAgeMs: 60_000,
});

/** A policy with every field valid: a floor below the relay minimum is raised to it, a negative cap means none. */
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

/** The policy a wallet / app configuration asks for: `maxRate` in sompi per gram, `maxFeeKas` in KAS (0 = no total cap). Timings stay the defaults. */
export function policyFromSettings(s: { dynamic: boolean; maxRate: number; maxFeeKas: number }): FeePolicy {
  return normalizePolicy({ dynamic: s.dynamic, maxRate: BigInt(Math.max(0, Math.floor(s.maxRate))), maxFeeSompi: BigInt(Math.max(0, Math.round(s.maxFeeKas * 1e8))) });
}

// ------------------------------------------------------------------------------------------------ settings (config)

/** The fee settings of an app / soak configuration: `maxRate` in sompi per gram, `maxFeeKas` in KAS (0 = no total cap). */
export interface FeeSettings {
  dynamic: boolean;
  maxRate: number;
  maxFeeKas: number;
}

/** Bounds of `maxRate` (sompi per gram; the floor is the relay minimum) and `maxFeeKas`. */
export const FEE_MAX_RATE_MIN = 100;
export const FEE_MAX_RATE_MAX = 1_000_000;
export const FEE_MAX_KAS_MAX = 1_000;

export const DEFAULT_FEES: Readonly<FeeSettings> = Object.freeze({ dynamic: true, maxRate: 1_000, maxFeeKas: 1 });

/** `fees.maxRate`: a whole number of sompi per gram in [100, 1,000,000] (a number or a numeric string); null when invalid. */
export function parseMaxRate(v: unknown): number | null {
  const n = typeof v === 'number' ? v : typeof v === 'string' && /^\s*\d+\s*$/.test(v) ? Number(v) : NaN;
  return Number.isInteger(n) && n >= FEE_MAX_RATE_MIN && n <= FEE_MAX_RATE_MAX ? n : null;
}

/** `fees.maxFeeKas`: KAS in [0, 1000] with at most 8 decimals (a number or a numeric string); 0 = no total cap; null when invalid. */
export function parseMaxFeeKas(v: unknown): number | null {
  const n = typeof v === 'number' ? v : typeof v === 'string' && /^\s*\d+(\.\d+)?\s*$/.test(v) ? Number(v) : NaN;
  if (!Number.isFinite(n) || n < 0 || n > FEE_MAX_KAS_MAX) return null;
  return Math.abs(Math.round(n * 1e8) / 1e8 - n) < 1e-12 ? n : null;
}

// ------------------------------------------------------------------------------------------------ the estimate

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
/** a usable feerate: a finite number (or numeric string / bigint) greater than 0 */
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
 * Reads the node's answer: the wRPC / SDK shape `{ estimate: { priorityBucket, normalBuckets[], lowBuckets[] } }`, or the inner object alone.
 * Null when any of the three buckets is missing or has a non-finite / non-positive feerate (a half-readable answer is not trusted).
 */
export function parseFeeEstimate(raw: unknown): FeeEstimate | null {
  if (!isRecord(raw)) return null;
  const e = isRecord(raw.estimate) ? raw.estimate : raw;
  // an estimate this module already parsed (a NodeApi hands those over): validated again, never trusted blindly
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

export const BUCKET_OF: Readonly<Record<Urgency, 'priority' | 'normal' | 'low'>> = Object.freeze({ high: 'priority', normal: 'normal', low: 'low' });

// ------------------------------------------------------------------------------------------------ the rate

export type FeeSource = 'estimate' | 'floor';

/** Why the floor was paid: the policy is switched off, or there was no usable estimate. */
export type FloorReason = 'disabled' | 'unavailable' | 'funds';

export interface RatePick {
  urgency: Urgency;
  /** sompi per gram */
  rate: bigint;
  source: FeeSource;
  /** source 'floor' only */
  reason?: FloorReason;
  /** true when the bucket's feerate was moved by the floor or by `maxRate` (source 'estimate' only) */
  clamped: boolean;
  /** which bound moved it */
  clampedTo?: 'floor' | 'maxRate';
  /** the node's feerate for the bucket (unrounded), source 'estimate' only */
  bucketFeerate?: number;
  /** the node's inclusion-time estimate for the bucket, seconds */
  estimatedSeconds?: number;
}

/** The lowest rate this policy pays. */
export const floorOf = (p: FeePolicy): bigint => (p.floor < RELAY_FEE_FLOOR ? RELAY_FEE_FLOOR : p.floor);
/** The highest rate a transaction of this policy may DECLARE (what the confirmation checks the builder against): `maxRate` when dynamic, else the floor. */
export const declaredRateLimit = (p: Pick<FeePolicy, 'dynamic' | 'floor' | 'maxRate'>): bigint => {
  const floor = p.floor < RELAY_FEE_FLOOR ? RELAY_FEE_FLOOR : p.floor;
  return p.dynamic && p.maxRate > floor ? p.maxRate : floor;
};

/** The highest rate this policy pays (never below the floor). */
export const ceilingOf = (p: FeePolicy): bigint => (p.maxRate < floorOf(p) ? floorOf(p) : p.maxRate);

/** Which rate to pay for `urgency` given the node's estimate (null = none usable). Pure; never throws. */
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
 * The total-fee cap. `fee` was built at `rate`: when it is above `maxFeeSompi` (0 = no cap) and the rate is above the floor, the rate to rebuild at
 * (once): max(floor, floor_div(rate * maxFeeSompi, fee)). Null = keep the transaction as built (no cap hit, or already at the floor: the cap never
 * pushes below the floor, so a floor-rate transaction above the cap is still sent).
 */
export function capForTotal(policy: FeePolicy, rate: bigint, fee: bigint): bigint | null {
  const floor = floorOf(policy);
  if (policy.maxFeeSompi <= 0n || fee <= policy.maxFeeSompi || rate <= floor || fee <= 0n) return null;
  const lowered = (rate * policy.maxFeeSompi) / fee;
  const next = lowered < floor ? floor : lowered;
  return next < rate ? next : null;
}

/** What actually happened to one transaction's fee (for the disclosure): the pick, the final rate and the cap. */
export interface FeeChoice {
  urgency: Urgency;
  /** the rate the transaction was finally built at, sompi per gram */
  rate: bigint;
  /** the fee of the built transaction, sompi */
  fee: bigint;
  source: FeeSource;
  reason?: FloorReason;
  /** the bucket's feerate was moved by the floor / `maxRate` */
  clamped: boolean;
  clampedTo?: 'floor' | 'maxRate';
  bucketFeerate?: number;
  estimatedSeconds?: number;
  /** the total cap lowered the rate from this one (the picked rate) */
  cappedFrom?: bigint;
  /** the fee is above the total cap and could not be lowered (rate already at the floor) */
  overCap: boolean;
  /** the total cap in force, sompi (0 = none) */
  maxFeeSompi: bigint;
  floor: bigint;
}

/** Combines the pick with what was built. `rate` / `fee` are the FINAL rate and fee (after the cap rebuild, if any). */
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

// ------------------------------------------------------------------------------------------------ build with the cap

/** The slice of a built transaction the cap looks at. */
export interface BuiltFeeLike {
  fee: { fee: string; feeRate?: string };
}

/** A request that names its feerate (`fee.feeRate`, a decimal string) like every kob-wasm build request. */
export interface FeeRequestLike {
  fee?: { feeRate?: string | null; feeMode?: unknown } | null;
}

export interface CappedBuild<Req, B> {
  built: B;
  request: Req;
  /** the rate of the returned transaction (the floor when the request named none) */
  rate: bigint;
}

/**
 * Builds `request` and applies the total cap: when the fee comes out above `policy.maxFeeSompi` at a rate above the floor, the request is rebuilt ONCE at
 * `capForTotal`'s rate. A failing rebuild keeps the first transaction (a lower rate can only make the fee smaller, but the builder is not ours to trust
 * blindly). The build errors of the first call propagate unchanged.
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

// ------------------------------------------------------------------------------------------------ what the planners carry

/** What a planner needs to choose a rate: the policy and the newest usable estimate (null = none). Read once per planning environment. */
export interface FeeContext {
  policy: FeePolicy;
  estimate: FeeEstimate | null;
}

/** The pick of a context, or the floor pick when the environment carries no context. */
export function pickFor(ctx: FeeContext | undefined, urgency: Urgency): RatePick {
  return pickRate(ctx?.policy ?? normalizePolicy({ dynamic: false }), ctx?.estimate ?? null, urgency);
}

/** The fee fields every planning environment carries: `feeRate` is an explicit override, `fees` the policy input, `feePick` what the planner picked. */
export interface FeeEnvFields {
  feeRate?: bigint;
  fees?: FeeContext;
  feePick?: RatePick;
}

/**
 * The environment with the rate of `urgency`: `feeRate` (and `feePick`) set from the policy. An environment that already names a `feeRate` (a caller's
 * override) or carries no policy is returned unchanged: it pays what it always paid (the builder default, the floor).
 */
export function withUrgency<E extends FeeEnvFields>(env: E, urgency: Urgency): E {
  if (!env.fees || env.feeRate !== undefined) return env;
  const pick = pickRate(env.fees.policy, env.fees.estimate, urgency);
  return { ...env, feeRate: pick.rate, feePick: pick };
}

/**
 * The same environment at the floor, for the retry of a plan the wallet cannot fund at the picked rate (the estimate must never make a payable
 * transaction impossible). Null when the environment is already at the floor or has no policy.
 */
export function atFloor<E extends FeeEnvFields>(env: E): E | null {
  if (!env.fees || !env.feePick || env.feeRate === undefined) return null;
  const floor = floorOf(env.fees.policy);
  if (env.feeRate <= floor) return null;
  return { ...env, feeRate: floor, feePick: { urgency: env.feePick.urgency, rate: floor, source: 'floor', reason: 'funds', clamped: false } };
}

/** Remembers what the built transaction pays for the confirmation screen (no-op when the environment has no policy / pick). */
export function recordFee(env: FeeEnvFields, built: BuiltFeeLike, rate: bigint): void {
  if (!env.fees || !env.feePick) return;
  rememberFeeChoice(built, describeFee(env.fees.policy, env.feePick, rate, BigInt(built.fee.fee)));
}

// choices of built transactions, for the confirmation screen (a WeakMap: no plan type carries it, a rebuilt object simply has none)
const choices = new WeakMap<object, FeeChoice>();
export function rememberFeeChoice(built: object, choice: FeeChoice): void {
  choices.set(built, choice);
}
export function feeChoiceOf(built: object): FeeChoice | null {
  return choices.get(built) ?? null;
}

/** What the app holds: the policy and (when it is dynamic) the oracle that reads the node's estimate. */
export interface FeeService {
  policy: FeePolicy;
  oracle: FeeOracle | null;
}

// ------------------------------------------------------------------------------------------------ the oracle

export interface FeeOracleOptions {
  /** reads the node's answer (raw: `parseFeeEstimate` interprets it); may reject or return null */
  fetch: () => Promise<unknown>;
  /** refresh and age limits; default the policy defaults */
  refreshMs?: number;
  maxAgeMs?: number;
  /** a call that does not answer within this long counts as failed, ms (default 4000) */
  timeoutMs?: number;
  /** clock, ms (injectable for tests) */
  now?: () => number;
}

/**
 * Cached reader of the fee estimate. `get()` asks the node at most once per `refreshMs` (concurrent callers share one call), keeps the last GOOD
 * estimate and serves it until it is `maxAgeMs` old, and NEVER rejects: no usable estimate is `null` (the policy then pays the floor).
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

  /** The newest usable estimate (refreshed when the last try is `refreshMs` old), or null. */
  async get(): Promise<FeeEstimate | null> {
    if (this.now() - this.lastTry >= this.refreshMs || this.now() < this.lastTry) {
      this.inflight ??= this.refresh().finally(() => {
        this.inflight = null;
      });
    }
    if (this.inflight) await this.inflight;
    return this.fresh();
  }

  /** The cached estimate without any call (null when none or too old). */
  peek(): FeeEstimate | null {
    return this.fresh();
  }
}

/**
 * The policy of `settings` and, when it is dynamic, an oracle over the node's `getFeeEstimate` (a node, or a transport, without it answers null: the
 * floor). Shared by the wallet (app/services.ts) and the soak bots.
 */
export function createFeeService(settings: { dynamic: boolean; maxRate: number; maxFeeKas: number }, node: { getFeeEstimate?: () => Promise<FeeEstimate | null> }, now?: () => number): FeeService {
  const policy = policyFromSettings(settings);
  const oracle = policy.dynamic
    ? new FeeOracle({ fetch: async () => (node.getFeeEstimate ? await node.getFeeEstimate() : null), refreshMs: policy.refreshMs, maxAgeMs: policy.maxAgeMs, ...(now ? { now } : {}) })
    : null;
  return { policy, oracle };
}

/** The planners' fee context of one moment: the policy and (only when it is dynamic) the oracle's estimate. */
export async function readFeeContext(policy: FeePolicy, oracle: Pick<FeeOracle, 'get'> | null | undefined): Promise<FeeContext> {
  if (!policy.dynamic || !oracle) return { policy, estimate: null };
  return { policy, estimate: await oracle.get() };
}
