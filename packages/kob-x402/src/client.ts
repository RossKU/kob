// KobX402Client: the payer flow.
//
//   request -> 402 -> select offer -> derive requestHash -> build + sign through KobWasm -> preflight ->
//   SAVE the signed artifact -> retry with PAYMENT-SIGNATURE -> verify PAYMENT-RESPONSE -> return the response.
//
// Retry (`retry`, docs/spec/x402-retry.md; default 3 attempts, 4 re-sends each, backoff with jitter, deadline = the shorter of
// 120 s and the offer's maxTimeoutSeconds): an unknown outcome (no answer, timeout, 5xx, settlement_pending) RE-SENDS the same
// stored artifact (same payment id, same transaction: the merchant and the facilitator answer it from memory, a payment that
// went through is found, never paid again); a failure that left the payer's funds where they were (order_conflict, the node
// refused the transaction, the authorization expired) REBUILDS: a fresh chain context and quote, the limits, `approve` and the
// preflight again, a fresh payment id, and the payment's ANCHOR (an input of the first attempt the payer owns) spent by every
// attempt, so two attempts of one payment can never both be accepted; anything else stops.
//
// Safety properties:
//  * redirects are rejected on both requests and the effective URL must equal the requested URL (a signed
//    PAYMENT-SIGNATURE is never forwarded to a redirect target);
//  * `requestHash` is derived locally from the request the client itself sends (never taken from the server);
//  * the retry of one payment stays inside its own `paidFetch` (see above); a NEW `fetch()` does NOT silently sign a second,
//    independent payment for the same resource while an earlier artifact is still live (the merchant may already hold it):
//    the caller opts in with `allowResign`, and the client then revokes the earlier artifact first. Paid requests for one
//    resource run one at a time in a client;
//  * nothing is paid without a spend authorisation: a per-asset `capabilities.maxAmount` ceiling, or an `approve`
//    policy hook that says yes to the built payment's cost; a swap is never built without a `maxPay` bound for the
//    pay asset it spends;
//  * the KAS a payer funds into a merchant token output (the "carrier") is capped (`maxCarrierSompi`, default 2 KAS);
//  * the signed artifact is durably recorded BEFORE it is disclosed, so a crash cannot lose a payment the
//    merchant may already hold; `revoke(paymentId)` invalidates it by spending one of its inputs back to the payer;
//  * the PAYMENT-RESPONSE must show success, the transaction id the client itself signed, and the accepted amount.

import { randomBytes } from 'node:crypto';
import { canonicalJson, httpRequestHash, normalizeBody, normalizeMethod, normalizeUrl, requirementsHash } from './canonical.ts';
import { KobX402Error } from './errors.ts';
import { decodePaymentResponse, encodePaymentSignature } from './headers.ts';
import { payableAssets, rankOffers, parsePaymentRequired } from './offers.ts';
import type { PayerCapabilities, SelectedOffer } from './offers.ts';
import { MemoryArtifactStore } from './artifact-store.ts';
import { assertSecureUrl } from './url-policy.ts';
import type { ArtifactRecord, ArtifactStore } from './artifact-store.ts';
import type { InputSignature, KobWasm, PayRequest, PayResult, SignRequest, SwapQuote, TokenSpec, TokenUtxoJson } from './wasm.ts';
import { resolveFeeRate, withFeeFloorAsync, type FeeRateSource } from './fee.ts';
import { backoffMs, classifyFailure, classifyStatus, outpointKey, retryDeadline, retryPolicy } from './retry.ts';
import type { AttemptSummary, RetryOptions, RetryPolicy, RetryStep } from './retry.ts';
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
  /**
   * Swap-and-pay: the most one payment may spend of each pay asset, by asset: `KAS` in sompi (fee included), a token
   * covenant id in its base units. A swap pays only with an asset named here (the first of the offer's pay assets the
   * payer can pay that has a bound); the bound is enforced by the builder and the preflight. Required for a swap unless
   * `approve` says yes.
   */
  maxPay?: Record<string, string>;
  /**
   * KAS-only shorthand for `maxPay: { KAS: ... }` (sompi). It bounds a swap that pays with KAS and is never read in the
   * units of a token.
   */
  maxPayAmount?: string;
  /**
   * Most KAS (sompi) the payer will fund into a merchant token output or its own token change (default 200000000 = 2
   * KAS). The wasm builder and the preflight both enforce it; an offer above it is refused before anything is signed.
   */
  maxCarrierSompi?: string;
  /**
   * Spend policy hook (user / agent policy). A payment is authorised without asking only when its merchant asset has
   * an explicit `capabilities.maxAmount` ceiling. Anything else is refused unless this hook returns true; it is asked
   * after the payment is built and preflighted, with what it costs (`cost`), and before anything is stored or sent. A
   * swap always needs a `maxPay` bound for its pay asset: without one it is not built, whatever this hook says.
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
  /**
   * Retry of a payment that failed or whose outcome is unknown (default `{ attempts: 3, resends: 4 }`; `false`: one attempt,
   * no re-send). See `RetryOptions` and docs/spec/x402-retry.md: an unknown outcome re-sends the same signed artifact; a
   * failure that left the payer's funds where they were rebuilds the payment (fresh quote, limits, `approve` and preflight
   * again; the anchor input spent by every attempt); a refusal stops. A caller-chosen `paymentId` names the first attempt;
   * a rebuilt attempt takes a fresh id from `newPaymentId`.
   */
  retry?: RetryOptions | false;
}

export interface PaymentApproval {
  offer: SelectedOffer;
  url: string;
  method: string;
  /** Why no explicit authorisation covers the offer. */
  reasons: ('no_spend_cap' | 'no_kas_cap')[];
  /** What the built (not yet stored or sent) payment costs. */
  cost: PaymentCost;
  /**
   * Attempt number (1 = the first; more when a retry rebuilt the payment after the attempt `replaces` failed). Every
   * rebuilt attempt is asked again, at its own cost (a fresh quote may cost more); its anchor input makes it exclude the
   * attempts before it.
   */
  attempt: number;
  replaces?: string;
}

/** What a built payment costs the payer. */
export interface PaymentCost {
  /** What the merchant receives (`KAS` in sompi, a token in base units). */
  asset: string;
  amount: string;
  /** Swap-and-pay: the asset the payer pays with and the units of it the payer gives up (sompi including the fee for KAS). */
  payAsset?: string;
  payerSpent?: string;
  /** The network fee of the payment transaction, in sompi. */
  feeSompi: string;
  /** KAS (sompi) the payer funds into the merchant's token output (kcc20 and token-receiving swaps; `0` otherwise). */
  carrierSompi: string;
  /**
   * Everything the payment takes from the payer's KAS, in sompi: the amount of a native payment, the carrier and the fee
   * (and the whole cost of a swap paid with KAS). This is what the KAS ceiling is checked against.
   */
  kasSpent: string;
}

export interface PaymentReceipt {
  paymentId: string;
  transactionId: string;
  amount: string;
  asset: string;
  network: string;
  kind: OfferKind;
  settlement: SettlementResponse;
  /** Signed attempts (1: the first one paid) and re-sends over all of them. */
  attempts: number;
  resends: number;
  /** Payment ids of the earlier attempts (now `superseded` in the store). */
  superseded: string[];
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
  #retry: RetryPolicy;
  /** Paid requests in flight per `METHOD url` (one at a time per resource). */
  #locks = new Map<string, Promise<void>>();

  constructor(options: KobX402ClientOptions) {
    if (!options.privateKeys?.length && !options.wallet) throw new KobX402Error('bad_request', 'privateKeys or a wallet signer is required');
    for (const [asset, v] of Object.entries({ ...(options.maxPay ?? {}), ...(options.maxPayAmount !== undefined ? { 'maxPayAmount (KAS)': options.maxPayAmount } : {}) })) {
      if (typeof v !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(v)) throw new KobX402Error('bad_request', `maxPay bound for ${asset} must be a decimal integer string`);
    }
    try {
      this.#retry = retryPolicy(options.retry);
    } catch (e) {
      throw new KobX402Error('bad_request', (e as Error).message);
    }
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
    const candidates = this.#candidates(ranked, caps);

    // one paid request per resource at a time: the in-flight check and the signing are not interleaved with another call's
    return this.#exclusive(`${method} ${expectedHref}`, async () => {
      await this.#guardResign(expectedHref, method);

      // Each candidate is built (and preflighted) first: what it costs is known only then. One that no explicit ceiling
      // covers is paid only when `approve` says yes to that cost; a refused build is discarded (never stored, never sent).
      let chosen: { offer: SelectedOffer; reasons: PaymentApproval['reasons']; paymentId: string; reqHash: string; built: PayResult; anchor: string | undefined } | undefined;
      let refused = ranked.length - candidates.length;
      for (const { offer, reasons } of candidates) {
        if (reasons.length > 0 && !this.#o.approve) {
          refused++;
          continue;
        }
        const paymentId = idOverride ?? (this.#o.newPaymentId ?? newId)();
        if (!/^[A-Za-z0-9_-]{16,128}$/.test(paymentId)) throw new KobX402Error('bad_request', 'payment id must match ^[A-Za-z0-9_-]{16,128}$');
        const reqHash = hashOverride ? lc(hashOverride) : httpRequestHash(method, expectedHref, normalizeBody(bodyBytes), requirementsHash(offer.requirements));
        if (!HEX64.test(reqHash)) throw new KobX402Error('bad_request', 'requestHash must be 32-byte hex');
        const extensions = paymentIdentifierExtensions(pr, paymentId);
        const { built, payerOwned } = await this.#buildPayment(offer, reqHash, paymentId, extensions, pr);
        if (!(await this.#authorized(offer, [...reasons], built, caps, expectedHref, method, 1))) {
          refused++;
          continue;
        }
        // the anchor: the first input the payer owns; every rebuilt attempt of this payment must spend it too
        const anchor = built.consumed.map(outpointKey).find((k) => payerOwned.has(k));
        chosen = { offer, reasons, paymentId, reqHash, built, anchor };
        break;
      }
      if (!chosen) {
        throw new KobX402Error(
          'spend_not_authorized',
          `${refused} offer(s) could be paid but none is authorised: set capabilities.maxAmount for the merchant asset and maxPay for the pay asset of a swap, or provide an approve policy`,
        );
      }
      const { offer, paymentId, reqHash, built, anchor } = chosen;

      // Durable BEFORE disclosure. A failure here aborts the payment: nothing has been sent.
      const record = this.#record(offer, paymentId, reqHash, built, expectedHref, method, { attempt: 1, ...(anchor ? { anchor } : {}) });
      await this.#store.save(record);

      return this.#settleWithRetry(record, { offer, reasons: chosen.reasons, pr, caps, baseInit, url: expectedHref, method });
    });
  }

  /**
   * Re-sends a STORED artifact (crash recovery, or after a `payment_pending` error) without signing again: the
   * merchant's idempotency (payment id + request hash) returns the settlement of that very transaction. `init` must
   * be the same request the artifact was signed for (method, body); the request hash is re-derived and must match.
   * While the outcome stays unknown the artifact is re-sent within the `retry` bounds; it is never rebuilt here (a failed
   * artifact is reported; a new `fetch` pays anew).
   */
  async resume(paymentId: string, init: RequestInit = {}): Promise<PaidFetchResult> {
    const peek = await this.#store.load(paymentId);
    if (!peek) throw new KobX402Error('bad_request', `no artifact ${paymentId}`, { paymentId });
    return this.#exclusive(`${peek.method} ${peek.url}`, async () => {
      const rec = await this.#store.load(paymentId);
      if (!rec) throw new KobX402Error('bad_request', `no artifact ${paymentId}`, { paymentId });
      if (rec.status === 'settled' || rec.status === 'revoked' || rec.status === 'superseded') {
        throw new KobX402Error('bad_request', `the payment is already ${rec.status}`, { paymentId });
      }
      if (rec.expiresAtMs <= this.#now()) throw new KobX402Error('bad_request', 'the artifact authorization has expired; call fetch again to re-quote and re-sign', { paymentId });
      const hash = httpRequestHash(normalizeMethod(init.method), rec.url, normalizeBody(requestBody(init.body)), requirementsHash(rec.paymentPayload.accepted));
      if (hash !== rec.requestHash) throw new KobX402Error('bad_request', 'this request is not the one the artifact was signed for', { paymentId });
      return this.#settleWithRetry(rec, { url: rec.url, method: rec.method, baseInit: { ...init, redirect: 'error' } });
    });
  }

  /**
   * Discloses `record` and follows the outcome: re-sends it while the outcome is unknown, rebuilds the payment after a
   * failure that left the payer's funds where they were (when `rebuild` context is given), stops otherwise. Bounded by
   * the retry policy and the deadline (the shorter of its budget and the offer's `maxTimeoutSeconds`).
   */
  async #settleWithRetry(
    first: ArtifactRecord,
    s: {
      url: string;
      method: string;
      baseInit: RequestInit;
      offer?: SelectedOffer;
      reasons?: PaymentApproval['reasons'];
      pr?: PaymentRequired;
      caps?: PayerCapabilities;
    },
  ): Promise<PaidFetchResult> {
    const policy = this.#retry;
    const deadline = retryDeadline(policy, this.#now(), first.paymentPayload.accepted.maxTimeoutSeconds);
    const done: AttemptSummary[] = [];
    let record = first;
    let sends = 0;
    let resends = 0;
    let totalResends = 0;
    for (;;) {
      sends++;
      const out = await this.#discloseOnce(record, record.kind, s.baseInit);
      if (out.ok) {
        const superseded = done.map((a) => a.paymentId);
        for (const id of superseded) await this.#mark(id, { status: 'superseded', note: `attempt ${record.paymentId} settled and spent the anchor ${record.anchor ?? ''}` });
        const p = out.result.payment as PaymentReceipt;
        p.attempts = done.length + 1;
        p.resends = totalResends;
        p.superseded = superseded;
        return out.result;
      }
      const current: AttemptSummary = { paymentId: record.paymentId, transactionId: record.transactionId, outcome: out.outcome, sends };
      const giveUp = (why?: string): KobX402Error => {
        const e = out.error;
        if (done.length > 0 || sends > 1) {
          e.attempts = [...done, current];
          if (why) e.message = `${e.message} (${why})`;
        }
        return e;
      };
      if (out.step === 'stop') throw giveUp();
      if (out.step === 'resend') {
        if (resends >= policy.resends) throw giveUp(`gave up after ${sends} send(s) of attempt ${done.length + 1}`);
        const wait = backoffMs(policy, resends + 1, policy.random());
        if (this.#now() + wait >= deadline) throw giveUp('the retry deadline passed');
        resends++;
        totalResends++;
        await policy.sleep(wait);
        continue;
      }
      // rebuild: a new attempt, anchored to the first one (never without the offer context of a fresh paidFetch)
      if (!s.offer || !s.pr || !s.caps || !s.reasons) throw giveUp();
      const n = done.length + 1;
      if (n >= policy.attempts) throw giveUp(`gave up after ${n} attempt(s)`);
      if (!record.anchor) throw giveUp('the first attempt spends no input of the payer, so it is never rebuilt');
      const wait = backoffMs(policy, n, policy.random());
      if (this.#now() + wait >= deadline) throw giveUp('the retry deadline passed');
      done.push(current);
      await policy.sleep(wait);
      try {
        record = await this.#rebuild(record, n + 1, { ...s, offer: s.offer, pr: s.pr, caps: s.caps, reasons: s.reasons });
      } catch (e) {
        // the rebuild was refused (the anchor is spent, a limit, approve, the preflight): nothing more was sent
        if (e instanceof KobX402Error) e.attempts = [...done];
        throw e;
      }
      sends = 0;
      resends = 0;
    }
  }

  /**
   * The next attempt of a payment whose attempt `prev` failed: a fresh chain context and quote, the payer's limits and
   * `approve` again, the preflight, a fresh payment id, and the payment's anchor spent (or nothing is sent).
   */
  async #rebuild(
    prev: ArtifactRecord,
    attempt: number,
    s: { url: string; method: string; offer: SelectedOffer; reasons: PaymentApproval['reasons']; pr: PaymentRequired; caps: PayerCapabilities },
  ): Promise<ArtifactRecord> {
    const paymentId = (this.#o.newPaymentId ?? newId)();
    if (!/^[A-Za-z0-9_-]{16,128}$/.test(paymentId)) throw new KobX402Error('bad_request', 'payment id must match ^[A-Za-z0-9_-]{16,128}$');
    const extensions = paymentIdentifierExtensions(s.pr, paymentId);
    const { built } = await this.#buildPayment(s.offer, prev.requestHash, paymentId, extensions, s.pr, prev.anchor);
    if (!(await this.#authorized(s.offer, [...s.reasons], built, s.caps, s.url, s.method, attempt, prev.paymentId))) {
      throw new KobX402Error('spend_not_authorized', `the retry of ${prev.paymentId} (attempt ${attempt}) is not authorised at its new cost; nothing more was paid`, {
        paymentId: prev.paymentId,
        transactionId: prev.transactionId,
      });
    }
    const record = this.#record(s.offer, paymentId, prev.requestHash, built, s.url, s.method, { attempt, replaces: prev.paymentId, ...(prev.anchor ? { anchor: prev.anchor } : {}) });
    await this.#store.save(record);
    return record;
  }

  #record(offer: SelectedOffer, paymentId: string, requestHash: string, built: PayResult, url: string, method: string, extra: Partial<ArtifactRecord>): ArtifactRecord {
    const now = this.#now();
    return {
      paymentId,
      createdAtMs: now,
      updatedAtMs: now,
      status: 'signed',
      url,
      method,
      requestHash,
      transactionId: built.transactionId,
      kind: offer.kind,
      amount: offer.requirements.amount,
      asset: offer.requirements.asset,
      network: offer.requirements.network,
      expiresAtMs: built.expiresAtMs,
      consumed: built.consumed,
      paymentPayload: built.paymentPayload,
      ...extra,
    };
  }

  /**
   * Whether a built payment may be paid: the KAS it takes held to the payer's KAS ceiling; without an explicit ceiling for
   * its merchant asset (or for KAS), `approve` must say yes to its cost. Asked for every attempt, a rebuilt one included.
   */
  async #authorized(
    offer: SelectedOffer,
    reasons: PaymentApproval['reasons'],
    built: PayResult,
    caps: PayerCapabilities,
    url: string,
    method: string,
    attempt: number,
    replaces?: string,
  ): Promise<boolean> {
    // the KAS the payment takes (amount, carrier, fee; a KAS-paid swap's whole cost) is held to the payer's KAS ceiling
    const cost = paymentCost(offer, built);
    const kasCap = this.#kasCeiling(caps);
    if (kasCap !== undefined && BigInt(cost.kasSpent) > BigInt(kasCap)) return false;
    if (kasCap === undefined && BigInt(cost.kasSpent) > 0n && !reasons.includes('no_spend_cap')) reasons.push('no_kas_cap');
    if (reasons.length === 0) return true;
    if (!this.#o.approve) return false;
    const req: PaymentApproval = { offer, url, method, reasons, cost, attempt };
    if (replaces !== undefined) req.replaces = replaces;
    return (await this.#o.approve(req)) === true;
  }

  /** Runs `f` after every earlier call holding `key` finished. */
  async #exclusive<T>(key: string, f: () => Promise<T>): Promise<T> {
    const prev = this.#locks.get(key) ?? Promise.resolve();
    let release!: () => void;
    const mine = new Promise<void>((r) => (release = r));
    const tail = prev.then(() => mine);
    this.#locks.set(key, tail);
    await prev;
    try {
      return await f();
    } finally {
      release();
      if (this.#locks.get(key) === tail) this.#locks.delete(key);
    }
  }

  /**
   * One send of a stored artifact. The outcome is classified for the retry: `resend` (unknown: no answer, a 5xx, a 2xx
   * without a settlement, `settlement_pending`), `rebuild` (the facilitator failed it and released its inputs) or `stop`.
   */
  async #discloseOnce(
    record: ArtifactRecord,
    kind: OfferKind,
    baseInit: RequestInit,
  ): Promise<{ ok: true; result: PaidFetchResult } | { ok: false; error: KobX402Error; step: RetryStep; outcome: string }> {
    const { paymentId, transactionId, url: expectedHref } = record;
    const headers = new Headers(baseInit.headers);
    headers.set(HEADER_PAYMENT_SIGNATURE, encodePaymentSignature(record.paymentPayload));
    const retryInit: RequestInit = { ...baseInit, headers };

    let second: Response;
    try {
      second = await this.#send(expectedHref, retryInit, 'paid retry', true);
    } catch (e) {
      if (e instanceof KobX402Error && e.code === 'redirect') {
        // never sent again to a merchant that redirects a paid request
        await this.#mark(paymentId, { status: 'pending', note: 'the paid retry was redirected; the merchant may hold the payment' });
        return { ok: false, error: pendingError(paymentId, transactionId, 'the paid request was redirected; the payment may have been received', e), step: 'stop', outcome: 'redirect' };
      }
      await this.#mark(paymentId, { status: 'pending', note: 'the paid retry failed before a response; the merchant may hold the payment' });
      return { ok: false, error: pendingError(paymentId, transactionId, 'the paid request failed; the payment may have been received', e), step: 'resend', outcome: 'unknown' };
    }

    if (second.status === 402) {
      const failure = await this.#readFailure(second);
      await this.#mark(paymentId, { status: 'rejected', failure });
      const error = new KobX402Error('payment_failed', `payment rejected: ${failure.diagnostic ?? 'unknown'}: ${failure.message ?? ''}`, {
        diagnostic: failure.diagnostic,
        retryable: failure.retryable === true,
        details: failure.details,
        paymentId,
        transactionId,
        status: 402,
      });
      return { ok: false, error, step: classifyFailure(failure.diagnostic, failure.retryable === true), outcome: failure.diagnostic ?? 'rejected' };
    }

    const header = second.headers.get(HEADER_PAYMENT_RESPONSE);
    if (second.status < 200 || second.status >= 300 || !header) {
      await this.#mark(paymentId, { status: 'pending', note: `paid retry answered ${second.status} without a settlement` });
      const k = await this.#readKaspa(second);
      const error = pendingError(paymentId, transactionId, `paid retry answered ${second.status} without a valid PAYMENT-RESPONSE${k.diagnostic ? ` (${k.diagnostic})` : ''}`, undefined, second.status);
      if (k.diagnostic) error.diagnostic = k.diagnostic;
      error.retryable = k.retryable === true;
      return { ok: false, error, step: classifyStatus(second.status, k.diagnostic, k.retryable === true), outcome: k.diagnostic ?? `http_${second.status}` };
    }
    let settlement: SettlementResponse;
    try {
      settlement = decodePaymentResponse(header);
      verifySettlement(settlement, record.amount, record.network, transactionId);
    } catch (e) {
      await this.#mark(paymentId, { status: 'pending', note: 'PAYMENT-RESPONSE did not verify' });
      const error = new KobX402Error('invalid_settlement', `PAYMENT-RESPONSE rejected: ${(e as Error).message}`, {
        cause: e,
        paymentId,
        transactionId,
        status: second.status,
      });
      return { ok: false, error, step: 'stop', outcome: 'invalid_settlement' };
    }
    await this.#mark(paymentId, { status: 'settled' });
    return {
      ok: true,
      result: {
        response: second,
        payment: { paymentId, transactionId, amount: record.amount, asset: record.asset, network: record.network, kind, settlement, attempts: 1, resends: 0, superseded: [] },
      },
    };
  }

  /** `extensions.kaspa` of a JSON error body (409 / 5xx of a paywall), bounded; empty when there is none. */
  async #readKaspa(res: Response): Promise<{ diagnostic?: string; retryable?: boolean }> {
    try {
      const k = (JSON.parse(await readText(res)) as { extensions?: { kaspa?: { diagnostic?: unknown; retryable?: unknown } } })?.extensions?.kaspa;
      const out: { diagnostic?: string; retryable?: boolean } = {};
      if (typeof k?.diagnostic === 'string') out.diagnostic = k.diagnostic;
      if (typeof k?.retryable === 'boolean') out.retryable = k.retryable;
      return out;
    } catch {
      return {};
    }
  }

  /**
   * Invalidates a signed payment by spending one of its P2PK inputs back to the payer. Only meaningful while the
   * payment is not accepted; a transaction that is already accepted cannot be revoked (its inputs are spent). Wallet mode
   * revokes kcc20 and swap-and-pay payments (the wallet signs the self-spend); a native payment's revoke needs `privateKeys`.
   */
  async revoke(paymentId: string): Promise<{ transactionId: string; transaction: string; submitted: boolean; spent?: { txid: string; index: number } }> {
    const rec = await this.#store.load(paymentId);
    if (!rec) throw new KobX402Error('revoke_failed', `no artifact ${paymentId}`, { paymentId });
    if (rec.status === 'settled' || rec.status === 'revoked' || rec.status === 'superseded') {
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
    if (!this.#o.submit) return { transactionId: r.transactionId, transaction: r.transaction, submitted: false, spent: r.spent };
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
    return { transactionId: r.transactionId, transaction: r.transaction, submitted: true, spent: r.spent };
  }

  // ------------------------------------------------------------------------------------------------ internals

  /**
   * The most KAS one payment may take from the payer (amount, carrier, fee together): `maxPay.KAS` (or `maxPayAmount`)
   * when set, else `capabilities.maxAmount.KAS`.
   */
  #kasCeiling(caps: PayerCapabilities): string | undefined {
    const m = caps.maxAmount?.KAS;
    return this.#payBound('KAS') ?? (m === undefined ? undefined : m.toString());
  }

  /** The payer's bound on one payment's spend of `asset` (a swap's pay asset), counted in that asset's own units. */
  #payBound(asset: string): string | undefined {
    return this.#o.maxPay?.[asset] ?? (asset === 'KAS' ? this.#o.maxPayAmount : undefined);
  }

  /**
   * The ranked offers that may be paid, best first, with what keeps each from being paid without asking. A swap needs a
   * `maxPay` bound for the pay asset it spends (the first of its payable assets that has one) and is left out without
   * one: `approve` cannot stand in for the bound the builder enforces. An offer whose merchant asset has no
   * `capabilities.maxAmount` ceiling needs `approve` (`no_spend_cap`).
   */
  #candidates(ranked: SelectedOffer[], caps: PayerCapabilities): { offer: SelectedOffer; reasons: PaymentApproval['reasons'] }[] {
    const out: { offer: SelectedOffer; reasons: PaymentApproval['reasons'] }[] = [];
    for (const candidate of ranked) {
      let offer = candidate;
      if (offer.kind === 'swap') {
        const bounded = payableAssets(offer, caps).find((a) => this.#payBound(a) !== undefined);
        if (bounded === undefined) continue;
        offer = { ...offer, payAsset: bounded };
      }
      const reasons: PaymentApproval['reasons'] = [];
      if (caps.maxAmount?.[offer.asset] === undefined) reasons.push('no_spend_cap');
      out.push({ offer, reasons });
    }
    return out;
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
    // Attempts of one payment share its anchor input: the revoke of one that spends an input of another invalidates that
    // one as well, and a second revoke of the same input could never be submitted.
    const revoked = new Map<string, string>();
    for (const r of live) {
      const by = r.consumed.map(outpointKey).find((k) => revoked.has(k));
      if (by !== undefined) {
        await this.#mark(r.paymentId, { status: 'revoked', revokeTransactionId: revoked.get(by) as string, note: `input ${by} spent by the revoke of another attempt` });
        continue;
      }
      let rv: Awaited<ReturnType<KobX402Client['revoke']>>;
      try {
        rv = await this.revoke(r.paymentId);
      } catch (e) {
        throw new KobX402Error('payment_in_flight', `the earlier payment ${r.paymentId} could not be revoked: ${(e as Error).message}`, { cause: e, paymentId: r.paymentId });
      }
      if (!rv.submitted) throw new KobX402Error('payment_in_flight', `the revoke of ${r.paymentId} was built but not submitted (configure submit)`, { paymentId: r.paymentId });
      if (rv.spent) revoked.set(outpointKey(rv.spent), rv.transactionId);
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

  /**
   * Builds, signs and preflights a payment from a fresh chain context (a fresh quote for a swap). With `anchor` (a retry):
   * the anchor must still be one of the payer's unspent outputs (else an earlier attempt may have been accepted: nothing is
   * built), and the payment must spend it; when the builder's coin choice leaves it out, the build is repeated once with the
   * payer's outputs that outrank it set aside, and a payment that still does not spend it is refused (never sent).
   */
  async #buildPayment(
    offer: SelectedOffer,
    requestHash: string,
    paymentId: string,
    extensions: NonNullable<PaymentPayload['extensions']>,
    pr: PaymentRequired,
    anchor?: string,
  ): Promise<{ built: PayResult; payerOwned: Set<string> }> {
    const o = this.#o;
    const q: ChainContextQuery = { payerAddress: o.payerAddress, network: o.network, kind: offer.kind, asset: offer.asset, amount: offer.requirements.amount };
    if (offer.payAsset) q.payAsset = offer.payAsset;
    const ctx = await o.context.load(q);
    const payerOwned = new Set([...ctx.utxos.map(outpointKey), ...collectOutpoints(ctx.tokenUtxos).map(outpointKey)]);
    if (anchor === undefined) return { built: await this.#buildFrom(ctx, offer, requestHash, paymentId, extensions, pr), payerOwned };
    if (!payerOwned.has(anchor)) {
      throw new KobX402Error(
        'payment_pending',
        `the payment's anchor input ${anchor} is no longer an unspent output of the payer: an earlier attempt may have been accepted, so the payment is not rebuilt (resume or reconcile the earlier attempt)`,
        { diagnostic: 'retry_anchor_spent' },
      );
    }
    const spends = (b: PayResult): boolean => b.consumed.some((c) => outpointKey(c) === anchor);
    let built = await this.#buildFrom(ctx, offer, requestHash, paymentId, extensions, pr);
    if (spends(built)) return { built, payerOwned };
    const narrowed = narrowToAnchor(ctx, anchor);
    if (narrowed) {
      try {
        built = await this.#buildFrom(narrowed, offer, requestHash, paymentId, extensions, pr);
      } catch (e) {
        throw new KobX402Error('payment_failed', `the retry cannot be built around the payment's anchor input ${anchor}: ${(e as Error).message}`, {
          cause: e,
          diagnostic: 'retry_unanchored',
          retryable: true,
          paymentId,
        });
      }
      if (spends(built)) return { built, payerOwned };
    }
    throw new KobX402Error('payment_failed', `the rebuilt payment does not spend the payment's anchor input ${anchor}; it was not sent`, {
      diagnostic: 'retry_unanchored',
      retryable: true,
      paymentId,
    });
  }

  async #buildFrom(
    ctx: ChainContext,
    offer: SelectedOffer,
    requestHash: string,
    paymentId: string,
    extensions: NonNullable<PaymentPayload['extensions']>,
    pr: PaymentRequired,
  ): Promise<PayResult> {
    const o = this.#o;
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
    const bound = offer.kind === 'swap' && offer.payAsset ? this.#payBound(offer.payAsset) : undefined;
    if (bound !== undefined) req.maxPayAmount = bound;
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
    if (bound !== undefined && offer.payAsset) {
      pf.maxPay = bound;
      pf.maxPayAsset = offer.payAsset;
    }
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

/** What a built payment costs the payer (what `approve` is shown). */
export function paymentCost(offer: SelectedOffer, built: PayResult): PaymentCost {
  const fee = BigInt(built.feeSompi);
  const carrier = BigInt(offer.requirements.extra.token?.carrier ?? '0');
  let kas: bigint;
  if (offer.kind === 'native') kas = BigInt(offer.requirements.amount) + fee;
  else if (offer.kind === 'kcc20') kas = carrier + fee;
  else if (built.kasSpent !== undefined) kas = BigInt(built.kasSpent);
  else if (offer.payAsset === 'KAS' && built.payerSpent !== undefined) kas = BigInt(built.payerSpent);
  else kas = carrier + fee;
  const cost: PaymentCost = {
    asset: offer.requirements.asset,
    amount: offer.requirements.amount,
    feeSompi: built.feeSompi,
    carrierSompi: carrier.toString(),
    kasSpent: kas.toString(),
  };
  if (offer.kind === 'swap') {
    if (offer.payAsset) cost.payAsset = offer.payAsset;
    if (built.payerSpent !== undefined) cost.payerSpent = built.payerSpent;
  }
  return cost;
}

/**
 * The chain context without the payer's outputs that a largest-first coin choice would take before the anchor: in the anchor's
 * own list (KAS coins, or the token UTXOs of its covenant), every other output at least as large. `undefined` when the anchor is
 * in neither list.
 */
function narrowToAnchor(ctx: ChainContext, anchor: string): ChainContext | undefined {
  const kas = ctx.utxos.find((u) => outpointKey(u) === anchor);
  if (kas) {
    const a = BigInt(kas.amount);
    return { ...ctx, utxos: ctx.utxos.filter((u) => u === kas || BigInt(u.amount) < a) };
  }
  const tokens = ctx.tokenUtxos ?? [];
  const keyOf = (t: TokenUtxoJson): string => outpointKey({ txid: String(t.transactionId ?? ''), index: Number(t.index) });
  const tok = tokens.find((t) => keyOf(t) === anchor);
  if (!tok) return undefined;
  const units = (t: TokenUtxoJson): bigint => {
    const st = t.state as { amount?: unknown } | undefined;
    try {
      return BigInt(String(st?.amount ?? t.amount ?? 0));
    } catch {
      return 0n;
    }
  };
  const a = units(tok);
  return { ...ctx, tokenUtxos: tokens.filter((t) => t === tok || t.covenantId !== tok.covenantId || units(t) < a) };
}

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
