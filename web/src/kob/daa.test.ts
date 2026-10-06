import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import type { Clock } from './plan-types';
import {
  DAY_SECONDS, MAX_IDLE_DAA, clampRate, daaToSeconds, daaToUnix, dayOrderFor, durationToDaa, expiryFor, formatJst, formatUtc, gtcRenewalUnix,
  renewalUnixForExpiry, secondsToDaa, secondsToNextMidnight, unixToDaa,
} from './daa';

const kob = loadKobNode();
// 15:00:00 UTC; golden day-order vector: deadline 1_790_726_400, expiry D0 + 324_000 + 3_240
const clock: Clock = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };

describe('rate and conversions', () => {
  it('clamps the measured rate to 9.5..10.5 DAA/s and falls back to nominal', () => {
    expect(clampRate(undefined)).toBe(10_000);
    expect(clampRate(null)).toBe(10_000);
    expect(clampRate(Number.NaN)).toBe(10_000);
    expect(clampRate(20_000)).toBe(10_500);
    expect(clampRate(1)).toBe(9_500);
    expect(clampRate(9_876.4)).toBe(9_876);
  });

  it('converts seconds to DAA rounding up and back', () => {
    expect(secondsToDaa(clock, 60n)).toBe(600n);
    expect(secondsToDaa({ ...clock, rateMilli: 9_500 }, 1n)).toBe(10n); // ceil(9.5)
    expect(secondsToDaa({ ...clock, rateMilli: 10_500 }, 3n)).toBe(32n); // ceil(31.5)
    expect(daaToSeconds(clock, 600n)).toBe(60n);
    expect(durationToDaa(clock, { seconds: 20n })).toBe(200n);
    expect(durationToDaa(clock, { daa: 123n })).toBe(123n);
    expect(() => secondsToDaa(clock, -1n)).toThrow(RangeError);
  });

  it('maps DAA <-> wall clock around the reading, for the future and the past', () => {
    expect(daaToUnix(clock, 1_000_000n)).toBe(1_790_694_000n);
    expect(daaToUnix(clock, 1_000_600n)).toBe(1_790_694_060n);
    expect(daaToUnix(clock, 999_400n)).toBe(1_790_693_940n);
    expect(unixToDaa(clock, 1_790_694_060n)).toBe(1_000_600n);
    expect(unixToDaa(clock, 1_790_693_940n)).toBe(999_400n);
    // unixToDaa never ends an order early: it rounds up
    const slow: Clock = { ...clock, rateMilli: 9_500 };
    expect(unixToDaa(slow, clock.unixSeconds + 1n)).toBe(clock.daa + 10n);
  });
});

describe('lifetimes', () => {
  it('day order = kob.dayOrder (until the next 00:00 UTC, +1% margin) and carries the deadline', () => {
    const d = dayOrderFor(kob, clock);
    expect(d).toEqual({ expiryDaa: 1_000_000n + 324_000n + 3_240n, deadline: 1_790_726_400n });
    const e = expiryFor('day', clock, { kob });
    expect(e.expiryDaa).toBe(d.expiryDaa);
    expect(e.deadline).toBe(d.deadline);
    expect(e.approxUnixSeconds).toBe(daaToUnix(clock, d.expiryDaa));
    // the on-chain refund opens at or after midnight at the nominal rate
    expect(e.approxUnixSeconds).toBeGreaterThanOrEqual(d.deadline);
    expect(() => expiryFor('day', clock)).toThrow(/kob/);
  });

  it('day order at the measured rate clamp', () => {
    const fast = dayOrderFor(kob, { ...clock, rateMilli: 99_999 });
    expect(fast).toEqual(dayOrderFor(kob, { ...clock, rateMilli: 10_500 }));
  });

  it('GTC = placement + MAX_IDLE (90 days); GTD maps the date; IOC / FOK = activeFrom + 300', () => {
    expect(MAX_IDLE_DAA).toBe(90n * 86_400n * 10n);
    expect(expiryFor('gtc', clock).expiryDaa).toBe(1_000_000n + 77_760_000n);
    expect(expiryFor('gtc', clock).approxUnixSeconds).toBe(clock.unixSeconds + 90n * DAY_SECONDS);
    expect(expiryFor('gtd', clock, { at: clock.unixSeconds + 3600n }).expiryDaa).toBe(1_036_000n);
    expect(() => expiryFor('gtd', clock)).toThrow(/at/);
    expect(expiryFor('ioc', clock).expiryDaa).toBe(1_000_300n);
    expect(expiryFor('fok', clock, { activeFrom: 1_000_030n, lifeDaa: 100n }).expiryDaa).toBe(1_000_130n);
  });

  it('GTC renewal reminder on day 85', () => {
    expect(gtcRenewalUnix(clock.unixSeconds)).toBe(clock.unixSeconds + 85n * DAY_SECONDS);
    const e = expiryFor('gtc', clock);
    expect(renewalUnixForExpiry(clock, e.expiryDaa)).toBe(gtcRenewalUnix(clock.unixSeconds));
  });

  it('seconds to the next UTC midnight, exactly at midnight is a whole day', () => {
    expect(secondsToNextMidnight(1_790_694_000n)).toBe(32_400n);
    expect(secondsToNextMidnight(1_790_726_400n)).toBe(86_400n);
    expect(secondsToNextMidnight(1_790_726_399n)).toBe(1n);
  });

  it('formats UTC and JST without a locale', () => {
    expect(formatUtc(1_790_726_400n)).toBe('2026-09-30 00:00 UTC');
    expect(formatJst(1_790_726_400n)).toBe('2026-09-30 09:00 JST');
    expect(formatUtc(0n)).toBe('1970-01-01 00:00 UTC');
  });
});
