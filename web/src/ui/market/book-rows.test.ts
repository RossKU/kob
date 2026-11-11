import { describe, expect, it } from 'vitest';
import { BOOK_FETCH_DEPTH, BOOK_ROWS_MAX, BOOK_ROWS_MIN, rowsFor } from './book-rows';

describe('the levels per side that fit the book card', () => {
  it('a side holds as many 21 px rows as fit, at least 8 and at most 40', () => {
    expect(rowsFor(21 * 15)).toBe(15);
    expect(rowsFor(21 * 15 + 20)).toBe(15);
    expect(rowsFor(21 * 15 - 0.4)).toBe(15); // sub-pixel noise of a flex share
    expect(rowsFor(21 * 14 - 2)).toBe(13);
    expect(rowsFor(100)).toBe(BOOK_ROWS_MIN);
    expect(rowsFor(0)).toBe(BOOK_ROWS_MIN);
    expect(rowsFor(5000)).toBe(BOOK_ROWS_MAX);
  });

  it('the page asks its data source for more levels than the card can show (grouping merges levels)', () => {
    expect(BOOK_FETCH_DEPTH).toBeGreaterThanOrEqual(BOOK_ROWS_MAX * 2);
    expect(BOOK_FETCH_DEPTH).toBeLessThanOrEqual(200); // the indexer's page cap
  });
});
