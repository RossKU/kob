// The retry rules of one x402 payment (docs/spec/x402-retry.md), the same as the Rust `kob_x402::client::retry` (the
// classification is compared with the wasm build's `retryDecision` in test/retry-wasm.test.ts).
//
//   resend  : the outcome is unknown (no answer, timeout, 5xx, `settlement_pending`, `node_unavailable`) or the same signed
//             payment may still succeed (`rate_limited`, `intent_not_executable`): send the SAME artifact again (same payment
//             id, same transaction). The paywall and the facilitator answer an identical retry from memory / their ledger, so
//             a payment that went through while its answer was lost is found, never paid again.
//   rebuild : the facilitator failed the attempt and released its inputs (`order_conflict`, the node refused it, the
//             authorization expired): build a NEW attempt from the chain state of the moment (a fresh quote), held again to
//             every limit and preflighted, under a fresh payment id, and spending the payment's ANCHOR (an input of the first
//             attempt the payer owns), so that any two attempts of one payment exclude each other on chain.
//   stop    : retrying cannot succeed (a policy refusal, an invalid offer, a payment id conflict, anything unknown).

export type RetryStep = 'resend' | 'rebuild' | 'stop';

/** Diagnostics after which the same signed payment is sent again. */
export const RESEND_DIAGNOSTICS: readonly string[] = ['settlement_pending', 'node_unavailable', 'rate_limited', 'intent_not_executable', 'invoice_pending'];

/** Diagnostics of an attempt the facilitator failed (its inputs released) that a new attempt may get past. */
export const REBUILD_DIAGNOSTICS: readonly string[] = [
  'order_conflict',
  'order_not_spendable',
  'invalid_kaspa_exact_utxo',
  'invalid_kaspa_exact_transaction',
  'invalid_kaspa_exact_fee',
  'invalid_kaspa_exact_mass',
  'expired_authorization',
  'expired',
];

/** The step after a failure with `diagnostic` and its `retryable` flag (`internal` follows the flag; anything unlisted stops). */
export function classifyFailure(diagnostic: string | undefined, retryable: boolean): RetryStep {
  if (diagnostic === undefined) return 'stop';
  if (RESEND_DIAGNOSTICS.includes(diagnostic)) return 'resend';
  if (REBUILD_DIAGNOSTICS.includes(diagnostic)) return 'rebuild';
  if (diagnostic === 'internal' && retryable) return 'resend';
  return 'stop';
}

/**
 * The step after an HTTP answer to a paid request that is not a verified success. `status` 0 = no answer (connection error,
 * timeout). No answer, a 5xx and a 2xx without a settlement leave the outcome unknown: resend. A 402 and a 409 follow the
 * diagnostic; a 429 resends; any other 4xx stops.
 */
export function classifyStatus(status: number, diagnostic: string | undefined, retryable: boolean): RetryStep {
  if (status === 0 || (status >= 200 && status < 300) || status === 429) return 'resend';
  if (status === 402 || status === 409) return classifyFailure(diagnostic, retryable);
  if (status >= 500 && status < 600) {
    return diagnostic !== undefined && diagnostic !== 'internal' && classifyFailure(diagnostic, retryable) === 'stop' ? 'stop' : 'resend';
  }
  return 'stop';
}

/** Bounds of the retry of one payment. `retry: false` on the client is `{ attempts: 1, resends: 0 }`. */
export interface RetryOptions {
  /** Signed attempts at most, the first included (default 3; 1 = never rebuild). */
  attempts?: number;
  /** Re-sends of one signed attempt at most (default 4; 0 = never re-send). */
  resends?: number;
  /** First backoff step in ms (default 500); it doubles per step up to `maxDelayMs` (default 8000), with equal jitter. */
  baseDelayMs?: number;
  maxDelayMs?: number;
  /** Overall budget in ms from the first paid send (default 120000), further capped by the offer's `maxTimeoutSeconds`. */
  budgetMs?: number;
  /** Uniform draw in [0, 1) for the jitter (default Math.random). */
  random?: () => number;
  /** Waits `ms` (default a timer; tests inject one). */
  sleep?: (ms: number) => Promise<void>;
}

export interface RetryPolicy {
  attempts: number;
  resends: number;
  baseDelayMs: number;
  maxDelayMs: number;
  budgetMs: number;
  random: () => number;
  sleep: (ms: number) => Promise<void>;
}

export const DEFAULT_RETRY: Readonly<Omit<RetryPolicy, 'random' | 'sleep'>> = { attempts: 3, resends: 4, baseDelayMs: 500, maxDelayMs: 8_000, budgetMs: 120_000 };

function nonNegInt(name: string, v: number | undefined, dflt: number, min = 0): number {
  if (v === undefined) return dflt;
  if (!Number.isInteger(v) || v < min) throw new RangeError(`retry.${name} must be an integer >= ${min}`);
  return v;
}

/** The policy of `options` (`false`: one attempt, no re-send). */
export function retryPolicy(options: RetryOptions | false | undefined): RetryPolicy {
  const o = options === false ? { attempts: 1, resends: 0 } : (options ?? {});
  return {
    attempts: nonNegInt('attempts', o.attempts, DEFAULT_RETRY.attempts, 1),
    resends: nonNegInt('resends', o.resends, DEFAULT_RETRY.resends),
    baseDelayMs: nonNegInt('baseDelayMs', o.baseDelayMs, DEFAULT_RETRY.baseDelayMs),
    maxDelayMs: nonNegInt('maxDelayMs', o.maxDelayMs, DEFAULT_RETRY.maxDelayMs),
    budgetMs: nonNegInt('budgetMs', o.budgetMs, DEFAULT_RETRY.budgetMs),
    random: o.random ?? Math.random,
    sleep: o.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms))),
  };
}

/** Backoff before step `n` (1-based): half of the exponential step fixed, half scaled by `unit` (the Rust `delay_ms`). */
export function backoffMs(p: Pick<RetryPolicy, 'baseDelayMs' | 'maxDelayMs'>, n: number, unit: number): number {
  const exp = Math.min(p.baseDelayMs * 2 ** Math.min(Math.max(n - 1, 0), 20), p.maxDelayMs);
  const u = Number.isFinite(unit) ? Math.min(Math.max(unit, 0), 1) : 0.5;
  const half = Math.floor(exp / 2);
  return half + Math.floor((exp - half) * u);
}

/** Deadline (unix ms) of a payment starting at `startMs` for an offer of `maxTimeoutSeconds`. */
export function retryDeadline(p: Pick<RetryPolicy, 'budgetMs'>, startMs: number, maxTimeoutSeconds: number): number {
  return startMs + Math.min(p.budgetMs, maxTimeoutSeconds * 1000);
}

/** `txid:index` of an outpoint (lower-case txid). */
export function outpointKey(o: { txid: string; index: number }): string {
  return `${o.txid.toLowerCase()}:${o.index}`;
}

/** One attempt as the error / receipt report it. */
export interface AttemptSummary {
  paymentId: string;
  transactionId: string;
  /** How the attempt ended: settled, a failure diagnostic, or `unknown` (no usable answer). */
  outcome: string;
  /** Sends of this attempt (1 + re-sends). */
  sends: number;
}
