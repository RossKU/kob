// How long each wallet takes to sign an auction, and the extra lead an auction's start gets from it (pure, plus a small per-wallet memory).
//
// An auction (a market order, a limit that crosses, a Dutch / rising order) starts MARKET_ACTIVATION_DAA after the clock it was planned with and
// may still be sent AUCTION_START_MARGIN_DAA after that start (wallet/sign.ts `auctionSubmitBy`): the wallet has AUCTION_WINDOW_DAA (5 s) from the
// plan to the clock check after its signature. A person reading an external wallet's popup often takes longer, and the attempt then stops with
// `clock-moved` (nothing is sent). So:
//  * the send measures the signing time of every auction (SendStatus.tsx: from the moment the wallet is handed the transaction to the clock check
//    after its signature) and keeps the last one of each wallet (`rememberSigningMs`: in memory, and in localStorage when the browser allows it);
//  * the first attempt of an auction with a wallet whose last signing took longer than the window is planned with a clock later by the shortfall
//    plus RETIME_LEAD_DAA (`firstAttemptLead`, OrderTicket `prepareLevel`), the same move a re-timing makes;
//  * a re-timing after `clock-moved` gives at least the measured signing time of the failed attempt plus RETIME_LEAD_DAA (`retimeLead`).
// Only times move, and only later: the activation lead and the clock check after the signature stay as they are, and the terms the user approved
// are the same (signer/retime.ts `sameOrBetter`). A lead derived from a measurement is capped at MAX_MEASURED_LEAD_DAA (60 s): a wallet slower
// than that still ends with `clock-moved` after its re-timings, and no order waits more than about a minute for its start because of an old reading.
import { MARKET_ACTIVATION_DAA } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import { AUCTION_START_MARGIN_DAA } from '../../wallet/sign';

/**
 * The extra lead of a re-timed auction's start (DAA, 3 s more at each re-timing of the same order): the re-timed transaction is checked, signed
 * and its clock read again before it is sent, which over a slow link takes longer than the start's own lead (MARKET_ACTIVATION_DAA) leaves.
 */
export const RETIME_LEAD_DAA = 30n;

/** From the plan to the last DAA score its signed auction may be sent at (5 s): the time a wallet has to sign it. */
export const AUCTION_WINDOW_DAA = MARKET_ACTIVATION_DAA + AUCTION_START_MARGIN_DAA;

/** The most a measured or remembered signing time adds to an auction's lead (600 DAA, 60 s); the fixed RETIME_LEAD_DAA steps come on top. */
export const MAX_MEASURED_LEAD_DAA = 600n;

/** the nominal rate (milli-DAA per second) when the clock has none */
const NOMINAL_RATE_MILLI = 10_000;

const capped = (lead: bigint): bigint => (lead > MAX_MEASURED_LEAD_DAA ? MAX_MEASURED_LEAD_DAA : lead);

/** `ms` as DAA at `rateMilli` (the node's measured rate; 10 DAA/s without one), rounded up. */
export function daaOfMs(ms: number, rateMilli?: number): bigint {
  const rate = rateMilli !== undefined && Number.isFinite(rateMilli) && rateMilli > 0 ? Math.round(rateMilli) : NOMINAL_RATE_MILLI;
  const m = Number.isFinite(ms) ? Math.max(0, Math.ceil(ms)) : 0;
  return (BigInt(m) * BigInt(rate) + 999_999n) / 1_000_000n;
}

/** `c` moved `lead` DAA later, its unix time with it at its rate (a date converted with it lands where it did; only the clock's "now" moves). */
export function laterClock(c: Clock, lead: bigint): Clock {
  return { ...c, daa: c.daa + lead, unixSeconds: c.unixSeconds + (lead * 1000n) / BigInt(Math.max(1, Math.round(c.rateMilli))) };
}

/**
 * The extra lead (DAA) of an auction's first attempt with a wallet whose last auction signature took `signingMs`: the shortfall against
 * AUCTION_WINDOW_DAA plus RETIME_LEAD_DAA, capped at MAX_MEASURED_LEAD_DAA; 0 when it fit the window or nothing was measured yet.
 */
export function firstAttemptLead(signingMs: number | null, rateMilli?: number): bigint {
  if (signingMs === null) return 0n;
  const need = daaOfMs(signingMs, rateMilli);
  return need <= AUCTION_WINDOW_DAA ? 0n : capped(need - AUCTION_WINDOW_DAA + RETIME_LEAD_DAA);
}

/**
 * The lead (DAA) of a re-timing whose own step is `lead` (the previous lead plus RETIME_LEAD_DAA): at least the signing time the failed attempt
 * measured (`signingMs`) plus RETIME_LEAD_DAA, that part capped at MAX_MEASURED_LEAD_DAA. Never shorter than `lead`.
 */
export function retimeLead(lead: bigint, signingMs: number | undefined, rateMilli?: number): bigint {
  if (signingMs === undefined) return lead;
  const measured = capped(daaOfMs(signingMs, rateMilli) + RETIME_LEAD_DAA);
  return measured > lead ? measured : lead;
}

/**
 * The moments transactions of one attempt were handed to the wallet (the request, and each update of it the private-key signer was given while
 * the user read): a signing time counts from the hand-over of the transaction the wallet signed, not from an older plan it replaced.
 */
export class SigningTimer {
  private readonly at = new Map<string, number>();
  constructor(private readonly now: () => number = Date.now) {}
  handed(txId: string): void {
    this.at.set(txId, this.now());
  }
  /** ms since `txId` was handed over; null when it never was */
  since(txId: string): number | null {
    const t = this.at.get(txId);
    return t === undefined ? null : Math.max(0, this.now() - t);
  }
}

// ------------------------------------------------------------------------------------------------ remembered signing times

const KEY_PREFIX = 'kob.signing-ms.v1:';
/** a reading longer than this (a popup left open for an hour) is not a signing time */
const MAX_REMEMBERED_MS = 3_600_000;
const memory = new Map<string, number>();

const usable = (ms: unknown): ms is number => typeof ms === 'number' && Number.isFinite(ms) && ms >= 0 && ms <= MAX_REMEMBERED_MS;

/** Keeps `ms` as the last auction signing time of wallet `walletId` (the adapter id). Never throws. */
export function rememberSigningMs(walletId: string, ms: number): void {
  if (!usable(ms)) return;
  const v = Math.round(ms);
  memory.set(walletId, v);
  try {
    localStorage.setItem(KEY_PREFIX + walletId, String(v));
  } catch {
    /* no storage: the memory of this page */
  }
}

/** The last auction signing time of wallet `walletId` (ms), or null when none was measured. Never throws. */
export function rememberedSigningMs(walletId: string): number | null {
  const m = memory.get(walletId);
  if (m !== undefined) return m;
  try {
    const raw = localStorage.getItem(KEY_PREFIX + walletId);
    const v = raw === null ? NaN : Number(raw);
    if (!usable(v)) return null;
    memory.set(walletId, v);
    return v;
  } catch {
    return null;
  }
}

/** Forgets every remembered signing time of this page (tests). */
export function forgetSigningTimes(): void {
  memory.clear();
}
