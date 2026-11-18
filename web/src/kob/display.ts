/**
 * Which characters a displayed name, ticker or text may hold.
 *
 * The definition is one file, `crates/kob-protocol/data/display-chars.json`, which the Rust side
 * (`kob-protocol/src/display.rs`) reads too. The cases in it run in both test suites. The Rust
 * module has the rules:
 * - refuse what draws nothing or reorders,
 * - keep emoji joiners, selectors and flag tags in their place,
 * - flag look-alikes, and never refuse them.
 */
import table from '../../../crates/kob-protocol/data/display-chars.json';

const hex = (h: string): number => parseInt(h, 16);
const ranges = (r: string[][]): [number, number][] => r.map(([lo, hi]) => [hex(lo!), hex(hi!)]);
const REFUSE = ranges(table.refuse);
const LOOK_RANGES = ranges(table.lookalike_ranges);
const LOOK_CHARS = new Set([...table.lookalike_chars].map((c) => c.codePointAt(0)!));
const ZWJ = hex(table.zwj);
const [VS_LO, VS_HI] = table.variation.map(hex) as [number, number];
const TAG_BASE = hex(table.tag_base);
const [TAG_LO, TAG_HI] = table.tag_range.map(hex) as [number, number];
const TAG_END = hex(table.tag_end);

const inRanges = (r: [number, number][], c: number): boolean => r.some(([lo, hi]) => c >= lo && c <= hi);
const isSpace = (c: number): boolean => /\s/u.test(String.fromCodePoint(c));
const isVs = (c: number): boolean => c >= VS_LO && c <= VS_HI;
const isTag = (c: number): boolean => c >= TAG_LO && c <= TAG_HI;
const visible = (c: number): boolean => !isSpace(c) && c !== ZWJ && !isVs(c) && !isTag(c) && !inRanges(REFUSE, c);

/** Tells for each character (code point) of `s` whether it is refused. */
export function badMask(s: string): boolean[] {
  const cs = [...s].map((c) => c.codePointAt(0)!);
  const bad = new Array<boolean>(cs.length).fill(false);
  for (let i = 0; i < cs.length; ) {
    const u = cs[i]!;
    if (inRanges(REFUSE, u)) bad[i] = true;
    else if (u === ZWJ) {
      const prev = i > 0 && (visible(cs[i - 1]!) || (cs[i - 1] === VS_HI && !bad[i - 1]));
      bad[i] = !(prev && i + 1 < cs.length && visible(cs[i + 1]!));
    } else if (isVs(u)) bad[i] = !(i > 0 && visible(cs[i - 1]!));
    else if (isTag(u)) {
      let j = i;
      while (j < cs.length && isTag(cs[j]!)) j++;
      const ok = i > 0 && cs[i - 1] === TAG_BASE && cs[j - 1] === TAG_END && j - i >= 2;
      for (let k = i; k < j; k++) bad[k] = !ok;
      i = j;
      continue;
    }
    i++;
  }
  return bad;
}

/**
 * True if `s` has an invisible or reordering character, such as a control, format or filler
 * character, or a stray joiner, selector or tag.
 */
export const hasBadChar = (s: string): boolean => badMask(s).some(Boolean);

/** `s` with each refused character replaced by `replacement`. By default they are removed. */
export function replaceBad(s: string, replacement = ''): string {
  const bad = badMask(s);
  return [...s].map((c, i) => (bad[i] ? replacement : c)).join('');
}

/** True if `s` has a character drawn like a Latin letter. It is a warning, never a refusal. */
export const hasLookalike = (s: string): boolean => [...s].some((c) => LOOK_CHARS.has(c.codePointAt(0)!) || inRanges(LOOK_RANGES, c.codePointAt(0)!));

/** The shared test cases of the table. */
export const DISPLAY_CASES: readonly { s: string; bad: boolean; lookalike: boolean }[] = table.cases;
