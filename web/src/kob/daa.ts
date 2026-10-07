// DAA score <-> wall clock, order lifetimes and the protocol timing defaults (docs/spec/matcher.md section 10, items 2-4 and 10).
//
// The chain has no clock: covenants see DAA scores (about 10 per second at 10 BPS). The wallet reads the node's virtual DAA score D0
// and the UTC wall clock T0 together (`Clock`), maps wall-clock wishes to DAA with the measured rate r (clamped to 9.5..10.5 DAA/s)
// and shows "expires around" for the way back. The day-order rule itself lives in Rust (`kob.dayOrder`, defaults.rs): never re-derive it.
import type { Clock } from './plan-types';
import type { KobWasm } from './wasm';
import { ceilDiv, roundDiv } from './units';

// ------------------------------------------------------------------------------------------------ protocol constants (mirror defaults.rs / contracts)

/** Nominal DAA rate in milli-DAA per second (10 DAA/s) and the accepted measurement range (matcher.md 10.10). */
export const DAA_RATE_NOMINAL = 10_000;
export const DAA_RATE_MIN = 9_500;
export const DAA_RATE_MAX = 10_500;

/** `MAX_IDLE` of the covenants: 90 days at 10 DAA/s. An order untouched this long is refundable by anyone. */
export const MAX_IDLE_DAA = 77_760_000n;
/** Covenant IOC / FOK kill: refundable this many DAA after the order became fillable (60 s). */
export const IOC_KILL_DAA = 600n;
/** Wallet life of an IOC / FOK order (30 s, matcher.md 10.4): the covenant kill is at IOC_KILL_DAA. */
export const IOC_LIFE_DAA = 300n;
/** Market order: auction length from the touch to the slippage bound (20 s), activation delay, slippage bound (matcher.md 10.2). */
export const MARKET_AUCTION_DAA = 200n;
export const MARKET_ACTIVATION_DAA = 30n;
export const SLIPPAGE_BPS = 300n;
/** Stop auction length (30 s) and trigger rest time R (5 s: the evidence order's exposure, matcher.md §4 / §10); used by the conditional planner. */
export const STOP_BAND_DAA = 300n;
export const MIN_REST_DAA = 50n;

export const DAY_SECONDS = 86_400n;
export const GTC_DAYS = 90n;
/** A GTC order should be renewed (cancel-replace) before this day of its 90-day life (matcher.md 10.10). */
export const GTC_RENEW_DAY = 85n;

// ------------------------------------------------------------------------------------------------ rate and conversions

/** Integer milli-DAA/s within the accepted range; a missing or non-finite measurement falls back to the nominal 10 DAA/s. */
export function clampRate(rateMilli: number | null | undefined): number {
  if (rateMilli == null || !Number.isFinite(rateMilli)) return DAA_RATE_NOMINAL;
  return Math.min(DAA_RATE_MAX, Math.max(DAA_RATE_MIN, Math.round(rateMilli)));
}

const rateOf = (clock: Clock): bigint => BigInt(clampRate(clock.rateMilli));

/** DAA elapsed in `seconds` at the clock's rate, rounded up (a wish for "at least this long"). */
export const secondsToDaa = (clock: Clock, seconds: bigint): bigint => {
  if (seconds < 0n) throw new RangeError('secondsToDaa: negative duration');
  return ceilDiv(seconds * rateOf(clock), 1000n);
};

/** Seconds that `daa` DAA take at the clock's rate, rounded to the nearest second. */
export const daaToSeconds = (clock: Clock, daa: bigint): bigint => {
  if (daa < 0n) throw new RangeError('daaToSeconds: negative duration');
  return roundDiv(daa * 1000n, rateOf(clock));
};

/** A span of time given either in DAA (protocol units) or in seconds (what a user types). */
export type Duration = { daa: bigint } | { seconds: bigint };

export const durationToDaa = (clock: Clock, d: Duration): bigint => ('daa' in d ? d.daa : secondsToDaa(clock, d.seconds));

/** Wall-clock time (UTC unix seconds) at which the chain reaches `daa`: "expires around". Works for past scores too. */
export function daaToUnix(clock: Clock, daa: bigint): bigint {
  const delta = daa - clock.daa;
  return delta >= 0n ? clock.unixSeconds + daaToSeconds(clock, delta) : clock.unixSeconds - daaToSeconds(clock, -delta);
}

/** DAA score the chain will have at UTC time `unix` (rounded so the order does not end early). Works for past times too. */
export function unixToDaa(clock: Clock, unix: bigint): bigint {
  const delta = unix - clock.unixSeconds;
  return delta >= 0n ? clock.daa + secondsToDaa(clock, delta) : clock.daa - secondsToDaa(clock, -delta);
}

/**
 * The DAA score for a user's date, on the side of the user's intent: the score at the measured rate and at the nominal 10 DAA/s, the
 * earlier of the two for an end (`'end'`: a GTD expiry never lets honest matchers fill past the date unless the chain is slower than
 * both rates) and the later for a start (`'start'`: a timed activation never opens before the date unless the chain is faster than
 * both). With a rate measured over hours the two differ little; a stale or wrong rate cannot move the date the wrong way.
 */
export function unixToDaaBound(clock: Clock, unix: bigint, side: 'start' | 'end'): bigint {
  const measured = unixToDaa(clock, unix);
  const nominal = unixToDaa({ ...clock, rateMilli: DAA_RATE_NOMINAL }, unix);
  if (unix <= clock.unixSeconds) return measured;
  return side === 'end' ? (measured < nominal ? measured : nominal) : measured > nominal ? measured : nominal;
}

// ------------------------------------------------------------------------------------------------ lifetimes

export type ExpiryKind = 'gtc' | 'gtd' | 'day' | 'ioc' | 'fok';

export interface Expiry {
  kind: ExpiryKind;
  /** `expiryDaa` of the order state */
  expiryDaa: bigint;
  /** day orders: the wall-clock deadline (UTC unix seconds) of the placement record; otherwise null */
  deadline: bigint | null;
  /** approximate UTC unix seconds at `expiryDaa` ("expires around") */
  approxUnixSeconds: bigint;
}

export interface ExpiryOptions {
  /** required for 'day' (the rule lives in kob-wasm) */
  kob?: KobWasm;
  /** 'gtd': the user's date, UTC unix seconds */
  at?: bigint;
  /** 'ioc' / 'fok': life in DAA counted from `activeFrom` (default IOC_LIFE_DAA) */
  lifeDaa?: bigint;
  /** 'ioc' / 'fok': DAA from which the order is fillable (default: now) */
  activeFrom?: bigint;
}

/**
 * Maps a lifetime kind to the on-chain `expiryDaa` (and the day-order deadline).
 *  - gtc: placement + MAX_IDLE (the covenant also refunds after 90 days without activity);
 *  - gtd: the user's date, the earlier of the measured and the nominal rate (`unixToDaaBound`); NOT clamped here (guards.checkExpiry
 *    refuses dates beyond 90 days);
 *  - day: `kob.dayOrder` (until the next 00:00 UTC, 1% margin, deadline in the placement record);
 *  - ioc / fok: `activeFrom + life` (default 300 DAA, the covenant kills at 600 after it became fillable).
 */
export function expiryFor(kind: ExpiryKind, clock: Clock, opt: ExpiryOptions = {}): Expiry {
  let expiryDaa: bigint;
  let deadline: bigint | null = null;
  switch (kind) {
    case 'gtc':
      expiryDaa = clock.daa + MAX_IDLE_DAA;
      break;
    case 'gtd':
      if (opt.at === undefined) throw new Error('expiryFor(gtd) needs `at`');
      expiryDaa = unixToDaaBound(clock, opt.at, 'end');
      break;
    case 'day': {
      if (!opt.kob) throw new Error('expiryFor(day) needs `kob`');
      const d = dayOrderFor(opt.kob, clock);
      expiryDaa = d.expiryDaa;
      deadline = d.deadline;
      break;
    }
    case 'ioc':
    case 'fok':
      expiryDaa = (opt.activeFrom ?? clock.daa) + (opt.lifeDaa ?? IOC_LIFE_DAA);
      break;
  }
  return { kind, expiryDaa, deadline, approxUnixSeconds: daaToUnix(clock, expiryDaa) };
}

/** Day order until the next 00:00 UTC, computed by kob-wasm (matcher.md 10.10). */
export function dayOrderFor(kob: KobWasm, clock: Clock): { expiryDaa: bigint; deadline: bigint } {
  const d = kob.dayOrder(clock.daa, clock.unixSeconds, BigInt(clampRate(clock.rateMilli)));
  return { expiryDaa: BigInt(d.expiryDaa), deadline: BigInt(d.deadline) };
}

/** Seconds left until the next 00:00 UTC. */
export const secondsToNextMidnight = (unix: bigint): bigint => (unix / DAY_SECONDS + 1n) * DAY_SECONDS - unix;

/** Day 85 of a GTC order placed at `placementUnix`: the date to remind the user to renew (cancel-replace). */
export const gtcRenewalUnix = (placementUnix: bigint): bigint => placementUnix + GTC_RENEW_DAY * DAY_SECONDS;

/** Renewal reminder for an existing order from its on-chain `expiryDaa` (five days before it, i.e. day 85 of 90). */
export const renewalUnixForExpiry = (clock: Clock, expiryDaa: bigint): bigint =>
  daaToUnix(clock, expiryDaa) - (GTC_DAYS - GTC_RENEW_DAY) * DAY_SECONDS;

// ------------------------------------------------------------------------------------------------ display

const two = (n: number): string => String(n).padStart(2, '0');

/** `2026-09-30 00:00 UTC`: locale-independent UTC timestamp for the disclosure screens. */
export function formatUtc(unix: bigint): string {
  const d = new Date(Number(unix) * 1000);
  return `${d.getUTCFullYear()}-${two(d.getUTCMonth() + 1)}-${two(d.getUTCDate())} ${two(d.getUTCHours())}:${two(d.getUTCMinutes())} UTC`;
}

/** `2026-09-30 09:00 JST`: the same instant in Japan Standard Time (UTC+9, no DST), for the day-order hint. */
export function formatJst(unix: bigint): string {
  const d = new Date((Number(unix) + 9 * 3600) * 1000);
  return `${d.getUTCFullYear()}-${two(d.getUTCMonth() + 1)}-${two(d.getUTCDate())} ${two(d.getUTCHours())}:${two(d.getUTCMinutes())} JST`;
}
