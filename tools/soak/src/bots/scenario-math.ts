// Pure parts of the trader scenarios added after the 2026-10-13 gap review (no `@/` imports: unit-tested with plain `node --test`):
// the limit ladder's setting and rung size, the choice of the GTC order to renew, the dated pair limits (GTD, day, timed activation),
// the pair limit a pair amend moves, and the token merge's selection summary. traders.ts turns them into web-planner intents.

/** The web ticket's limit ladder bounds (web/src/ui/ticket/form-state.ts MIN_ / MAX_LIMIT_LADDER_LEVELS). */
export const LADDER_MIN_LEVELS = 2;
export const LADDER_MAX_LEVELS = 20;

type Rand = () => number;
const between = (r: Rand, lo: number, hi: number) => lo + r() * (hi - lo);

/**
 * Rungs of one ladder, 2 to 20: most ladders are short (2 to 6 rungs, 70 %), the rest anywhere up to the ticket's 20, so the long ladders (one
 * transaction per rung, signed in turn) appear every few hours per trader instead of using up the wallet's KAS on every draw.
 */
export function ladderLevels(r: Rand = Math.random): number {
  const n = r() < 0.7 ? 2 + Math.floor(r() * 5) : 7 + Math.floor(r() * 14);
  return Math.min(LADDER_MAX_LEVELS, Math.max(LADDER_MIN_LEVELS, n));
}

/** The structural `LimitLadderSetting` of web/src/ui/ticket/ladder.ts. */
export type LadderSetting = { levels: number } & ({ mode: 'pct'; bps: bigint } | { mode: 'end'; end: bigint });

/**
 * The ladder's setting as a user types it: half the time a percent step (3 to 40 bps between rungs, compounded), half the time an end price 30 to
 * 300 bps behind the start (a buy ladder ends below its start, a sell ladder above). The web's `limitLadderPrices` rounds and refuses collapsed rungs.
 */
export function ladderSetting(side: 'buy' | 'sell', start: bigint, levels: number, r: Rand = Math.random): LadderSetting {
  if (r() < 0.5) return { levels, mode: 'pct', bps: BigInt(Math.round(between(r, 3, 40))) };
  const bps = BigInt(Math.round(between(r, 30, 300) * 100));
  const move = (start * bps) / 1_000_000n;
  return { levels, mode: 'end', end: side === 'buy' ? start - move : start + move };
}

/**
 * Base units of ONE rung: the ladder's budget (`budget` base units, the whole ladder) split evenly over its rungs, never below `min` (half a
 * dollar of the token): every rung of a ladder has the same size, as in the ticket.
 */
export function rungSize(budget: bigint, levels: number, min: bigint): bigint {
  const each = budget / BigInt(Math.max(1, levels));
  return each < min ? min : each;
}

/**
 * The renewal candidate among a trader's orders: the OLDEST (lowest placement DAA) of those the caller found renewable (a live GTC limit or
 * stop / take-profit / OCO, not an exit, not already being replaced). A real user renews when the reminder (day 85 of 90) comes; the soak's
 * orders are hours old, so the oldest stands in for the one due first. null when none.
 */
export function oldestFirst<T extends { genesis: { daa: number } }>(views: readonly T[]): T | null {
  let best: T | null = null;
  for (const v of views) if (best === null || v.genesis.daa < best.genesis.daa) best = v;
  return best;
}

/** A dated pair limit: the structural `LimitIntent` of web/src/kob/intent-simple.ts. */
export interface DatedLimit {
  type: 'limit';
  side: 'buy' | 'sell';
  amount: bigint;
  price: bigint;
  lifetime: { kind: 'gtd'; at: bigint } | { kind: 'day' };
  activeFrom?: { unixSeconds: bigint };
}

/**
 * A pair limit with a lifetime (the pair ticket's GTD / day / timed activation; the plain pair limits of the soak are GTD of 5 to 20 min):
 *   gtd    resting 30 to 120 min, then the keepers refund it (its custody of A or B goes back to the maker)
 *   day    ends at the next 00:00 UTC (the planner computes the deadline and its on-chain expiry)
 *   timed  inactive for 2 to 6 min (activeFrom), then a GTD of 30 to 90 min from its activation: the matcher must not fill it before
 */
export function datedPairLimit(kind: 'gtd' | 'day' | 'timed', side: 'buy' | 'sell', amount: bigint, price: bigint, nowUnix: bigint, r: Rand = Math.random): DatedLimit {
  const min = (lo: number, hi: number) => BigInt(Math.round(between(r, lo, hi) * 60));
  if (kind === 'day') return { type: 'limit', side, amount, price, lifetime: { kind: 'day' } };
  if (kind === 'gtd') return { type: 'limit', side, amount, price, lifetime: { kind: 'gtd', at: nowUnix + min(30, 120) } };
  const from = nowUnix + min(2, 6);
  return { type: 'limit', side, amount, price, activeFrom: { unixSeconds: from }, lifetime: { kind: 'gtd', at: from + min(30, 90) } };
}

/**
 * The new price of a pair amend: the resting pair limit moves 5 to 25 bps away from the market (an ask up, a bid down), at least one unit, so
 * the replacement rests again instead of crossing (prices are B base units per whole A).
 */
export function pairAmendPrice(side: 'buy' | 'sell', price: bigint, r: Rand = Math.random): bigint {
  const bps = BigInt(Math.round(between(r, 5, 25) * 100));
  let move = (price * bps) / 1_000_000n;
  if (move < 1n) move = 1n;
  const p = side === 'sell' ? price + move : price - move;
  return p < 1n ? 1n : p;
}

/**
 * The reference of an external token's KAS book (no Binance price: config `trade.refKas`): the book's touch midpoint (one side when only one) when it
 * lies within `maxBps` of the configured reference, else the configured one (sompi per whole token).
 */
export function externalReference(t: { bid: bigint | null; ask: bigint | null }, configured: bigint, maxBps: number): bigint {
  const mid = t.bid !== null && t.ask !== null ? (t.bid + t.ask) / 2n : (t.bid ?? t.ask);
  if (mid === null || mid <= 0n || configured <= 0n) return configured;
  const dist = mid > configured ? mid - configured : configured - mid;
  return dist * 10_000n <= configured * BigInt(Math.round(maxBps)) ? mid : configured;
}

/**
 * The reference of a token with a fixed KAS price (`perToken` sompi per whole token, an external token's `trade.refKas`): its USD value follows the
 * KAS feed `kas`; null while the KAS feed is stale (the bots pause on that book).
 */
export function fixedKasRef(kas: { kasUsd: number; ts: number } | null, perToken: bigint): { kasUsd: number; perToken: bigint; usd: number; ts: number } | null {
  if (!kas || perToken <= 0n) return null;
  return { kasUsd: kas.kasUsd, perToken, usd: (Number(perToken) / 1e8) * kas.kasUsd, ts: kas.ts };
}

/** The merged total of a merge's inputs (base units). */
export const mergeTotal = (inputs: readonly { amount: string }[]): bigint => inputs.reduce((s, u) => s + BigInt(u.amount), 0n);
