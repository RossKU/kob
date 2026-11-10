// The KAS/USD price the indexer serves (`GET /v1/price/kas-usd`, an opt-in route of kobd: `[api] usd_price`). The browser never asks a price API
// itself (the CSP stays closed, a viewer's address does not go to a third party): the indexer fetches the price, caches it and says where it
// came from and when. Display only: no figure the user types or signs is ever converted with it (app/fiat.ts).

/** The route's answer: `usd` per KAS, `at` unix SECONDS of the fetch, `source` = `coingecko`, or `vwap:<exchange>,<exchange>,...` (USDT taken as USD). */
export interface KasUsdQuote {
  usd: number;
  at: number;
  source: string;
}

/** The app asks every minute, only while the page is shown and USD is the display currency (the server refreshes its upstream at most that often, and a CDN keeps the answer a minute). */
export const KAS_USD_POLL_MS = 60_000;
/** After a failed read the wait doubles each time, up to this (10 minutes); the first success brings back the normal pace. */
export const KAS_USD_MAX_BACKOFF_MS = 10 * 60_000;
/** An indexer that has no such route (404) is asked again after this, ms. */
export const KAS_USD_RETRY_UNSUPPORTED_MS = 10 * 60_000;

/** A price this far out is nonsense (the server refuses it too). */
const MAX_USD = 1_000;
const SOURCE = /^[a-z0-9]{1,16}(:[a-z0-9]{1,16}(,[a-z0-9]{1,16}){0,7})?$/;

/** The answer as a quote, or null for anything else (a wrong shape is "no price", never a number shown). */
export function adaptKasUsd(raw: unknown): KasUsdQuote | null {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return null;
  const r = raw as Record<string, unknown>;
  const { usd, at, source } = r;
  if (typeof usd !== 'number' || !Number.isFinite(usd) || usd <= 0 || usd >= MAX_USD) return null;
  if (typeof at !== 'number' || !Number.isSafeInteger(at) || at <= 0) return null;
  if (typeof source !== 'string' || !SOURCE.test(source)) return null;
  return { usd, at, source };
}
