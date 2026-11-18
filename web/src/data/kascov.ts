// kascov.io as a source of token names and icons for the market list, and nothing else.
//
// KOB keeps no token list of its own, neither in the repository nor in the indexer. The market
// list is completed live from the covenant explorer kascov.io. It serves a static JSON of every
// token it has seen on the network, with CORS `*` and `max-age=30`:
//
//     https://kascov.io/data/{mainnet|testnet-10}/tokens.json
//
// What the app takes from it, and what it does not:
// - Only a display ticker, a display name, an image, and the declared decimals and program hash
//   to flag a disagreement. These are the token creator's own claims, recorded in its genesis. They
//   have the status of a KCC-23 declaration, "declared by its creator", and nobody has checked
//   them.
// - Never a fact. Supply, powers, the decimals a market trades with, prices and orders come from
//   the chain and the KOB indexer. Where kascov's ticker or decimals disagree with what KOB
//   derives, KOB wins and the list says so (ui/market/kascov-merge.ts).
// - Never an authority. kascov's `review`, `alive` and `invalid_reason` are not read at all. Its
//   `status` is read only as a labelled third-party remark. A token it marks `verified` shows a
//   small "Verified by kascov" chip instead of "Genesis not verified". The chip says kascov
//   checked the token's history and KOB has not checked it itself. It changes no warning, no fact
//   and no rule. Whether a token is listed is decided by the program hash alone, against the
//   programs this build pins and enables for the network (`kascovTradable`). kascov's
//   `lineage_complete` ("provably genesis") is not read, because it only says kascov's index began
//   before the token did.
// - A price is read in one way only, as whether kascov prices the token at all. The spot is
//   `market.spot_num_sompi` over `spot_den`, an exact ratio of KAS sompi per base unit. It is
//   kascov's own replay of the pools of other venues, a marginal price before fees. The check is
//   strict: two positive safe integers or digit strings, else the token is unpriced. It only
//   decides whether the chart may offer the optional kascov line (ui/market/PriceChart.tsx). The
//   line itself comes from kascov's candles (data/kascov-candles.ts), fetched only while the
//   viewer has it on. KOB never uses a kascov price for a warning, an ordering, a check or a
//   figure.
// - Every failure is an `ok: false` result, never a throw. That covers unreachable, slow, not
//   JSON, a changed shape and a response for another network. The list then has no kascov data.
//
// Icons. `claimed_image_hash` is the SHA-256 of the image's raw bytes, committed by the creator in
// the token's genesis. Checked against real tokens, 37 of 37 saved copies on mainnet and 27 of 27
// on testnet-10 hash to it. An image is shown only when the bytes the browser fetched hash to it.
// - The creators' own hosts (`claimed_image`, mostly storage.googleapis.com) send no
//   `Access-Control-Allow-Origin`. The browser may display such an image in an <img> but cannot
//   read its bytes, so it cannot hash them. This checked path therefore never asks them. The app
//   may show such an image unchecked, and labelled so, when this path gave nothing and the viewer
//   allows it (data/logos.ts, Settings "creator sites").
// - kascov.io keeps a copy of each hash-committed image
//   (`https://kascov.io/img/{network}/{covenant_id}`, CORS `*`, immutable). Those bytes are
//   fetched, and only those, so a viewer's IP goes to kascov.io and never to the creator's host.
//   If kascov has no copy (404) there is no checked icon. A redirect is refused, so a copy cannot
//   send the viewer on to another host.
// - The verified bytes are shown from a `blob:` URL the app makes itself (CSP
//   `img-src 'self' data: blob:`, web/csp.mjs). So a hash mismatch, a host that swapped the file
//   and a refused fetch all end the same way, with no checked icon.
// Only https URLs are used. Formats are told by their magic bytes, never by the server's
// Content-Type. PNG, JPEG, WebP and GIF are shown as they are. Another picture format, such as
// SVG, is drawn to a raster picture first (data/svg-logo.ts), so the page shows only raster
// images. HTML is never shown.
import { DEFAULT_TOKEN_NAMES_URL, defaultStorage, type StorageLike } from '../config';
import { sanitizeUntrusted } from '../kob/registry';
import { isPicture, shownPicture, type Rasterize } from './svg-logo';

/**
 * The data set's base (`AppConfig.tokenNamesUrl`, default https://kascov.io/data). An empty
 * string turns the feature off.
 */
export const DEFAULT_BASE = DEFAULT_TOKEN_NAMES_URL;
export type KascovNetwork = 'mainnet' | 'testnet-10';

/** The kascov data set of a network of this app, null for a network kascov does not serve. */
export const kascovNetwork = (network: string | null | undefined): KascovNetwork | null => (network === 'mainnet' || network === 'testnet-10' ? network : null);
const TRAILING_SLASHES = /\/+$/;
/** The URL of a network's `tokens.json` under `base`. */
export const kascovTokensUrl = (n: KascovNetwork, base = DEFAULT_BASE): string => `${base.replace(TRAILING_SLASHES, '')}/${n}/tokens.json`;
/**
 * Where the saved copies of the images are, `img` beside the data set's last path segment
 * (https://kascov.io/data -> https://kascov.io/img).
 */
export const kascovImagesBase = (base = DEFAULT_BASE): string => new URL('../img', `${base.replace(TRAILING_SLASHES, '')}/`).href.replace(TRAILING_SLASHES, '');
/**
 * The token's own page on the explorer, a link for the viewer to follow that the app never
 * fetches. It is `https://kascov.io/#/<network>/token/<covenant id>` on the origin of the data
 * set (`https://kascov.io/data` -> `https://kascov.io`). Null when the data set is off or its base
 * is not an https URL.
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

// --- The parsed list ---

/**
 * What the app reads of one kascov token. Every string is restricted, with control, zero-width
 * and bidi characters replaced and the length capped.
 */
export interface KascovToken {
  covenantId: string;
  /** `claimed_ticker`, the creator's claim. Null without one. */
  ticker: string | null;
  /** `claimed_name`. */
  name: string | null;
  /**
   * `claimed_decimals` (0..=18), a display claim only. It is compared with what KOB derives and
   * never used to trade.
   */
  decimals: number | null;
  /**
   * The token program's template hash as kascov names it (`standard.view_template_hash`). The
   * 46-byte KRON family has none.
   */
  templateHash: string | null;
  /** SHA-256 of the image's raw bytes, as the creator committed it (`claimed_image_hash`). */
  imageHash: string | null;
  /**
   * The address of the image as the creator claimed it (`claimed_image`), kept with or without a
   * hash. It is not validated here. data/logo-url.ts `parseLogoRef` decides whether the app may
   * load it at all: https or ipfs, no IP literal, and an SVG address only for checked bytes. The
   * checked path never fetches from it, because it checks the hash against kascov's saved copy
   * only. It is the address shown unchecked when kascov holds no copy
   * (ui/market/use-token-logo.ts).
   */
  imageUrl: string | null;
  /**
   * Whether kascov marks the token `verified` (`status`). That is its own third-party remark, and
   * KOB has not checked the token by it.
   */
  verified: boolean;
  /**
   * The token's total supply in base units as kascov reports it (`supply_str`, else a safe-integer
   * `supply`). It is null without one. It is also null for a token kascov says can still be
   * issued, with `issuance_remaining_str` other than 0, because the supply is not fixed. It is for
   * display only. The market list divides the last price by it for a market cap, labelled as
   * kascov's, and only for a token kascov marks `verified`.
   */
  supply?: bigint | null;
  /**
   * Whether kascov prices the token, meaning its `market` carries a valid spot. False for
   * `{ unpriced_reason }`, no `market`, or a spot that is not two positive safe integers.
   */
  priced: boolean;
  /**
   * kascov's spot, KAS sompi per base unit as the exact ratio `num / den`. Null when unpriced. It
   * is for display only and gates the chart's optional kascov line.
   */
  spot: KascovSpot | null;
}

/** kascov's spot price: `num` sompi per `den` base units, both positive. */
export interface KascovSpot { num: bigint; den: bigint }

/**
 * How long the list request (about 2.4 MB, 380 KB gzipped) may take, headers and body together.
 * On a slow phone link the answer can take more than 8 s. A cut request leaves every row
 * 'Unnamed token', and the next try waits out the backoff.
 */
export const KASCOV_LIST_TIMEOUT_MS = 30_000;

/** Why a list request gave nothing. */
export type KascovFailure = 'unsupported' | 'network' | 'timeout' | 'http' | 'too-large' | 'shape';
/** The parsed list with kascov's build time, or why there is none. */
export type KascovResult = { ok: true; tokens: KascovToken[]; generatedAtMs: number | null } | { ok: false; reason: KascovFailure };

/**
 * How often a view that shows names or icons may fetch the list again while it stays visible.
 * kascov regenerates the file about every 2 minutes and sends no ETag or Last-Modified. The file
 * is 0.8 MB on mainnet and 2.4 MB on testnet-10. This is the one constant of the policy
 * (docs/ops/web-hosting.md).
 */
export const KASCOV_REFRESH_MS = 120_000;
/** After a failed fetch the next try waits 30 s, then twice as long each time, at most 10 min. */
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

/** Returns an image URL the app may fetch: https only, no credentials, a plain length. */
export function safeImageUrl(v: unknown): string | null {
  if (typeof v !== 'string' || v.length > 2048) return null;
  try {
    const u = new URL(v);
    return u.protocol === 'https:' && u.username === '' && u.password === '' && u.hostname !== '' ? u.href : null;
  } catch {
    return null;
  }
}

/**
 * `supply_str` as exact digits, else a safe-integer `supply`. Null when absent or malformed, or
 * when the token can still be issued (`issuance_remaining_str` is not 0).
 */
function supplyOf(o: Record<string, unknown>): bigint | null {
  const remaining = o.issuance_remaining_str;
  if (typeof remaining === 'string' && !/^0+$/.test(remaining)) return null;
  if (typeof o.supply_str === 'string') return /^\d{1,40}$/.test(o.supply_str) ? BigInt(o.supply_str) : null;
  return typeof o.supply === 'number' && Number.isSafeInteger(o.supply) && o.supply >= 0 ? BigInt(o.supply) : null;
}

/**
 * Reads a positive safe integer, or a string of 1..30 digits without a leading zero, as a bigint.
 * Anything else gives null, such as a float, zero, a negative, other text or an object.
 */
export function positiveBigOf(v: unknown): bigint | null {
  if (typeof v === 'number') return Number.isSafeInteger(v) && v > 0 ? BigInt(v) : null;
  return typeof v === 'string' && /^[1-9]\d{0,29}$/.test(v) ? BigInt(v) : null;
}

/**
 * The spot of an entry's `market`, `spot_num_sompi` over `spot_den`. Null unless both are positive
 * integers. An unpriced market is `{ unpriced_reason }` and has neither.
 */
export function spotOf(market: unknown): KascovSpot | null {
  const m = asObject(market);
  const num = positiveBigOf(m?.spot_num_sompi);
  const den = positiveBigOf(m?.spot_den);
  return num !== null && den !== null ? { num, den } : null;
}

/** Parses one entry of `tokens`. Null when it has no usable covenant id. */
export function parseKascovToken(raw: unknown): KascovToken | null {
  const o = asObject(raw);
  const covenantId = hex64(o?.covenant_id);
  if (!o || !covenantId) return null;
  const dec = o.claimed_decimals;
  const imageHash = hex64(o.claimed_image_hash);
  const spot = spotOf(o.market);
  return {
    covenantId,
    ticker: text(o.claimed_ticker, 16),
    name: text(o.claimed_name, 64),
    decimals: typeof dec === 'number' && Number.isInteger(dec) && dec >= 0 && dec <= 18 ? dec : null,
    templateHash: hex64(asObject(o.standard)?.view_template_hash),
    imageHash,
    imageUrl: typeof o.claimed_image === 'string' && o.claimed_image.trim().length > 0 && o.claimed_image.length <= 2048 ? o.claimed_image.trim() : null,
    verified: o.status === 'verified',
    supply: supplyOf(o),
    priced: spot !== null,
    spot,
  };
}

/** `generated_at_ms` of a document, when kascov built it. Null if absent or not a plain number. */
export const generatedAtOf = (doc: unknown): number | null => {
  const g = asObject(doc)?.generated_at_ms;
  return typeof g === 'number' && Number.isSafeInteger(g) && g > 0 ? g : null;
};

/**
 * The tokens of a `tokens.json` document, or null when its shape is not the one this reader
 * knows. That is when `tokens` is not an array, the response is for another network, or every
 * entry is unusable. A single bad entry is skipped. A repeated covenant id keeps its first entry.
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

// --- The client, with a bounded memory cache ---

/** Options of `createKascovClient`. */
export interface KascovClientOptions {
  fetch?: typeof fetch;
  now?: () => number;
  /** The whole request, headers to last byte (default `KASCOV_LIST_TIMEOUT_MS`). */
  timeoutMs?: number;
  /** How long an answer is reused before the next fetch (default `KASCOV_REFRESH_MS`). */
  ttlMs?: number;
  /**
   * The wait after the first failure, doubling per further failure up to `maxBackoffMs`. The
   * defaults are 30 s and 10 min.
   */
  backoffMs?: number;
  maxBackoffMs?: number;
  /** The largest body read (default 12 MB). testnet-10's file is about 2.4 MB. */
  maxBytes?: number;
}

/** Loads and caches the kascov token list of each network. */
export interface KascovClient {
  /**
   * The list of `network`, the cached one while it is fresh, else from one shared request. It
   * never rejects. A failure after an earlier success keeps serving the earlier list, because
   * names do not change, and the next try waits out the back-off. An aborted `signal` ends only
   * this caller's wait.
   */
  load(network: string, o?: { signal?: AbortSignal | undefined; base?: string | undefined }): Promise<KascovResult>;
  /**
   * Milliseconds until `load(network)` will fetch again. It is at least 1 s, and the refresh period
   * when nothing is cached.
   */
  nextInMs(network: string, base?: string): number;
  /** Forgets every cached answer, for tests. */
  clear(): void;
}

/** Reads a response body up to `max` bytes. Null when it is larger. */
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
  /** The last good list, else the last failure. */
  result: KascovResult;
  /** When `load` fetches again. */
  nextAt: number;
  failures: number;
}

/** Makes a kascov list client with its own cache. */
export function createKascovClient(o: KascovClientOptions = {}): KascovClient {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const now = o.now ?? Date.now;
  const timeoutMs = o.timeoutMs ?? KASCOV_LIST_TIMEOUT_MS;
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
      // kascov did not build a new file. Keep the parsed list, the same object, so nothing
      // re-renders.
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
            // A failure after a success keeps the earlier list on show.
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


/** The app's one client, which holds the session cache. */
export const kascov: KascovClient = createKascovClient();

// --- Icons: fetch, hash, show ---

/** The raster formats the page shows as they are. */
export type ImageMime = 'image/png' | 'image/jpeg' | 'image/webp' | 'image/gif';

/**
 * The raster format of the bytes by their magic number. Null for anything else, such as SVG, HTML
 * or a truncated file.
 */
export function sniffImage(b: Uint8Array): ImageMime | null {
  if (b.length >= 8 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47 && b[4] === 0x0d && b[5] === 0x0a && b[6] === 0x1a && b[7] === 0x0a) return 'image/png';
  if (b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return 'image/jpeg';
  if (b.length >= 12 && b[0] === 0x52 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x46 && b[8] === 0x57 && b[9] === 0x45 && b[10] === 0x42 && b[11] === 0x50) return 'image/webp';
  if (b.length >= 6 && b[0] === 0x47 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x38 && (b[4] === 0x37 || b[4] === 0x39) && b[5] === 0x61) return 'image/gif';
  return null;
}

/**
 * Lower-case hex SHA-256 of the bytes. Null where the browser has no `crypto.subtle`, as on an
 * insecure page. No icon can then be checked, so none is shown.
 */
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

/** Options of `fetchVerifiedImage`. */
export interface IconFetchOptions {
  fetch?: typeof fetch;
  signal?: AbortSignal | undefined;
  /** Per image (default 8 s). */
  timeoutMs?: number;
  /** The largest image read (default 1.5 MB). The largest saved copy seen is 0.5 MB. */
  maxBytes?: number;
  sha256?: (b: Uint8Array) => Promise<string | null>;
  /** Called with the URL of a source that answered 404, because it holds no copy. */
  onMissing?: (url: string) => void;
  /**
   * Draws a checked file that is not PNG, JPEG, WEBP or GIF to a raster picture (default
   * `rasterize` of data/svg-logo.ts).
   */
  rasterize?: Rasterize;
}

/** Image bytes that hashed to the committed hash, as the page shows them, and their source. */
export interface VerifiedImage { bytes: Uint8Array; mime: ImageMime; url: string }

/**
 * The first of `urls` whose bytes hash to `hash` and are a picture the page can show
 * (data/svg-logo.ts). Null when none does. The URLs are tried in order, https only. A failed fetch
 * (CORS, 404, timeout, too large), bytes that hash otherwise and a file that is not a showable
 * picture are all skipped alike, and the caller shows a letter avatar.
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
      const res = await fetchFn(url, { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', redirect: 'error' });
      if (!res.ok) {
        if (res.status === 404) o.onMissing?.(url);
        continue;
      }
      const bytes = await readCapped(res, o.maxBytes ?? 1_500_000);
      if (!bytes || bytes.length === 0) continue;
      if (!isPicture(bytes)) continue;
      if ((await digest(bytes)) !== want) continue;
      // Shown as they are, or for SVG, AVIF and the like as the picture drawn from the original
      // bytes (data/svg-logo.ts).
      const r = await shownPicture(bytes, o.rasterize);
      if (r) return { bytes: r.bytes, mime: r.mime, url };
    } catch {
      /* CORS, offline, aborted or a bad body. This source gave nothing. */
    } finally {
      clearTimeout(timer);
      o.signal?.removeEventListener('abort', onAbort);
    }
  }
  return null;
}

/**
 * The sources of a token's icon. That is kascov's saved copy only, never the creator's own host,
 * whose URL would give the viewer's IP away. Empty without a committed hash.
 */
export function iconSources(n: KascovNetwork, t: Pick<KascovToken, 'covenantId' | 'imageHash'> & Partial<Pick<KascovToken, 'imageUrl'>>, base = DEFAULT_BASE): string[] {
  if (!t.imageHash || !base) return [];
  return [kascovImageUrl(n, t.covenantId, base)];
}

/** Options of `createIconLoader`. */
export interface IconLoaderOptions extends IconFetchOptions {
  /** Images fetched at once (default 4). */
  limit?: number;
  /** Makes the blob URL of verified bytes (default `URL.createObjectURL`). */
  makeUrl?: (bytes: Uint8Array, mime: ImageMime) => string;
  now?: () => number;
  /** How long a failed icon is not tried again in this session (default 5 min). */
  failTtlMs?: number;
  /**
   * Where kascov.io's 404s for saved copies are remembered. The default is this browser's storage
   * for a day, and `null` remembers none.
   */
  missing?: MissingCopies | null;
}

/** Loads verified token icons as blob URLs. */
export interface IconLoader {
  /**
   * The blob URL of the verified icon, null when there is none. It never rejects. The caller does
   * not call it at all while icons are turned off.
   */
  load(n: KascovNetwork, t: Pick<KascovToken, 'covenantId' | 'imageHash' | 'imageUrl'>, o?: { signal?: AbortSignal | undefined; base?: string | undefined }): Promise<string | null>;
  clear(): void;
}

/**
 * Saved copies kascov.io answered 404 for, kept per network and covenant id. kascov keeps one for
 * most tokens that commit to an image, not for all. A browser prints every 404 in its console,
 * and only not asking again stops that. So a token whose copy is missing is not asked for it for a
 * day, and the icon falls back to its letter quietly. The creator's own host is never asked on
 * this checked path.
 */
export interface MissingCopies {
  has(network: string, covenantId: string): boolean;
  add(network: string, covenantId: string): void;
}

export const MISSING_COPY_KEY = 'kob.icons.missing.v1';
export const MISSING_COPY_TTL_MS = 24 * 3_600_000;
const MISSING_COPY_MAX = 1000;

/**
 * The remembered 404s in `storage`. With a failing or absent storage they are kept in memory only,
 * for the session.
 */
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
      /* An unreadable store is an empty one. */
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
        /* Storage is full or blocked. The session's memory still holds it. */
      }
    },
  };
}

/** Makes an icon loader with its own verified-image cache. */
export function createIconLoader(o: IconLoaderOptions = {}): IconLoader {
  const limit = Math.max(1, o.limit ?? 4);
  const now = o.now ?? Date.now;
  const failTtlMs = o.failTtlMs ?? 300_000;
  const missing = o.missing === undefined ? createMissingCopies() : o.missing;
  const makeUrl = o.makeUrl ?? ((bytes, mime) => URL.createObjectURL(new Blob([bytes as Uint8Array<ArrayBuffer>], { type: mime })));
  // Verified images by their hash. Bytes that hash to H are the image H, whichever token committed
  // to it, so a hash proven once serves every token that names it. Nothing else is shared between
  // tokens. A request in flight and a miss belong to one token and its own sources, so a token
  // whose sources serve other bytes cannot make another token's icon fail.
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
      // The saved copy kascov.io answered 404 for is not asked for again, because each 404 is a
      // line in the console.
      const sources = iconSources(n, t, base).filter((u) => !(u === copy && missing?.has(n, t.covenantId)));
      // The fetch is not tied to the caller's signal. A row scrolled away or a view left must not
      // cancel what the next visit will use.
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

/** The app's one icon loader, which holds the session's verified images. */
export const iconLoader: IconLoader = createIconLoader();
