// When Unlock is offered: by the node's past median time, else by the wall clock with a margin
// (TN10's node clock ran 127-130 s behind).
import { describe, expect, it } from 'vitest';
import { MAX_WAIT_MS, MEDIAN_POLL_MS, UNLOCK_MARGIN_MS, nextUnlockCheck, unlockOpen } from './unlock-clock';
import { minutesBefore } from './unlock';

const UNTIL = 1_800_000_000_000n;
const at = (ms: number) => Number(UNTIL) + ms;

describe('unlockOpen', () => {
  it('follows the node clock when it is known, not the wall clock', () => {
    // 128 s after the lock time by the wall clock, the node clock is still 38 s before it.
    expect(unlockOpen(UNTIL, UNTIL - 38_000n, at(128_000))).toBe(false);
    expect(unlockOpen(UNTIL, UNTIL, at(130_000))).toBe(false);
    expect(unlockOpen(UNTIL, UNTIL + 1n, at(131_000))).toBe(true);
    // a node clock past the time opens it even when the wall clock is behind
    expect(unlockOpen(UNTIL, UNTIL + 5_000n, at(-1_000))).toBe(true);
  });

  it('without the node clock, opens after the margin, which is more than the lag seen on TN10', () => {
    expect(UNLOCK_MARGIN_MS).toBeGreaterThan(130_000);
    expect(unlockOpen(UNTIL, null, at(130_000))).toBe(false);
    expect(unlockOpen(UNTIL, null, at(UNLOCK_MARGIN_MS - 1))).toBe(false);
    expect(unlockOpen(UNTIL, null, at(UNLOCK_MARGIN_MS))).toBe(true);
  });
});

describe('nextUnlockCheck', () => {
  it('waits for the wall clock to reach the lock time before it asks the node', () => {
    expect(nextUnlockCheck([UNTIL], null, at(-20_000), true, false)).toBe(20_000);
    expect(nextUnlockCheck([UNTIL], null, at(-200_000), true, false)).toBe(MAX_WAIT_MS);
    // a lock months away: a capped wait, checked again (a computer that slept, a clock set forward)
    expect(nextUnlockCheck([UNTIL], null, at(-180 * 86_400_000), true, false)).toBe(MAX_WAIT_MS);
  });

  it('asks at once, then every poll, until the node clock is past the time', () => {
    expect(nextUnlockCheck([UNTIL], null, at(5_000), true, false)).toBe(0);
    expect(nextUnlockCheck([UNTIL], UNTIL - 100_000n, at(5_000), true, true)).toBe(MEDIAN_POLL_MS);
    // a failed read (no median yet) waits a poll too
    expect(nextUnlockCheck([UNTIL], null, at(5_000), true, true)).toBe(MEDIAN_POLL_MS);
    expect(nextUnlockCheck([UNTIL], UNTIL + 1n, at(131_000), true, true)).toBeNull();
  });

  it('without the call, only renders again when the margin has passed', () => {
    expect(nextUnlockCheck([UNTIL], null, at(10_000), false, false)).toBe(MAX_WAIT_MS);
    expect(nextUnlockCheck([UNTIL], null, at(UNLOCK_MARGIN_MS - 10_000), false, false)).toBe(10_000);
    expect(nextUnlockCheck([UNTIL], null, at(UNLOCK_MARGIN_MS), false, false)).toBeNull();
  });

  it('takes the first lock still closed', () => {
    const later = UNTIL + 60_000n;
    expect(nextUnlockCheck([later, UNTIL], UNTIL + 1n, at(30_000), true, true)).toBe(30_000);
    expect(nextUnlockCheck([], null, at(0), true, false)).toBeNull();
  });
});

describe('minutesBefore', () => {
  it('rounds up and says at least 1 at the lock time itself', () => {
    expect(minutesBefore(UNTIL, UNTIL - 61_000n)).toBe(2n);
    expect(minutesBefore(UNTIL, UNTIL)).toBe(1n);
  });
});
