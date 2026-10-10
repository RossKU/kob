// `KobWasm` over the node build of kob-wasm (`scripts/build-wasm.sh`, output crates/kob-wasm/pkg-node).
//
// The bindings speak JSON strings. A failure of an x402 binding is an exception whose message is a JSON object
// `{ reason, diagnostic, message, retryable, details? }` (the `extensions.kaspa` shape); it is re-thrown as a
// `KobX402Error` carrying those fields.

import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { KobX402Error } from './errors.ts';
import type {
  IntentPayRequest,
  IntentPayResult,
  KobWasm,
  PayRequest,
  PayResult,
  PreflightResult,
  PreparedIntent,
  PreparedRevoke,
  RevokeResult,
  SignedTransaction,
  TokenOfferRequest,
  UnsignedKcc20,
  PreparedSwap,
} from './wasm.ts';
import type { PaymentRequirements, TokenExtra } from './types.ts';

/** The x402 part of the wasm-bindgen surface. */
export interface KobBindings {
  version(): string;
  x402Canonical(json: string): string;
  x402Sha256(text: string): string;
  x402RequirementsHash(requirements: string): string;
  x402RequestHash(method: string, url: string, body: string, requirementsHash: string): string;
  x402AddressToSpk(address: string): string;
  x402SignedAuthDigest(request: string): string;
  x402PayloadCommitDigest(request: string): string;
  x402NativeRequirements(request: string): string;
  x402Kcc20Requirements(request: string): string;
  x402TokenOffer(request: string): string;
  x402ResolveToken(network: string, asset: string): string;
  x402SwapRequirements(request: string): string;
  x402PayNative(request: string): string;
  x402PayKcc20(request: string): string;
  x402BuildKcc20Unsigned(request: string): string;
  x402FinishKcc20(built: string, template: string, signatures: string): string;
  x402PrepareSwap(request: string): string;
  x402FinishSwap(prepared: string, signatures: string): string;
  x402PaySwap(request: string): string;
  x402Preflight(request: string): string;
  x402Revoke(request: string): string;
  x402FinishRevoke(prepared: string, signatures: string): string;
  x402ExpireIntent(request: string): string;
  x402IntentRequirements(request: string): string;
  x402PrepareIntent(request: string): string;
  x402FinishIntent(prepared: string, signatures: string): string;
  x402PayIntent(request: string): string;
  x402CancelIntent(request: string): string;
  x402FinishCancel(built: string, signatures: string): string;
  x402InvoiceId(invoice: string): string;
  x402CheckInvoice(invoice: string, id: string, nowMs: number): string;
  x402RetryDecision(request: string): string;
  x402Diagnostics(): string;
}

export interface KobWasmNode extends KobWasm {
  /** The raw bindings (hashes, digests and requirement builders that the SDK does not wrap). */
  raw: KobBindings;
}

const DEFAULT_PKG = join(dirname(fileURLToPath(import.meta.url)), '../../../crates/kob-wasm/pkg-node');

function fail(e: unknown): never {
  const msg = e instanceof Error ? e.message : String(e);
  try {
    const j = JSON.parse(msg) as { reason?: string; diagnostic?: string; message?: string; retryable?: boolean; details?: unknown };
    if (j && typeof j === 'object' && typeof j.message === 'string') {
      const init: ConstructorParameters<typeof KobX402Error>[2] = { cause: e };
      if (j.diagnostic) init.diagnostic = j.diagnostic;
      if (j.reason) init.reason = j.reason;
      if (j.retryable !== undefined) init.retryable = j.retryable;
      if (j.details !== undefined) init.details = j.details;
      throw new KobX402Error('payment_failed', j.message, init);
    }
  } catch (inner) {
    if (inner instanceof KobX402Error) throw inner;
  }
  throw new KobX402Error('payment_failed', msg, { cause: e });
}

function run<T>(f: () => string): T {
  let text: string;
  try {
    text = f();
  } catch (e) {
    return fail(e);
  }
  return JSON.parse(text) as T;
}

function payBody(req: PayRequest, extra: Record<string, unknown>): Record<string, unknown> {
  const options: Record<string, unknown> = {};
  if (req.maxFeeSompi) options.maxFeeSompi = req.maxFeeSompi;
  if (req.feeRate !== undefined) options.feeRate = req.feeRate;
  return { offer: req.requirements, requestHash: req.requestHash, nowMs: String(req.nowMs), options, ...extra };
}

export function createKobWasm(opts: { pkgDir?: string } = {}): KobWasmNode {
  const require = createRequire(import.meta.url);
  const kob = require(join(opts.pkgDir ?? DEFAULT_PKG, 'kob_wasm.js')) as KobBindings;
  const j = JSON.stringify;

  const keys = (req: PayRequest): string[] => {
    if (!req.privateKeys?.length) throw new KobX402Error('bad_request', 'this builder signs with local keys: privateKeys is required');
    return req.privateKeys;
  };
  const owner = (req: PayRequest): Record<string, unknown> => {
    if (req.privateKeys?.length) return { secretKeys: req.privateKeys };
    if (req.payerPublicKey) return { payerPublicKey: req.payerPublicKey };
    throw new KobX402Error('bad_request', 'privateKeys or payerPublicKey is required');
  };
  const tokenBody = (req: PayRequest): Record<string, unknown> => {
    const b = payBody(req, { ...owner(req), tokenUtxos: req.tokenUtxos ?? [], funding: req.utxos });
    const o = b.options as Record<string, unknown>;
    o.paymentId = req.paymentId;
    if (req.ttlSeconds !== undefined) o.ttlSeconds = req.ttlSeconds;
    if (req.maxCarrierSompi) o.maxCarrierSompi = req.maxCarrierSompi;
    // the payer's own policy runs BEFORE anything is built or signed: pinned tokens, custody opt-in
    if (req.tokens) b.tokens = req.tokens;
    if (req.allowIssuerControlled) b.allowIssuerControlled = true;
    return b;
  };
  const swapBody = (req: PayRequest): Record<string, unknown> => {
    if (!req.quote) throw new KobX402Error('bad_request', 'a swap payment needs a quote');
    const b = payBody(req, { ...owner(req), quote: req.quote, tokenUtxos: req.tokenUtxos ?? [], funding: req.utxos });
    const o = b.options as Record<string, unknown>;
    o.paymentIdentifier = req.paymentId;
    if (req.maxPayAmount) {
      o.maxPay = req.maxPayAmount;
      o.maxPayAsset = req.payAsset ?? 'KAS';
    }
    if (req.maxCarrierSompi) o.maxCarrierSompi = req.maxCarrierSompi;
    if (req.tokens) b.tokens = req.tokens;
    if (req.allowIssuerControlled) b.allowIssuerControlled = true;
    return b;
  };

  const intentBody = (req: IntentPayRequest): Record<string, unknown> => {
    const owner: Record<string, unknown> = req.privateKeys?.length
      ? { secretKeys: req.privateKeys }
      : req.payerPublicKey
        ? { payerPublicKey: req.payerPublicKey }
        : (() => {
            throw new KobX402Error('bad_request', 'privateKeys or payerPublicKey is required');
          })();
    const options: Record<string, unknown> = { ...req.options };
    if (req.paymentId) options.paymentIdentifier = req.paymentId;
    const b: Record<string, unknown> = {
      offer: req.requirements,
      payAsset: req.payAsset,
      requestHash: req.requestHash,
      nowMs: String(req.nowMs),
      tokenUtxos: req.tokenUtxos ?? [],
      funding: req.utxos,
      options,
      ...owner,
    };
    if (req.tokens) b.tokens = req.tokens;
    if (req.allowIssuerControlled) b.allowIssuerControlled = true;
    return b;
  };

  const api: KobWasmNode = {
    raw: kob,
    version: () => kob.version(),
    addressToScriptPublicKey(address) {
      try {
        return kob.x402AddressToSpk(address);
      } catch (e) {
        return fail(e);
      }
    },
    resolveToken: (network, asset) => run(() => kob.x402ResolveToken(network, asset)),
    tokenOffer: (req: TokenOfferRequest) => run<TokenExtra>(() => kob.x402TokenOffer(j(req))),
    payNative(req) {
      const b = payBody(req, { secretKey: keys(req)[0], utxos: req.utxos });
      (b.options as Record<string, unknown>).paymentId = req.paymentId;
      if (req.ttlSeconds !== undefined) (b.options as Record<string, unknown>).ttlSeconds = req.ttlSeconds;
      return run<PayResult>(() => kob.x402PayNative(j(b)));
    },
    payKcc20: (req) => run<PayResult>(() => kob.x402PayKcc20(j(tokenBody(req)))),
    paySwap: (req) => run<PayResult>(() => kob.x402PaySwap(j(swapBody(req)))),
    buildKcc20Unsigned: (req) => run<UnsignedKcc20>(() => kob.x402BuildKcc20Unsigned(j(tokenBody(req)))),
    finishKcc20: (u, sigs) => run<PayResult>(() => kob.x402FinishKcc20(j(u.built), j(u.template), j(sigs))),
    prepareSwap: (req) => run<PreparedSwap>(() => kob.x402PrepareSwap(j(swapBody(req)))),
    finishSwap: (p, sigs) => run<PayResult>(() => kob.x402FinishSwap(j(p), j(sigs))),
    preflight(req) {
      const b: Record<string, unknown> = {
        offer: req.requirements,
        payload: req.paymentPayload,
        requestHash: req.requestHash,
        nowMs: String(req.nowMs),
      };
      if (req.virtualDaaScore) b.virtualDaaScore = req.virtualDaaScore;
      if (req.tokens) b.tokens = req.tokens;
      if (req.allowIssuerControlled) b.allowIssuerControlled = true;
      if (req.maxPay) {
        b.maxPay = req.maxPay;
        b.maxPayAsset = req.maxPayAsset ?? 'KAS';
      }
      if (req.maxCarrierSompi) b.maxCarrierSompi = req.maxCarrierSompi;
      return run<PreflightResult>(() => kob.x402Preflight(j(b)));
    },
    payIntent: (req) => run<IntentPayResult>(() => kob.x402PayIntent(j(intentBody(req)))),
    prepareIntent: (req) => run<PreparedIntent>(() => kob.x402PrepareIntent(j(intentBody(req)))),
    finishIntent: (p, sigs) => run<IntentPayResult>(() => kob.x402FinishIntent(j(p), j(sigs))),
    cancelIntent(req) {
      const b: Record<string, unknown> = { intent: req.intent };
      if (req.privateKeys?.length) b.secretKeys = req.privateKeys;
      if (req.feeRate !== undefined) b.feeRate = req.feeRate;
      return run(() => kob.x402CancelIntent(j(b)));
    },
    finishCancel: (built, sigs) => run<SignedTransaction>(() => kob.x402FinishCancel(j(built), j(sigs))),
    invoiceId(invoice) {
      try {
        return kob.x402InvoiceId(j(invoice));
      } catch (e) {
        return fail(e);
      }
    },
    checkInvoice: (invoice, id, nowMs) => run(() => kob.x402CheckInvoice(j(invoice), id, nowMs)),
    retryDecision: (req) => run<{ step: 'resend' | 'rebuild' | 'stop' }>(() => kob.x402RetryDecision(j(req))).step,
    diagnostics: () => run<string[]>(() => kob.x402Diagnostics()),
    revoke(req) {
      const b: Record<string, unknown> = { payload: req.paymentPayload, secretKeys: req.privateKeys };
      if (req.feeRate !== undefined) b.feeRate = req.feeRate;
      if (req.maxFeeSompi) b.maxFeeSompi = req.maxFeeSompi;
      if (req.tokens) b.tokens = req.tokens;
      return run<RevokeResult>(() => kob.x402Revoke(j(b)));
    },
    prepareRevoke(req) {
      const b: Record<string, unknown> = { payload: req.paymentPayload, payerPublicKey: req.payerPublicKey };
      if (req.feeRate !== undefined) b.feeRate = req.feeRate;
      if (req.tokens) b.tokens = req.tokens;
      return run<PreparedRevoke>(() => kob.x402Revoke(j(b)));
    },
    finishRevoke: (p, sigs) => run<RevokeResult>(() => kob.x402FinishRevoke(j(p), j(sigs))),
    expireIntent(req) {
      const b: Record<string, unknown> = { intent: req.intent };
      if (req.feeRate !== undefined) b.feeRate = req.feeRate;
      return run(() => kob.x402ExpireIntent(j(b)));
    },
  };
  return api;
}

/** Builds an offer entry through the Rust requirement builders (used to cross-check the TS builders). */
export function rustOffer(kob: KobBindings, kind: 'native' | 'kcc20' | 'swap' | 'intent', request: Record<string, unknown>): PaymentRequirements {
  const f =
    kind === 'native'
      ? kob.x402NativeRequirements
      : kind === 'kcc20'
        ? kob.x402Kcc20Requirements
        : kind === 'intent'
          ? kob.x402IntentRequirements
          : kob.x402SwapRequirements;
  return JSON.parse(f(JSON.stringify(request))) as PaymentRequirements;
}
