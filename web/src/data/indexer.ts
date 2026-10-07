// Client of the KOB indexer read API (crates/kob-executor, `/v1`): typed REST access (`HttpIndexer`) and the WebSocket feed
// (`IndexerFeed`, `/v1/ws`). The indexer is a CONVENIENCE and an aggregator, never an authority for anything the wallet signs: the
// pre-sign screen re-derives everything from the transaction itself. So every failure here is a typed, recoverable error.
//
// Conventions of the API: JSON, snake_case, ids/keys lower-case hex, sompi as DECIMAL STRINGS (see `sompi()` for bigint), errors as
// `{"error":{"code","message"}}`, rate limiting as 429 (+ Retry-After), busy database as 503 (+ Retry-After).
import type {
  BookView, EventView, HealthView, IndexerTokenView, TokenStanding, OrderView, Page, StrayView, TokenEventView, TokenUtxoView, WsFrame,
  FillNoticeView, BookNotice, OrderNotice, ReorgNotice, HealthCursorNotice, HealthTickNotice,
  CandleInterval, CandlesView, DepthView, StatsView, TradesView, PairBookView, PairCandlesView, PairFillsView, PairSummaryView,
} from './indexer-types';
import { adaptPairCandles, adaptPairFills, adaptPairSummaries, sanitizePairBook } from './pair-api';
import type { Hex } from '../kob/types';
import { dropZeroDepthLevels, dropEmptyRows } from './book-view';
import { plainUntrusted } from '../kob/registry';

/** Largest indexer response body read (bytes); a longer one is refused, not parsed (default of `HttpIndexerOptions.maxBodyBytes`). */
export const MAX_INDEXER_BODY_BYTES = 8 * 1024 * 1024;

class BodyTooLarge extends Error {}

/** The body as text, refusing (BodyTooLarge) one longer than `max` bytes: by Content-Length up front, else while it streams. */
async function readCapped(res: Response, max: number): Promise<string> {
  const declared = Number(res.headers.get('content-length') ?? NaN);
  if (Number.isFinite(declared) && declared > max) {
    await res.body?.cancel().catch(() => undefined);
    throw new BodyTooLarge();
  }
  const reader = res.body?.getReader();
  if (!reader) {
    const text = await res.text();
    if (new TextEncoder().encode(text).length > max) throw new BodyTooLarge();
    return text;
  }
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > max) {
      await reader.cancel().catch(() => undefined);
      throw new BodyTooLarge();
    }
    chunks.push(value);
  }
  const all = new Uint8Array(total);
  let at = 0;
  for (const c of chunks) {
    all.set(c, at);
    at += c.byteLength;
  }
  return new TextDecoder().decode(all);
}

// ------------------------------------------------------------------------------------------------ errors and helpers

export type IndexerErrorKind = 'http' | 'network' | 'timeout' | 'aborted' | 'parse' | 'bad-request';

export class IndexerError extends Error {
  readonly kind: IndexerErrorKind;
  /** HTTP status (0 when there was no response) */
  readonly status: number;
  /** the API's error code (`not_found`, `rate_limited`, `db_busy`, ...) or a synthetic one (`network`, `timeout`, `aborted`, ...) */
  readonly code: string;
  readonly retryAfterSec?: number;
  constructor(kind: IndexerErrorKind, status: number, code: string, message: string, retryAfterSec?: number) {
    super(message);
    this.name = 'IndexerError';
    this.kind = kind;
    this.status = status;
    this.code = code;
    this.retryAfterSec = retryAfterSec;
  }
  /** worth retrying later (rate limit, busy, overloaded, timeout, network) as opposed to a request the server refused */
  get transient(): boolean {
    return this.kind === 'network' || this.kind === 'timeout' || this.status === 429 || this.status === 503 || this.status === 504 || this.status >= 500;
  }
}

/** Sompi decimal string -> bigint. Throws on anything but an unsigned decimal integer (never coerces through `number`). */
export function sompi(v: string | number | bigint): bigint {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number') {
    if (!Number.isSafeInteger(v) || v < 0) throw new RangeError(`not a safe non-negative integer: ${v}`);
    return BigInt(v);
  }
  if (!/^\d+$/.test(v)) throw new RangeError(`not a decimal integer: ${String(v).slice(0, 24)}`);
  return BigInt(v);
}

export const sompiOrNull = (v: string | number | bigint | null | undefined): bigint | null => (v === null || v === undefined ? null : sompi(v));

const HASH = /^[0-9a-f]{64}$/i;
function assertHash(what: string, v: string): string {
  if (!HASH.test(v)) throw new IndexerError('bad-request', 0, 'bad_request', `${what} must be 32 bytes of hex`);
  return v.toLowerCase();
}
function assertHex(what: string, v: string): string {
  if (!/^[0-9a-f]+$/i.test(v) || v.length % 2) throw new IndexerError('bad-request', 0, 'bad_request', `${what} must be hex`);
  return v.toLowerCase();
}

// ------------------------------------------------------------------------------------------------ REST client

export interface RequestOptions {
  signal?: AbortSignal;
  /** overrides the client's default timeout, ms */
  timeoutMs?: number;
}

export interface OrdersQuery {
  maker?: Hex;
  token?: Hex;
  status?: string;
  limit?: number;
  cursor?: string;
}
export interface FillsQuery {
  token?: Hex;
  side?: 'ask' | 'bid' | 'sell' | 'buy' | 1 | 2;
  limit?: number;
  /** id of the oldest fill already seen (paging backwards) */
  before?: number | string;
}
export interface TokenUtxosQuery {
  /** x-only public key of the owner (or an order covenant id: custody and strays) */
  owner: Hex;
  token?: Hex;
  spent?: boolean;
  limit?: number;
  /** follow `next_cursor` up to this many pages (default 10) */
  maxPages?: number;
}

export interface TradesQuery {
  limit?: number;
  /** a trade id (exclusive): paging backwards */
  before?: number | string;
}
export interface CandlesQuery {
  interval: CandleInterval;
  /** unix ms, inclusive */
  from?: number;
  /** unix ms, exclusive */
  to?: number;
  limit?: number;
}

export const CANDLE_INTERVALS: readonly CandleInterval[] = ['1m', '5m', '1h', '1d'];

/** HTTP answers that mean "this indexer does not serve that route" (an older build): the market-data readers resolve to null on them. */
const isUnsupported = (e: unknown): boolean => e instanceof IndexerError && (e.status === 404 || e.status === 405 || e.status === 501);

export interface IndexerApi {
  health(o?: RequestOptions): Promise<HealthView>;
  tokens(o?: RequestOptions): Promise<IndexerTokenView[]>;
  book(token: Hex, q?: { depth?: number; aggregate?: boolean }, o?: RequestOptions): Promise<BookView>;
  /** null when the indexer does not know the covenant id (yet: a fresh placement needs a few seconds) */
  order(id: Hex, o?: RequestOptions): Promise<OrderView | null>;
  orders(q?: OrdersQuery, o?: RequestOptions): Promise<Page<OrderView>>;
  /** follows `next_cursor` until exhausted or `maxPages` (default 20) */
  allOrders(q?: Omit<OrdersQuery, 'cursor'>, o?: RequestOptions & { maxPages?: number }): Promise<OrderView[]>;
  orderEvents(id: Hex, q?: { after?: number | string; limit?: number }, o?: RequestOptions): Promise<Page<EventView> | null>;
  fills(q?: FillsQuery, o?: RequestOptions): Promise<Page<EventView>>;
  strays(q?: { maker?: Hex; limit?: number }, o?: RequestOptions): Promise<StrayView[]>;
  tokenEvents(q?: { token?: Hex; limit?: number }, o?: RequestOptions): Promise<TokenEventView[]>;
  /**
   * `GET /v1/token-utxos?owner=&token=&spent=` (proven token holdings of either family, decoded state included; follows `next_cursor`). An indexer
   * without the endpoint (404 / 501 / 405) resolves to `null` (never an error toast); callers fall back to the local token tracker. The indexer
   * cannot list issuance (genesis) outputs and may list a holding the chain has since spent: callers verify every UTXO on the node.
   */
  tokenUtxos(q: TokenUtxosQuery, o?: RequestOptions): Promise<TokenUtxoView[] | null>;
  /**
   * Market data (M5): `GET /v1/trades|candles|stats|depth/{token}`. Each resolves to `null` when the indexer answers 404 / 405 / 501 (an indexer
   * without the route, or a token it does not know): the views show "no data" and fall back where they can (trades -> `/v1/fills`, depth -> the book).
   */
  trades(token: Hex, q?: TradesQuery, o?: RequestOptions): Promise<TradesView | null>;
  candles(token: Hex, q: CandlesQuery, o?: RequestOptions): Promise<CandlesView | null>;
  stats(token: Hex, o?: RequestOptions): Promise<StatsView | null>;
  depth(token: Hex, q?: { levels?: number }, o?: RequestOptions): Promise<DepthView | null>;
  /**
   * Token/token pairs (docs/ops/executor.md 5.4; oriented: base A, quote B): `GET /v1/pairs/{base}/{quote}/book?depth=` (levels of source `direct` =
   * resting KobPair orders, `entry` = KobIfdPair entries at their limit, `route` = quotes implied through the two KAS books; prices quote base units
   * per base base unit), `GET /v1/pairs?token=` (pairs with live pair orders), `/candles` (derived from the two KAS series: a pair fill never makes
   * a price) and `/fills` (pair-order fills: volume only, with `volume_24h`). Each resolves to `null` when the indexer does not serve the route
   * (404 / 405 / 501): the pair view says "unavailable" and still lets the user place orders. Malformed rows are dropped (data/pair-api.ts).
   */
  pairBook(base: Hex, quote: Hex, q?: { depth?: number }, o?: RequestOptions): Promise<PairBookView | null>;
  pairs(token?: Hex, o?: RequestOptions): Promise<PairSummaryView[] | null>;
  pairCandles(base: Hex, quote: Hex, q: CandlesQuery, o?: RequestOptions): Promise<PairCandlesView | null>;
  pairFills(base: Hex, quote: Hex, q?: TradesQuery, o?: RequestOptions): Promise<PairFillsView | null>;
}

export interface HttpIndexerOptions {
  /** base URL, e.g. `https://kob.example` (no trailing slash needed) */
  baseUrl: string;
  fetch?: typeof fetch;
  /** default per-request timeout, ms (default 10 s) */
  timeoutMs?: number;
  /** retries after 429 / 503-with-Retry-After (default 2) */
  maxRetries?: number;
  /** upper bound of one backoff wait, ms (default 10 s) */
  maxBackoffMs?: number;
  sleep?: (ms: number, signal?: AbortSignal) => Promise<void>;
  /** how long a "token-utxos is unsupported" answer is remembered, ms (default 5 min) */
  unsupportedTtlMs?: number;
  now?: () => number;
  /** largest response body read, bytes (default `MAX_INDEXER_BODY_BYTES`) */
  maxBodyBytes?: number;
}

const abortableSleep = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((resolve, reject) => {
    if (signal?.aborted) return reject(new IndexerError('aborted', 0, 'aborted', 'The request was cancelled.'));
    const t = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(t);
      reject(new IndexerError('aborted', 0, 'aborted', 'The request was cancelled.'));
    };
    signal?.addEventListener('abort', onAbort, { once: true });
  });

export class HttpIndexer implements IndexerApi {
  private readonly base: string;
  private readonly f: typeof fetch;
  private readonly timeoutMs: number;
  private readonly maxRetries: number;
  private readonly maxBackoffMs: number;
  private readonly sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  private readonly unsupportedTtlMs: number;
  private readonly now: () => number;
  private readonly maxBodyBytes: number;
  private tokenUtxosUnsupportedUntil = 0;

  constructor(o: HttpIndexerOptions) {
    this.base = o.baseUrl.trim().replace(/\/+$/, '');
    this.f = o.fetch ?? ((...a) => fetch(...a));
    this.timeoutMs = o.timeoutMs ?? 10_000;
    this.maxRetries = o.maxRetries ?? 2;
    this.maxBackoffMs = o.maxBackoffMs ?? 10_000;
    this.sleep = o.sleep ?? abortableSleep;
    this.unsupportedTtlMs = o.unsupportedTtlMs ?? 5 * 60_000;
    this.now = o.now ?? Date.now;
    this.maxBodyBytes = o.maxBodyBytes ?? MAX_INDEXER_BODY_BYTES;
  }

  /** Base URL of the WebSocket feed derived from this client's base. */
  get feedUrl(): string {
    return toFeedUrl(this.base);
  }

  private url(path: string, query: Record<string, string | number | boolean | undefined> = {}): string {
    const q = new URLSearchParams();
    for (const [k, v] of Object.entries(query)) if (v !== undefined) q.set(k, String(v));
    const s = q.toString();
    return `${this.base}${path}${s ? '?' + s : ''}`;
  }

  /** One GET with timeout, caller cancellation and 429 / 503 backoff. Returns status + parsed body. */
  private async request(url: string, o: RequestOptions = {}): Promise<{ status: number; body: unknown }> {
    for (let attempt = 0; ; attempt++) {
      const ctl = new AbortController();
      const timeoutMs = o.timeoutMs ?? this.timeoutMs;
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        ctl.abort();
      }, timeoutMs);
      const onCallerAbort = () => ctl.abort();
      if (o.signal?.aborted) {
        clearTimeout(timer);
        throw new IndexerError('aborted', 0, 'aborted', 'The request was cancelled.');
      }
      o.signal?.addEventListener('abort', onCallerAbort, { once: true });
      let res: Response;
      let text: string;
      try {
        res = await this.f(url, { headers: { accept: 'application/json' }, signal: ctl.signal });
        text = await readCapped(res, this.maxBodyBytes);
      } catch (e) {
        if (e instanceof BodyTooLarge) throw new IndexerError('parse', 0, 'too_large', `The indexer answered more than ${Math.round(this.maxBodyBytes / 1024)} KiB: not read.`);
        if (o.signal?.aborted) throw new IndexerError('aborted', 0, 'aborted', 'The request was cancelled.');
        if (timedOut) throw new IndexerError('timeout', 0, 'timeout', `The indexer did not answer within ${Math.round(timeoutMs / 1000)}s.`);
        throw new IndexerError('network', 0, 'network', `The indexer is unreachable: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        clearTimeout(timer);
        o.signal?.removeEventListener('abort', onCallerAbort);
      }
      let body: unknown = null;
      if (text) {
        try {
          body = JSON.parse(text);
        } catch {
          body = null;
          if (res.ok) throw new IndexerError('parse', res.status, 'parse', 'The indexer answered something that is not JSON.');
        }
      }
      const retryAfter = parseRetryAfter(res.headers.get('retry-after'));
      const retryable = res.status === 429 || (res.status === 503 && retryAfter !== undefined);
      if (retryable && attempt < this.maxRetries) {
        const wait = Math.min(this.maxBackoffMs, retryAfter !== undefined ? retryAfter * 1000 : 500 * 2 ** attempt);
        await this.sleep(wait, o.signal);
        continue;
      }
      if (!res.ok) throw httpError(res.status, body, retryAfter);
      return { status: res.status, body };
    }
  }

  private async get<T>(path: string, query: Record<string, string | number | boolean | undefined>, o?: RequestOptions): Promise<T> {
    return (await this.request(this.url(path, query), o)).body as T;
  }

  /** GET that maps 404 to null (unknown resource). */
  private async getOrNull<T>(path: string, query: Record<string, string | number | boolean | undefined>, o?: RequestOptions): Promise<T | null> {
    try {
      return await this.get<T>(path, query, o);
    } catch (e) {
      if (e instanceof IndexerError && e.status === 404) return null;
      throw e;
    }
  }

  async health(o?: RequestOptions) {
    return this.get<HealthView>('/v1/health', {}, o);
  }

  async tokens(o?: RequestOptions) {
    const r = await this.get<{ tokens?: IndexerTokenView[] }>('/v1/tokens', {}, o);
    return Array.isArray(r?.tokens) ? r.tokens.filter((v) => typeof v?.covenant_id === 'string' && /^[0-9a-f]{64}$/.test(v.covenant_id)).map(normalizeTokenView) : [];
  }

  async book(token: Hex, q: { depth?: number; aggregate?: boolean } = {}, o?: RequestOptions) {
    // bids that can no longer fund one base unit come back with amount 0: they are not liquidity and never enter the book (they stay in the maker's orders)
    return dropEmptyRows(await this.get<BookView>(`/v1/books/${assertHash('token', token)}`, { depth: q.depth, aggregate: q.aggregate }, o));
  }

  async order(id: Hex, o?: RequestOptions) {
    return this.getOrNull<OrderView>(`/v1/orders/${assertHash('covenant id', id)}`, {}, o);
  }

  async orders(q: OrdersQuery = {}, o?: RequestOptions) {
    return this.get<Page<OrderView>>(
      '/v1/orders',
      {
        maker: q.maker && assertHex('maker', q.maker),
        token: q.token && assertHash('token', q.token),
        status: q.status,
        limit: q.limit,
        cursor: q.cursor,
      },
      o,
    );
  }

  async allOrders(q: Omit<OrdersQuery, 'cursor'> = {}, o: RequestOptions & { maxPages?: number } = {}) {
    const out: OrderView[] = [];
    let cursor: string | undefined;
    for (let page = 0; page < (o.maxPages ?? 20); page++) {
      const p = await this.orders({ ...q, cursor }, o);
      out.push(...p.items);
      if (!p.next_cursor) break;
      cursor = p.next_cursor;
    }
    return out;
  }

  async orderEvents(id: Hex, q: { after?: number | string; limit?: number } = {}, o?: RequestOptions) {
    return this.getOrNull<Page<EventView>>(`/v1/orders/${assertHash('covenant id', id)}/events`, { after: q.after, limit: q.limit }, o);
  }

  async fills(q: FillsQuery = {}, o?: RequestOptions) {
    return this.get<Page<EventView>>(
      '/v1/fills',
      { token: q.token && assertHash('token', q.token), side: q.side, limit: q.limit, before: q.before },
      o,
    );
  }

  async strays(q: { maker?: Hex; limit?: number } = {}, o?: RequestOptions) {
    const r = await this.get<{ items?: StrayView[] }>('/v1/strays', { maker: q.maker && assertHex('maker', q.maker), limit: q.limit }, o);
    return Array.isArray(r?.items) ? r.items : [];
  }

  async tokenEvents(q: { token?: Hex; limit?: number } = {}, o?: RequestOptions) {
    const r = await this.get<{ items?: TokenEventView[] }>('/v1/token-events', { token: q.token && assertHash('token', q.token), limit: q.limit }, o);
    return Array.isArray(r?.items) ? r.items : [];
  }

  async tokenUtxos(q: TokenUtxosQuery, o?: RequestOptions): Promise<TokenUtxoView[] | null> {
    if (this.now() < this.tokenUtxosUnsupportedUntil) return null;
    const out: TokenUtxoView[] = [];
    let cursor: string | undefined;
    try {
      for (let page = 0; page < (q.maxPages ?? 10); page++) {
        const r = await this.get<{ items?: TokenUtxoView[]; next_cursor?: string | null } | TokenUtxoView[]>(
          '/v1/token-utxos',
          { owner: assertHex('owner', q.owner), token: q.token && assertHash('token', q.token), spent: q.spent ?? false, limit: q.limit, cursor },
          o,
        );
        if (Array.isArray(r)) return [...out, ...r];
        if (Array.isArray(r?.items)) out.push(...r.items);
        cursor = typeof r?.next_cursor === 'string' && r.next_cursor !== '' ? r.next_cursor : undefined;
        if (cursor === undefined) break;
      }
    } catch (e) {
      if (e instanceof IndexerError && (e.status === 404 || e.status === 501 || e.status === 405)) {
        this.tokenUtxosUnsupportedUntil = this.now() + this.unsupportedTtlMs;
        return null;
      }
      throw e;
    }
    return out;
  }

  /** GET that maps "route not served" (404 / 405 / 501) to null. */
  private async getOptional<T>(path: string, query: Record<string, string | number | boolean | undefined>, o?: RequestOptions): Promise<T | null> {
    try {
      return await this.get<T>(path, query, o);
    } catch (e) {
      if (isUnsupported(e)) return null;
      throw e;
    }
  }

  async trades(token: Hex, q: TradesQuery = {}, o?: RequestOptions): Promise<TradesView | null> {
    const r = await this.getOptional<TradesView>(`/v1/trades/${assertHash('token', token)}`, { limit: q.limit, before: q.before }, o);
    return r && Array.isArray(r.items) ? { ...r, next_cursor: r.next_cursor ?? null } : null;
  }

  async candles(token: Hex, q: CandlesQuery, o?: RequestOptions): Promise<CandlesView | null> {
    if (!CANDLE_INTERVALS.includes(q.interval)) throw new IndexerError('bad-request', 0, 'bad_request', `unknown candle interval ${String(q.interval)}`);
    const r = await this.getOptional<CandlesView>(`/v1/candles/${assertHash('token', token)}`, { interval: q.interval, from: q.from, to: q.to, limit: q.limit }, o);
    return r && Array.isArray(r.items) ? r : null;
  }

  async stats(token: Hex, o?: RequestOptions): Promise<StatsView | null> {
    const r = await this.getOptional<StatsView>(`/v1/stats/${assertHash('token', token)}`, {}, o);
    return r && typeof r === 'object' && typeof r.price_basis === 'string' ? r : null;
  }

  async depth(token: Hex, q: { levels?: number } = {}, o?: RequestOptions): Promise<DepthView | null> {
    const r = await this.getOptional<DepthView>(`/v1/depth/${assertHash('token', token)}`, { levels: q.levels }, o);
    return r && Array.isArray(r.bids) && Array.isArray(r.asks) ? dropZeroDepthLevels(r) : null;
  }

  /** the path of a pair route: both ids hex32 and different (`base == quote` is a 400 of the server: refused here already) */
  private pairPath(base: Hex, quote: Hex, tail: string): string {
    const b = assertHash('base', base);
    const q = assertHash('quote', quote);
    if (b === q) throw new IndexerError('bad-request', 0, 'bad_request', 'a pair needs two different tokens');
    return `/v1/pairs/${b}/${q}/${tail}`;
  }

  async pairBook(base: Hex, quote: Hex, q: { depth?: number } = {}, o?: RequestOptions): Promise<PairBookView | null> {
    const r = await this.getOptional<PairBookView>(this.pairPath(base, quote, 'book'), { depth: q.depth }, o);
    return r && typeof r === 'object' && Array.isArray(r.asks) && Array.isArray(r.bids) ? sanitizePairBook(r) : null;
  }

  async pairs(token?: Hex, o?: RequestOptions): Promise<PairSummaryView[] | null> {
    return adaptPairSummaries(await this.getOptional<unknown>('/v1/pairs', { token: token && assertHash('token', token) }, o));
  }

  async pairCandles(base: Hex, quote: Hex, q: CandlesQuery, o?: RequestOptions): Promise<PairCandlesView | null> {
    if (!CANDLE_INTERVALS.includes(q.interval)) throw new IndexerError('bad-request', 0, 'bad_request', `unknown candle interval ${String(q.interval)}`);
    return adaptPairCandles(await this.getOptional<unknown>(this.pairPath(base, quote, 'candles'), { interval: q.interval, from: q.from, to: q.to, limit: q.limit }, o));
  }

  async pairFills(base: Hex, quote: Hex, q: TradesQuery = {}, o?: RequestOptions): Promise<PairFillsView | null> {
    return adaptPairFills(await this.getOptional<unknown>(this.pairPath(base, quote, 'fills'), { limit: q.limit, before: q.before }, o));
  }
}

export { sanitizePairBook } from './pair-api';

const STANDINGS: readonly TokenStanding[] = ['official', 'unverified', 'delisted'];
const POWER = /^[a-z0-9-]{1,32}$/;

/**
 * A `/v1/tokens` item made safe to render: an unknown `standing` is `unverified` (never `official`; a missing one stays missing: an executor without the registry), `powers` is a list of short
 * lower-case words (anything else is dropped), `ticker` is a string (empty when the token is not in the registry).
 */
export function normalizeTokenView(v: IndexerTokenView): IndexerTokenView {
  const out: IndexerTokenView = { ...v };
  if (typeof v.ticker !== 'string') out.ticker = '';
  if (v.standing !== undefined && v.standing !== null) out.standing = STANDINGS.includes(v.standing) ? v.standing : 'unverified';
  if (v.powers !== undefined) out.powers = Array.isArray(v.powers) ? [...new Set(v.powers.filter((p): p is string => typeof p === 'string' && POWER.test(p)))] : [];
  return out;
}

function parseRetryAfter(v: string | null): number | undefined {
  if (v === null) return undefined;
  const n = Number(v.trim());
  return Number.isFinite(n) && n >= 0 ? n : undefined;
}

function httpError(status: number, body: unknown, retryAfterSec?: number): IndexerError {
  const err = (body as { error?: { code?: unknown; message?: unknown } } | null)?.error;
  // the indexer's own words are shown to the user: a short plain code, and a message without control / bidi / zero-width characters
  const code = typeof err?.code === 'string' && /^[A-Za-z0-9_.-]{1,64}$/.test(err.code) ? err.code : `http_${status}`;
  const said = typeof err?.message === 'string' ? plainUntrusted(err.message) : '';
  const message = said || `The indexer answered HTTP ${status}.`;
  return new IndexerError('http', status, code, message, retryAfterSec);
}

/** `https://host/base` -> `wss://host/base/v1/ws` (and http -> ws); a ws(s) URL just gets the path appended. */
export function toFeedUrl(base: string): string {
  const b = base.trim().replace(/\/+$/, '');
  const ws = b.replace(/^http(s?):/i, (_m, s: string) => `ws${s}:`);
  return `${ws}/v1/ws`;
}

// ------------------------------------------------------------------------------------------------ WebSocket feed

/** Channel names accepted by the server (`api/ws.rs`). */
export function isValidChannel(c: string): boolean {
  return c === 'health' || c === 'reorg' || c === 'fills' || /^(fills|book|order):[0-9a-f]{64}$/i.test(c);
}

/** Structural subset of the browser / `ws` WebSocket the feed needs. */
export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code?: number; reason?: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}
export type WebSocketCtor = new (url: string) => WebSocketLike;

export type FeedStatus = 'idle' | 'connecting' | 'open' | 'reconnecting' | 'closed';

export interface ResyncEvent {
  /** `server`: the server told us we fell behind; `reconnect`: the socket was down, events were missed */
  reason: 'server' | 'reconnect';
  missedEvents?: number;
}

export interface FeedEventMap {
  status: { status: FeedStatus; attempt: number };
  open: { reconnect: boolean };
  close: { code: number | null; reason: string; willReconnect: boolean };
  /** every well-formed frame, before dispatch */
  frame: WsFrame;
  health: { type: string; data: HealthCursorNotice | HealthTickNotice };
  reorg: ReorgNotice;
  fill: { channel: string; fill: FillNoticeView };
  book: BookNotice;
  order: OrderNotice;
  resync: ResyncEvent;
  subscribed: { channels: string[]; errors: string[] };
  /** socket errors and server `error` frames; the feed keeps running */
  error: { message: string };
}

export interface IndexerFeedOptions {
  /** the indexer BASE url (http/https/ws/wss); `/v1/ws` is appended */
  url: string;
  webSocket?: WebSocketCtor;
  backoffBaseMs?: number;
  backoffMaxMs?: number;
  /** jitter source in [0,1) */
  random?: () => number;
  /** client keepalive `{"op":"ping"}` interval, ms (the server drops sockets idle for 60 s); 0 disables */
  pingIntervalMs?: number;
}

const WS_OPEN = 1;
const CHANNELS_PER_MESSAGE = 24; // the server caps one message at 4 KiB; a channel name is at most 71 chars

export class IndexerFeed {
  private readonly wsUrl: string;
  private readonly Ctor: WebSocketCtor;
  private readonly baseMs: number;
  private readonly maxMs: number;
  private readonly random: () => number;
  private readonly pingMs: number;
  private ws: WebSocketLike | null = null;
  private state: FeedStatus = 'idle';
  private attempt = 0;
  private everOpened = false;
  private wanted = new Set<string>();
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private readonly handlers = new Map<string, Set<(v: never) => void>>();

  constructor(o: IndexerFeedOptions) {
    this.wsUrl = toFeedUrl(o.url);
    const Ctor = o.webSocket ?? (globalThis as { WebSocket?: WebSocketCtor }).WebSocket;
    if (!Ctor) throw new Error('IndexerFeed: no WebSocket implementation available');
    this.Ctor = Ctor;
    this.baseMs = o.backoffBaseMs ?? 500;
    this.maxMs = o.backoffMaxMs ?? 15_000;
    this.random = o.random ?? Math.random;
    this.pingMs = o.pingIntervalMs ?? 25_000;
  }

  get status(): FeedStatus {
    return this.state;
  }

  get url(): string {
    return this.wsUrl;
  }

  /** currently wanted channels */
  channels(): string[] {
    return [...this.wanted].sort();
  }

  on<E extends keyof FeedEventMap>(event: E, cb: (v: FeedEventMap[E]) => void): () => void {
    let set = this.handlers.get(event);
    if (!set) this.handlers.set(event, (set = new Set()));
    set.add(cb as (v: never) => void);
    return () => set!.delete(cb as (v: never) => void);
  }

  private emit<E extends keyof FeedEventMap>(event: E, v: FeedEventMap[E]): void {
    for (const cb of [...(this.handlers.get(event) ?? [])]) {
      try {
        (cb as (x: FeedEventMap[E]) => void)(v);
      } catch {
        /* a subscriber's bug must not tear the feed down */
      }
    }
  }

  private setStatus(s: FeedStatus): void {
    this.state = s;
    this.emit('status', { status: s, attempt: this.attempt });
  }

  /** Starts (or resumes) the connection; idempotent. */
  connect(): void {
    if (this.state === 'closed') this.state = 'idle';
    if (this.ws || this.retryTimer || this.state === 'connecting' || this.state === 'open') return;
    this.open();
  }

  private open(): void {
    this.setStatus(this.everOpened ? 'reconnecting' : 'connecting');
    let ws: WebSocketLike;
    try {
      ws = new this.Ctor(this.wsUrl);
    } catch (e) {
      this.emit('error', { message: `cannot open the feed: ${e instanceof Error ? e.message : String(e)}` });
      this.scheduleReconnect();
      return;
    }
    this.ws = ws;
    ws.onopen = () => {
      if (this.ws !== ws) return;
      const reconnect = this.everOpened;
      this.everOpened = true;
      this.attempt = 0;
      this.setStatus('open');
      this.sendSubscribe([...this.wanted]);
      this.startPing();
      this.emit('open', { reconnect });
      // events published while the socket was down are gone: the app must refetch what it shows
      if (reconnect) this.emit('resync', { reason: 'reconnect' });
    };
    ws.onmessage = (ev) => {
      if (this.ws === ws) this.onMessage(ev.data);
    };
    ws.onerror = () => {
      if (this.ws === ws) this.emit('error', { message: 'WebSocket error' });
    };
    ws.onclose = (ev) => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.stopPing();
      const willReconnect = this.state !== 'closed';
      this.emit('close', { code: ev?.code ?? null, reason: ev?.reason ?? '', willReconnect });
      if (willReconnect) this.scheduleReconnect();
    };
  }

  private scheduleReconnect(): void {
    if (this.state === 'closed') return;
    const exp = Math.min(this.maxMs, this.baseMs * 2 ** Math.min(this.attempt, 20));
    const delay = Math.round(exp * (0.5 + 0.5 * this.random()));
    this.attempt++;
    this.setStatus('reconnecting');
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      if (this.state !== 'closed') this.open();
    }, delay);
  }

  private startPing(): void {
    this.stopPing();
    if (this.pingMs > 0) this.pingTimer = setInterval(() => this.send({ op: 'ping' }), this.pingMs);
  }

  private stopPing(): void {
    if (this.pingTimer) clearInterval(this.pingTimer);
    this.pingTimer = null;
  }

  private send(msg: unknown): boolean {
    const ws = this.ws;
    if (!ws || ws.readyState !== WS_OPEN) return false;
    try {
      ws.send(JSON.stringify(msg));
      return true;
    } catch {
      return false;
    }
  }

  private sendSubscribe(channels: string[]): void {
    for (let i = 0; i < channels.length; i += CHANNELS_PER_MESSAGE) {
      this.send({ op: 'subscribe', channels: channels.slice(i, i + CHANNELS_PER_MESSAGE) });
    }
  }

  /** Adds channels (sent now if the socket is open, and re-sent after every reconnect). Throws on an invalid name. */
  subscribe(channels: string[]): void {
    for (const c of channels) if (!isValidChannel(c)) throw new Error(`invalid feed channel: ${c}`);
    const fresh = channels.filter((c) => !this.wanted.has(c.toLowerCase()));
    for (const c of channels) this.wanted.add(c.toLowerCase());
    if (fresh.length) this.sendSubscribe(fresh.map((c) => c.toLowerCase()));
  }

  unsubscribe(channels: string[]): void {
    const had = channels.map((c) => c.toLowerCase()).filter((c) => this.wanted.delete(c));
    for (let i = 0; i < had.length; i += CHANNELS_PER_MESSAGE) this.send({ op: 'unsubscribe', channels: had.slice(i, i + CHANNELS_PER_MESSAGE) });
  }

  /** Resolves once the socket is open (immediately if it already is). */
  whenOpen(timeoutMs = 10_000): Promise<void> {
    if (this.state === 'open') return Promise.resolve();
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        off();
        reject(new Error('feed did not open in time'));
      }, timeoutMs);
      const off = this.on('open', () => {
        clearTimeout(t);
        off();
        resolve();
      });
    });
  }

  private onMessage(data: unknown): void {
    let frame: WsFrame;
    try {
      const text = typeof data === 'string' ? data : data instanceof ArrayBuffer ? new TextDecoder().decode(data) : String(data);
      const v: unknown = JSON.parse(text);
      if (typeof v !== 'object' || v === null || typeof (v as WsFrame).type !== 'string') return;
      frame = v as WsFrame;
    } catch {
      return;
    }
    this.emit('frame', frame);
    const ch = frame.channel ?? '';
    switch (frame.type) {
      case 'resync': {
        const missed = (frame.data as { missed_events?: number } | undefined)?.missed_events;
        this.emit('resync', { reason: 'server', missedEvents: typeof missed === 'number' ? missed : undefined });
        return;
      }
      case 'subscribed': {
        const d = (frame.data ?? {}) as { channels?: string[]; errors?: string[] };
        this.emit('subscribed', { channels: d.channels ?? [], errors: d.errors ?? [] });
        return;
      }
      case 'error':
        this.emit('error', { message: String((frame.data as { message?: unknown } | undefined)?.message ?? 'server error') });
        return;
      case 'fill':
        this.emit('fill', { channel: ch, fill: frame.data as FillNoticeView });
        return;
      case 'book':
        this.emit('book', frame.data as BookNotice);
        return;
      case 'order':
        this.emit('order', frame.data as OrderNotice);
        return;
      case 'reorg':
        this.emit('reorg', frame.data as ReorgNotice);
        return;
      case 'cursor':
      case 'health':
        if (ch === 'health') this.emit('health', { type: frame.type, data: frame.data as HealthCursorNotice | HealthTickNotice });
        return;
      default:
        return; // pong, unsubscribed, unknown: nothing to do
    }
  }

  /** Stops reconnecting and closes the socket. The instance can be `connect()`ed again. */
  close(): void {
    this.state = 'closed';
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.stopPing();
    const ws = this.ws;
    this.ws = null;
    if (ws) {
      ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null;
      try {
        ws.close(1000, 'client closed');
      } catch {
        /* already closed */
      }
    }
    this.setStatus('closed');
  }
}
