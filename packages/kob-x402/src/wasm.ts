// The Rust side the SDK needs. Everything that must equal the Rust verifier byte for byte (transaction building,
// signing, authorization digests, token output scripts, preflight) runs in `kob-wasm`; the SDK only orchestrates.
// `wasm-node.ts` implements this interface over the wasm-bindgen bindings; tests inject a stub.
//
// Conventions: JSON-shaped values in and out (64-bit integers as decimal strings, bytes as lowercase hex, spks as
// `version u16 BE hex || script hex`). Every function is synchronous and pure: the wasm reads no clock and does no
// I/O, so `nowMs` and the chain snapshot are arguments.

import type {
  Custody,
  NetworkId,
  Outpoint,
  PayerUtxo,
  PaymentPayload,
  PaymentRequirements,
  Resource,
  TokenExtra,
  TokenFamily,
} from './types.ts';
import { KobX402Error } from './errors.ts';

/**
 * A token UTXO as the kob-protocol JSON carries it: `{ transactionId, index, amount, blockDaaScore, covenantId, state }`, the
 * state being a KCC-20 state `{ amount, owner, owner_scheme, borrow_scheme, borrow_guard, extension_commitment }` or a KRON
 * state `{ owner, id_type, amount, is_minter }` (a KRON token is held by address presence, `id_type` 3: the payer also
 * supplies a KAS UTXO of the owner, which authorises the token input).
 */
export type TokenUtxoJson = Record<string, unknown>;

/**
 * A swap-and-pay quote in kob-x402 `Quote` JSON: `{ lockTime, orders: [OrderRef] }` where an `OrderRef` is
 * `{ leg: { kind: "bid", order, amount } | { kind: "ask", order, custody, amount } }` (amount in token base units). Produced by the order source
 * (indexer); the lock time is a recent virtual DAA score.
 */
export interface SwapQuote {
  lockTime: string;
  orders: Record<string, unknown>[];
}

/**
 * A token as the payer / merchant configures it; hashes default to the embedded registry entry of the covenant id. Either
 * family: the family is the pinned program's (a KRON token has no `extensionCommitment`; omit it).
 */
export interface TokenSpec {
  covenantId: string;
  templateHash?: string;
  extensionCommitment?: string;
  custody?: Custody;
  ticker?: string;
  decimals?: number;
}

/** A signing request of an unsigned payment (kob-protocol `SignRequest`): what a wallet signs. */
export interface SignRequest {
  inputIndex: number;
  pubkey: string;
  sighashType: number;
  sighash: string;
  redeemScript: string | null;
}

/** A wallet's answer for one input (kob-protocol `InputSignature`). */
export interface InputSignature {
  inputIndex: number;
  /** 64 or 65 bytes hex. */
  signature: string;
}

/** What the payer-side builders need to build one payment. */
export interface PayRequest {
  /** The selected offer, exactly as received (`paymentPayload.accepted` is a copy of it). */
  requirements: PaymentRequirements;
  /** Independently derived request fingerprint (`httpRequestHash`), 32-byte hex. */
  requestHash: string;
  payerAddress: string;
  /** Local signing keys (32-byte hex secret keys): the all-in-one builders. */
  privateKeys?: string[];
  /** The payer's x-only public key (64 hex): the unsigned builders of the wallet flow. */
  payerPublicKey?: string;
  /** Payer P2PK KAS UTXOs (funding, fee, change; the whole payment for a native offer). */
  utxos: PayerUtxo[];
  /** UTXOs of the payer's tokens (kcc20, and the pay asset of a swap-and-pay: KCC-20 or KRON). Sourced from an indexer. */
  tokenUtxos?: TokenUtxoJson[];
  /** Swap-and-pay: the orders to fill. */
  quote?: SwapQuote;
  /** Swap-and-pay: covenant id of the token the payer pays with (informational; the quote decides). */
  payAsset?: string;
  /** Swap-and-pay: the most the payer will spend of `payAsset`, in that asset's units (decimal string); a route that pays with another asset is refused. */
  maxPayAmount?: string;
  /** Payer-pinned token allowlist (default: the tokens the offer names). */
  tokens?: TokenSpec[];
  /** Accept issuer-controlled tokens in this payment (default false). */
  allowIssuerControlled?: boolean;
  /** `payment-identifier` id the payload carries. */
  paymentId: string;
  extensions?: PaymentPayload['extensions'];
  resource?: Resource;
  nowMs: number;
  /** Highest acceptable fee in sompi (decimal string); the builder fails above it. */
  maxFeeSompi?: string;
  /**
   * Fee rate of the payment transaction in sompi per gram (every profile; default: the relay floor, 100). Pass the node's
   * estimate for the urgency of the payment (`feeRateFromEstimate`: a merchant's HTTP request waits, so the priority bucket),
   * the same dynamic policy as the executor's (`crates/kob-executor/src/fee.rs`).
   */
  feeRate?: number;
  /** Authorization lifetime in seconds (native and kcc20; default: the offer's `maxTimeoutSeconds`). The client passes a short one. */
  ttlSeconds?: number;
  /** Most KAS (sompi) the payer funds into a merchant token output / its own token change (kcc20, swap; default 2 KAS). */
  maxCarrierSompi?: string;
}

export interface PayResult {
  paymentPayload: PaymentPayload;
  /** Recomputed transaction id of the signed transaction. */
  transactionId: string;
  /** Outpoints the transaction spends. */
  consumed: Outpoint[];
  feeSompi: string;
  /** Authorization expiry (unix ms). */
  expiresAtMs: number;
  /** Swap-and-pay: units of the pay asset the payer gives up (sompi incl. fee when paying KAS). */
  payerSpent?: string;
  /** Swap-and-pay: sompi the payment takes from the payer's KAS coins (fee, carriers, and the cost when paying KAS). */
  kasSpent?: string;
  warnings?: string[];
}

/** Step 1 of the wallet flow (kcc20): the unsigned payment; opaque `built` and `template` go back to `finishKcc20`. */
export interface UnsignedKcc20 {
  built: { sign: SignRequest[]; [key: string]: unknown };
  template: unknown;
}

/** Step 1 of the wallet flow (swap-and-pay). */
export interface PreparedSwap {
  built: { sign: SignRequest[]; [key: string]: unknown };
  payAsset: string;
  payerSpent: string;
  warnings: string[];
  [key: string]: unknown;
}

export interface PreflightRequest {
  requirements: PaymentRequirements;
  paymentPayload: PaymentPayload;
  requestHash: string;
  virtualDaaScore?: string;
  nowMs: number;
  tokens?: TokenSpec[];
  allowIssuerControlled?: boolean;
  /** Swap-and-pay: the payer's bound on what the payment costs, counted in `maxPayAsset`. */
  maxPay?: string;
  /** The asset `maxPay` counts: `KAS` (the default) or a token covenant id; a payment in another asset is refused. */
  maxPayAsset?: string;
  /** The payer's ceiling on the merchant carrier (default 2 KAS). */
  maxCarrierSompi?: string;
}

export interface PreflightResult {
  ok: boolean;
  diagnostic?: string;
  reason?: string;
  message?: string;
  retryable?: boolean;
  details?: unknown;
  transactionId?: string;
  feeSompi?: string;
  payerSpent?: string | null;
}

export interface RevokeRequest {
  /** The signed payment to invalidate: one of its payer-owned inputs is spent back to the payer. */
  paymentPayload: PaymentPayload;
  privateKeys: string[];
  feeRate?: number;
  maxFeeSompi?: string;
  tokens?: TokenSpec[];
}

/** The wallet revoke (kcc20 and swap-and-pay payments): the payer's key instead of local keys. */
export interface PrepareRevokeRequest {
  paymentPayload: PaymentPayload;
  /** The wallet's x-only public key (64 hex). */
  payerPublicKey: string;
  feeRate?: number;
  tokens?: TokenSpec[];
}

/** Step 1 of the wallet revoke: `built.sign` is what the wallet signs. */
export interface PreparedRevoke {
  built: { sign: SignRequest[]; [key: string]: unknown };
  spent: Outpoint;
}

export interface RevokeResult {
  /** Signed self-spend as `kaspa-sdk-safe-json-v2.0.0` text (loads with `Transaction.deserializeFromSafeJSON`). */
  transaction: string;
  transactionId: string;
  /** The payment input the self-spend consumes. */
  spent: Outpoint;
}

export interface TokenOfferRequest {
  network: NetworkId;
  /** KCC-20 covenant id (the x402 asset). */
  asset: string;
  /** Merchant address (P2PK Schnorr): owner of the merchant token output. */
  payTo: string;
  /** Token base units. */
  amount: string;
  custody: Custody;
  carrier?: string;
  templateHash?: string;
  extensionCommitment?: string;
  ticker?: string;
  decimals?: number;
}

/** The payer's terms of an intent (decimal strings): see `IntentPayRequest`. */
export interface IntentOptions {
  /** Router actor (fill shape); default the one-order resting shape of the intent kind. */
  actor?: string;
  /** KasToToken (pay asset KAS): most sompi the asks may demand at their quotes. Required. */
  maxPay?: string;
  /** KasToToken: most sompi for the merchant's carrier, fillers and the network fee (default carrier + 1 KAS). */
  maxExtra?: string;
  /** TokenToKas / TokenSwap (pay asset a token): most units sold. Required. */
  maxSell?: string;
  /** Units locked (default `maxSell`). */
  lockAmount?: string;
  /** KAS of a token intent's UTXO the keeper may spend on fillers and the fee (default 1 KAS). */
  keeperValue?: string;
  lockCarrier?: string;
  /** How long the facilitator may execute (default 5 min; at most the offer's `maxTimeoutSeconds`). */
  expiresInMs?: number;
  /** Fee rate of the creation, sompi per gram (default the relay floor 100); see `PayRequest.feeRate`. */
  feeRate?: number;
}

/** What an intent payment needs (the creation is the payer's only signature). */
export interface IntentPayRequest {
  requirements: PaymentRequirements;
  requestHash: string;
  /** `KAS` or the pay token's covenant id (one of the offer's `payAssets`). */
  payAsset: string;
  privateKeys?: string[];
  payerPublicKey?: string;
  utxos: PayerUtxo[];
  tokenUtxos?: TokenUtxoJson[];
  nowMs: number;
  options: IntentOptions;
  paymentId?: string;
  tokens?: TokenSpec[];
  allowIssuerControlled?: boolean;
  /**
   * The rate a creation that cannot be built at `options.feeRate` (its fee above a bound, funds short of the dearer fee) is
   * retried at, once (default the relay floor 100): an estimate never makes a payable intent impossible.
   */
  feeRateFloor?: number;
}

/** The intent a payment created: keep it to cancel the intent if it is not executed. */
export interface IntentHandle {
  actor: string;
  state: Record<string, unknown>;
  intent: Record<string, unknown>;
  lock?: Record<string, unknown> | null;
}

export interface IntentPayResult extends PayResult {
  /** The payer's worst case in units of the pay asset (token units sold at most, or the intent's KAS). */
  payerSpent: string;
  intent: IntentHandle;
}

/** Step 1 of the wallet flow (intent): `built.sign` is what the wallet signs once. */
export interface PreparedIntent {
  built: { sign: SignRequest[]; [key: string]: unknown };
  actor: string;
  worstKas: string;
  worstTokens: string;
  [key: string]: unknown;
}

/** Anyone's expiry of an intent from its deadline on (no signature). */
export interface ExpireIntentRequest {
  intent: IntentHandle;
  feeRate?: number;
}

export interface CancelIntentRequest {
  intent: IntentHandle;
  privateKeys?: string[];
  feeRate?: number;
}

export interface SignedTransaction {
  /** kob-protocol transaction JSON text (safe JSON with UTXO entries). */
  transaction: string;
  transactionId: string;
}

export interface KobWasm {
  version(): string;
  /** Serialized spk of an address (`version u16 BE hex || script hex`), the value of `extra.payToScriptPublicKey`. */
  addressToScriptPublicKey(address: string): string;
  /** Pinned program hash and extension commitment of a registry token (used for `route.payAssets` entries the configuration leaves out). */
  resolveToken(network: NetworkId, asset: string): { templateHash: string; extensionCommitment: string; family?: TokenFamily };
  /** `extra.token` of a `kcc20` offer, including the merchant token output's spk computed from the pinned program. */
  tokenOffer(req: TokenOfferRequest): TokenExtra;
  /** Local-key builders. A native payment signs the request authorization digest with the funding key, so it needs a local key. */
  payNative(req: PayRequest): PayResult;
  payKcc20(req: PayRequest): PayResult;
  paySwap(req: PayRequest): PayResult;
  /** Wallet flow: build unsigned, let a wallet sign `built.sign`, finish with the signatures only. */
  buildKcc20Unsigned(req: PayRequest): UnsignedKcc20;
  finishKcc20(unsigned: UnsignedKcc20, signatures: InputSignature[]): PayResult;
  prepareSwap(req: PayRequest): PreparedSwap;
  finishSwap(prepared: PreparedSwap, signatures: InputSignature[]): PayResult;
  /** Payer-side verification of a built payment against a snapshot (the same verifier logic the facilitator runs). */
  preflight(req: PreflightRequest): PreflightResult;
  revoke(req: RevokeRequest): RevokeResult;
  /** Wallet revoke, step 1 (kcc20 and swap-and-pay payments; a native payment's revoke needs the local key). */
  prepareRevoke?(req: PrepareRevokeRequest): PreparedRevoke;
  /** Wallet revoke, step 2: the signed self-spend. */
  finishRevoke?(prepared: PreparedRevoke, signatures: InputSignature[]): RevokeResult;
  /** Intent mode (optional in custom implementations): build and sign the creation locally. */
  payIntent?(req: IntentPayRequest): IntentPayResult;
  prepareIntent?(req: IntentPayRequest): PreparedIntent;
  finishIntent?(prepared: PreparedIntent, signatures: InputSignature[]): IntentPayResult;
  /** The payer's cancel: signed with `privateKeys`, else `{ built }` for a wallet. */
  cancelIntent?(req: CancelIntentRequest): SignedTransaction | { built: { sign: SignRequest[]; [key: string]: unknown } };
  finishCancel?(built: { built: unknown }, signatures: InputSignature[]): SignedTransaction;
  /**
   * The expiry of an intent, built by anyone from its deadline on (no signature): its KAS and locked tokens back to the payer.
   * A node accepts it once its past median time reached the deadline (`lockTime`, unix ms).
   */
  expireIntent?(req: ExpireIntentRequest): SignedTransaction & { lockTime: string };
  /** Invoice id (Rust): SHA-256 of the canonical JSON. */
  invoiceId?(invoice: unknown): string;
  /** Checks a fetched invoice against its id and its structure at `nowMs`. */
  checkInvoice?(invoice: unknown, id: string, nowMs: number): { id: string; expiresAtMs: number; kaspaUri: string | null };
}

/**
 * Loads the node build of `kob-wasm` (`scripts/build-wasm.sh`) as a `KobWasm`. Kept dynamic so this module (and
 * everything that only takes a `KobWasm` by injection) does not depend on the generated bindings being present.
 */
export async function loadKobWasm(options: { pkgDir?: string } = {}): Promise<KobWasm> {
  const spec = './wasm-node.ts';
  let mod: { createKobWasm?: (opts?: { pkgDir?: string }) => KobWasm };
  try {
    mod = (await import(spec)) as typeof mod;
  } catch (e) {
    throw new KobX402Error('unsupported', 'the kob-wasm node bindings are not available (run scripts/build-wasm.sh)', { cause: e });
  }
  if (typeof mod.createKobWasm !== 'function') throw new KobX402Error('unsupported', 'wasm-node.ts does not export createKobWasm');
  return mod.createKobWasm(options);
}
