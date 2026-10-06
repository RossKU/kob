// Invoices (`kob-invoice-v1`): the x402 requirements of one sale, the merchant's reference and an expiry, served by the
// facilitator at `GET /invoices/<id>` (the URL a QR code carries; no QR rendering here).
//
// Merchant: `newInvoice` builds one, `InvoiceClient.create` registers it (API key), `status` / `awaitPaid` read its
// invoice-level status. Payer: `fetchInvoice` loads it from its URL and checks that it hashes to the id in the URL (the
// URL is self-verifying), then pays one of its `accepts` (any profile; `payInvoiceWithIntent` for an intent route) with
// `InvoiceClient.pay` (`POST /invoices/<id>/pay`, no API key). One invoice is paid once: a duplicate or late payment is
// refused before broadcast (`invoice_paid` / `invoice_expired`) and kept as evidence by the facilitator.
//
// A KAS-only invoice can also be paid by any wallet through the `kaspa:<address>?amount=<KAS>` URI (`kaspaUri`); the
// facilitator does not see such a payment, so the merchant watches its address instead of the invoice status.

import { canonicalHashHex } from './canonical.ts';
import { KobX402Error } from './errors.ts';
import type { FetchLike } from './client.ts';
import type { KobWasm } from './wasm.ts';
import { assertSecureUrl } from './url-policy.ts';
import { ASSET_KAS, INVOICE_VERSION, X402_VERSION } from './types.ts';
import type { Invoice, InvoiceStatus, NetworkId, PaymentPayload, PaymentRequirements, SettlementResponse } from './types.ts';

const MAX_RESPONSE_BYTES = 1024 * 1024;
const HEX64 = /^[0-9a-f]{64}$/;

/** `YYYY-MM-DDTHH:MM:SS.mmmZ` of unix milliseconds (the binding's timestamp form). */
export function isoMs(ms: number): string {
  return new Date(ms).toISOString();
}

/** A new invoice (`expiresAtMs` unix ms). */
export function newInvoice(p: { network: NetworkId; reference: string; expiresAtMs: number; memo?: string; accepts: PaymentRequirements[] }): Invoice {
  const inv: Invoice = {
    x402Version: X402_VERSION,
    invoiceVersion: INVOICE_VERSION,
    network: p.network,
    reference: p.reference,
    expiresAt: isoMs(p.expiresAtMs),
    accepts: p.accepts,
  };
  if (p.memo !== undefined) inv.memo = p.memo;
  return inv;
}

/** The invoice id: SHA-256 of its canonical JSON (equal to the Rust `Invoice::id`). */
export function invoiceId(inv: Invoice): string {
  return canonicalHashHex(inv);
}

/** The id at the end of an invoice URL (`.../invoices/<id>`). */
export function invoiceIdFromUrl(url: string): string {
  const m = /\/invoices\/([0-9a-fA-F]{64})\/?$/.exec(new URL(url).pathname);
  if (!m) throw new KobX402Error('bad_request', `not an invoice URL: ${url}`);
  return m[1]!.toLowerCase();
}

/** The `kaspa:` payment URI of a KAS invoice (its first plain `standard-native` entry), or `null`. */
export function kaspaUri(inv: Invoice): string | null {
  const r = inv.accepts.find((a) => a.asset === ASSET_KAS && a.extra?.route === undefined && a.extra?.profile === 'standard-native');
  if (!r) return null;
  const sompi = BigInt(r.amount);
  const whole = sompi / 100_000_000n;
  const frac = sompi % 100_000_000n;
  const amount = frac === 0n ? whole.toString() : `${whole}.${frac.toString().padStart(8, '0').replace(/0+$/, '')}`;
  return `${r.payTo}?amount=${amount}`;
}

export interface InvoiceClientOptions {
  /** The facilitator's base URL. */
  url: string;
  /** Merchant API key (`create` only). */
  apiKey?: string;
  timeoutMs?: number;
  /** `pay` observes the settlement: give it the facilitator's settle wait and some margin. Default 60 s. */
  payTimeoutMs?: number;
  fetch?: FetchLike;
  allowInsecureHttp?: boolean;
}

/** `POST /invoices` answer. */
export interface RegisteredInvoice {
  id: string;
  /** Where the invoice is served (absolute when the facilitator has a public URL). */
  url: string;
  invoice: Invoice;
  /** False when the same invoice was registered before (idempotent). */
  created: boolean;
}

/** The facilitator's invoice endpoints. */
export class InvoiceClient {
  #o: InvoiceClientOptions;
  #base: string;
  #fetch: FetchLike;

  constructor(options: InvoiceClientOptions) {
    assertSecureUrl(options.url, 'the facilitator URL', options.allowInsecureHttp === true);
    this.#o = options;
    this.#base = options.url.replace(/\/+$/, '');
    const f = options.fetch ?? (globalThis.fetch as FetchLike | undefined);
    if (!f) throw new KobX402Error('unsupported', 'no fetch implementation available');
    this.#fetch = f;
  }

  /** The URL of an invoice on this facilitator (what a QR code carries). */
  urlOf(id: string): string {
    return `${this.#base}/invoices/${id}`;
  }

  /** Merchant: registers an invoice (API key). Idempotent: the same invoice returns the same id. */
  async create(inv: Invoice): Promise<RegisteredInvoice> {
    if (!this.#o.apiKey) throw new KobX402Error('bad_request', 'registering an invoice needs the merchant API key');
    const r = await this.#call<RegisteredInvoice>('POST', '/invoices', inv, true);
    if (r.id !== invoiceId(inv)) throw new KobX402Error('facilitator', 'the facilitator registered another invoice id than this invoice hashes to');
    return r;
  }

  /** The invoice of `id`, checked: it must hash to `id`. */
  async get(id: string): Promise<Invoice> {
    const inv = await this.#call<Invoice>('GET', `/invoices/${id}`);
    if (invoiceId(inv) !== id.toLowerCase()) throw new KobX402Error('facilitator', 'the served invoice does not hash to its id');
    return inv;
  }

  /** Invoice-level status (a read, never an await). */
  status(id: string): Promise<InvoiceStatus> {
    return this.#call<InvoiceStatus>('GET', `/invoices/${id}/status`);
  }

  /**
   * Polls the status until the invoice is `paid` or `expired` (or `timeoutMs` passes: the last status). A `failed`
   * status keeps polling: the invoice can still be paid until it expires.
   */
  async awaitPaid(id: string, o: { timeoutMs?: number; intervalMs?: number; signal?: AbortSignal } = {}): Promise<InvoiceStatus> {
    const until = Date.now() + (o.timeoutMs ?? 120_000);
    for (;;) {
      const s = await this.status(id);
      if (s.status === 'paid' || s.status === 'expired' || Date.now() >= until || o.signal?.aborted) return s;
      await new Promise((r) => setTimeout(r, o.intervalMs ?? 1_000));
    }
  }

  /** Payer: settles one payment of the invoice (no API key). Returns the settlement whatever its outcome. */
  async pay(id: string, payload: PaymentPayload): Promise<SettlementResponse> {
    const r = await this.#call<SettlementResponse>('POST', `/invoices/${id}/pay`, payload, false, this.#o.payTimeoutMs ?? 60_000);
    if (r === null || typeof r !== 'object' || typeof r.success !== 'boolean') {
      throw new KobX402Error('facilitator', 'the facilitator answered the payment with something that is not a settlement');
    }
    return r;
  }

  async #call<T>(method: string, path: string, body?: unknown, keyed = false, timeoutMs?: number): Promise<T> {
    const headers: Record<string, string> = { accept: 'application/json' };
    if (body !== undefined) headers['content-type'] = 'application/json';
    if (keyed && this.#o.apiKey) headers.authorization = `Bearer ${this.#o.apiKey}`;
    const init: RequestInit = { method, headers, redirect: 'error', signal: AbortSignal.timeout(timeoutMs ?? this.#o.timeoutMs ?? 10_000) };
    if (body !== undefined) init.body = JSON.stringify(body);
    let res: Response;
    try {
      res = await this.#fetch(this.#base + path, init);
    } catch (e) {
      throw new KobX402Error('facilitator', `${method} ${path}: the facilitator is unreachable: ${(e as Error).message}`, { cause: e });
    }
    const text = await res.text();
    if (text.length > MAX_RESPONSE_BYTES) throw new KobX402Error('facilitator', 'facilitator response too large');
    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch {
      throw new KobX402Error('facilitator', `${method} ${path}: HTTP ${res.status} with a non-JSON body`, { status: res.status });
    }
    const settlement = json !== null && typeof json === 'object' && typeof (json as { success?: unknown }).success === 'boolean';
    if (res.ok || settlement) return json as T;
    const kaspa = (json as { extensions?: { kaspa?: { diagnostic?: string; retryable?: boolean; message?: string } } }).extensions?.kaspa;
    const init2: ConstructorParameters<typeof KobX402Error>[2] = { status: res.status, details: json };
    if (kaspa?.diagnostic) init2.diagnostic = kaspa.diagnostic;
    if (kaspa?.retryable !== undefined) init2.retryable = kaspa.retryable;
    throw new KobX402Error('facilitator', `${method} ${path}: HTTP ${res.status}${kaspa?.message ? `: ${kaspa.message}` : ''}`, init2);
  }
}

/** A fetched, checked invoice. */
export interface FetchedInvoice {
  invoice: Invoice;
  id: string;
  /** Where to pay it (`POST`). */
  payUrl: string;
  statusUrl: string;
  /** The `kaspa:` fallback of a KAS invoice. */
  kaspaUri: string | null;
  expiresAtMs: number;
}

/**
 * Payer: loads an invoice from its URL (`.../invoices/<id>`) and checks it: it hashes to the id of the URL, it is an
 * invoice of this version, unexpired at `nowMs` (with the Rust checks when `wasm` provides them).
 */
export async function fetchInvoice(
  url: string,
  o: { nowMs: number; wasm?: KobWasm; fetch?: FetchLike; allowInsecureHttp?: boolean; timeoutMs?: number },
): Promise<FetchedInvoice> {
  assertSecureUrl(url, 'the invoice URL', o.allowInsecureHttp === true);
  const id = invoiceIdFromUrl(url);
  const f = o.fetch ?? (globalThis.fetch as FetchLike | undefined);
  if (!f) throw new KobX402Error('unsupported', 'no fetch implementation available');
  const res = await f(url, { method: 'GET', headers: { accept: 'application/json' }, redirect: 'error', signal: AbortSignal.timeout(o.timeoutMs ?? 10_000) });
  const text = await res.text();
  if (!res.ok) throw new KobX402Error('facilitator', `GET ${url}: HTTP ${res.status}`, { status: res.status });
  if (text.length > MAX_RESPONSE_BYTES) throw new KobX402Error('facilitator', 'invoice too large');
  const invoice = JSON.parse(text) as Invoice;
  if (invoiceId(invoice) !== id) throw new KobX402Error('invalid_payment_required', 'the invoice does not hash to the id of its URL');
  if (invoice.invoiceVersion !== INVOICE_VERSION || invoice.x402Version !== X402_VERSION) {
    throw new KobX402Error('invalid_payment_required', 'not a kob-invoice-v1 invoice');
  }
  let expiresAtMs = Date.parse(invoice.expiresAt);
  let uri = kaspaUri(invoice);
  if (o.wasm?.checkInvoice) {
    const c = o.wasm.checkInvoice(invoice, id, o.nowMs);
    expiresAtMs = c.expiresAtMs;
    uri = c.kaspaUri;
  } else if (!(expiresAtMs > o.nowMs)) {
    throw new KobX402Error('invalid_payment_required', 'the invoice has expired', { diagnostic: 'invoice_expired' });
  }
  const base = url.replace(/\/+$/, '');
  return { invoice, id, payUrl: `${base}/pay`, statusUrl: `${base}/status`, kaspaUri: uri, expiresAtMs };
}

/**
 * The lifetime of an intent paying `inv` through `requirements`, created at `nowMs`: the wanted lifetime (default 5 min),
 * at most the offer's `maxTimeoutSeconds`, and never past the invoice's expiry. The authorization expires, and the
 * router intent's `deadline` (from which anyone may expire the intent and return the payer's funds) lies, at
 * `nowMs + lifetime`.
 */
export function intentLifetimeMs(inv: FetchedInvoice, requirements: { maxTimeoutSeconds: number }, nowMs: number, wantedMs?: number): number {
  const left = inv.expiresAtMs - nowMs;
  if (!(left > 0)) throw new KobX402Error('invalid_payment_required', 'the invoice has expired', { diagnostic: 'invoice_expired' });
  return Math.min(wantedMs ?? 300_000, requirements.maxTimeoutSeconds * 1000, left);
}

/** True when a hex string is an invoice id. */
export function isInvoiceId(s: string): boolean {
  return HEX64.test(s);
}
