// Reads right after the user's own action.
//
// A CDN in front of the indexer keeps its answers for a second or more (docs/api-reference.md
// 5.7). So the book or the orders read a moment after the user placed, cancelled, amended or
// refunded an order could still be the old ones. For FRESH_MS after such an action, every read of
// a live route carries the bypass parameter `_nc=<unique>`. The server ignores it, the CDN keys
// its cache on the whole URL and misses, and the answer is `no-store`. Only this browser's reads
// are affected, and nothing else changes. An indexer that has never heard of the parameter
// ignores it too.

/**
 * How long after an own action the live reads bypass the cache. It is longer than the 1 to 3 s a
 * cached answer lives, with room for the indexer to apply the block.
 */
export const FRESH_MS = 30_000;

/** The query parameter of the bypass (docs/api-reference.md 5.7). */
export const BYPASS_PARAM = '_nc';

let lastAction = Number.NEGATIVE_INFINITY;
let seq = 0;

/** Records that the user's own transaction was just sent: a placement, cancel, amend or refund. */
export function markOwnAction(now: number = Date.now()): void {
  lastAction = now;
}

/** Whether the last own action is recent enough that live reads should bypass the cache. */
export function recentOwnAction(now: number = Date.now()): boolean {
  return now - lastAction < FRESH_MS;
}

/** Forgets the last action, for tests. */
export function resetOwnAction(): void {
  lastAction = Number.NEGATIVE_INFINITY;
}

/** A value no two reads share, for the cache-busting parameter. */
export function bypassValue(now: number = Date.now()): string {
  return `${now.toString(36)}${(seq++ % 1296).toString(36)}`;
}

/**
 * Whether `path` under `/v1` is a route whose answers an own action changes. Those are:
 * - the books, depth, tape and 24 h stats of a token,
 * - the orders and their events, and the fills,
 * - the pair book and pair fills,
 * - the strays, token UTXOs and token events.
 * Candles, lists, health and the activity totals are not read afresh.
 */
export function isLiveRoute(path: string): boolean {
  return /^\/v1\/(books|depth|trades|orders|fills|strays|token-utxos|token-events|stats\/[0-9a-fA-F]+)(\/|$)/.test(path) || /^\/v1\/pairs\/[^/]+\/[^/]+\/(book|fills)$/.test(path);
}
