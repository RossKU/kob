// KobX402Client: the payer flow.
//
//   request -> 402 -> select offer -> derive requestHash -> build + sign through KobWasm -> preflight ->
//   SAVE the signed artifact -> retry with PAYMENT-SIGNATURE -> verify PAYMENT-RESPONSE -> return the response.
//
// Safety properties:
//  * redirects are rejected on both requests and the effective URL must equal the requested URL (a signed
//    PAYMENT-SIGNATURE is never forwarded to a redirect target);
//  * `requestHash` is derived locally from the request the client itself sends (never taken from the server);
//  * a payment is never re-sent automatically (maxPaymentRetries = 0). A retryable failure (`order_conflict`) is
//    surfaced with `retryable: true`, but a fresh `fetch()` does NOT silently sign a second, independent payment for
//    the same resource while an earlier artifact is still live (the merchant may already hold it): the caller opts in
//    with `allowResign`, and the client then revokes the earlier artifact first;
//  * nothing is paid without a spend authorisation: a per-asset `capabilities.maxAmount` ceiling (and, for swaps,
//    `maxPayAmount`), or an `approve` policy hook that says yes;
//  * the KAS a payer funds into a merchant token output (the "carrier") is capped (`maxCarrierSompi`, default 2 KAS);
//  * the signed artifact is durably recorded BEFORE it is disclosed, so a crash cannot lose a payment the
//    merchant may already hold; `revoke(paymentId)` invalidates it by spending one of its inputs back to the payer;
//  * the PAYMENT-RESPONSE must show success, the transaction id the client itself signed, and the accepted amount.

import { randomBytes } from 'node:crypto';
import { canonicalJson, httpRequestHash, normalizeBody, normalizeMethod, normalizeUrl, requirementsHash } from './canonical.ts';
import { KobX402Error } from './errors.ts';
import { decodePaymentResponse, encodePaymentSignature } from './headers.ts';
import { rankOffers, parsePaymentRequired } from './offers.ts';
import type { PayerCapabilities, SelectedOffer } from './offers.ts';
import { MemoryArtifactStore } from './artifact-store.ts';
import { assertSecureUrl } from './url-policy.ts';
import type { ArtifactRecord, ArtifactStore } from './artifact-store.ts';
import type { InputSignature, KobWasm, PayRequest, PayResult, SignRequest, SwapQuote, TokenSpec, TokenUtxoJson } from './wasm.ts';
import { resolveFeeRate, withFeeFloorAsync, type FeeRateSource } from './fee.ts';
import {
  HEADER_PAYMENT_REQUIRED,
  HEADER_PAYMENT_RESPONSE,
  HEADER_PAYMENT_SIGNATURE,
  PAYMENT_IDENTIFIER_KEY,
  X402_VERSION,
} from './types.ts';
import type {
  KaspaFailure,
  NetworkId,
  OfferKind,
  PayerUtxo,
  PaymentIdentifierExtension,
  PaymentPayload,
  PaymentRequired,
  SettlementResponse,
} from './types.ts';

const MAX_BODY_BYTES = 1024 * 1024;
/** Default authorization lifetime of a native / kcc20 payment. */
export const DEFAULT_AUTH_TTL_SECONDS = 600;
const HEX64 = /^[0-9a-f]{64}$/;

export interface ChainContextQuery {
  payerAddress: string;
  network: NetworkId;
  kind: OfferKind;
  /** Merchant asset of the offer. */
  asset: string;
  /** Amount the merchant is offered (sompi or token base units). */
  amount: string;
  /** Swap-and-pay: the asset the payer pays with (a covenant id, or `KAS`). */
  payAsset?: string;
}

export interface ChainContext {
  /** P2PK KAS UTXOs of the payer. */
  utxos: PayerUtxo[];
  /** The payer's token UTXOs (the KCC-20 token of a kcc20 offer; the pay asset of a swap: a KCC-20 or a KRON token), kob-protocol `TokenUtxo` JSON. */
  tokenUtxos?: TokenUtxoJson[];
  /** Swap-and-pay: the orders to fill (kob-x402 `Quote` JSON), from the order source / indexer. */
  quote?: SwapQuote;
  virtualDaaScore?: string;
}

/** Where the payer's spendable chain state comes from (see `rpcContextProvider` in kaspa.ts). */
export interface ChainContextProvider {
  load(query: ChainContextQuery): Promise<ChainContext>;
}

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface PaidFetchInit extends RequestInit {
  /** Use this `payment-identifier` id instead of a fresh random one. */
  paymentId?: string;
  /** Override the derived request fingerprint (32-byte hex). Use only when the server fingerprints differently. */
  requestHash?: string;
}

/** A wallet that signs the inputs of an unsigned payment (kcc20 and swap-and-pay; KasWare, Kaspire, Kastle, ...). */
export interface WalletSigner {
  /** The wallet's x-only public key (64 hex): the owner of the payer's KAS and token UTXOs. */
  publicKey: string;
  signInputs(requests: SignRequest[]): Promise<InputSignature[]>;
}

export interface KobX402ClientOptions {
  wasm: KobWasm;
  network: NetworkId;
  payerAddress: string;
  /** Local signing keys (32-byte hex secret keys): dev-key mode, supports every profile and `revoke`. */
  privateKeys?: string[];
  /**
   * Wallet mode (instead of `privateKeys`): kcc20 and swap-and-pay payments are built unsigned, the wallet signs,
   * and only signatures come back. A native KAS payment additionally signs the request-authorization digest with the
   * funding key, which wallets do not offer, so native offers need `privateKeys`.
   */
  wallet?: WalletSigner;
  /** Payer-pinned token allowlist for kcc20 / swap payments (default: the tokens the offer names). */
  tokens?: TokenSpec[];
  context: ChainContextProvider;
  /** What the payer holds and accepts; the network is the client's own. */
  capabilities?: Omit<PayerCapabilities, 'network'>;
  store?: ArtifactStore;
  fetch?: FetchLike;
  now?: () => number;
  newPaymentId?: () => string;
  /** Submits a signed safe-JSON transaction (for `revoke`); see `rpcSubmitter`. */
  submit?: (safeJsonTx: string) => Promise<string>;
  maxFeeSompi?: string;
  /**
   * Fee rate of the payer's transactions (payments and revokes), sompi per gram, or a function asked before each one (the
   * dynamic fee policy: `feeRateFromEstimate(await rpc.getFeeEstimate(), 'high')`, a merchant's request waits for the payment).
   * Undefined: the builders' relay floor. A payment that cannot be built at the rate (its fee above `maxFeeSompi`, funds short
   * of the dearer fee) is retried once at `feeRateFloor`: an estimate never makes a payable payment impossible.
   */
  feeRate?: FeeRateSource;
  /** The rate a payment is retried at when it cannot be built at `feeRate` (default the relay floor, 100 sompi per gram). */
  feeRateFloor?: number;
  /** Swap-and-pay: most the payer will spend of the pay asset (base units). Required for a swap unless `approve` says yes. */
  maxPayAmount?: string;
  /**
   * Most KAS (sompi) the payer will fund into a merchant token output or its own token change (default 200000000 = 2
   * KAS). The wasm builder and the preflight both enforce it; an offer above it is refused before anything is signed.
   */
  maxCarrierSompi?: string;
  /**
   * Spend policy hook (user / agent policy). A payment is authorised without asking only when its merchant asset has
   * an explicit `capabilities.maxAmount` ceiling and, for a swap, `maxPayAmount` is set. Anything else is refused
   * unless this hook returns true for the offer (no unlimited autopay by default).
   */
  approve?: (request: PaymentApproval) => boolean | Promise<boolean>;
  /**
   * Re-signing policy. While an artifact for the same url + method is still live (signed, pending, or rejected
   * but not revoked and not expired) a new `fetch` refuses to sign a second independent payment, because the merchant
   * that rejected or stalled the first may already have broadcast it. With this hook returning true the client first
   * REVOKES the live artifacts (needs `privateKeys` and `submit`) and signs anew only when every revoke was submitted.
   */
  allowResign?: (prior: ArtifactRecord[]) => boolean | Promise<boolean>;
  /** Abort a merchant request that has not finished after this many ms (default 30000; 0 disables). */
  requestTimeoutMs?: number;
  /**
   * Lifetime of the authorization a native or kcc20 payment signs, in seconds (default 600). A merchant may advertise a
   * `maxTimeoutSeconds` of decades, and a signed payment stays spendable by whoever holds it until it is accepted: the payer
   * signs the shorter of this and the offer's `maxTimeoutSeconds`. (Swap-and-pay signs 60 s.)
   */
  authorizationTtlSeconds?: number;
  /** Allow plain http to a host that is not loopback (a private network you control). Default false: https only. */
  allowInsecureHttp?: boolean;
}

export interface PaymentApproval {
  offer: SelectedOffer;
  url: string;
  method: string;
  /** Why no explicit authorisation covers the offer. */
  reasons: ('no_spend_cap' | 'no_max_pay')[];
}

export interface PaymentReceipt {
  paymentId: string;
  transactionId: string;
  amount: string;
  asset: string;
  network: string;
  kind: OfferKind;
  settlement: SettlementResponse;
}

export interface PaidFetchResult {
  response: Response;
  /** Present when a payment was made. */
  payment?: PaymentReceipt;
}

function newId(): string {
  return randomBytes(24).toString('base64url');
}

function lc(s: string | undefined): string {
  return (s ?? '').toLowerCase();
}

export class KobX402Client {
  #o: KobX402ClientOptions;
  #store: ArtifactStore;
  #fetch: FetchLike;
  #now: () => number;

  constructor(options: KobX402ClientOptions) {
    if (!options.privateKeys?.length && !options.wallet) throw new KobX402Error('bad_request', 'privateKeys or a wallet signer is required');
    this.#o = options;
    this.#store = options.store ?? new MemoryArtifactStore();
    this.#now = options.now ?? Date.now;
    const f = options.fetch ?? (globalThis.fetch as FetchLike | undefined);
    if (!f) throw new KobX402Error('unsupported', 'no fetch implementation available');
    this.#fetch = f;
  }

  get store(): ArtifactStore {
    return this.#store;
  }

  /** `fetch` with automatic payment; returns the merchant's response. */
  async fetch(url: string, init: PaidFetchInit = {}): Promise<Response> {
    return (await this.paidFetch(url, init)).response;
  }

  async paidFetch(url: string, init: PaidFetchInit = {}): Promise<PaidFetchResult> {
    const { paymentId: idOverride, requestHash: hashOverride, ...rest } = init;
    const expectedHref = normalizeUrl(url);
    assertSecureUrl(expectedHref, 'the resource URL', this.#o.allowInsecureHttp === true);
    const method = normalizeMethod(rest.method);
    const bodyBytes = requestBody(rest.body);
    const baseInit: RequestInit = { ...rest, redirect: 'error' };

    const first = await this.#send(expectedHref, baseInit, 'payment challenge', false);
    if (first.status !== 402) return { response: first };

    const pr = await this.#readPaymentRequired(first);
    const caps: PayerCapabilities = { ...(this.#o.capabilities ?? {}), network: this.#o.network };
    const ranked = rankOffers(pr, caps);
    if (ranked.length === 0) throw new KobX402Error('no_acceptable_offer', 'the 402 has no Kaspa exact offer this payer can pay');
    const offer = await this.#authorize(ranked, caps, expectedHref, method);
    await this.#guardResign(expectedHref, method);

    const paymentId = idOverride ?? (this.#o.newPaymentId ?? newId)();
    if (!/^[A-Za-z0-9_-]{16,128}$/.test(paymentId)) throw new KobX402Error('bad_request', 'payment id must match ^[A-Za-z0-9_-]{16,128}$');
    const reqHash = hashOverride ? lc(hashOverride) : httpRequestHash(method, expectedHref, normalizeBody(bodyBytes), requirementsHash(offer.requirements));
    if (!HEX64.test(reqHash)) throw new KobX402Error('bad_request', 'requestHash must be 32-byte hex');

    const extensions = paymentIdentifierExtensions(pr, paymentId);
    const built = await this.#buildPayment(offer, reqHash, paymentId, extensions, pr);

    // Durable BEFORE disclosure. A failure here aborts the payment: nothing has been sent.
    const now = this.#now();
    const record: ArtifactRecord = {
      paymentId,
      createdAtMs: now,
      updatedAtMs: now,
      status: 'signed',
      url: expectedHref,
      method,
      requestHash: reqHash,
      transactionId: built.transactionId,
      kind: offer.kind,
      amount: offer.requirements.amount,
      asset: offer.requirements.asset,
      network: offer.requirements.network,
      expiresAtMs: built.expiresAtMs,
      consumed: built.consumed,
      paymentPayload: built.paymentPayload,
    };
    await this.#store.save(record);

    return this.#disclose(record, offer.kind, baseInit);
  }

  /**
   * Re-sends a STORED artifact (crash recovery, or after a `payment_pending` error) without signing again: the
   * merchant's idempotency (payment id + request hash) returns the settlement of that very transaction. `init` must
   * be the same request the artifact was signed for (method, body); the request hash is re-derived and must match.
   */
  async resume(paymentId: string, init: RequestInit = {}): Promise<PaidFetchResult> {
    const rec = await this.#store.load(paymentId);
    if (!rec) throw new KobX402Error('bad_request', `no artifact ${paymentId}`, { paymentId });
    if (rec.status === 'settled' || rec.status === 'revoked') throw new KobX402Error('bad_request', `the payment is already ${rec.status}`, { paymentId });
    if (rec.expiresAtMs <= this.#now()) throw new KobX402Error('bad_request', 'the artifact authorization has expired; call fetch again to re-quote and re-sign', { paymentId });
    const hash = httpRequestHash(normalizeMethod(init.method), rec.url, normalizeBody(requestBody(init.body)), requirementsHash(rec.paymentPayload.accepted));
    if (hash !== rec.requestHash) throw new KobX402Error('bad_request', 'this request is not the one the artifact was signed for', { paymentId });
    return this.#disclose(rec, rec.kind, { ...init, redirect: 'error' });
  }

  /** Discloses a stored artifact (the paid retry) and verifies the outcome. */
  async #disclose(record: ArtifactRecord, kind: OfferKind, baseInit: RequestInit): Promise<PaidFetchResult> {
    const { paymentId, transactionId, url: expectedHref } = record;
    const headers = new Headers(baseInit.headers);
    headers.set(HEADER_PAYMENT_SIGNATURE, encodePaymentSignature(record.paymentPayload));
    const retryInit: RequestInit = { ...baseInit, headers };

    let second: Response;
    try {
      second = await this.#send(expectedHref, retryInit, 'paid retry', true);
    } catch (e) {
      await this.#mark(paymentId, { status: 'pending', note: 'the paid retry failed before a response; the merchant may hold the payment' });
      throw pendingError(paymentId, transactionId, 'the paid request failed; the payment may have been received', e);
    }

    if (second.status === 402) {
      const failure = await this.#readFailure(second);
      await this.#mark(paymentId, { status: 'rejected', failure });
      throw new KobX402Error('payment_failed', `payment rejected: ${failure.diagnostic ?? 'unknown'}: ${failure.message ?? ''}`, {
        diagnostic: failure.diagnostic,
        retryable: failure.retryable === true,
        details: failure.details,
        paymentId,
        transactionId,
        status: 402,
      });
    }

    const header = second.headers.get(HEADER_PAYMENT_RESPONSE);
    if (second.status < 200 || second.status >= 300 || !header) {
      await this.#mark(paymentId, { status: 'pending', note: `paid retry answered ${second.status} without a settlement` });
      throw pendingError(paymentId, transactionId, `paid retry answered ${second.status} without a valid PAYMENT-RESPONSE`, undefined, second.status);
    }
    let settlement: SettlementResponse;
    try {
      settlement = decodePaymentResponse(header);
      verifySettlement(settlement, record.amount, record.network, transactionId);
    } catch (e) {
      await this.#mark(paymentId, { status: 'pending', note: 'PAYMENT-RESPONSE did not verify' });
      throw new KobX402Error('invalid_settlement', `PAYMENT-RESPONSE rejected: ${(e as Error).message}`, {
        cause: e,
        paymentId,
        transactionId,
        status: second.status,
      });
    }
    await this.#mark(paymentId, { status: 'settled' });
    return {
      response: second,
      payment: { paymentId, transactionId, amount: record.amount, asset: record.asset, network: record.network, kind, settlement },
    };
  }

  /**
   * Invalidates a signed payment by spending one of its P2PK inputs back to the payer. Only meaningful while the
   * payment is not accepted; a transaction that is already accepted cannot be revoked (its inputs are spent). Wallet mode
   * revokes kcc20 and swap-and-pay payments (the wallet signs the self-spend); a native payment's revoke needs `privateKeys`.
   */
  async revoke(paymentId: string): Promise<{ transactionId: string; transaction: string; submitted: boolean }> {
    const rec = await this.#store.load(paymentId);
    if (!rec) throw new KobX402Error('revoke_failed', `no artifact ${paymentId}`, { paymentId });
    if (rec.status === 'settled' || rec.status === 'revoked') {
      throw new KobX402Error('revoke_failed', `the payment is ${rec.status}`, { paymentId });
    }
    let r: ReturnType<KobWasm['revoke']>;
    try {
      const feeRate = await resolveFeeRate(this.#o.feeRate);
      if (this.#o.privateKeys?.length) {
        const req: Parameters<KobWasm['revoke']>[0] = { paymentPayload: rec.paymentPayload, privateKeys: this.#o.privateKeys };
        if (this.#o.tokens) req.tokens = this.#o.tokens;
        r = await withFeeFloorAsync(feeRate, this.#o.feeRateFloor, async (rate) => {
          if (rate === undefined) delete req.feeRate;
          else req.feeRate = rate;
          return this.#o.wasm.revoke(req);
        });
      } else {
        // wallet mode (kcc20 and swap-and-pay payments): the wallet signs the self-spend of one of the payment's inputs
        const wallet = this.#o.wallet as WalletSigner;
        const w = this.#o.wasm;
        if (!w.prepareRevoke || !w.finishRevoke) throw new KobX402Error('revoke_failed', 'this KobWasm has no wallet revoke (prepareRevoke / finishRevoke)', { paymentId });
        const req: Parameters<NonNullable<KobWasm['prepareRevoke']>>[0] = { paymentPayload: rec.paymentPayload, payerPublicKey: wallet.publicKey };
        if (this.#o.tokens) req.tokens = this.#o.tokens;
        const prepared = await withFeeFloorAsync(feeRate, this.#o.feeRateFloor, async (rate) => {
          if (rate === undefined) delete req.feeRate;
          else req.feeRate = rate;
          return w.prepareRevoke!(req);
        });
        r = w.finishRevoke(prepared, await wallet.signInputs(prepared.built.sign));
      }
    } catch (e) {
      if (e instanceof KobX402Error) throw e;
      throw new KobX402Error('revoke_failed', `cannot build the revoke transaction: ${(e as Error).message}`, { cause: e, paymentId });
    }
    if (!this.#o.submit) return { transactionId: r.transactionId, transaction: r.transaction, submitted: false };
    let id: string;
    try {
      id = await this.#o.submit(r.transaction);
    } catch (e) {
      throw new KobX402Error('revoke_failed', `the revoke transaction was not accepted: ${(e as Error).message}`, { cause: e, paymentId });
    }
    // The node's answer is a claim: the revoke transaction's id is the one the client computed from the transaction it signed.
    if (typeof id !== 'string' || lc(id) !== lc(r.transactionId)) {
      throw new KobX402Error('revoke_failed', `the node answered another transaction id than the revoke transaction's (${String(id)}, expected ${r.transactionId}): the artifact stays live`, { paymentId });
    }
    await this.#mark(paymentId, { status: 'revoked', revokeTransactionId: r.transactionId });
    return { transactionId: r.transactionId, transaction: r.transaction, submitted: true };
  }

  // ------------------------------------------------------------------------------------------------ internals

  /**
   * the first ranked offer that is authorised to be paid. Authorised = an explicit ceiling for its merchant asset
   * (already enforced by `rankOffers`) and, for a swap, `maxPayAmount`; otherwise the `approve` hook must say yes.
   */
  async #authorize(ranked: SelectedOffer[], caps: PayerCapabilities, url: string, method: string): Promise<SelectedOffer> {
    const o = this.#o;
    let refused = 0;
    for (const offer of ranked) {
      const reasons: PaymentApproval['reasons'] = [];
      if (caps.maxAmount?.[offer.asset] === undefined) reasons.push('no_spend_cap');
      if (offer.kind === 'swap' && !o.maxPayAmount) reasons.push('no_max_pay');
      if (reasons.length === 0) return offer;
      if (o.approve && (await o.approve({ offer, url, method, reasons })) === true) return offer;
      refused++;
    }
    throw new KobX402Error(
      'spend_not_authorized',
      `${refused} offer(s) could be paid but none is authorised: set capabilities.maxAmount for the merchant asset (and maxPayAmount for swaps) or provide an approve policy`,
    );
  }

  /** no second independent payment for the same resource while an earlier artifact is live. */
  async #guardResign(url: string, method: string): Promise<void> {
    const now = this.#now();
    const live = (await this.#store.list()).filter(
      (r) => r.url === url && r.method === method && (r.status === 'signed' || r.status === 'pending' || r.status === 'rejected') && r.expiresAtMs > now,
    );
    if (live.length === 0) return;
    const ids = live.map((r) => r.paymentId);
    const allowed = this.#o.allowResign ? (await this.#o.allowResign(live)) === true : false;
    if (!allowed) {
      throw new KobX402Error(
        'payment_in_flight',
        `an earlier signed payment for this resource is still live (${ids.join(', ')}): the merchant may hold it. Reconcile or revoke() it, or opt in with allowResign`,
        { paymentId: ids[0] as string, transactionId: (live[0] as ArtifactRecord).transactionId },
      );
    }
    for (const r of live) {
      let rv: Awaited<ReturnType<KobX402Client['revoke']>>;
      try {
        rv = await this.revoke(r.paymentId);
      } catch (e) {
        throw new KobX402Error('payment_in_flight', `the earlier payment ${r.paymentId} could not be revoked: ${(e as Error).message}`, { cause: e, paymentId: r.paymentId });
      }
      if (!rv.submitted) throw new KobX402Error('payment_in_flight', `the revoke of ${r.paymentId} was built but not submitted (configure submit)`, { paymentId: r.paymentId });
    }
  }

  async #send(expectedHref: string, init: RequestInit, stage: string, paid: boolean): Promise<Response> {
    let res: Response;
    try {
      res = await this.#fetch(expectedHref, this.#withTimeout(init));
    } catch (e) {
      const msg = `${(e as Error).message} ${((e as Error).cause as Error | undefined)?.message ?? ''}`;
      if (/redirect/i.test(msg)) throw new KobX402Error('redirect', `${stage} was redirected; redirects are rejected`, { cause: e });
      if (paid) throw e;
      throw new KobX402Error('payment_failed', `${stage} failed: ${(e as Error).message}`, { cause: e });
    }
    assertTarget(res, expectedHref, stage);
    return res;
  }

  /** a merchant that never answers cannot stall the payer forever. */
  #withTimeout(init: RequestInit): RequestInit {
    const ms = this.#o.requestTimeoutMs ?? 30_000;
    if (!(ms > 0)) return init;
    const t = AbortSignal.timeout(ms);
    return { ...init, signal: init.signal ? AbortSignal.any([init.signal, t]) : t };
  }

  async #readPaymentRequired(res: Response): Promise<PaymentRequired> {
    const header = res.headers.get(HEADER_PAYMENT_REQUIRED);
    if (header) return parsePaymentRequired(header);
    return parsePaymentRequired(await readText(res));
  }

  async #readFailure(res: Response): Promise<KaspaFailure & { reason?: string }> {
    let pr: PaymentRequired | undefined;
    try {
      const header = res.headers.get(HEADER_PAYMENT_REQUIRED);
      pr = parsePaymentRequired(header ?? (await readText(res)));
    } catch {
      return { diagnostic: 'unparsable_corrective_402', retryable: false, message: 'the corrective 402 could not be parsed' };
    }
    const k = pr.extensions?.kaspa;
    const out: KaspaFailure & { reason?: string } = {
      diagnostic: typeof k?.diagnostic === 'string' ? k.diagnostic : pr.error ?? 'unknown',
      retryable: k?.retryable === true,
      message: typeof k?.message === 'string' ? k.message : pr.error ?? '',
    };
    if (k?.details !== undefined) out.details = k.details;
    if (pr.error) out.reason = pr.error;
    return out;
  }

  async #mark(paymentId: string, patch: Partial<Omit<ArtifactRecord, 'paymentId'>>): Promise<void> {
    try {
      await this.#store.update(paymentId, patch);
    } catch {
      // The outcome is already decided; a failed bookkeeping write must not hide it from the caller.
    }
  }

  async #buildPayment(
    offer: SelectedOffer,
    requestHash: string,
    paymentId: string,
    extensions: NonNullable<PaymentPayload['extensions']>,
    pr: PaymentRequired,
  ): Promise<PayResult> {
    const o = this.#o;
    const q: ChainContextQuery = { payerAddress: o.payerAddress, network: o.network, kind: offer.kind, asset: offer.asset, amount: offer.requirements.amount };
    if (offer.payAsset) q.payAsset = offer.payAsset;
    const ctx = await o.context.load(q);
    if (ctx.utxos.length === 0 && !ctx.tokenUtxos?.length) throw new KobX402Error('no_funds', 'the payer has no spendable outputs');
    const needsTokens = offer.kind === 'kcc20' || (offer.kind === 'swap' && offer.payAsset !== 'KAS');
    if (needsTokens && !ctx.tokenUtxos?.length) throw new KobX402Error('no_funds', 'the payer has no token UTXOs of the asset this offer is paid with');
    if (offer.kind === 'swap' && !ctx.quote) throw new KobX402Error('no_funds', 'no swap quote: the chain context has no orders to fill');
    const nowMs = this.#now();
    const req: PayRequest = {
      requirements: offer.requirements,
      requestHash,
      payerAddress: o.payerAddress,
      utxos: ctx.utxos,
      paymentId,
      nowMs,
    };
    if (o.privateKeys?.length) req.privateKeys = o.privateKeys;
    if (ctx.tokenUtxos) req.tokenUtxos = ctx.tokenUtxos;
    if (ctx.quote) req.quote = ctx.quote;
    if (offer.payAsset) req.payAsset = offer.payAsset;
    if (o.tokens) req.tokens = o.tokens;
    if (o.capabilities?.allowIssuerControlled) req.allowIssuerControlled = true;
    if (o.maxFeeSompi) req.maxFeeSompi = o.maxFeeSompi;
    const feeRate = await resolveFeeRate(o.feeRate);
    if (offer.kind !== 'swap') {
      const ttl = o.authorizationTtlSeconds ?? DEFAULT_AUTH_TTL_SECONDS;
      if (!Number.isInteger(ttl) || ttl <= 0) throw new KobX402Error('bad_request', 'authorizationTtlSeconds must be a positive integer');
      req.ttlSeconds = Math.min(ttl, offer.requirements.maxTimeoutSeconds);
    }
    if (o.maxPayAmount) req.maxPayAmount = o.maxPayAmount;
    if (o.maxCarrierSompi) req.maxCarrierSompi = o.maxCarrierSompi;

    let result: PayResult;
    try {
      result = await withFeeFloorAsync(feeRate, o.feeRateFloor, (rate) => {
        if (rate === undefined) delete req.feeRate;
        else req.feeRate = rate;
        return this.#sign(offer, req);
      });
    } catch (e) {
      if (e instanceof KobX402Error) throw e;
      const info = e as { diagnostic?: string; reason?: string; retryable?: boolean; details?: unknown; message?: string };
      throw new KobX402Error('payment_failed', `cannot build the payment: ${info.message ?? String(e)}`, {
        cause: e,
        paymentId,
        ...(info.diagnostic ? { diagnostic: info.diagnostic } : {}),
        ...(info.reason ? { reason: info.reason } : {}),
        ...(info.retryable !== undefined ? { retryable: info.retryable } : {}),
        ...(info.details !== undefined ? { details: info.details } : {}),
      });
    }
    // Unsigned metadata (the payment-identifier extension echoes the server's advertised schema; the resource) is
    // not covered by any digest: it is set here, once, whatever the builder produced.
    result.paymentPayload.extensions = extensions;
    if (!result.paymentPayload.resource) result.paymentPayload.resource = pr.resource;
    this.#checkBuilt(result, offer, requestHash, paymentId, nowMs);

    // Everything the transaction spends must be an output the payer's own chain context reported.
    const known = new Set([...ctx.utxos, ...collectOutpoints(ctx.tokenUtxos), ...collectOutpoints(ctx.quote)].map((u) => (typeof u === 'string' ? u : `${u.txid}:${u.index}`)));
    for (const c of result.consumed) {
      if (!known.has(`${c.txid}:${c.index}`)) throw new KobX402Error('payment_failed', `built payment spends ${c.txid}:${c.index}, which is not in the payer's chain context`, { paymentId });
    }

    const pf: Parameters<KobWasm['preflight']>[0] = { requirements: offer.requirements, paymentPayload: result.paymentPayload, requestHash, nowMs };
    if (ctx.virtualDaaScore) pf.virtualDaaScore = ctx.virtualDaaScore;
    if (o.tokens) pf.tokens = o.tokens;
    if (o.capabilities?.allowIssuerControlled) pf.allowIssuerControlled = true;
    if (offer.kind === 'swap' && o.maxPayAmount) pf.maxPay = o.maxPayAmount;
    if (o.maxCarrierSompi) pf.maxCarrierSompi = o.maxCarrierSompi;
    const report = o.wasm.preflight(pf);
    if (!report.ok) {
      throw new KobX402Error('preflight_failed', `the built payment failed preflight: ${report.diagnostic ?? ''} ${report.message ?? ''}`.trim(), {
        diagnostic: report.diagnostic,
        paymentId,
      });
    }
    if (report.transactionId && report.transactionId !== result.transactionId) {
      throw new KobX402Error('preflight_failed', 'preflight recomputed a different transaction id', { paymentId });
    }
    return result;
  }

  /** Local-key builders, or the wallet flow (unsigned -> wallet signs -> finish) for kcc20 and swap-and-pay. */
  async #sign(offer: SelectedOffer, req: PayRequest): Promise<PayResult> {
    const o = this.#o;
    if (o.privateKeys?.length) {
      return offer.kind === 'native' ? o.wasm.payNative(req) : offer.kind === 'kcc20' ? o.wasm.payKcc20(req) : o.wasm.paySwap(req);
    }
    const wallet = o.wallet as WalletSigner;
    if (offer.kind === 'native') {
      throw new KobX402Error('unsupported', 'a native KAS payment signs the request authorization digest with the funding key: it needs a local key (wallets sign transactions, not digests)');
    }
    req.payerPublicKey = wallet.publicKey;
    if (offer.kind === 'kcc20') {
      const unsigned = o.wasm.buildKcc20Unsigned(req);
      return o.wasm.finishKcc20(unsigned, await wallet.signInputs(unsigned.built.sign));
    }
    const prepared = o.wasm.prepareSwap(req);
    return o.wasm.finishSwap(prepared, await wallet.signInputs(prepared.built.sign));
  }

  /** Cheap structural checks on what the wasm returned, independent of the wasm's own verification. */
  #checkBuilt(r: PayResult, offer: SelectedOffer, requestHash: string, paymentId: string, nowMs: number): void {
    const p = r.paymentPayload;
    const fail = (m: string): never => {
      throw new KobX402Error('payment_failed', `built payment is inconsistent: ${m}`, { paymentId });
    };
    if (p.x402Version !== X402_VERSION) fail('x402Version');
    if (canonicalJson(p.accepted) !== canonicalJson(offer.requirements)) fail('accepted differs from the selected offer');
    if (lc(p.payload.requestHash) !== requestHash) fail('requestHash differs from the derived fingerprint');
    if (p.payload.transactionEncoding !== offer.requirements.extra.transactionEncoding) fail('transactionEncoding');
    if (!HEX64.test(r.transactionId)) fail('transaction id');
    if (p.extensions?.[PAYMENT_IDENTIFIER_KEY]?.info?.id !== paymentId) fail('payment identifier not echoed');
    if (!(r.expiresAtMs > nowMs)) fail('authorization already expired');
    if (r.consumed.length === 0) fail('no consumed outpoints');
  }
}

// ------------------------------------------------------------------------------------------------- helpers

/** Every `{ transactionId, index }` outpoint found anywhere in a JSON value (kob-protocol UTXO JSON, quotes, order legs). */
function collectOutpoints(v: unknown, out: { txid: string; index: number }[] = []): { txid: string; index: number }[] {
  if (Array.isArray(v)) for (const x of v) collectOutpoints(x, out);
  else if (v !== null && typeof v === 'object') {
    const r = v as Record<string, unknown>;
    if (typeof r.transactionId === 'string' && typeof r.index === 'number') out.push({ txid: r.transactionId.toLowerCase(), index: r.index });
    for (const x of Object.values(r)) collectOutpoints(x, out);
  }
  return out;
}

function requestBody(body: RequestInit['body']): string | Uint8Array | null {
  if (body === undefined || body === null) return null;
  if (typeof body === 'string') return body;
  if (body instanceof Uint8Array) return body;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  if (body instanceof URLSearchParams) return body.toString();
  throw new KobX402Error('bad_request', 'paid requests support string, Uint8Array, ArrayBuffer and URLSearchParams bodies only');
}

function paymentIdentifierExtensions(pr: PaymentRequired, id: string): NonNullable<PaymentPayload['extensions']> {
  const adv = pr.extensions?.[PAYMENT_IDENTIFIER_KEY] as PaymentIdentifierExtension | undefined;
  const ext: PaymentIdentifierExtension = { info: { ...(adv?.info ?? { required: false }), id } };
  if (adv?.schema) ext.schema = adv.schema;
  return { [PAYMENT_IDENTIFIER_KEY]: ext };
}

function assertTarget(res: Response, expectedHref: string, stage: string): void {
  let effective: string;
  try {
    effective = new URL(res.url).href;
  } catch {
    throw new KobX402Error('redirect', `${stage} did not expose a valid effective response URL`);
  }
  if (res.redirected || (res.status >= 300 && res.status < 400) || effective !== expectedHref) {
    throw new KobX402Error('redirect', `${stage} redirected away from the authorized request URL`);
  }
}

/** Reads at most MAX_BODY_BYTES: the body is consumed in chunks and cancelled at the cap, never buffered whole. */
async function readText(res: Response): Promise<string> {
  const tooLarge = (): KobX402Error => new KobX402Error('invalid_payment_required', 'response body too large');
  const declared = Number(res.headers.get('content-length') ?? '0');
  if (declared > MAX_BODY_BYTES) {
    await res.body?.cancel().catch(() => undefined);
    throw tooLarge();
  }
  if (!res.body) return '';
  const reader = res.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_BODY_BYTES) {
      await reader.cancel().catch(() => undefined);
      throw tooLarge();
    }
    chunks.push(value);
  }
  return new TextDecoder().decode(Buffer.concat(chunks));
}

function pendingError(paymentId: string, transactionId: string, message: string, cause?: unknown, status?: number): KobX402Error {
  const init: ConstructorParameters<typeof KobX402Error>[2] = { paymentId, transactionId };
  if (cause !== undefined) init.cause = cause;
  if (status !== undefined) init.status = status;
  return new KobX402Error('payment_pending', `${message} (artifact ${paymentId} is stored; reconcile or revoke it)`, init);
}

/** The PAYMENT-RESPONSE of a paid retry must show success for exactly the transaction the client signed. */
export function verifySettlement(s: SettlementResponse, amount: string, network: string, transactionId: string): void {
  if (s === null || typeof s !== 'object' || s.success !== true) throw new KobX402Error('invalid_settlement', 'settlement is not a success');
  if (typeof s.transaction !== 'string' || !HEX64.test(lc(s.transaction))) throw new KobX402Error('invalid_settlement', 'settlement transaction is not a transaction id');
  if (lc(s.transaction) !== lc(transactionId)) throw new KobX402Error('invalid_settlement', 'settlement transaction id differs from the signed transaction');
  if (s.amount !== amount) throw new KobX402Error('invalid_settlement', 'settlement amount differs from the accepted amount');
  if (s.network !== network) throw new KobX402Error('invalid_settlement', 'settlement network differs from the accepted network');
}
