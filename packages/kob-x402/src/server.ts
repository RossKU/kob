// Resource-server paywall for the Kaspa x402 `exact` binding.
//
//   unpaid request        -> 402 + PAYMENT-REQUIRED (offers built from configuration through KobWasm)
//   PAYMENT-SIGNATURE     -> decode; `accepted` must equal one of OUR offers (canonical JSON equality);
//                            `payment-identifier` id required; the transaction must declare its id (safe JSON `id`);
//                            requestHash recomputed independently from the
//                            request we actually received and compared with the payload's; POST /settle to the
//                            facilitator with that requestHash; on success serve the resource with PAYMENT-RESPONSE
//                            (only after the settlement shows OUR offer's amount and network); on failure return a
//                            corrective 402 carrying the diagnostic in `extensions.kaspa`.
//
// Idempotency: a payment id is remembered together with the transaction it was settled with. A request repeating
// that id is answered from memory only when it carries the same transaction; any other transaction under a
// remembered id is refused (`kaspa_payment_identifier_conflict`) and never reaches the facilitator. A settlement
// is served only when its transaction id is the one the request's transaction declares.
//
// One settled payment is served to the request that carries its payment id, never to another id: a broadcast
// transaction is public, its payment id is not. The same transaction under another id is refused
// (`kaspa_payment_identifier_conflict`), by this paywall while it remembers the transaction and by the KOB facilitator
// (which binds the transaction to its id in its durable ledger) after a restart. The payer's own retry (same id, same
// transaction) is served again with `replayed: true`: from memory, or from the facilitator's `extensions.kob.replayed`
// mark, which it records durably, so a restarted paywall never hands the handler the same payment as a new one.
//
// Framework-agnostic core: `paywall.handle(request: Request): Promise<Response>`; `toNodeListener` adapts it to
// node:http. The merchant's own resource is a `handler(request, paid)` that only runs after settlement.

import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import type { IncomingMessage, Server, ServerResponse } from 'node:http';
import { canonicalJson, httpRequestHash, normalizeBody, normalizeMethod, normalizeUrl, requirementsHash } from './canonical.ts';
import { KobX402Error } from './errors.ts';
import { FacilitatorClient } from './facilitator-client.ts';
import type { Facilitator, FacilitatorOptions } from './facilitator-client.ts';
import { decodePaymentSignature, encodePaymentRequired, encodePaymentResponse } from './headers.ts';
import { isIntentOffer } from './intent.ts';
import { buildOffers, isRecord } from './offers.ts';
import type { KobWasm } from './wasm.ts';
import {
  HEADER_PAYMENT_REQUIRED,
  HEADER_PAYMENT_RESPONSE,
  HEADER_PAYMENT_SIGNATURE,
  PAYMENT_IDENTIFIER_KEY,
  PAYLOAD_EXACT_TX,
  X402_VERSION,
} from './types.ts';
import type {
  Finality,
  KaspaFailure,
  NetworkId,
  OfferSpec,
  PaymentPayload,
  PaymentRequired,
  PaymentRequirements,
  Resource,
  SettlementResponse,
} from './types.ts';

const HEX64 = /^[0-9a-f]{64}$/;
const ID_RE = /^[A-Za-z0-9_-]{16,128}$/;
/** JSON schema advertised with the `payment-identifier` extension. */
const PAYMENT_ID_SCHEMA = {
  $schema: 'https://json-schema.org/draft/2020-12/schema',
  type: 'object',
  required: ['required'],
  properties: { required: { type: 'boolean' }, id: { type: 'string', minLength: 16, maxLength: 128, pattern: '^[A-Za-z0-9_-]+$' } },
  additionalProperties: true,
};

export interface PaidContext {
  paymentId: string;
  requestHash: string;
  /**
   * Recomputed transaction id from the facilitator's settlement: the transaction that paid the merchant (for an intent
   * payment the execution, not the creation the request carried).
   */
  transactionId: string;
  amount: string;
  asset: string;
  network: string;
  payer?: string;
  settlement: SettlementResponse;
  /** The request URL as hashed (public origin applied). */
  url: string;
  method: string;
  /** The request body bytes (the `Request` passed to the handler has its body consumed). */
  body: Uint8Array;
  /**
   * True when this payment was already answered (the payer's retry of the same payment id and transaction, from this
   * paywall's memory or from the facilitator's durable `extensions.kob.replayed` mark): the handler runs again,
   * nothing is charged, and the handler must not count it as a new payment.
   */
  replayed: boolean;
}

export type PaywallHandler = (request: Request, paid: PaidContext) => Response | Promise<Response>;

export interface PaywallConfig {
  wasm: KobWasm;
  network: NetworkId;
  /** Merchant address (Schnorr P2PK for token profiles). */
  payTo: string;
  offers: OfferSpec[] | ((request: Request) => OfferSpec[]);
  facilitator: FacilitatorOptions | Facilitator;
  /** The protected resource; runs only after a verified settlement. */
  handler: PaywallHandler;
  resource?: { description?: string; mimeType?: string };
  /** Public base URL (behind a proxy): the origin the payer requested. Default: the request's own URL. */
  publicUrl?: string;
  maxTimeoutSeconds?: number;
  /**
   * The finality the offers ask for and a settlement must show (`extensions.kaspa.finality`) before the resource is served. Default
   * `confirmed`: the transaction is in the virtual chain and `confirmationsDaa` deeper (the facilitator's setting, about 10 s at its
   * default 100). `accepted` serves at depth 0, a few seconds sooner, while a payer can still race a conflicting spend through
   * another node; choose it only for resources whose loss that risk covers.
   */
  finality?: Finality;
  maxBodyBytes?: number;
  now?: () => number;
  /** Max remembered settled payment ids (oldest evicted). Only settled payments enter it. */
  maxLedgerEntries?: number;
  /** Max payments being settled at once (default 1000); a request beyond it is answered 503 (retryable) and not remembered. */
  maxSettling?: number;
}

export interface Paywall {
  handle(request: Request): Promise<Response>;
  /** The entries of `accepts` for a request (static configuration: the same list every time). */
  offers(request?: Request): PaymentRequirements[];
  nodeListener(options?: NodeAdapterOptions): (req: IncomingMessage, res: ServerResponse) => void;
}

interface Local {
  reason: string;
  diagnostic: string;
  message: string;
  status?: number;
}

interface LedgerEntry {
  requestHash: string;
  requirementsHash: string;
  /** The transaction id the payment declares (and the facilitator settled). */
  transactionId: string;
  /** SHA-256 of the transaction text of the request this id was first seen with. */
  transactionDigest: string;
  state: 'settling' | 'settled';
  settlement?: SettlementResponse;
  at: number;
}

function otherIdConflict(): Response {
  return json(409, {
    error: 'invalid_payload',
    extensions: { kaspa: { diagnostic: 'kaspa_payment_identifier_conflict', retryable: false, message: 'this transaction was settled under another payment id' } },
  });
}

function lc(s: unknown): string {
  return typeof s === 'string' ? s.toLowerCase() : '';
}

export function createPaywall(config: PaywallConfig): Paywall {
  const facilitator: Facilitator = 'settle' in config.facilitator ? config.facilitator : new FacilitatorClient(config.facilitator);
  const maxBody = config.maxBodyBytes ?? 1024 * 1024;
  const maxLedger = config.maxLedgerEntries ?? 10_000;
  const maxSettling = config.maxSettling ?? 1_000;
  /** Payment ids being settled now (removed when the settlement ends): kept apart from the ledger of settled payments. */
  const settling = new Map<string, LedgerEntry>();
  const now = config.now ?? Date.now;
  const ledger = new Map<string, LedgerEntry>();
  /** Settled transaction id -> the payment id it was settled under (the transaction is public, the id is not). */
  const served = new Map<string, string>();
  let staticOffers: PaymentRequirements[] | undefined;

  const build = (specs: OfferSpec[]): PaymentRequirements[] =>
    buildOffers(specs, {
      wasm: config.wasm,
      network: config.network,
      payTo: config.payTo,
      maxTimeoutSeconds: config.maxTimeoutSeconds ?? 60,
      finality: config.finality ?? 'confirmed',
    });

  const offersFor = (request?: Request): PaymentRequirements[] => {
    if (typeof config.offers !== 'function') return (staticOffers ??= build(config.offers));
    if (!request) throw new KobX402Error('bad_request', 'this paywall prices per request: pass the request');
    return build(config.offers(request));
  };

  const resourceUrl = (request: Request): string => {
    const u = new URL(request.url);
    // origin + path: a request path starting with `//` must not be read as a protocol-relative reference
    return config.publicUrl ? normalizeUrl(new URL(new URL(config.publicUrl).origin + u.pathname + u.search)) : normalizeUrl(u);
  };

  const resourceOf = (url: string): Resource => {
    const r: Resource = { url };
    if (config.resource?.description) r.description = config.resource.description;
    if (config.resource?.mimeType) r.mimeType = config.resource.mimeType;
    return r;
  };

  const challenge = (url: string, accepts: PaymentRequirements[], failure?: Local & Partial<KaspaFailure>, status = 402): Response => {
    const pr: PaymentRequired = {
      x402Version: X402_VERSION,
      resource: resourceOf(url),
      accepts,
      extensions: { [PAYMENT_IDENTIFIER_KEY]: { info: { required: true }, schema: PAYMENT_ID_SCHEMA } },
    };
    if (failure) {
      pr.error = failure.reason;
      const k: KaspaFailure = { diagnostic: failure.diagnostic, retryable: failure.retryable === true, message: failure.message };
      if (failure.details !== undefined) k.details = failure.details;
      pr.extensions = { ...pr.extensions, kaspa: k };
    }
    return new Response(JSON.stringify(pr), {
      status,
      headers: { 'content-type': 'application/json', 'cache-control': 'no-store', [HEADER_PAYMENT_REQUIRED]: encodePaymentRequired(pr) },
    });
  };

  const remember = (id: string, e: LedgerEntry): void => {
    ledger.set(id, e);
    while (ledger.size > maxLedger) {
      const oldest = ledger.keys().next().value;
      if (oldest === undefined) break;
      ledger.delete(oldest);
    }
  };

  const serve = async (request: Request, paid: PaidContext): Promise<Response> => {
    let res: Response;
    try {
      res = await config.handler(request, paid);
    } catch {
      // Paid but the resource failed: the settlement stays recorded, a retry with the same id re-runs the handler.
      res = json(500, { error: 'resource_failed', paymentId: paid.paymentId });
    }
    const headers = new Headers(res.headers);
    headers.set(HEADER_PAYMENT_RESPONSE, encodePaymentResponse(paid.settlement));
    return new Response(res.body, { status: res.status, statusText: res.statusText, headers });
  };

  async function handle(request: Request): Promise<Response> {
    const url = resourceUrl(request);
    const method = normalizeMethod(request.method);
    let accepts: PaymentRequirements[];
    try {
      accepts = offersFor(request);
    } catch {
      return json(500, { error: 'unexpected_settle_error' });
    }

    const header = request.headers.get(HEADER_PAYMENT_SIGNATURE);
    if (!header) return challenge(url, accepts);

    const declared = Number(request.headers.get('content-length') ?? '0');
    if (declared > maxBody) return json(413, { error: 'invalid_payload' });
    const body = new Uint8Array(await request.arrayBuffer());
    if (body.byteLength > maxBody) return json(413, { error: 'invalid_payload' });

    // ---- decode and bind to OUR offer
    let payload: PaymentPayload;
    try {
      payload = decodePaymentSignature(header);
    } catch (e) {
      return challenge(url, accepts, { reason: 'invalid_payload', diagnostic: 'invalid_kaspa_x402_payload', message: (e as Error).message });
    }
    const bad = shapeError(payload);
    if (bad) return challenge(url, accepts, bad);

    let offer: PaymentRequirements | undefined;
    try {
      const wanted = canonicalJson(payload.accepted);
      offer = accepts.find((o) => canonicalJson(o) === wanted);
    } catch {
      offer = undefined;
    }
    if (!offer) {
      return challenge(url, accepts, {
        reason: 'invalid_payment_requirements',
        diagnostic: 'invalid_kaspa_x402_accepted',
        message: 'accepted does not match an offer of this server',
      });
    }

    // ---- payment identifier (required for exact)
    const idExt = payload.extensions?.[PAYMENT_IDENTIFIER_KEY];
    const id = isRecord(idExt) && isRecord(idExt.info) ? (idExt.info as { id?: unknown }).id : undefined;
    if (id === undefined) {
      return challenge(url, accepts, { reason: 'invalid_payload', diagnostic: 'missing_kaspa_payment_identifier', message: 'the payment-identifier extension with an id is required' });
    }
    if (typeof id !== 'string' || !ID_RE.test(id)) {
      return challenge(url, accepts, { reason: 'invalid_payload', diagnostic: 'invalid_kaspa_payment_identifier', message: 'payment-identifier id must match ^[A-Za-z0-9_-]{16,128}$' });
    }

    // ---- the transaction the request carries: its declared id and its exact bytes identify the payment
    const declaredId = declaredTransactionId(payload.payload.transaction);
    if (declaredId === undefined) {
      return challenge(url, accepts, {
        reason: 'invalid_payload',
        diagnostic: 'invalid_kaspa_x402_payload',
        message: 'the transaction must declare its id (safe JSON `id`, 32-byte hex)',
      });
    }
    const txDigest = transactionDigest(payload.payload.transaction);

    // ---- request fingerprint, computed independently from what we received
    const reqsHash = requirementsHash(offer);
    const requestHash = httpRequestHash(method, url, normalizeBody(body), reqsHash);
    if (lc(payload.payload.requestHash) !== requestHash) {
      return challenge(url, accepts, {
        reason: 'invalid_payload',
        diagnostic: 'invalid_kaspa_x402_payload',
        message: 'requestHash does not match this request',
      });
    }

    // ---- idempotency: same id + same fingerprint + same offer + same transaction -> cached outcome; anything else conflicts
    const prior = ledger.get(id) ?? settling.get(id);
    if (prior) {
      if (prior.requestHash !== requestHash || prior.requirementsHash !== reqsHash) {
        return json(409, { error: 'invalid_payload', extensions: { kaspa: { diagnostic: 'kaspa_payment_identifier_conflict', retryable: false, message: 'payment id is bound to a different request' } } });
      }
      if (prior.transactionId !== declaredId || prior.transactionDigest !== txDigest) {
        return json(409, { error: 'invalid_payload', extensions: { kaspa: { diagnostic: 'kaspa_payment_identifier_conflict', retryable: false, message: 'payment id is bound to a different transaction' } } });
      }
      if (prior.state === 'settling') {
        return json(409, { error: 'invalid_transaction_state', extensions: { kaspa: { diagnostic: 'settlement_pending', retryable: true, message: 'this payment id is being settled' } } });
      }
      const settlement = prior.settlement as SettlementResponse;
      return serve(request, paidContext(id, requestHash, settlement, offer, url, method, body, true));
    }
    // a transaction settled under another payment id is that payment, not this request's
    const owner = served.get(declaredId);
    if (owner !== undefined && owner !== id) return otherIdConflict();
    // requests being settled are held apart from settled payments, in a bounded set: any number of well-formed requests that never
    // settle cannot push a settled payment out of the ledger (a full set answers 503, nothing is remembered)
    if (settling.size >= maxSettling) {
      return json(503, { error: 'unexpected_settle_error', extensions: { kaspa: { diagnostic: 'rate_limited', retryable: true, message: 'too many payments are being settled; retry' } } }, { 'retry-after': '1' });
    }
    settling.set(id, { requestHash, requirementsHash: reqsHash, transactionId: declaredId, transactionDigest: txDigest, state: 'settling', at: now() });

    // ---- settle through the facilitator
    let settlement: SettlementResponse;
    try {
      settlement = await facilitator.settle({
        x402Version: X402_VERSION,
        paymentPayload: payload,
        paymentRequirements: offer,
        requestHash,
        resource: resourceOf(url),
      });
    } catch {
      settling.delete(id);
      return json(503, { error: 'unexpected_settle_error', extensions: { kaspa: { diagnostic: 'node_unavailable', retryable: true, message: 'the facilitator did not answer' } } }, { 'retry-after': '2' });
    }

    if (!settlement.success) {
      settling.delete(id);
      const k = isRecord(settlement.extensions?.kaspa) ? (settlement.extensions?.kaspa as Partial<KaspaFailure>) : {};
      const diagnostic = typeof k.diagnostic === 'string' ? k.diagnostic : 'internal';
      const failure: Local & Partial<KaspaFailure> = {
        reason: settlement.errorReason ?? 'unexpected_settle_error',
        diagnostic,
        message: typeof k.message === 'string' ? k.message : 'payment was not accepted',
        retryable: k.retryable === true,
      };
      if (k.details !== undefined) failure.details = k.details;
      if (diagnostic === 'unauthorized') return json(502, { error: 'unexpected_settle_error' });
      if (diagnostic === 'rate_limited' || diagnostic === 'node_unavailable') return challenge(url, accepts, failure, 503);
      return challenge(url, accepts, failure);
    }

    // Fail closed on a success that does not show OUR offer.
    if (!HEX64.test(lc(settlement.transaction)) || settlement.amount !== offer.amount || settlement.network !== offer.network) {
      settling.delete(id);
      return json(502, { error: 'invalid_transaction_state', extensions: { kaspa: { diagnostic: 'internal', retryable: false, message: 'the facilitator settlement does not match the offer' } } });
    }
    // ... and on one that stands on less than the finality the offer asked for (`extensions.kaspa.finality`)
    if (finalityRank(settledFinality(settlement)) < finalityRank(offerFinality(offer))) {
      settling.delete(id);
      return json(502, { error: 'invalid_transaction_state', extensions: { kaspa: { diagnostic: 'internal', retryable: false, message: 'the facilitator settlement is weaker than the finality of the offer' } } });
    }
    // The transaction the payer sent declares its id (safe JSON `id`, which the facilitator checks against the recomputed id); a
    // facilitator answer for another transaction than the one in this request does not settle this payment. An intent payment
    // sends the intent's creation and is settled by the facilitator's execution: the settlement names the execution in
    // `transaction` and the creation in `extensions.kob.intent.creation`, which is what the request's transaction must be.
    const txid = settledRequestTransaction(settlement, isIntentOffer(offer));
    if (declaredId !== txid) {
      settling.delete(id);
      return json(502, { error: 'invalid_transaction_state', extensions: { kaspa: { diagnostic: 'internal', retryable: false, message: 'the facilitator settled another transaction than the one in this request' } } });
    }
    // settled meanwhile under another id (two requests racing with one transaction): served to that id only
    const owner2 = served.get(txid);
    if (owner2 !== undefined && owner2 !== id) {
      settling.delete(id);
      return otherIdConflict();
    }
    const entry: LedgerEntry = { requestHash, requirementsHash: reqsHash, transactionId: txid, transactionDigest: txDigest, state: 'settled', settlement, at: now() };
    settling.delete(id);
    remember(id, entry);
    served.set(txid, id);
    while (served.size > maxLedger) {
      const oldest = served.keys().next().value;
      if (oldest === undefined) break;
      served.delete(oldest);
    }
    // the facilitator answered this payment before (to this paywall before a restart, or to a retry it forgot)
    const replayed = isRecord(settlement.extensions?.kob) && (settlement.extensions?.kob as { replayed?: unknown }).replayed === true;
    return serve(request, paidContext(id, requestHash, settlement, offer, url, method, body, replayed));
  }

  return {
    handle,
    offers: offersFor,
    nodeListener: (options) => toNodeListener(handle, { maxBodyBytes: maxBody, ...options }),
  };
}

function paidContext(
  paymentId: string,
  requestHash: string,
  settlement: SettlementResponse,
  offer: PaymentRequirements,
  url: string,
  method: string,
  body: Uint8Array,
  replayed: boolean,
): PaidContext {
  const c: PaidContext = {
    paymentId,
    requestHash,
    transactionId: lc(settlement.transaction),
    amount: offer.amount,
    asset: offer.asset,
    network: offer.network,
    settlement,
    url,
    method,
    body,
    replayed,
  };
  if (settlement.payer) c.payer = settlement.payer;
  return c;
}

/**
 * The id of the transaction the request carried, as the settlement reports it: `transaction` for a direct payment, the
 * intent's creation (`extensions.kob.intent.creation`) for an intent payment, whose `transaction` is the execution.
 * Undefined when the settlement does not name it as a 32-byte hex id.
 */
export function settledRequestTransaction(s: SettlementResponse, intent: boolean): string | undefined {
  const kob = s.extensions?.kob;
  const creation = isRecord(kob) && isRecord(kob.intent) ? kob.intent.creation : undefined;
  const id = intent ? creation : s.transaction;
  return typeof id === 'string' && HEX64.test(lc(id)) ? lc(id) : undefined;
}

/** `mempool` < `accepted` < `confirmed`; anything else (missing) ranks lowest. */
function finalityRank(f: unknown): number {
  return f === 'confirmed' ? 3 : f === 'accepted' ? 2 : f === 'mempool' ? 1 : 0;
}
const offerFinality = (offer: PaymentRequirements): unknown => (offer.extra as { finality?: unknown } | undefined)?.finality ?? 'accepted';
const settledFinality = (s: SettlementResponse): unknown => (isRecord(s.extensions?.kaspa) ? (s.extensions?.kaspa as { finality?: unknown }).finality : undefined);

/** SHA-256 (hex) of the transaction text a payment carries. */
function transactionDigest(transaction: string): string {
  return createHash('sha256').update(transaction, 'utf8').digest('hex');
}

/** The `id` a safe-JSON transaction declares (lower-case 64 hex), or undefined when it declares none / is not JSON. */
export function declaredTransactionId(transaction: string): string | undefined {
  try {
    const id = (JSON.parse(transaction) as { id?: unknown } | null)?.id;
    return typeof id === 'string' && HEX64.test(lc(id)) ? lc(id) : undefined;
  } catch {
    return undefined;
  }
}

function shapeError(p: unknown): Local | undefined {
  if (!isRecord(p)) return { reason: 'invalid_payload', diagnostic: 'invalid_kaspa_x402_payload', message: 'payload is not an object' };
  if (p.x402Version !== X402_VERSION) return { reason: 'invalid_x402_version', diagnostic: 'invalid_kaspa_x402_payload', message: 'unsupported x402Version' };
  const inner = p.payload;
  if (!isRecord(inner) || inner.type !== PAYLOAD_EXACT_TX || typeof inner.requestHash !== 'string' || typeof inner.transaction !== 'string') {
    return { reason: 'invalid_payload', diagnostic: 'invalid_kaspa_x402_payload', message: 'payload is not an exact-transaction payload' };
  }
  if (!isRecord(p.accepted)) return { reason: 'invalid_payment_requirements', diagnostic: 'invalid_kaspa_x402_accepted', message: 'accepted is missing' };
  return undefined;
}

function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store', ...headers } });
}

// ------------------------------------------------------------------------------------------------- node:http

export interface NodeAdapterOptions {
  maxBodyBytes?: number;
  /** Take the origin from `X-Forwarded-Proto` / `X-Forwarded-Host` (only behind a trusted proxy). Prefer `publicUrl`. */
  trustProxy?: boolean;
}

/** Adapts a `Request -> Response` handler to node:http (`http.createServer(toNodeListener(paywall.handle))`). */
export function toNodeListener(
  handle: (request: Request) => Promise<Response>,
  options: NodeAdapterOptions = {},
): (req: IncomingMessage, res: ServerResponse) => void {
  const maxBody = options.maxBodyBytes ?? 1024 * 1024;
  return (req, res) => {
    void (async () => {
      try {
        const chunks: Buffer[] = [];
        let size = 0;
        for await (const c of req) {
          size += (c as Buffer).length;
          if (size > maxBody) {
            res.writeHead(413, { 'content-type': 'application/json' }).end('{"error":"invalid_payload"}');
            req.destroy();
            return;
          }
          chunks.push(c as Buffer);
        }
        const proto = options.trustProxy ? String(req.headers['x-forwarded-proto'] ?? 'http') : 'http';
        const host = options.trustProxy ? String(req.headers['x-forwarded-host'] ?? req.headers.host ?? 'localhost') : String(req.headers.host ?? 'localhost');
        const headers = new Headers();
        for (const [k, v] of Object.entries(req.headers)) {
          if (v === undefined) continue;
          if (Array.isArray(v)) for (const x of v) headers.append(k, x);
          else headers.set(k, v);
        }
        const method = req.method ?? 'GET';
        const init: RequestInit = { method, headers };
        if (method !== 'GET' && method !== 'HEAD' && size > 0) init.body = Buffer.concat(chunks);
        const response = await handle(new Request(`${proto}://${host}${req.url ?? '/'}`, init));
        const out: Record<string, string> = {};
        response.headers.forEach((v, k) => (out[k] = v));
        res.writeHead(response.status, out);
        res.end(method === 'HEAD' ? undefined : Buffer.from(await response.arrayBuffer()));
      } catch {
        if (!res.headersSent) res.writeHead(500, { 'content-type': 'application/json' });
        res.end('{"error":"unexpected_settle_error"}');
      }
    })();
  };
}

/**
 * A node:http server around a paywall. `PAYMENT-SIGNATURE` carries a whole signed transaction (a KCC-20 spend with
 * its unlocking scripts is tens of KB of base64), far above node's 16 KB default header limit, so the limit is raised.
 */
export function createNodeServer(paywall: Paywall, options: NodeAdapterOptions & { maxHeaderSize?: number } = {}): Server {
  return createServer({ maxHeaderSize: options.maxHeaderSize ?? 256 * 1024 }, paywall.nodeListener(options));
}
