// Reads right after the user's own action. The indexer's answers are kept for a second or more by a CDN in front of it (docs/api-reference.md 5.7), so the book
// or the orders read a moment after the user placed, cancelled, amended or refunded an order could still be the old ones. For FRESH_MS after such an action every
// read of a live route carries the bypass parameter `_nc=<unique>`: the server ignores it, the CDN keys its cache on the whole URL (a miss) and the answer is
// `no-store`. Only this browser's reads are affected; nothing else changes, and an indexer that has never heard of the parameter ignores it too.

/** How long after an own action the live reads bypass the cache: longer than the 1 to 3 s a cached answer lives, with room for the indexer to apply the block. */
export const FRESH_MS = 30_000;

/** The query parameter of the bypass (docs/api-reference.md 5.7). */
export const BYPASS_PARAM = '_nc';

let lastAction = Number.NEGATIVE_INFINITY;
let seq = 0;

/** The user's own transaction was just sent (a placement, cancel, amend, refund). */
export function markOwnAction(now: number = Date.now()): void {
  lastAction = now;
}

/** Whether the last own action is recent enough that live reads should bypass the cache. */
export function recentOwnAction(now: number = Date.now()): boolean {
  return now - lastAction < FRESH_MS;
}

/** Forget the last action (tests). */
export function resetOwnAction(): void {
  lastAction = Number.NEGATIVE_INFINITY;
}

/** A value no two reads share (the cache-busting parameter). */
export function bypassValue(now: number = Date.now()): string {
  return `${now.toString(36)}${(seq++ % 1296).toString(36)}`;
}

/**
 * The routes whose answers an own action changes (the books, depth and tape of its token, the orders and their events, the fills, the pair book and pair fills,
 * the maker's strays and token UTXOs): their path under `/v1`. Candles, lists, health and the totals are not read afresh.
 */
export function isLiveRoute(path: string): boolean {
  return /^\/v1\/(books|depth|trades|orders|fills|strays|token-utxos|token-events|stats\/[0-9a-fA-F]+)(\/|$)/.test(path) || /^\/v1\/pairs\/[^/]+\/[^/]+\/(book|fills)$/.test(path);
}
