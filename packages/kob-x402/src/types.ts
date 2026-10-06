// Wire types of the Kaspa x402 `exact` binding (elldeeone/kaspa-x402 v1.0.0-rc.1, `kaspa-exact-v2`) plus the two
// KOB contributions: the `kcc20` profile (`extra.token`), swap-and-pay (`extra.route`, binding `kob-swap-v1`), its intent
// mode (binding `kob-intent-v1`) and invoices (`kob-invoice-v1`).
// Mirrors `crates/kob-x402/src/wire.rs` field for field. Amounts are canonical decimal uint64 strings, hashes are
// lowercase hex, addresses are bech32 strings.

export const X402_VERSION = 2;
export const SCHEME_EXACT = 'exact';
export const BINDING_EXACT = 'kaspa-exact-v2';
export const BINDING_SWAP = 'kob-swap-v1';
/** Intent-based swap-and-pay: the payer signs a KOB router intent once, the facilitator executes it. */
export const BINDING_INTENT = 'kob-intent-v1';
/** The KOB router artifact the intent offers name (`extra.route.router`): `kob_protocol::router::ROUTER_ARTIFACT_ID`. */
export const ROUTER_ARTIFACT_ID = 'a85ee4b6c560818e056db2008d06050b503a93f5d194095f4585e5b82f451f5d';
/** `invoiceVersion` of an invoice. */
export const INVOICE_VERSION = 'kob-invoice-v1';
export const TX_ENCODING = 'kaspa-sdk-safe-json-v2.0.0';
export const ASSET_KAS = 'KAS';
export const PAYLOAD_EXACT_TX = 'exact-transaction';
/** Authorization: Schnorr signature over the request digest by a P2PK input (the binding). */
export const AUTH_VERSION_SIGNED = 'kaspa-x402-exact-request-authorization-v1';
/** Authorization: the digest is committed in the transaction payload (token profile, swap-and-pay). */
export const AUTH_VERSION_PAYLOAD = 'kob-x402-payload-commitment-v1';
export const PAYMENT_IDENTIFIER_KEY = 'payment-identifier';
export const KASPA_EXTENSION_KEY = 'kaspa';

export const HEADER_PAYMENT_REQUIRED = 'PAYMENT-REQUIRED';
export const HEADER_PAYMENT_SIGNATURE = 'PAYMENT-SIGNATURE';
export const HEADER_PAYMENT_RESPONSE = 'PAYMENT-RESPONSE';

export type NetworkId = 'kaspa:mainnet' | 'kaspa:testnet-10';
export const NETWORKS: readonly NetworkId[] = ['kaspa:mainnet', 'kaspa:testnet-10'];

export type Profile = 'standard-native' | 'kcc20' | 'additive';
export type Finality = 'accepted' | 'confirmed';
export type Custody = 'unconditional' | 'issuer-controlled';
/**
 * Token family: the program lineage of a token. A merchant asset (`kcc20` profile, `extra.token.family`) is always
 * `kcc20`; a swap-and-pay pay asset may also be a `kron` token (46-byte state, no extension commitment).
 */
export type TokenFamily = 'kcc20' | 'kron';

/** Any JSON value. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonRecord = Record<string, unknown>;

export interface Resource {
  url: string;
  description?: string;
  mimeType?: string;
  [key: string]: unknown;
}

/** `extra.token` of a `kcc20` offer (custody is explicit: the founder's point on kas-smiths.org topic 15 #20). */
export interface TokenExtra {
  family: 'kcc20';
  templateHash: string;
  extensionCommitment: string;
  custody: Custody;
  /** Exact KAS value (sompi, decimal string) of the merchant token output. */
  carrier: string;
  /** spk of the merchant token output: P2SH(program prefix || state || suffix). */
  tokenScriptPublicKey: string;
  decimals?: number;
  ticker?: string;
  [key: string]: unknown;
}

export interface RoutePayAsset {
  /** A token covenant id (KCC-20 program of any pinned template, KaspaCom's included, or a KRON token), or `KAS` (only when the merchant receives a token). */
  asset: string;
  /** Absent for `KAS`. The pinned program; its family is the program's. */
  templateHash?: string;
  /** Absent for `KAS`; all zero for a KRON token (it has none). */
  extensionCommitment?: string;
  [key: string]: unknown;
}

/** `extra.route` of a swap-and-pay offer (payer-signed, or intent mode with `router`). */
export interface RouteExtra {
  binding: typeof BINDING_SWAP | typeof BINDING_INTENT;
  /** Intent mode: the router artifact id the facilitator executes. */
  router?: string;
  critical: boolean;
  payAssets: RoutePayAsset[];
  [key: string]: unknown;
}

export interface ExactExtra {
  binding: string;
  profile: string;
  finality: string;
  transactionEncoding: string;
  /** Serialized spk of `payTo` (version u16 BE hex || script hex). */
  payToScriptPublicKey: string;
  token?: TokenExtra;
  route?: RouteExtra;
  [key: string]: unknown;
}

/** One offer of `accepts`. Unknown fields are kept: the requirements hash covers the complete object. */
export interface PaymentRequirements {
  scheme: string;
  network: string;
  amount: string;
  asset: string;
  payTo: string;
  maxTimeoutSeconds: number;
  extra: ExactExtra;
  [key: string]: unknown;
}

export interface PaymentIdentifierInfo {
  required: boolean;
  id?: string;
  [key: string]: unknown;
}

export interface PaymentIdentifierExtension {
  info: PaymentIdentifierInfo;
  schema?: JsonRecord;
}

/** Public error reasons of the binding (`spec/errors.md`). */
export type PublicReason =
  | 'invalid_x402_version'
  | 'invalid_scheme'
  | 'invalid_network'
  | 'invalid_payment_requirements'
  | 'invalid_payload'
  | 'invalid_transaction_state'
  | 'unsupported_scheme'
  | 'unexpected_settle_error';

/** Local diagnostics (`extensions.kaspa.diagnostic`); the wire reason stays one of `PublicReason`. */
export const DIAGNOSTICS = [
  'invalid_kaspa_x402_amount',
  'invalid_kaspa_x402_binding',
  'invalid_kaspa_x402_payload',
  'invalid_kaspa_x402_accepted',
  'invalid_kaspa_payment_identifier',
  'missing_kaspa_payment_identifier',
  'kaspa_payment_identifier_conflict',
  'unsupported_kaspa_exact_profile',
  'invalid_kaspa_exact_transaction',
  'invalid_kaspa_exact_transaction_id',
  'invalid_kaspa_exact_payment_output',
  'invalid_kaspa_exact_signature',
  'invalid_kaspa_exact_utxo',
  'invalid_kaspa_exact_fee',
  'invalid_kaspa_exact_mass',
  'invalid_kaspa_exact_replay',
  'invalid_kaspa_exact_finality',
  'expired_authorization',
  'authorization_exceeds_max_timeout',
  'invalid_authorization',
  'token_not_allowlisted',
  'token_template_mismatch',
  'token_borrow_enabled',
  'token_owner_scheme',
  'token_conservation',
  'token_custody_policy',
  'carrier_mismatch',
  'underpayment',
  'overpayment',
  'route_unsupported',
  'route_aliasing',
  'unknown_order_template',
  'order_conflict',
  'order_not_spendable',
  'pay_asset_not_accepted',
  'intent_not_executable',
  'intent_expired',
  'intent_spent',
  'invoice_unknown',
  'invoice_expired',
  'invoice_paid',
  'invoice_pending',
  'invalid_invoice',
  'replay',
  'expired',
  'rate_limited',
  'unauthorized',
  'node_unavailable',
  'settlement_pending',
  'internal',
] as const;
export type Diagnostic = (typeof DIAGNOSTICS)[number];

/** `extensions.kaspa` of a failed response. */
export interface KaspaFailure {
  diagnostic: string;
  retryable: boolean;
  message: string;
  details?: unknown;
}

/** The `402` body (and `PAYMENT-REQUIRED` header) object. */
export interface PaymentRequired {
  x402Version: number;
  resource: Resource;
  accepts: PaymentRequirements[];
  error?: string;
  extensions?: {
    'payment-identifier'?: PaymentIdentifierExtension;
    kaspa?: KaspaFailure;
    [key: string]: unknown;
  };
}

export interface Authorization {
  version: string;
  /** Signed version only. */
  inputIndex?: number;
  /** ISO-8601 UTC, millisecond precision. */
  expiresAt: string;
  digest: string;
  /** Signed version only. */
  signature?: string;
}

export interface OutpointJson {
  txid: string;
  index: number;
}

export interface RoutePayload {
  binding: string;
  payAsset: string;
  orders: OutpointJson[];
  /** Intent mode only: the router actor and the payer's terms. */
  intent?: IntentTerms;
}

/** `payload.route.intent` (intent mode): what the facilitator needs, besides the offer, to recompute the intent. */
export interface IntentTerms {
  /** Router actor (`KasToToken_buy`, `TokenToKas_sell2`, ...): the fill shape the keeper uses. */
  actor: string;
  /** The payer's x-only key (64 hex). */
  payer: string;
  maxPay?: string;
  maxExtra?: string;
  maxSell?: string;
  lockOutputIndex?: number;
  lockAmount?: string;
}

export interface ExactPayload {
  type: string;
  profile: string;
  payerAddress?: string;
  /** The signed transaction as `kaspa-sdk-safe-json-v2.0.0` (a JSON text). */
  transaction: string;
  transactionEncoding: string;
  paymentOutputIndex: number;
  requestHash: string;
  challengeId?: string;
  authorization: Authorization;
  route?: RoutePayload;
}

/** The `PAYMENT-SIGNATURE` object. */
export interface PaymentPayload {
  x402Version: number;
  accepted: PaymentRequirements;
  payload: ExactPayload;
  resource?: Resource;
  extensions?: { 'payment-identifier'?: PaymentIdentifierExtension; [key: string]: unknown };
}

/** Body of `POST /verify` and `POST /settle`. `requestHash` is mandatory and never inferred from the payload. */
export interface FacilitatorRequest {
  x402Version: number;
  paymentPayload: PaymentPayload;
  paymentRequirements: PaymentRequirements;
  requestHash: string;
  resource?: Resource;
}

export interface VerifyResponse {
  isValid: boolean;
  invalidReason?: string;
  payer?: string;
  extensions?: { kaspa?: KaspaFailure; [key: string]: unknown };
}

/** `PAYMENT-RESPONSE` / `POST /settle` response. */
export interface SettlementResponse {
  success: boolean;
  errorReason?: string;
  /** Recomputed transaction id, or `""` on failure. */
  transaction: string;
  network?: string;
  payer?: string;
  amount?: string;
  extensions?: { kaspa?: KaspaFailure | JsonRecord; [key: string]: unknown };
}

export interface SupportedKind {
  x402Version: number;
  scheme: string;
  network: string;
  extra?: JsonRecord;
}

export interface SupportedResponse {
  kinds: SupportedKind[];
  extensions?: string[];
  signers?: Record<string, string[]>;
}

// ------------------------------------------------------------------------------------------- offer configuration

/** A token accepted as payment or paid to the merchant (metadata beyond the covenant id). */
export interface TokenOfferSpec {
  custody: Custody;
  /** KAS value of the merchant token output in sompi (decimal string). Default: the wasm's policy minimum. */
  carrier?: string;
  /** Pinned program hash; the wasm resolves it from its registry when absent. */
  templateHash?: string;
  extensionCommitment?: string;
  ticker?: string;
  decimals?: number;
}

/** What the merchant sells, in server configuration terms. Built into `accepts` entries through `KobWasm`. */
export type OfferSpec =
  | { kind: 'native'; amount: string; finality?: Finality }
  | { kind: 'kcc20'; asset: string; amount: string; token: TokenOfferSpec; finality?: Finality }
  | {
      kind: 'swap';
      /** What the merchant receives. */
      receive: 'kas' | 'kcc20';
      /** Amount the merchant receives (sompi or token base units). */
      amount: string;
      /** Merchant asset when `receive` is `kcc20`. */
      asset?: string;
      token?: TokenOfferSpec;
      /**
       * Tokens the payer may pay with, of either family. `family: 'kron'` with a `templateHash` needs no extension
       * commitment (a KRON token has none: all zero on the wire).
       */
      payAssets: { asset: string; templateHash?: string; extensionCommitment?: string; family?: TokenFamily }[];
      finality?: Finality;
      /**
       * `signed` (default): the payer signs a route over named orders (`kob-swap-v1`). `intent`: the payer signs a KOB router
       * intent once and the facilitator executes it (`kob-intent-v1`; pay tokens on a KCC-20 program of the 112-byte state or a
       * KRON program, the merchant token on a KCC-20 one; KaspaCom's program, pending review, only through `signed`).
       */
      mode?: 'signed' | 'intent';
    };

export type OfferKind = 'native' | 'kcc20' | 'swap';

/** A transaction outpoint. */
export interface Outpoint {
  txid: string;
  index: number;
}

/** An unspent output as the payer's chain context reports it. */
export interface PayerUtxo {
  txid: string;
  index: number;
  /** Decimal string. */
  amount: string;
  /** Serialized spk: version u16 BE hex || script hex. */
  scriptPublicKey: string;
  /** Decimal string. */
  blockDaaScore: string;
  isCoinbase: boolean;
  covenantId?: string | null;
  address?: string;
}

// ------------------------------------------------------------------------------------------------ invoices

/**
 * An invoice (`kob-invoice-v1`): the x402 requirements of one sale, the merchant's reference and an expiry. Its id is the
 * SHA-256 of its canonical JSON; the facilitator serves it at `GET /invoices/<id>` (the URL a QR code carries).
 */
export interface Invoice {
  x402Version: number;
  invoiceVersion: typeof INVOICE_VERSION;
  network: string;
  /** The merchant's own id of the sale (1..=128 printable ASCII). */
  reference: string;
  /** ISO-8601 UTC with milliseconds. */
  expiresAt: string;
  memo?: string;
  /** Ways to pay; any one of them pays the invoice. */
  accepts: PaymentRequirements[];
}

/** Invoice-level state. */
export type InvoiceState = 'unpaid' | 'pending' | 'paid' | 'expired' | 'failed';

export interface InvoicePayment {
  /** The transaction that pays the merchant (an intent: its execution). */
  transaction: string;
  acceptedDaaScore?: string;
  payer?: string;
  /** Index of the paid entry in `accepts`. */
  acceptedIndex: number;
  response?: SettlementResponse;
}

export interface InvoiceAttempt {
  transaction: string;
  state: string;
  reason?: string;
}

/** A refused duplicate / late payment the facilitator keeps watching (a refund candidate once `observed` is `accepted`). */
export interface ExtraPayment {
  kind: 'duplicate' | 'late';
  transaction: string;
  payer?: string;
  observed: 'refused' | 'accepted' | 'intent-on-chain';
  acceptedDaaScore?: string;
  at: string;
}

/** `GET /invoices/<id>/status`. */
export interface InvoiceStatus {
  id: string;
  reference: string;
  status: InvoiceState;
  expiresAt: string;
  payment?: InvoicePayment;
  attempts: InvoiceAttempt[];
  extraPayments: ExtraPayment[];
}
