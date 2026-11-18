// kascov.io's price history of one token, for the chart's optional third-party line
// (ui/market/PriceChart.tsx). Nothing here runs unless the viewer turns that line on.
//
//     GET https://kascov.io/data/{network}/token/{covenant id}/candles?bucket={15m|1h|1d}&phase=all
//         -> { bucket, bucket_ms, token_id, network,
//              candles: [{ t (ms), close: { base_amount, quote_sompi }, carried, ... }], ... }
//     It is served with CORS `*` and max-age=30.
//
// It is kascov's own replay of the pools and curves of other venues (KaspaRocket, KRON, ...), a
// marginal price before fees. It is not KOB's price, and KOB uses it for no figure, no warning
// and no check. Only the close of each candle is read, as the exact ratio
// `quote_sompi / base_amount` in sompi per base unit, like the spot in data/kascov.ts. Parsing is
// strict. A candle without two positive safe integers (or digit strings) and a safe-integer time
// is skipped. A document for another bucket, token or network, or whose `candles` is not a list,
// is a failure. Every failure is an `ok: false` result, never a throw.
//
// The privacy cost is that this request names the token the viewer is looking at. kascov.io
// learns it, and the viewer's IP address, but only while the line is on. The token list request
// does not. The same CSP as the list applies (connect-src https:, web/csp.mjs), so there is no
// new host.
import type { CandleInterval } from './indexer-types';
import { DEFAULT_BASE, kascovNetwork, positiveBigOf, readCapped, type KascovNetwork } from './kascov';

/**
 * The buckets used. kascov also serves 1m, 4h and 1w, which are not worth the bytes here. 1m is
 * about 10 MB for a token with a month of history, mostly carried minutes.
 */
export type KascovBucket = '15m' | '1h' | '1d';

/**
 * The kascov bucket drawn under a KOB candle interval.
 * - 1m and 5m use 15m, the finest kascov bucket that stays small. The line is then a step per 15
 *   minutes under those bars.
 * - 1h uses 1h and 1d uses 1d.
 * The line is laid onto KOB's own bars afterwards (ui/market/kascov-overlay.ts), so a coarser
 * bucket never adds a point to the chart's time axis.
 */
export function kascovBucketFor(interval: CandleInterval): KascovBucket {
  return interval === '1h' ? '1h' : interval === '1d' ? '1d' : '15m';
}

/** Milliseconds of a bucket. */
export const KASCOV_BUCKET_MS: Record<KascovBucket, number> = { '15m': 900_000, '1h': 3_600_000, '1d': 86_400_000 };

/**
 * One candle's close, `quoteSompi / baseAmount` sompi per base unit, at the bucket that starts at
 * `t` (ms).
 */
export interface KascovCandle { t: number; quoteSompi: bigint; baseAmount: bigint }

/** Why a candle request gave nothing. */
export type KascovCandlesFailure = 'unsupported' | 'network' | 'timeout' | 'http' | 'too-large' | 'shape';
export type KascovCandlesResult = { ok: true; candles: KascovCandle[] } | { ok: false; reason: KascovCandlesFailure };

/** How long one candle request may take, headers and body together. */
export const KASCOV_CANDLES_TIMEOUT_MS = 15_000;
/** The largest body read. The 15m series of a token with a month of history is about 0.75 MB. */
export const KASCOV_CANDLES_MAX_BYTES = 6_000_000;
/** At most this many of the newest candles are kept. */
export const KASCOV_CANDLES_MAX = 6_000;

const TRAILING_SLASHES = /\/+$/;
const HEX64 = /^[0-9a-f]{64}$/;

/** The URL of one token's candles in one bucket under `base`. */
export const kascovCandlesUrl = (n: KascovNetwork, covenantId: string, bucket: KascovBucket, base = DEFAULT_BASE): string =>
  `${base.replace(TRAILING_SLASHES, '')}/${n}/token/${covenantId}/candles?bucket=${bucket}&phase=all`;

const asObject = (v: unknown): Record<string, unknown> | null => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null);

/**
 * The closes of a candles document, oldest first, one per time. Null when the document is not the
 * one asked for, by bucket, token or network, or `candles` is not a list. A candle that is not
 * usable is skipped.
 */
export function parseKascovCandles(doc: unknown, want: { network: KascovNetwork; covenantId: string; bucket: KascovBucket }): KascovCandle[] | null {
  const root = asObject(doc);
  if (!root || !Array.isArray(root.candles)) return null;
  if (root.bucket !== want.bucket) return null;
  if (root.network !== undefined && root.network !== want.network) return null;
  if (root.token_id !== undefined && (typeof root.token_id !== 'string' || root.token_id.toLowerCase() !== want.covenantId)) return null;
  const byTime = new Map<number, KascovCandle>();
  for (const raw of root.candles) {
    const c = asObject(raw);
    const close = asObject(c?.close);
    const t = c?.t;
    const quoteSompi = positiveBigOf(close?.quote_sompi);
    const baseAmount = positiveBigOf(close?.base_amount);
    if (typeof t !== 'number' || !Number.isSafeInteger(t) || t <= 0 || quoteSompi === null || baseAmount === null) continue;
    byTime.set(t, { t, quoteSompi, baseAmount });
  }
  const all = [...byTime.values()].sort((a, b) => a.t - b.t);
  return all.length > KASCOV_CANDLES_MAX ? all.slice(all.length - KASCOV_CANDLES_MAX) : all;
}

/** Options of `fetchKascovCandles`. */
export interface KascovCandlesOptions {
  fetch?: typeof fetch;
  signal?: AbortSignal | undefined;
  /** The data set's base (`AppConfig.tokenNamesUrl`). An empty string turns it off. */
  base?: string;
  timeoutMs?: number;
  maxBytes?: number;
}

/** The candles of one token, or why not. Never throws. An aborted `signal` ends it as `network`. */
export async function fetchKascovCandles(network: string, covenantId: string, bucket: KascovBucket, o: KascovCandlesOptions = {}): Promise<KascovCandlesResult> {
  const n = kascovNetwork(network);
  const base = (o.base ?? DEFAULT_BASE).replace(TRAILING_SLASHES, '');
  const id = covenantId.toLowerCase();
  if (!n || base === '' || !HEX64.test(id)) return { ok: false, reason: 'unsupported' };
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), o.timeoutMs ?? KASCOV_CANDLES_TIMEOUT_MS);
  const onAbort = () => ctl.abort();
  o.signal?.addEventListener('abort', onAbort, { once: true });
  try {
    if (o.signal?.aborted) return { ok: false, reason: 'network' };
    const res = await fetchFn(kascovCandlesUrl(n, id, bucket, base), { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', headers: { accept: 'application/json' } });
    if (!res.ok) return { ok: false, reason: 'http' };
    const body = await readCapped(res, o.maxBytes ?? KASCOV_CANDLES_MAX_BYTES);
    if (!body) return { ok: false, reason: 'too-large' };
    const candles = parseKascovCandles(JSON.parse(new TextDecoder().decode(body)), { network: n, covenantId: id, bucket });
    return candles ? { ok: true, candles } : { ok: false, reason: 'shape' };
  } catch (e) {
    return { ok: false, reason: e instanceof SyntaxError ? 'shape' : o.signal?.aborted ? 'network' : ctl.signal.aborted ? 'timeout' : 'network' };
  } finally {
    clearTimeout(timer);
    o.signal?.removeEventListener('abort', onAbort);
  }
}
