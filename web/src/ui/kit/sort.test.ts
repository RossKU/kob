import { describe, expect, it } from 'vitest';
import { ariaSortOf, chooseSort, nextSort, parseSort, scaledUnits, sortBy, type SortColumn } from './sort';

const time: SortColumn = { key: 'time', label: 'Time', kind: 'time' };
const name: SortColumn = { key: 'name', label: 'Name', kind: 'text' };
const amount: SortColumn = { key: 'amount', label: 'Amount', kind: 'number' };

describe('the click cycle', () => {
  it('a number column: largest first, then smallest first, then the own order', () => {
    let s = nextSort(null, amount, time);
    expect(s).toEqual({ key: 'amount', dir: 'desc' });
    s = nextSort(s, amount, time);
    expect(s).toEqual({ key: 'amount', dir: 'asc' });
    expect(nextSort(s, amount, time)).toBeNull();
  });
  it('a text column starts from A', () => {
    expect(nextSort(null, name, time)).toEqual({ key: 'name', dir: 'asc' });
    expect(nextSort({ key: 'name', dir: 'asc' }, name, time)).toEqual({ key: 'name', dir: 'desc' });
  });
  it('the own order already is the time, newest first: its first click reverses it, the second returns to it', () => {
    const s = nextSort(null, time, time);
    expect(s).toEqual({ key: 'time', dir: 'asc' });
    expect(nextSort(s, time, time)).toBeNull();
  });
  it('another column starts over', () => {
    expect(nextSort({ key: 'amount', dir: 'asc' }, name, time)).toEqual({ key: 'name', dir: 'asc' });
  });
  it('aria-sort and the narrow control agree with it', () => {
    expect(ariaSortOf(null, 'time', time)).toBe('descending');
    expect(ariaSortOf(null, 'name', time)).toBe('none');
    expect(ariaSortOf({ key: 'name', dir: 'asc' }, 'name', time)).toBe('ascending');
    expect(chooseSort('time', 'desc', time)).toBeNull();
    expect(chooseSort('time', 'asc', time)).toEqual({ key: 'time', dir: 'asc' });
    expect(chooseSort('amount', 'desc', time)).toEqual({ key: 'amount', dir: 'desc' });
  });
});

describe('the comparison', () => {
  const rows = [
    { id: 'a', v: 10n as bigint | number | string | null },
    { id: 'b', v: 9 },
    { id: 'c', v: null },
    { id: 'd', v: 100n },
    { id: 'e', v: '' },
  ];
  it('numbers by value, never as text; empty values last in both directions; stable', () => {
    expect(sortBy(rows, { key: 'v', dir: 'asc' }, (r) => r.v).map((r) => r.id)).toEqual(['b', 'a', 'd', 'c', 'e']);
    expect(sortBy(rows, { key: 'v', dir: 'desc' }, (r) => r.v).map((r) => r.id)).toEqual(['d', 'a', 'b', 'c', 'e']);
  });
  it('no state: the same array', () => {
    expect(sortBy(rows, null, (r) => r.v)).toBe(rows);
  });
  it('text reads its numbers as numbers', () => {
    const t = [{ n: 'TUSD10' }, { n: 'TUSD2' }, { n: 'kaspa' }];
    expect(sortBy(t, { key: 'n', dir: 'asc' }, (r) => r.n).map((r) => r.n)).toEqual(['kaspa', 'TUSD2', 'TUSD10']);
  });
  it('amounts of tokens of other decimals compare by what they are worth in tokens', () => {
    // 1.5 of a 2-decimals token against 1.25 of an 8-decimals one
    expect(scaledUnits(150n, 2) > scaledUnits(125_000_000n, 8)).toBe(true);
    expect(scaledUnits(5n, 18)).toBe(5n);
  });
});

describe('a stored sort', () => {
  it('is read back only when it is of a known column', () => {
    expect(parseSort('{"key":"amount","dir":"asc"}', ['amount', 'time'])).toEqual({ key: 'amount', dir: 'asc' });
    expect(parseSort('{"key":"gone","dir":"asc"}', ['amount'])).toBeNull();
    expect(parseSort('{"key":"amount","dir":"up"}', ['amount'])).toBeNull();
    expect(parseSort('nonsense', ['amount'])).toBeNull();
    expect(parseSort(null, ['amount'])).toBeNull();
  });
});
