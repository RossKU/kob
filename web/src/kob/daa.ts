// Converts between DAA score and wall clock, and holds order lifetimes and the protocol timing
// defaults (docs/spec/matcher.md section 10, items 2 to 4 and 10).
//
// The chain has no clock. Covenants see DAA scores, about 10 per second at 10 BPS. The wallet reads
// the node's virtual DAA score and the UTC wall clock together (`Clock`). It maps wall-clock wishes
// to DAA with the measured rate, clamped to 9.5..10.5 DAA/s, and shows "expires around" for the way
// back. The day-order rule lives in Rust (`kob.dayOrder`, defaults.rs). It is never re-derived
// here.
import type { Clock } from './plan-types';
import type { KobWasm } from './wasm';
import { ceilDiv, roundDiv } from './units';

// --- Protocol constants, mirroring defaults.rs and the contracts ---

/** The nominal DAA rate in milli-DAA per second, which is 10 DAA/s. */
export const DAA_RATE_NOMINAL = 10_000;
/** The lowest measured DAA rate accepted, in milli-DAA per second (matcher.md 10, item 10). */
export const DAA_RATE_MIN = 9_500;
/** The highest measured DAA rate accepted, in milli-DAA per second. */
export const DAA_RATE_MAX = 10_500;

/** `MAX_IDLE` of the covenants, 90 days at 10 DAA/s. Anyone can refund an order idle this long. */
export const MAX_IDLE_DAA = 77_760_000n;
/**
 * The covenant IOC and FOK kill. The order is refundable this many DAA after it is fillable (60 s).
 */
export const IOC_KILL_DAA = 600n;
/** The wallet life of an IOC or FOK order, 30 s (matcher.md 10, item 4). The kill comes later. */
export const IOC_LIFE_DAA = 300n;
/**
 * Length of a market auction from the touch to the slippage bound, 20 s (matcher.md 10, item 2).
 */
export const MARKET_AUCTION_DAA = 200n;
/** Delay before a market order activates. */
export const MARKET_ACTIVATION_DAA = 30n;
/** Slippage bound of a market order in basis points. */
export const SLIPPAGE_BPS = 300n;
/** Length of a stop auction, 30 s. The conditional planner uses it. */
export const STOP_BAND_DAA = 300n;
/** Trigger rest time, 5 s, which is the exposure of the evidence order (matcher.md section 4). */
export const MIN_REST_DAA = 50n;

/** Seconds in a day. */
export const DAY_SECONDS = 86_400n;
/** Life of a GTC order in days. */
export const GTC_DAYS = 90n;
/** The day of a GTC order's life before which it should be renewed by cancel and replace. */
export const GTC_RENEW_DAY = 85n;

// --- Rate and conversions ---

/**
 * Returns an integer milli-DAA/s in the accepted range. A missing or non-finite rate gives 10
 * DAA/s.
 */
export function clampRate(rateMilli: number | null | undefined): number {
  if (rateMilli == null || !Number.isFinite(rateMilli)) return DAA_RATE_NOMINAL;
  return Math.min(DAA_RATE_MAX, Math.max(DAA_RATE_MIN, Math.round(rateMilli)));
}

const rateOf = (clock: Clock): bigint => BigInt(clampRate(clock.rateMilli));

/** DAA elapsed in `seconds` at the clock's rate, rounded up so the span is at least that long. */
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

/** Converts a Duration to DAA. */
export const durationToDaa = (clock: Clock, d: Duration): bigint => ('daa' in d ? d.daa : secondsToDaa(clock, d.seconds));

/** Returns the UTC unix seconds at which the chain reaches `daa`. It works for past scores too. */
export function daaToUnix(clock: Clock, daa: bigint): bigint {
  const delta = daa - clock.daa;
  return delta >= 0n ? clock.unixSeconds + daaToSeconds(clock, delta) : clock.unixSeconds - daaToSeconds(clock, -delta);
}

/** Returns the DAA score at UTC time `unix`, rounded so an order does not end early. */
export function unixToDaa(clock: Clock, unix: bigint): bigint {
  const delta = unix - clock.unixSeconds;
  return delta >= 0n ? clock.daa + secondsToDaa(clock, delta) : clock.daa - secondsToDaa(clock, -delta);
}

/**
 * Returns the DAA score for a user's date, on the safe side of the user's intent.
 *
 * It takes the score at the measured rate and at the nominal 10 DAA/s. For an `'end'`, such as a
 * GTD expiry, it takes the earlier one, so honest matchers do not fill past the date. For a
 * `'start'`, such as a timed activation, it takes the later one, so the order does not open early.
 * A stale or wrong rate therefore cannot move the date the wrong way.
 */
export function unixToDaaBound(clock: Clock, unix: bigint, side: 'start' | 'end'): bigint {
  const measured = unixToDaa(clock, unix);
  const nominal = unixToDaa({ ...clock, rateMilli: DAA_RATE_NOMINAL }, unix);
  if (unix <= clock.unixSeconds) return measured;
  return side === 'end' ? (measured < nominal ? measured : nominal) : measured > nominal ? measured : nominal;
}

// --- Lifetimes ---

/** The lifetime kinds of an order. */
export type ExpiryKind = 'gtc' | 'gtd' | 'day' | 'ioc' | 'fok';

/** The resolved lifetime of an order. */
export interface Expiry {
  kind: ExpiryKind;
  /** The `expiryDaa` of the order state. */
  expiryDaa: bigint;
  /**
   * For a day order, the wall-clock deadline (UTC unix seconds) of the placement record, else null.
   */
  deadline: bigint | null;
  /** Approximate UTC unix seconds at `expiryDaa`, shown as "expires around". */
  approxUnixSeconds: bigint;
}

/** Inputs of `expiryFor`. Which fields are read depends on the kind. */
export interface ExpiryOptions {
  /** Required for 'day', because the rule lives in kob-wasm. */
  kob?: KobWasm;
  /** For 'gtd', the user's date in UTC unix seconds. */
  at?: bigint;
  /** For 'ioc' and 'fok', the life in DAA counted from `activeFrom`. Defaults to IOC_LIFE_DAA. */
  lifeDaa?: bigint;
  /** For 'ioc' and 'fok', the DAA from which the order is fillable. Defaults to now. */
  activeFrom?: bigint;
}

/**
 * Maps a lifetime kind to the on-chain `expiryDaa` and, for a day order, the deadline.
 *
 * - gtc: placement plus MAX_IDLE. The covenant also refunds after 90 days without activity.
 * - gtd: the user's date via `unixToDaaBound`. It is not clamped here, because
 *   guards.checkExpiry refuses dates beyond 90 days.
 * - day: `kob.dayOrder`, which runs until the next 00:00 UTC with a 1% margin.
 * - ioc and fok: `activeFrom` plus the life, 300 DAA by default. The covenant kills at 600.
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

/** Computes the expiry and deadline of a day order, which ends at the next 00:00 UTC. */
export function dayOrderFor(kob: KobWasm, clock: Clock): { expiryDaa: bigint; deadline: bigint } {
  const d = kob.dayOrder(clock.daa, clock.unixSeconds, BigInt(clampRate(clock.rateMilli)));
  return { expiryDaa: BigInt(d.expiryDaa), deadline: BigInt(d.deadline) };
}

/** Returns the seconds left until the next 00:00 UTC. */
export const secondsToNextMidnight = (unix: bigint): bigint => (unix / DAY_SECONDS + 1n) * DAY_SECONDS - unix;

/** Returns the day to remind the user to renew a GTC order placed at `placementUnix`. */
export const gtcRenewalUnix = (placementUnix: bigint): bigint => placementUnix + GTC_RENEW_DAY * DAY_SECONDS;

/** Returns the renewal reminder of an existing order, five days before its on-chain `expiryDaa`. */
export const renewalUnixForExpiry = (clock: Clock, expiryDaa: bigint): bigint =>
  daaToUnix(clock, expiryDaa) - (GTC_DAYS - GTC_RENEW_DAY) * DAY_SECONDS;

// --- Display ---

const two = (n: number): string => String(n).padStart(2, '0');

/** Formats a UTC timestamp like `YYYY-MM-DD 00:00 UTC`, independent of the locale. */
export function formatUtc(unix: bigint): string {
  const d = new Date(Number(unix) * 1000);
  return `${d.getUTCFullYear()}-${two(d.getUTCMonth() + 1)}-${two(d.getUTCDate())} ${two(d.getUTCHours())}:${two(d.getUTCMinutes())} UTC`;
}

function localParts(unix: bigint, timeZone?: string): Record<string, string> {
  const f = new Intl.DateTimeFormat('en-GB', {
    ...(timeZone ? { timeZone } : {}), year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23', timeZoneName: 'short',
  });
  return Object.fromEntries(f.formatToParts(new Date(Number(unix) * 1000)).map((p) => [p.type, p.value]));
}

/**
 * Formats an instant in the reader's own time zone, like `YYYY-MM-DD 09:00 GMT+9`.
 * `timeZone` is an IANA name and defaults to the browser's.
 */
export function formatLocal(unix: bigint, timeZone?: string): string {
  const p = localParts(unix, timeZone);
  return `${p.year}-${p.month}-${p.day} ${p.hour}:${p.minute} ${p.timeZoneName}`;
}

/** Formats only the clock time of an instant in the reader's own time zone, like `09:00 GMT+9`. */
export function formatLocalTime(unix: bigint, timeZone?: string): string {
  const p = localParts(unix, timeZone);
  return `${p.hour}:${p.minute} ${p.timeZoneName}`;
}

/** Returns the next 00:00 UTC after `nowMs` in unix seconds, when a day order placed now ends. */
export const nextUtcMidnight = (nowMs: number): bigint => BigInt(Math.floor(nowMs / 86_400_000) + 1) * 86_400n;
