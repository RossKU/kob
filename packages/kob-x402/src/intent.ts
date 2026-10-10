// Intent-based swap-and-pay (`kob-intent-v1`), payer side: the payer signs ONE transaction, the creation of a KOB router
// intent that locks its funds with its worst case as terms (`maxPay` + `maxExtra` sompi, or `maxSell` token units) and
// binds the merchant's output; the facilitator verifies it, broadcasts it and executes the intent against the book, re-
// planning on a conflicting fill without the payer. Settlement is the execution accepted. Keep the returned `intent`
// handle: an intent that was not executed by its deadline is cancelled by the payer (`cancelIntent`), or expired by anyone
// from the deadline on (the facilitator does it); either returns everything it locked. The deadline is the
// authorization's expiry, never later than an invoice's.
//
// The creation is NOT broadcast by the payer: the facilitator does it (a creation that is public before the payment
// reaches the facilitator could be presented by anyone holding the offer, so the facilitator refuses one).

import { KobX402Error } from './errors.ts';
import { InvoiceClient, intentLifetimeMs } from './invoice.ts';
import type { FetchedInvoice } from './invoice.ts';
import type { CancelIntentRequest, ExpireIntentRequest, IntentPayRequest, IntentPayResult, InputSignature, KobWasm, PreparedIntent, SignedTransaction } from './wasm.ts';
import { BINDING_INTENT } from './types.ts';
import { withFeeFloor } from './fee.ts';
import { backoffMs, classifyFailure, classifyStatus, retryPolicy } from './retry.ts';
import type { RetryOptions, RetryStep } from './retry.ts';
import type { PaymentRequirements, SettlementResponse } from './types.ts';

function need<K extends keyof KobWasm>(wasm: KobWasm, k: K): NonNullable<KobWasm[K]> {
  const f = wasm[k];
  if (typeof f !== 'function') throw new KobX402Error('unsupported', `this KobWasm has no ${String(k)} (intent mode)`);
  return (f as (...a: unknown[]) => unknown).bind(wasm) as NonNullable<KobWasm[K]>;
}

/** True for an intent offer (`extra.route.binding` = `kob-intent-v1`). */
export function isIntentOffer(r: PaymentRequirements): boolean {
  return r.extra?.route?.binding === BINDING_INTENT;
}

/** Builds and signs (local keys) an intent creation for `req.requirements` (at `options.feeRate`, retried once at the floor). */
export function payIntent(wasm: KobWasm, req: IntentPayRequest): IntentPayResult {
  if (!isIntentOffer(req.requirements)) throw new KobX402Error('bad_request', 'not an intent offer (extra.route.binding kob-intent-v1)');
  const build = need(wasm, 'payIntent');
  return withFeeFloor(req.options.feeRate, req.feeRateFloor, (feeRate) => build(atRate(req, feeRate)));
}

/** Wallet flow, step 1: the unsigned creation (`built.sign`: the payer's only signatures). */
export function prepareIntent(wasm: KobWasm, req: IntentPayRequest): PreparedIntent {
  if (!isIntentOffer(req.requirements)) throw new KobX402Error('bad_request', 'not an intent offer (extra.route.binding kob-intent-v1)');
  const build = need(wasm, 'prepareIntent');
  return withFeeFloor(req.options.feeRate, req.feeRateFloor, (feeRate) => build(atRate(req, feeRate)));
}

function atRate(req: IntentPayRequest, feeRate: number | undefined): IntentPayRequest {
  const options = { ...req.options };
  if (feeRate === undefined) delete options.feeRate;
  else options.feeRate = feeRate;
  return { ...req, options };
}

/** Wallet flow, step 2. */
export function finishIntent(wasm: KobWasm, prepared: PreparedIntent, signatures: InputSignature[]): IntentPayResult {
  return need(wasm, 'finishIntent')(prepared, signatures);
}

/**
 * The payer's cancel of an intent the facilitator did not execute (its KAS and locked tokens come back). Broadcast it
 * once the authorization (or the invoice) expired without a settlement: before that it races the execution.
 */
export function cancelIntent(wasm: KobWasm, req: CancelIntentRequest): SignedTransaction | { built: { sign: unknown[]; [key: string]: unknown } } {
  return need(wasm, 'cancelIntent')(req);
}

/**
 * The intent's expiry, built by anyone (no signature) from its deadline on: its KAS (less at most 0.1 KAS) and its locked tokens
 * return to the payer's key. The facilitator that took the payment submits one itself; this is the path that needs no
 * facilitator (a payer whose facilitator is gone, or any keeper). Submit it once the node's past median time reached
 * `lockTime` (the deadline, unix ms); before that the node refuses it.
 */
export function expireIntent(wasm: KobWasm, req: ExpireIntentRequest): SignedTransaction & { lockTime: string } {
  return need(wasm, 'expireIntent')(req);
}

/** Wallet flow of the cancel, step 2. */
export function finishCancel(wasm: KobWasm, built: { built: unknown }, signatures: InputSignature[]): SignedTransaction {
  return need(wasm, 'finishCancel')(built, signatures);
}

/** Options of `payInvoiceWithIntent`. */
export interface PayInvoiceWithIntentOptions {
  /**
   * Stores the signed payment (above all `payment.intent`, the only handle `cancelIntent` needs) BEFORE anything
   * reaches the facilitator. It is awaited; if it throws, nothing is submitted and its error is rethrown. Persist the
   * whole `IntentPayResult` durably (it is JSON): after a crash or a timeout the facilitator may already have
   * broadcast the creation, and without the handle the payer cannot cancel or recover the intent.
   */
  persist?: (payment: IntentPayResult) => void | Promise<void>;
  /**
   * Re-sends of the SAME signed creation while its outcome is unknown or it may still go through as it is (no answer, a 5xx,
   * `settlement_pending`, `node_unavailable`, `rate_limited`, `intent_not_executable`: the facilitator answers an identical
   * retry from its ledger, and its keeper re-plans the execution against the current book itself). Default
   * `{ resends: 4 }` with backoff, within the intent's deadline; `false`: one send. A creation is never re-signed here: a
   * refusal is returned (or thrown) as before.
   */
  retry?: RetryOptions | false;
}

/**
 * Pays a fetched invoice through one of its intent entries: builds and signs the creation (request hash = the invoice id)
 * and submits it to the facilitator. Returns the payment (keep `payment.intent`) and the settlement; a
 * `settlement_pending` answer means the facilitator is still executing: poll the invoice status.
 *
 * The facilitator may broadcast the creation even when the submission then fails (timeout, 5xx, unreadable answer), so
 * the handle must never be lost:
 *  - `opts.persist` is awaited with the signed payment before the facilitator is called (a throw aborts, nothing sent);
 *  - when the submission throws, the signed payment is attached as `error.payment` (a `KobX402Error`; the same error
 *    object is rethrown, `details` untouched). An error that is not a `KobX402Error` is wrapped in a `KobX402Error`
 *    with code `payment_pending`, the original as `cause` and `payment` set. Use it to `cancelIntent` once the
 *    deadline passed, or to poll the invoice status.
 */
export async function payInvoiceWithIntent(
  wasm: KobWasm,
  invoices: InvoiceClient,
  inv: FetchedInvoice,
  req: Omit<IntentPayRequest, 'requirements' | 'requestHash'> & { acceptIndex?: number },
  opts: PayInvoiceWithIntentOptions = {},
): Promise<{ payment: IntentPayResult; settlement: SettlementResponse; sends: number }> {
  const idx = req.acceptIndex ?? inv.invoice.accepts.findIndex(isIntentOffer);
  const requirements = inv.invoice.accepts[idx];
  if (!requirements || !isIntentOffer(requirements)) throw new KobX402Error('no_acceptable_offer', 'the invoice has no intent entry');
  const { acceptIndex: _ignored, ...rest } = req;
  // the intent's deadline (the authorization's expiry) never lies past the invoice's expiry
  const expiresInMs = intentLifetimeMs(inv, requirements, rest.nowMs, rest.options.expiresInMs);
  const payment = payIntent(wasm, { ...rest, options: { ...rest.options, expiresInMs }, requirements, requestHash: inv.id });
  // the handle is stored before the facilitator can see the creation
  if (opts.persist) await opts.persist(payment);
  const policy = retryPolicy(opts.retry);
  const deadline = Math.min(Date.now() + policy.budgetMs, payment.expiresAtMs);
  for (let sends = 1; ; sends++) {
    let settlement: SettlementResponse | undefined;
    let error: KobX402Error | undefined;
    let step: RetryStep;
    try {
      settlement = await invoices.pay(inv.id, payment.paymentPayload);
      const k = settlement.extensions?.kaspa as { diagnostic?: unknown; retryable?: unknown } | undefined;
      step = settlement.success ? 'stop' : classifyFailure(typeof k?.diagnostic === 'string' ? k.diagnostic : undefined, k?.retryable === true);
    } catch (e) {
      error =
        e instanceof KobX402Error
          ? e
          : new KobX402Error('payment_pending', `the submission of the signed intent failed (${(e as Error)?.message ?? String(e)}); the facilitator may have broadcast it: cancel or reconcile with error.payment.intent`, {
              cause: e,
            });
      error.payment = payment;
      step = classifyStatus(error.status ?? 0, error.diagnostic, error.retryable);
    }
    // only the same signed creation is ever sent again: a rebuild is the payer's own new payment
    const wait = backoffMs(policy, sends, policy.random());
    if (step !== 'resend' || sends > policy.resends || Date.now() + wait >= deadline) {
      if (error) throw error;
      return { payment, settlement: settlement as SettlementResponse, sends };
    }
    await policy.sleep(wait);
  }
}
