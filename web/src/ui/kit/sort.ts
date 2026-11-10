// Sorting the lists and tables of the app by a column (the Portfolio's tables, the activity log): one state, one comparison, one click cycle.
//   state     null = the list's own order (newest first); else the column and the direction
//   clicks    a column's header: first its natural direction (numbers and times: largest first; text: A to Z), second the other way, third the own order again
//   values    numbers and bigints are compared as numbers, text with its numbers read as numbers ("TUSD2" before "TUSD10"), an empty value ("—", null) is last
//             in either direction
// Pure functions; the header button, the narrow-card control and the stored choice are in SortHeader.tsx.

export type SortDir = 'asc' | 'desc';

export interface SortState<K extends string = string> {
  key: K;
  dir: SortDir;
}

/** What a row has under a column: a number (a bigint too), text, or nothing (last). */
export type SortValue = number | bigint | string | null | undefined;

export interface SortColumn<K extends string = string> {
  key: K;
  label: string;
  /** numbers and times start with the largest first, text from A */
  kind: 'number' | 'time' | 'text';
}

/** The direction a column starts with. */
export const firstDir = (kind: SortColumn['kind']): SortDir => (kind === 'text' ? 'asc' : 'desc');

const other = (d: SortDir): SortDir => (d === 'asc' ? 'desc' : 'asc');

/**
 * The state after a click on `col`'s header. `own` is the column the list's own order stands for (the time, newest first): while nothing is chosen a click on it
 * goes the other way round (the own order already is its natural direction).
 */
export function nextSort<K extends string>(cur: SortState<K> | null, col: SortColumn<K>, own: SortColumn<K> | null = null): SortState<K> | null {
  const eff: SortState<K> | null = cur ?? (own ? { key: own.key, dir: firstDir(own.kind) } : null);
  if (!eff || eff.key !== col.key) return { key: col.key, dir: firstDir(col.kind) };
  if (eff.dir === firstDir(col.kind)) return { key: col.key, dir: other(eff.dir) };
  return null;
}

/** A choice in the narrow-card control (a column or its direction): the state, or null where it is the list's own order. */
export function chooseSort<K extends string>(key: K, dir: SortDir, own: SortColumn<K> | null): SortState<K> | null {
  return own && key === own.key && dir === firstDir(own.kind) ? null : { key, dir };
}

/** The `aria-sort` of a column's header (a list in its own order reads as sorted by `own`). */
export function ariaSortOf<K extends string>(cur: SortState<K> | null, key: K, own: SortColumn<K> | null = null): 'ascending' | 'descending' | 'none' {
  const eff = cur ?? (own ? { key: own.key, dir: firstDir(own.kind) } : null);
  return !eff || eff.key !== key ? 'none' : eff.dir === 'asc' ? 'ascending' : 'descending';
}

export const isEmptySortValue = (v: SortValue): v is null | undefined | '' => v === null || v === undefined || v === '';

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/** Two values that are not empty, ascending: numbers by value (a bigint with a number too), text by the collator; a number before a text. */
export function compareSortValues(a: SortValue, b: SortValue): number {
  const an = typeof a === 'number' || typeof a === 'bigint';
  const bn = typeof b === 'number' || typeof b === 'bigint';
  if (an && bn) return a! < b! ? -1 : a! > b! ? 1 : 0;
  if (an !== bn) return an ? -1 : 1;
  return collator.compare(String(a), String(b));
}

/**
 * `rows` ordered by `value(row, key)` in the state's direction; stable (equal values keep the list's own order), empty values last in either direction.
 * No state: the rows as they are (the same array).
 */
export function sortBy<T, K extends string>(rows: readonly T[], state: SortState<K> | null, value: (row: T, key: K) => SortValue): readonly T[] {
  if (!state) return rows;
  const sign = state.dir === 'asc' ? 1 : -1;
  const keyed = rows.map((row, i) => ({ row, i, v: value(row, state.key) }));
  keyed.sort((x, y) => {
    const xe = isEmptySortValue(x.v);
    const ye = isEmptySortValue(y.v);
    if (xe || ye) return xe && ye ? x.i - y.i : xe ? 1 : -1;
    return sign * compareSortValues(x.v, y.v) || x.i - y.i;
  });
  return keyed.map((k) => k.row);
}

/** An amount of base units as a number comparable across tokens of different decimals: units x 10^(18 - decimals) (decimals past 18: the units cut down). */
export function scaledUnits(units: bigint, decimals: number): bigint {
  return decimals <= 18 ? units * 10n ** BigInt(18 - decimals) : units / 10n ** BigInt(decimals - 18);
}

/** A stored sort: the state when it is of `keys`, else null. */
export function parseSort<K extends string>(raw: string | null, keys: readonly K[]): SortState<K> | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as { key?: unknown; dir?: unknown };
    return typeof v.key === 'string' && (keys as readonly string[]).includes(v.key) && (v.dir === 'asc' || v.dir === 'desc') ? { key: v.key as K, dir: v.dir } : null;
  } catch {
    return null;
  }
}
