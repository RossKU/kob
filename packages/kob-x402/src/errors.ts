// Errors of the SDK. `code` is a stable machine-readable label; `diagnostic` / `reason` / `retryable` / `details`
// carry what a facilitator reported in `extensions.kaspa`.

import type { IntentPayResult } from './wasm.ts';
import type { AttemptSummary } from './retry.ts';

export type ErrorCode =
  | 'invalid_header'
  | 'invalid_payment_required'
  | 'invalid_canonical_json'
  | 'no_acceptable_offer'
  | 'spend_not_authorized'
  | 'payment_in_flight'
  | 'policy_violation'
  | 'redirect'
  | 'no_funds'
  | 'preflight_failed'
  | 'artifact_store'
  | 'payment_failed'
  | 'payment_pending'
  | 'invalid_settlement'
  | 'facilitator'
  | 'bad_request'
  | 'unsupported'
  | 'revoke_failed';

export interface ErrorInit {
  cause?: unknown;
  diagnostic?: string;
  reason?: string;
  retryable?: boolean;
  details?: unknown;
  paymentId?: string;
  transactionId?: string;
  status?: number;
  payment?: IntentPayResult;
  attempts?: AttemptSummary[];
}

export class KobX402Error extends Error {
  code: ErrorCode;
  diagnostic: string | undefined;
  reason: string | undefined;
  /** True when the caller may call again (a fresh call re-quotes and re-signs; the SDK never re-sends a payment). */
  retryable: boolean;
  details: unknown;
  paymentId: string | undefined;
  transactionId: string | undefined;
  status: number | undefined;
  /**
   * Set by `payInvoiceWithIntent` when the submission failed after the creation was signed: the signed intent payment
   * (`payment.intent` is the handle `cancelIntent` needs). The facilitator may already have broadcast the creation, so
   * keep it and cancel / reconcile the intent; never assume nothing was sent.
   */
  payment: IntentPayResult | undefined;
  /**
   * Set when the client retried (`retry`): every attempt it signed, in order, with how each ended. The last one is the
   * attempt this error is about; earlier ones were failed by the facilitator (their inputs released) and share the
   * payment's anchor with it, so at most one of them can ever be accepted.
   */
  attempts: AttemptSummary[] | undefined;

  constructor(code: ErrorCode, message: string, init: ErrorInit = {}) {
    super(message, init.cause === undefined ? undefined : { cause: init.cause });
    this.name = 'KobX402Error';
    this.code = code;
    this.diagnostic = init.diagnostic;
    this.reason = init.reason;
    this.retryable = init.retryable ?? false;
    this.details = init.details;
    this.paymentId = init.paymentId;
    this.transactionId = init.transactionId;
    this.status = init.status;
    this.payment = init.payment;
    this.attempts = init.attempts;
  }
}
