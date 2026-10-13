import { describe, expect, it } from 'vitest';
import { SEEN_MIN_MS, SeenTracker } from './seen';

describe('signer: what has been shown before Approve', () => {
  it('the buttons alone are not enough: every part must have been fully visible for a moment', () => {
    const s = new SeenTracker(['title', 'line-0', 'order-0']);
    s.buttons(true, 0);
    expect(s.ready(5_000)).toBe(false); // a page clipped the frame down to its buttons
    expect(s.missing(5_000)).toEqual(['title', 'line-0', 'order-0']);
    for (const p of ['title', 'line-0', 'order-0']) s.part(p, true, 5_000);
    expect(s.ready(5_000 + SEEN_MIN_MS - 1)).toBe(false);
    expect(s.ready(5_000 + SEEN_MIN_MS)).toBe(true);
  });

  it('a flash shorter than the minimum does not count; a part seen long enough stays seen after it scrolls away', () => {
    const s = new SeenTracker(['a', 'b']);
    s.part('a', true, 0);
    s.part('a', false, SEEN_MIN_MS - 1); // flashed
    s.part('b', true, 0);
    s.part('b', false, SEEN_MIN_MS); // read, then scrolled out
    s.buttons(true, 1_000);
    expect(s.missing(2_000)).toEqual(['a']);
    expect(s.ready(2_000)).toBe(false);
    s.part('a', true, 2_000);
    expect(s.ready(2_000 + SEEN_MIN_MS)).toBe(true);
  });

  it('the buttons must be visible now', () => {
    const s = new SeenTracker(['a']);
    s.part('a', true, 0);
    s.buttons(true, 0);
    expect(s.ready(SEEN_MIN_MS)).toBe(true);
    s.buttons(false, SEEN_MIN_MS + 1); // clipped or covered again
    expect(s.ready(SEEN_MIN_MS + 2)).toBe(false);
    s.buttons(true, 2_000);
    expect(s.ready(2_000)).toBe(false); // not at once
    expect(s.ready(2_000 + SEEN_MIN_MS)).toBe(true);
  });

  it('ignores parts it does not know', () => {
    const s = new SeenTracker([]);
    s.part('x', true, 0);
    s.buttons(true, 0);
    expect(s.ready(SEEN_MIN_MS)).toBe(true);
  });
});
