// Counts in sentences: the dictionary has no plural syntax (i18n/index.ts), so a text that depends on a number has two keys, `<key>.one` and `<key>.other`, and the code picks
// one here. Never "order(s)" (review M-16).
import { formatNumber, t, type Params } from '../../i18n';

/** The text of `<key>.one` for a count of exactly one, else `<key>.other`; `count` is passed to the text (and `n` when it has no `count` of its own). */
export function tCount(key: string, count: number | bigint, params: Params = {}): string {
  return t(`${key}.${Number(count) === 1 ? 'one' : 'other'}`, { count: formatNumber(count), ...params });
}

/** The nouns `countText` knows (`orders.n.<noun>.one|other`). */
export const COUNT_NOUNS = ['order', 'utxo', 'fill', 'record', 'newRecord', 'input', 'tx', 'approval'] as const;
export type CountNoun = (typeof COUNT_NOUNS)[number];

/** A count with its noun, "1 order" / "3 orders", for a sentence that names more than one count. */
export const countText = (noun: CountNoun, n: number | bigint): string => t(`orders.n.${noun}.${Number(n) === 1 ? 'one' : 'other'}`, { n: formatNumber(n) });
