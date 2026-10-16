// The order button without waiting for the indexer, and the re-check that gates the signature (pure).
//
// The freshness rule of a click on the order button (OrderTicket `prepareLevel`):
//  1. The order is planned at once from the environment already on hand (use-ticket.ts) when its oldest part was read at most REVIEW_CACHE_MS ago,
//     with the node clock the ticket keeps fresh in the background (read every CLOCK_TICK_MS) projected to the click when it is at most
//     CLOCK_REUSE_MS old (`liveClockAt`), else the node clock read at the click (or, when the node takes longer than CLOCK_WAIT_MS, the cached
//     clock projected to now: `projectClock`). An auction that starts from it is still checked against the node clock once signed
//     (wallet/sign.ts `submitByDaa`).
//     Inputs of every transaction this page has submitted are left out of it, however old the read (app/pending-spends.ts). An environment older
//     than that, or a plan from it that has errors (holdings still being read for a sell ...), waits for a read started at most REVIEW_REUSE_MS
//     before the click instead.
//  2. The order signed is the plan of the environment AT THE CLICK: the book, the wallet's own orders (self-trade) and its holdings. When the parts
//     read again plan it into another transaction (a moved book, coins replaced after a submission), that one is signed instead, as long as it
//     keeps the terms the ticket showed (`withinTerms`: a buy pays at most, a sell receives at least the total shown; a market / close order's
//     slippage is narrowed to end at the worst price shown, `slippageWithin`); an error, or a term it cannot keep, stops it. Warnings never stop
//     it. The built transaction is decoded against the plan it was built from, whichever it is (ui/confirm/send.ts). A part on hand is the state at the click when
//     it is complete, no read of it is running or queued, no notice concerning it arrived since its read started, and either its read started within REVIEW_REUSE_MS of the click, or (`partCurrent`) the
//     feed has delivered its notices without a gap since before that read (IndexerFeed.liveSince: every change of the book or of the wallet's
//     orders sends one, and a notice starts a new read), the read is at most REVIEW_CACHE_MS old and this page has submitted nothing since. Only
//     the parts that are not are read again (from REVIEW_REUSE_MS before the click on); when none needs it, signing waits for nothing but rule 3.
//  3. The send's own checks (ui/confirm/send.ts) also re-read every input on the NODE (kob/node-verify.ts) and stop the order unless each one is
//     confirmed unspent with the amount and script the transaction commits to. The node, not the indexer, is the authority for the coins.
import type { Clock, OrderPlan } from '../../kob/plan-types';
import type { BuiltTx } from '../../kob/types';

/** a read this recent (and nothing newer on its way) is the state at the click, whatever the feed */
export const REVIEW_REUSE_MS = 3_000;
/** the order is planned at once from parts read at most this long ago (the background keeps them younger: ENV_REFRESH_MS) */
export const REVIEW_CACHE_MS = 30_000;
/** how long the order button waits for the node clock before it projects the cached one */
export const CLOCK_WAIT_MS = 500;
/** the ticket reads the node clock this often while it can place an order (a click then needs no round trip for it) */
export const CLOCK_TICK_MS = 2_000;
/** a background clock read at most this long before the click is projected to the click instead of reading the node again */
export const CLOCK_REUSE_MS = 3_000;

/** The background clock (`live`, read at `at`) projected to `now` when it is at most CLOCK_REUSE_MS old; null when there is none that fresh. */
export function liveClockAt(live: { clock: Clock; at: number } | null, now: number): Clock | null {
  if (!live || now < live.at || now - live.at > CLOCK_REUSE_MS) return null;
  return projectClock(live.clock, now - live.at);
}

/**
 * `c` advanced by the whole seconds of `elapsedMs` at its measured rate (DAA rounded down). Both halves of the pair move together, so a time
 * converted with it (a timed start, a GTD expiry, a day order) lands where a fresh read would put it, up to the rate's drift over the elapsed
 * time; the DAA is never ahead of that (an IOC life counted from it is never longer).
 */
export function projectClock(c: Clock, elapsedMs: number): Clock {
  const sec = BigInt(Math.max(0, Math.floor(elapsedMs / 1000)));
  return { ...c, unixSeconds: c.unixSeconds + sec, daa: c.daa + (sec * BigInt(Math.max(0, Math.round(c.rateMilli)))) / 1000n };
}

/** The clock of a click: `read()` when it answers within `waitMs`, else (or when it fails) `cached` projected from `cachedAtMs` to now. */
export async function reviewClock(
  read: () => Promise<Clock>,
  cached: Clock,
  cachedAtMs: number,
  o: { now?: () => number; waitMs?: number } = {},
): Promise<{ clock: Clock; projected: boolean }> {
  const now = o.now ?? Date.now;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const late = new Promise<null>((res) => (timer = setTimeout(() => res(null), o.waitMs ?? CLOCK_WAIT_MS)));
  const got = await Promise.race([read().catch(() => null), late]);
  if (timer) clearTimeout(timer);
  return got ? { clock: got, projected: false } : { clock: projectClock(cached, now() - cachedAtMs), projected: true };
}

/**
 * One part of the environment as the re-check sees it: the start of its newest read (null: none yet), whether a newer read is running or queued, and
 * when the newest notice concerning it arrived (0: none; a notice is recorded on arrival, before the feed's debounce starts its read).
 */
export interface PartFacts {
  at: number | null;
  busy: boolean;
  noticeAt: number;
}

/**
 * Whether a part on hand still is the state at `clickAt` (rule 2). `liveSince`: since when the feed has delivered the part's notices without a gap
 * (null: not live, or no feed); `lastSubmitAt`: this page's newest submission (its spends change the wallet's coins and orders).
 */
export function partCurrent(p: PartFacts, clickAt: number, ctx: { liveSince: number | null; lastSubmitAt: number }): boolean {
  // a change known since the read (a notice, a submission of this page), whose read has not started yet or is still running
  if (p.at === null || p.busy || p.at < ctx.lastSubmitAt || p.noticeAt >= p.at) return false;
  if (p.at >= clickAt - REVIEW_REUSE_MS) return true;
  return ctx.liveSince !== null && ctx.liveSince <= p.at && clickAt - p.at <= REVIEW_CACHE_MS;
}

const txKey = (b: BuiltTx): string => JSON.stringify(b.tx, (_k, v) => (typeof v === 'bigint' ? `${v}n` : v));

/** The same transaction, byte for byte (the unsigned JSON: inputs, outputs, scripts, fee). */
export const sameTransaction = (a: BuiltTx, b: BuiltTx): boolean => txKey(a) === txKey(b);

/** How far (bps of the shown total, plus one sompi) a plan made at the click may pass the total the ticket showed: rounding of a moved book. */
export const TERMS_ROUND_BPS = 1n;

/**
 * Whether a plan made at the click (`fresh`) keeps the terms the ticket showed (`shown`): a buy pays at most the all-in total it showed, a sell
 * receives at least it (the covenant's bound at the worst price, tip included), up to TERMS_ROUND_BPS. A start price, a coin selection, a change
 * or a fee that differ do not count: the plan made at the click is the order to sign. No shown total (no plan shown, a kind without one): kept.
 */
export function withinTerms(shown: Pick<OrderPlan, 'disclosure'> | null, fresh: Pick<OrderPlan, 'disclosure'>): boolean {
  const a = shown?.disclosure?.allInTotal ?? null;
  const b = fresh.disclosure?.allInTotal ?? null;
  if (a === null || b === null || shown!.disclosure!.side !== fresh.disclosure!.side) return true;
  const slack = (a * TERMS_ROUND_BPS) / 10_000n + 1n;
  return fresh.disclosure!.side === 'buy' ? b <= a + slack : b + slack >= a;
}

/**
 * The slippage (bps) that keeps an auction starting at `touch` within `worst`, the worst price the ticket showed (a buy pays at most it, a sell
 * receives at least it); null when the touch itself is beyond it (the market moved past the user's bound).
 */
export function slippageWithin(side: 'buy' | 'sell', touch: bigint, worst: bigint): bigint | null {
  if (touch <= 0n) return null;
  const room = side === 'buy' ? worst - touch : touch - worst;
  const bps = room > 0n ? (room * 10_000n) / touch : 0n;
  return bps >= 1n ? bps : null;
}
