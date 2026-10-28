// kascov.io as a source of token NAMES and ICONS for the market list, and nothing else.
//
// Founder decision 2026-10-27: KOB keeps no token list of its own (no data in the repository, none in the indexer). The market list is completed live from
// the covenant explorer kascov.io, which serves a static JSON of every token it has seen on the network (CORS `*`, `max-age=30`):
//
//     https://kascov.io/data/{mainnet|testnet-10}/tokens.json
//
// What the app takes from it, and what it does not:
//
//   * ONLY a display ticker, a display name, an image and, to flag a disagreement, the declared decimals and the program hash. These are the token creator's
//     own claims, recorded in its genesis (the same status as a KCC-23 declaration: "declared by its creator"). Nobody has checked them.
//   * Never a fact: supply, powers, the decimals a market trades with, prices and orders come from the chain and the KOB indexer exactly as before. Where
//     kascov's ticker or decimals disagree with what KOB derives, KOB wins and the list says so (ui/market/kascov-merge.ts).
//   * Never an authority: kascov's `review`, `alive` or `invalid_reason` are not read at all, and its `status` only as a labelled third-party remark: a token it
//     marks `verified` shows a small "Verified by kascov" chip instead of "Genesis not verified" (the chip says kascov checked the token's history and KOB has not
//     checked it itself; founder decision 2026-10-28). It changes no warning, no fact and no rule. Whether a token is listed is decided by the program hash
//     alone, against the programs this build pins and enables for the network (`kascovTradable`). kascov's `lineage_complete` ("provably genesis") is not
//     read: it only says kascov's index began before the token did.
//   * Every failure (unreachable, slow, not JSON, a changed shape, a response for another network) is an `ok: false` result, never a throw: the list is then
//     what it was before this module existed.
//
// ICONS. `claimed_image_hash` is the SHA-256 of the image's RAW BYTES, committed by the creator in the token's genesis (checked against real tokens of
// mainnet and testnet-10: 37 of 37 and 27 of 27 saved copies hash to it). An image is shown only when the bytes the browser fetched hash to it:
//
//   * the creators' own hosts (`claimed_image`, mostly storage.googleapis.com) send NO `Access-Control-Allow-Origin`, so the browser may display such an
//     image in an <img> but cannot read its bytes, hence cannot hash it. The app therefore never uses an <img> hotlink;
//   * kascov.io keeps a copy of each hash-committed image (`https://kascov.io/img/{network}/{covenant_id}`, CORS `*`, immutable): those bytes are fetched
//     FIRST, so the usual viewer's IP goes to kascov.io only, not to the creator's host. If kascov has no copy (404), the creator's own https URL is tried,
//     and works only where that host allows CORS;
//   * the verified bytes are shown from a `blob:` URL the app makes itself (CSP `img-src 'self' data: blob:`, web/csp.mjs), so a hash mismatch, a host that
//     swapped the file, or a refused fetch all end the same way: no icon (a letter avatar).
// Only https URLs are used. Raster formats only (PNG, JPEG, WebP, GIF, by their magic bytes, never by the server's Content-Type): no SVG, no HTML.
import { DEFAULT_TOKEN_NAMES_URL, defaultStorage, type StorageLike } from '../config';
import { sanitizeUntrusted } from '../kob/registry';

/** The data set's base (`AppConfig.tokenNamesUrl`, default https://kascov.io/data). '' = the feature is off. */
export const DEFAULT_BASE = DEFAULT_TOKEN_NAMES_URL;
export type KascovNetwork = 'mainnet' | 'testnet-10';

/** The kascov data set of a network of this app, null for a network kascov does not serve. */
export const kascovNetwork = (network: string | null | undefined): KascovNetwork | null => (network === 'mainnet' || network === 'testnet-10' ? network : null);
const TRAILING_SLASHES = /\/+$/;
export const kascovTokensUrl = (n: KascovNetwork, base = DEFAULT_BASE): string => `${base.replace(TRAILING_SLASHES, '')}/${n}/tokens.json`;
/** Where the saved copies of the images are: `img` beside the data set's last path segment (https://kascov.io/data -> https://kascov.io/img). */
export const kascovImagesBase = (base = DEFAULT_BASE): string => new URL('../img', `${base.replace(TRAILING_SLASHES, '')}/`).href.replace(TRAILING_SLASHES, '');
/**
 * The token's own page on the explorer (a link for the viewer to follow, never fetched by the app): `https://kascov.io/#/<network>/token/<covenant id>`, on the origin of
 * the data set (`https://kascov.io/data` -> `https://kascov.io`). Null when the data set is off or its base is not an https URL.
 */
export function kascovTokenUrl(n: KascovNetwork, covenantId: string, base = DEFAULT_BASE): string | null {
  try {
    const u = new URL(base.replace(TRAILING_SLASHES, ''));
    return u.protocol === 'https:' && u.username === '' && u.password === '' && /^[0-9a-f]{64}$/.test(covenantId) ? `${u.origin}/#/${n}/token/${covenantId}` : null;
  } catch {
    return null;
  }
}
/** kascov's saved copy of a token's hash-committed image. */
export const kascovImageUrl = (n: KascovNetwork, covenantId: string, base = DEFAULT_BASE): string => `${kascovImagesBase(base)}/${n}/${covenantId}`;

// ------------------------------------------------------------------------------------------------ the parsed list

/** What the app reads of one kascov token. Every string is restricted (control, zero-width and bidi characters replaced, length capped). */
export interface KascovToken {
  covenantId: string;
  /** `claimed_ticker`: the creator's claim, null without one */
  ticker: string | null;
  /** `claimed_name` */
  name: string | null;
  /** `claimed_decimals` (0..=18), a display claim only: compared with what KOB derives, never used to trade */
  decimals: number | null;
  /** the token program's template hash as kascov names it (`standard.view_template_hash`; the 46-byte KRON family has none) */
  templateHash: string | null;
  /** SHA-256 of the image's raw bytes, as the creator committed it (`claimed_image_hash`) */
  imageHash: string | null;
  /** the creator's own https URL of the image; only kept with a hash (without one nothing could be checked) */
  imageUrl: string | null;
  /** kascov marks the token `verified` (`status`): its own, third-party remark; KOB has not checked the token by it */
  verified: boolean;
}

export type KascovFailure = 'unsupported' | 'network' | 'timeout' | 'http' | 'too-large' | 'shape';
export type KascovResult = { ok: true; tokens: KascovToken[]; generatedAtMs: number | null } | { ok: false; reason: KascovFailure };

/**
 * How often a view that shows names or icons may fetch the list again while it stays visible (kascov regenerates the file about every 2 minutes and
 * sends no ETag or Last-Modified; it is 0.8 MB on mainnet, 2.4 MB on testnet-10). The ONE constant of the policy (docs/ops/web-hosting.md).
 */
export const KASCOV_REFRESH_MS = 120_000;
/** After a failed fetch the next try waits 30 s, then twice as long each time, at most 10 minutes. */
export const KASCOV_BACKOFF_BASE_MS = 30_000;
export const KASCOV_BACKOFF_MAX_MS = 600_000;

const HEX64 = /^[0-9a-f]{64}$/;
const hex64 = (v: unknown): string | null => (typeof v === 'string' && HEX64.test(v.toLowerCase()) ? v.toLowerCase() : null);
const text = (v: unknown, max: number): string | null => {
  if (typeof v !== 'string') return null;
  const s = sanitizeUntrusted(v.normalize('NFC').trim(), max);
  return s === '' ? null : s;
};
const asObject = (v: unknown): Record<string, unknown> | null => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null);

/** An image URL the app may fetch: https only, no credentials, a plain length. */
export function safeImageUrl(v: unknown): string | null {
  if (typeof v !== 'string' || v.length > 2048) return null;
  try {
    const u = new URL(v);
    return u.protocol === 'https:' && u.username === '' && u.password === '' && u.hostname !== '' ? u.href : null;
  } catch {
    return null;
  }
}

/** One entry of `tokens`, null when it has no usable covenant id. */
export function parseKascovToken(raw: unknown): KascovToken | null {
  const o = asObject(raw);
  const covenantId = hex64(o?.covenant_id);
  if (!o || !covenantId) return null;
  const dec = o.claimed_decimals;
  const imageHash = hex64(o.claimed_image_hash);
  return {
    covenantId,
    ticker: text(o.claimed_ticker, 16),
    name: text(o.claimed_name, 64),
    decimals: typeof dec === 'number' && Number.isInteger(dec) && dec >= 0 && dec <= 18 ? dec : null,
    templateHash: hex64(asObject(o.standard)?.view_template_hash),
    imageHash,
    imageUrl: imageHash ? safeImageUrl(o.claimed_image) : null,
    verified: o.status === 'verified',
  };
}

/** `generated_at_ms` of a document: when kascov built it; null when absent or not a plain number. */
export const generatedAtOf = (doc: unknown): number | null => {
  const g = asObject(doc)?.generated_at_ms;
  return typeof g === 'number' && Number.isSafeInteger(g) && g > 0 ? g : null;
};

/**
 * The tokens of a `tokens.json` document, or null when its shape is not the one this reader knows (`tokens` is not an array, a response for another
 * network, or entries that are all unusable). A single bad entry is skipped; a repeated covenant id keeps its first entry.
 */
export function parseKascovTokens(doc: unknown, network: KascovNetwork): KascovToken[] | null {
  const root = asObject(doc);
  if (!root || !Array.isArray(root.tokens)) return null;
  if (root.network !== undefined && root.network !== network) return null;
  const seen = new Set<string>();
  const out: KascovToken[] = [];
  for (const raw of root.tokens) {
    const t = parseKascovToken(raw);
    if (!t || seen.has(t.covenantId)) continue;
    seen.add(t.covenantId);
    out.push(t);
  }
  return root.tokens.length > 0 && out.length === 0 ? null : out;
}

// ------------------------------------------------------------------------------------------------ the client (memory cache, bounded)

export interface KascovClientOptions {
  fetch?: typeof fetch;
  now?: () => number;
  /** the whole request, headers to last byte (default 8 s) */
  timeoutMs?: number;
  /** how long an answer is reused before the next fetch (default `KASCOV_REFRESH_MS`) */
  ttlMs?: number;
  /** the wait after the first failure, doubling per further failure up to `maxBackoffMs` (defaults 30 s and 10 min) */
  backoffMs?: number;
  maxBackoffMs?: number;
  /** the largest body read (default 12 MB; testnet-10's file is about 2.4 MB) */
  maxBytes?: number;
}

export interface KascovClient {
  /**
   * The list of `network`: the cached one while it is fresh, else one shared request. Never rejects. A failure after an earlier success keeps serving the
   * earlier list (names do not change; the next try waits out the back-off). An aborted `signal` ends only this caller's wait.
   */
  load(network: string, o?: { signal?: AbortSignal | undefined; base?: string | undefined }): Promise<KascovResult>;
  /** Milliseconds until `load(network)` will fetch again (at least 1 s; the refresh period when nothing is cached). */
  nextInMs(network: string, base?: string): number;
  /** Forgets every cached answer (tests). */
  clear(): void;
}

/** Reads a response body up to `max` bytes; null when it is larger. */
export async function readCapped(res: Response, max: number): Promise<Uint8Array | null> {
  const declared = Number(res.headers.get('content-length'));
  if (Number.isFinite(declared) && declared > max) return null;
  const reader = res.body?.getReader();
  if (!reader) {
    const buf = new Uint8Array(await res.arrayBuffer());
    return buf.length > max ? null : buf;
  }
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    if (total > max) {
      void reader.cancel().catch(() => undefined);
      return null;
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

interface Entry {
  /** the last good list, else the last failure */
  result: KascovResult;
  /** when `load` fetches again */
  nextAt: number;
  failures: number;
}

export function createKascovClient(o: KascovClientOptions = {}): KascovClient {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const now = o.now ?? Date.now;
  const timeoutMs = o.timeoutMs ?? 8_000;
  const ttlMs = o.ttlMs ?? KASCOV_REFRESH_MS;
  const backoff = o.backoffMs ?? KASCOV_BACKOFF_BASE_MS;
  const maxBackoff = o.maxBackoffMs ?? KASCOV_BACKOFF_MAX_MS;
  const maxBytes = o.maxBytes ?? 12_000_000;
  const cache = new Map<string, Entry>();
  const inflight = new Map<string, Promise<KascovResult>>();

  async function request(n: KascovNetwork, base: string, previous: KascovResult | undefined): Promise<KascovResult> {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), timeoutMs);
    try {
      const res = await fetchFn(kascovTokensUrl(n, base), { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', headers: { accept: 'application/json' } });
      if (!res.ok) return { ok: false, reason: 'http' };
      const body = await readCapped(res, maxBytes);
      if (!body) return { ok: false, reason: 'too-large' };
      const doc: unknown = JSON.parse(new TextDecoder().decode(body));
      const generatedAtMs = generatedAtOf(doc);
      // kascov did not build a new file: keep the parsed list (the same object, so nothing re-renders)
      if (previous?.ok && generatedAtMs !== null && previous.generatedAtMs === generatedAtMs) return previous;
      const tokens = parseKascovTokens(doc, n);
      return tokens ? { ok: true, tokens, generatedAtMs } : { ok: false, reason: 'shape' };
    } catch (e) {
      return { ok: false, reason: e instanceof SyntaxError ? 'shape' : ctl.signal.aborted ? 'timeout' : 'network' };
    } finally {
      clearTimeout(timer);
    }
  }

  const wait = (p: Promise<KascovResult>, signal?: AbortSignal): Promise<KascovResult> => {
    if (!signal) return p;
    if (signal.aborted) return Promise.resolve({ ok: false, reason: 'network' });
    return Promise.race([p, new Promise<KascovResult>((resolve) => signal.addEventListener('abort', () => resolve({ ok: false, reason: 'network' }), { once: true }))]);
  };

  return {
    async load(network, opts = {}) {
      const n = kascovNetwork(network);
      const base = (opts.base ?? DEFAULT_BASE).replace(TRAILING_SLASHES, '');
      if (!n || base === '') return { ok: false, reason: 'unsupported' };
      const key = `${base}|${n}`;
      const hit = cache.get(key);
      if (hit && now() < hit.nextAt) return hit.result;
      let p = inflight.get(key);
      if (!p) {
        p = request(n, base, hit?.result).then((fresh) => {
          const failures = fresh.ok ? 0 : (hit?.failures ?? 0) + 1;
          const entry: Entry = {
            // a failure after a success keeps the earlier list on show
            result: fresh.ok || !hit?.result.ok ? fresh : hit.result,
            nextAt: now() + (fresh.ok ? ttlMs : Math.min(maxBackoff, backoff * 2 ** (failures - 1))),
            failures,
          };
          cache.set(key, entry);
          inflight.delete(key);
          return entry.result;
        });
        inflight.set(key, p);
      }
      return wait(p, opts.signal);
    },
    nextInMs(network, base = DEFAULT_BASE) {
      const n = kascovNetwork(network);
      const hit = n ? cache.get(`${base.replace(TRAILING_SLASHES, '')}|${n}`) : undefined;
      return hit ? Math.max(1_000, hit.nextAt - now()) : ttlMs;
    },
    clear() {
      cache.clear();
      inflight.clear();
    },
  };
}


/** The app's one client: the session cache. */
export const kascov: KascovClient = createKascovClient();

// ------------------------------------------------------------------------------------------------ icons: fetch, hash, show

export type ImageMime = 'image/png' | 'image/jpeg' | 'image/webp' | 'image/gif';

/** The raster format of the bytes by their magic number, null for anything else (SVG, HTML, a truncated file). */
export function sniffImage(b: Uint8Array): ImageMime | null {
  if (b.length >= 8 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47 && b[4] === 0x0d && b[5] === 0x0a && b[6] === 0x1a && b[7] === 0x0a) return 'image/png';
  if (b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return 'image/jpeg';
  if (b.length >= 12 && b[0] === 0x52 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x46 && b[8] === 0x57 && b[9] === 0x45 && b[10] === 0x42 && b[11] === 0x50) return 'image/webp';
  if (b.length >= 6 && b[0] === 0x47 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x38 && (b[4] === 0x37 || b[4] === 0x39) && b[5] === 0x61) return 'image/gif';
  return null;
}

/** Lower-case hex SHA-256 of the bytes; null where the browser has no `crypto.subtle` (an insecure page): no icon can then be checked, so none is shown. */
export async function sha256Hex(bytes: Uint8Array): Promise<string | null> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return null;
  try {
    const digest = new Uint8Array(await subtle.digest('SHA-256', bytes as Uint8Array<ArrayBuffer>));
    return [...digest].map((x) => x.toString(16).padStart(2, '0')).join('');
  } catch {
    return null;
  }
}

export interface IconFetchOptions {
  fetch?: typeof fetch;
  signal?: AbortSignal | undefined;
  /** per image (default 8 s) */
  timeoutMs?: number;
  /** the largest image read (default 1.5 MB; the largest saved copy seen is 0.5 MB) */
  maxBytes?: number;
  sha256?: (b: Uint8Array) => Promise<string | null>;
  /** called with the URL of a source that answered 404 (it holds no copy) */
  onMissing?: (url: string) => void;
}

export interface VerifiedImage { bytes: Uint8Array; mime: ImageMime; url: string }

/**
 * The first of `urls` (https only, tried in order) whose bytes hash to `hash` and are a raster image; null when none does. A fetch that fails (CORS, 404,
 * timeout, too large), bytes that hash otherwise, and a format that is not raster are all skipped alike: the caller shows a letter avatar.
 */
export async function fetchVerifiedImage(urls: readonly string[], hash: string, o: IconFetchOptions = {}): Promise<VerifiedImage | null> {
  const want = hex64(hash);
  if (!want) return null;
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const digest = o.sha256 ?? sha256Hex;
  for (const raw of urls) {
    const url = safeImageUrl(raw);
    if (!url) continue;
    if (o.signal?.aborted) return null;
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? 8_000);
    const onAbort = () => ctl.abort();
    o.signal?.addEventListener('abort', onAbort, { once: true });
    try {
      const res = await fetchFn(url, { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', redirect: 'follow' });
      if (!res.ok) {
        if (res.status === 404) o.onMissing?.(url);
        continue;
      }
      const bytes = await readCapped(res, o.maxBytes ?? 1_500_000);
      if (!bytes || bytes.length === 0) continue;
      const mime = sniffImage(bytes);
      if (!mime) continue;
      if ((await digest(bytes)) !== want) continue;
      return { bytes, mime, url };
    } catch {
      /* CORS, offline, aborted, a bad body: this source gave nothing */
    } finally {
      clearTimeout(timer);
      o.signal?.removeEventListener('abort', onAbort);
    }
  }
  return null;
}

/** The sources of a token's icon, best first: kascov's saved copy, then the creator's own https URL. Empty without a committed hash. */
export function iconSources(n: KascovNetwork, t: Pick<KascovToken, 'covenantId' | 'imageHash' | 'imageUrl'>, base = DEFAULT_BASE): string[] {
  if (!t.imageHash) return [];
  return [...(base ? [kascovImageUrl(n, t.covenantId, base)] : []), ...(t.imageUrl ? [t.imageUrl] : [])];
}

export interface IconLoaderOptions extends IconFetchOptions {
  /** images fetched at once (default 4) */
  limit?: number;
  /** blob URL of verified bytes (default `URL.createObjectURL`) */
  makeUrl?: (bytes: Uint8Array, mime: ImageMime) => string;
  now?: () => number;
  /** how long a failed icon is not tried again in this session (default 5 min) */
  failTtlMs?: number;
  /** where kascov.io's 404s for saved copies are remembered (default: this browser's storage for a day; `null`: not at all) */
  missing?: MissingCopies | null;
}

export interface IconLoader {
  /** The blob URL of the verified icon, null when there is none. Never rejects. Not called at all while icons are turned off (the caller decides). */
  load(n: KascovNetwork, t: Pick<KascovToken, 'covenantId' | 'imageHash' | 'imageUrl'>, o?: { signal?: AbortSignal | undefined; base?: string | undefined }): Promise<string | null>;
  clear(): void;
}

/**
 * Saved copies kascov.io answered 404 for (it keeps one for most tokens that commit to an image, not all). A browser prints every 404 in its console and
 * nothing can stop that but not asking again, so a token whose copy is missing is not asked for it for a day: the icon falls back to its letter quietly
 * (or to the creator's own host, where that allows it). Kept per network and covenant id.
 */
export interface MissingCopies {
  has(network: string, covenantId: string): boolean;
  add(network: string, covenantId: string): void;
}

export const MISSING_COPY_KEY = 'kob.icons.missing.v1';
export const MISSING_COPY_TTL_MS = 24 * 3_600_000;
const MISSING_COPY_MAX = 1000;

/** The remembered 404s in `storage` (a failing or absent storage: in memory only, for the session). */
export function createMissingCopies(storage: StorageLike | null = defaultStorage(), now: () => number = Date.now, ttlMs = MISSING_COPY_TTL_MS): MissingCopies {
  const memory = new Map<string, number>();
  const load = (): Map<string, number> => {
    const m = new Map(memory);
    try {
      const raw = storage?.getItem(MISSING_COPY_KEY);
      const doc: unknown = raw ? JSON.parse(raw) : null;
      if (doc && typeof doc === 'object' && !Array.isArray(doc)) {
        for (const [k, v] of Object.entries(doc)) if (typeof v === 'number' && Number.isFinite(v)) m.set(k, Math.max(m.get(k) ?? 0, v));
      }
    } catch {
      /* an unreadable store is an empty one */
    }
    return m;
  };
  return {
    has(network, covenantId) {
      const at = load().get(`${network}:${covenantId}`);
      return at !== undefined && now() - at < ttlMs;
    },
    add(network, covenantId) {
      memory.set(`${network}:${covenantId}`, now());
      const live = [...load()].filter(([, at]) => now() - at < ttlMs).sort((a, b) => b[1] - a[1]).slice(0, MISSING_COPY_MAX);
      try {
        storage?.setItem(MISSING_COPY_KEY, JSON.stringify(Object.fromEntries(live)));
      } catch {
        /* storage full or blocked: the session's memory still holds it */
      }
    },
  };
}

export function createIconLoader(o: IconLoaderOptions = {}): IconLoader {
  const limit = Math.max(1, o.limit ?? 4);
  const now = o.now ?? Date.now;
  const failTtlMs = o.failTtlMs ?? 300_000;
  const missing = o.missing === undefined ? createMissingCopies() : o.missing;
  const makeUrl = o.makeUrl ?? ((bytes, mime) => URL.createObjectURL(new Blob([bytes as Uint8Array<ArrayBuffer>], { type: mime })));
  // Verified images by their hash: bytes that hash to H ARE the image H, whichever token committed to it, so a hash proven once serves every token that
  // names it. Nothing else is shared between tokens: a request in flight and a miss belong to ONE token (its own sources), so a token whose sources
  // serve other bytes cannot make another token's icon fail.
  const verified = new Map<string, string>();
  const inflight = new Map<string, Promise<string | null>>();
  const failed = new Map<string, number>();
  let active = 0;
  const queue: (() => void)[] = [];
  const slot = async <T>(fn: () => Promise<T>): Promise<T> => {
    if (active >= limit) await new Promise<void>((resolve) => queue.push(resolve));
    active++;
    try {
      return await fn();
    } finally {
      active--;
      queue.shift()?.();
    }
  };
  return {
    async load(n, t, opts = {}) {
      const hash = t.imageHash;
      if (!hash) return null;
      const known = verified.get(hash);
      if (known) return known;
      const key = `${n}:${t.covenantId}:${hash}`;
      const running = inflight.get(key);
      if (running) return running;
      const failedAt = failed.get(key);
      if (failedAt !== undefined && now() - failedAt < failTtlMs) return null;
      const base = opts.base ?? DEFAULT_BASE;
      const copy = kascovImageUrl(n, t.covenantId, base);
      // the saved copy kascov.io answered 404 for is not asked for again (a 404 is a line in the console each time)
      const sources = iconSources(n, t, base).filter((u) => !(u === copy && missing?.has(n, t.covenantId)));
      // the fetch is not tied to the caller's signal: a row scrolled away or a view left must not cancel what the next visit will use
      const p = slot(() => fetchVerifiedImage(sources, hash, { ...o, signal: undefined, onMissing: (u) => { if (u === copy) missing?.add(n, t.covenantId); } }))
        .then((img) => {
          if (!img) {
            failed.set(key, now());
            return null;
          }
          const url = verified.get(hash) ?? makeUrl(img.bytes, img.mime);
          verified.set(hash, url);
          return url;
        })
        .finally(() => inflight.delete(key));
      inflight.set(key, p);
      return p;
    },
    clear() {
      verified.clear();
      inflight.clear();
      failed.clear();
    },
  };
}

/** The app's one icon loader: the session's verified images. */
export const iconLoader: IconLoader = createIconLoader();
