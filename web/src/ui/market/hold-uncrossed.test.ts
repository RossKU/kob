import { describe, expect, it } from 'vitest';
import { CROSSED_HOLD_MS, HOLD_FOREVER, holdUncrossed, type Held } from './hold-uncrossed';
import { buildBookModel } from './book-model';

const L = (price: string, amount: number) => ({ price, amount: String(amount), amount_estimated: false, orders: 1, scale: 1 });

describe('holdUncrossed', () => {
  it('shows an uncrossed value and remembers it', () => {
    const r = holdUncrossed('A', false, null, 1000);
    expect(r).toEqual({ show: 'A', matching: false, held: { value: 'A', at: 1000 } });
  });

  it('a crossed value shows the last uncrossed snapshot for a short time, then the crossed book itself', () => {
    const held: Held<string> = { value: 'A', at: 1000 };
    const during = holdUncrossed('X', true, held, 1000 + CROSSED_HOLD_MS - 1);
    expect(during).toMatchObject({ show: 'A', matching: true });
    const after = holdUncrossed('X', true, held, 1000 + CROSSED_HOLD_MS);
    expect(after).toMatchObject({ show: 'X', matching: true });
  });

  it('with HOLD_FOREVER the last uncrossed snapshot stays for as long as the book is crossed (a lagging indexer never blanks the book)', () => {
    const held: Held<string> = { value: 'A', at: 1000 };
    expect(holdUncrossed('X', true, held, 1000 + 10 * 3_600_000, HOLD_FOREVER)).toMatchObject({ show: 'A', matching: true, held });
    expect(holdUncrossed('B', false, held, 5, HOLD_FOREVER)).toMatchObject({ show: 'B', matching: false });
  });

  it('a crossed value with nothing held shows the crossed book (never a placeholder)', () => {
    expect(holdUncrossed('X', true, null, 5)).toMatchObject({ show: 'X', matching: true });
  });

  it('recovers as soon as the book uncrosses; a loading book (null) is not "matching"', () => {
    const held: Held<string> = { value: 'A', at: 1 };
    expect(holdUncrossed('B', false, held, 99_999)).toMatchObject({ show: 'B', matching: false, held: { value: 'B', at: 99_999 } });
    expect(holdUncrossed(null, false, held, 2)).toMatchObject({ show: null, matching: false });
  });

  it('works on real book models: the crossed one is never shown', () => {
    const ok = buildBookModel({ asks: [L('105', 3)], bids: [L('100', 2)] });
    const crossed = buildBookModel({ asks: [L('100', 3)], bids: [L('102', 2)] });
    expect(crossed.crossed).toBe(true);
    const a = holdUncrossed(ok, ok.crossed, null, 10);
    const b = holdUncrossed(crossed, crossed.crossed, a.held, 20);
    expect(b.show).toBe(ok);
    expect(b.show!.spread).toBe(5n);
    expect(b.matching).toBe(true);
  });
});
