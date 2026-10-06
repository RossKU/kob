import { describe, expect, it } from 'vitest';
import { LATE_MS, staleSinceOf } from './book-stale';

const base = { lastGoodAt: 1_000, failed: false, loadingSince: null, now: 2_000 };

describe('staleSinceOf', () => {
  it('a book that is current is not stale', () => {
    expect(staleSinceOf(base)).toBeNull();
    expect(staleSinceOf({ ...base, loadingSince: 1_900 })).toBeNull(); // a pull in flight for a moment
  });

  it('a failed newest pull dates the book from the last good response', () => {
    expect(staleSinceOf({ ...base, failed: true })).toBe(1_000);
  });

  it('a late pull (in flight for LATE_MS) does too, a quick one does not', () => {
    expect(staleSinceOf({ ...base, loadingSince: 2_000 - LATE_MS + 1, now: 2_000 })).toBeNull();
    expect(staleSinceOf({ ...base, loadingSince: 2_000 - LATE_MS, now: 2_000 })).toBe(1_000);
  });

  it('a crossed fresh book hidden behind an older snapshot dates from that snapshot; the earliest time wins', () => {
    expect(staleSinceOf({ ...base, heldAt: 700 })).toBe(700);
    expect(staleSinceOf({ ...base, failed: true, heldAt: 1_500 })).toBe(1_000);
  });

  it('nothing is stale without a book on screen (the first load is the skeleton / the error banner)', () => {
    expect(staleSinceOf({ lastGoodAt: null, failed: true, loadingSince: 0, now: 99_999 })).toBeNull();
  });
});
