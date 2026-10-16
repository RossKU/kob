// Review without waiting for the indexer, and the re-check that gates the signature (pure).
//
// The freshness rule of a Review (OrderTicket `prepareLevel`):
//  1. The confirmation opens from the environment already on hand (use-ticket.ts) when its oldest part was read at most REVIEW_CACHE_MS ago, planned
//     with the node clock read at the click (or, when the node takes longer than CLOCK_WAIT_MS, the cached clock projected to now: `projectClock`).
//     Inputs of every transaction this page has submitted are left out of it, however old the read (app/pending-spends.ts). An environment older
//     than that, or a plan from it that has errors (holdings still being read for a sell ...), waits for a read started at most REVIEW_REUSE_MS
//     before the click instead.
//  2. Signing stays blocked until the environment AT THE CLICK plans the SAME order into the SAME transaction, with no error and no warning the screen
//     did not show (`recheckVerdict`): the book, the wallet's own orders (self-trade) and its holdings. A part on hand is the state at the click when
//     it is complete, no read of it is running or queued, no notice concerning it arrived since its read started, and either its read started within REVIEW_REUSE_MS of the click, or (`partCurrent`) the
//     feed has delivered its notices without a gap since before that read (IndexerFeed.liveSince: every change of the book or of the wallet's
//     orders sends one, and a notice starts a new read), the read is at most REVIEW_CACHE_MS old and this page has submitted nothing since. Only
//     the parts that are not are read again (from REVIEW_REUSE_MS before the click on); when none needs it, signing waits for nothing but rule 3.
//  3. As before, the confirmation screen also re-reads every input on the NODE (kob/node-verify.ts) and blocks signing until each one is confirmed
//     unspent with the amount and script the transaction commits to. The node, not the indexer, is the authority for the coins.
import { errors, type Clock, type OrderPlan, type PlanIssue } from '../../kob/plan-types';
import type { BuiltTx } from '../../kob/types';
import { newWarnings } from './plan-expected';

/** a read this recent (and nothing newer on its way) is the state at the click, whatever the feed */
export const REVIEW_REUSE_MS = 3_000;
/** the confirmation opens at once from parts read at most this long ago (the background keeps them younger: ENV_REFRESH_MS) */
export const REVIEW_CACHE_MS = 30_000;
/** how long Review waits for the node clock before it projects the cached one */
export const CLOCK_WAIT_MS = 500;

/**
 * `c` advanced by the whole seconds of `elapsedMs` at its measured rate (DAA rounded down). Both halves of the pair move together, so a time
 * converted with it (a timed start, a GTD expiry, a day order) lands where a fresh read would put it, up to the rate's drift over the elapsed
 * time; the DAA is never ahead of that (an IOC life counted from it is never longer).
 */
export function projectClock(c: Clock, elapsedMs: number): Clock {
  const sec = BigInt(Math.max(0, Math.floor(elapsedMs / 1000)));
  return { ...c, unixSeconds: c.unixSeconds + sec, daa: c.daa + (sec * BigInt(Math.max(0, Math.round(c.rateMilli)))) / 1000n };
}

/** The clock of a Review: `read()` when it answers within `waitMs`, else (or when it fails) `cached` projected from `cachedAtMs` to now. */
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

/** The outcome of the re-check: `issues` are what changed in words (errors, or warnings the screen did not show); none: another transaction. */
export type RecheckVerdict = { ok: true } | { ok: false; issues: PlanIssue[] };

/** Whether the plan the confirmation shows (`shown`) still stands against the re-read environment (`fresh`, same intent, same clock and fees). */
export function recheckVerdict(shown: OrderPlan, fresh: OrderPlan): RecheckVerdict {
  const errs = errors(fresh);
  if (!fresh.ok || !fresh.built || errs.length > 0) return { ok: false, issues: errs };
  const warn = newWarnings(shown, fresh);
  if (warn.length > 0) return { ok: false, issues: warn };
  if (!shown.built || !sameTransaction(shown.built, fresh.built)) return { ok: false, issues: [] };
  return { ok: true };
}
