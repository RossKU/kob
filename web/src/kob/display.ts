/**
 * Which characters a displayed name, ticker or text may hold.
 *
 * The definition is one file, `crates/kob-protocol/data/display-chars.json`, which the Rust side
 * (`kob-protocol/src/display.rs`) reads too. The cases in it run in both test suites. The Rust
 * module has the rules:
 * - refuse what draws nothing or reorders,
 * - keep emoji joiners and selectors only between or after emoji (and in a keycap), and flag tags
 *   in their place,
 * - flag look-alikes and strings NFC would change, and never refuse them.
 * The Unicode-derived parts of the file are written by `web/scripts/gen-display-chars.mjs`.
 */
import table from '../../../crates/kob-protocol/data/display-chars.json';

const hex = (h: string): number => parseInt(h, 16);
const ranges = (r: string[][]): [number, number][] => r.map(([lo, hi]) => [hex(lo!), hex(hi!)]);
const REFUSE = ranges(table.refuse);
const EMOJI = ranges(table.emoji);
const LOOK_RANGES = ranges(table.lookalike_ranges);
const LOOK_CHARS = new Set([...table.lookalike_chars].map((c) => c.codePointAt(0)!));
const NFC_NO = ranges(table.nfc_no);
/** `${mark}:${base}` of every pair NFC composes. */
const NFC_COMPOSE = new Set(table.nfc_compose.flatMap(([m, bases]) => [...bases!].map((b) => `${hex(m!)}:${b.codePointAt(0)!}`)));
const ZWJ = hex(table.zwj);
const [VS_LO, VS_HI] = table.variation.map(hex) as [number, number];
const [MOD_LO, MOD_HI] = table.emoji_modifier.map(hex) as [number, number];
const KEYCAP = hex(table.keycap);
const KEYCAP_BASES = new Set([...table.keycap_bases].map((c) => c.codePointAt(0)!));
const TAG_BASE = hex(table.tag_base);
const [TAG_LO, TAG_HI] = table.tag_range.map(hex) as [number, number];
const TAG_END = hex(table.tag_end);

const inRanges = (r: [number, number][], c: number): boolean => r.some(([lo, hi]) => c >= lo && c <= hi);
const isVs = (c: number): boolean => c >= VS_LO && c <= VS_HI;
const isTag = (c: number): boolean => c >= TAG_LO && c <= TAG_HI;
const isEmoji = (c: number): boolean => inRanges(EMOJI, c);

/** Tells for each character (code point) of `s` whether it is refused. */
export function badMask(s: string): boolean[] {
  const cs = [...s].map((c) => c.codePointAt(0)!);
  const bad = new Array<boolean>(cs.length).fill(false);
  for (let i = 0; i < cs.length; ) {
    const u = cs[i]!;
    const prev = i > 0 ? cs[i - 1]! : null;
    const next = i + 1 < cs.length ? cs[i + 1]! : null;
    if (inRanges(REFUSE, u)) bad[i] = true;
    else if (u === ZWJ) {
      // between two emoji; the first may end in a skin tone or a kept selector
      const after = prev !== null && (isEmoji(prev) || (prev >= MOD_LO && prev <= MOD_HI) || (isVs(prev) && !bad[i - 1]));
      bad[i] = !(after && next !== null && isEmoji(next));
    } else if (isVs(u)) {
      // right after an emoji, or inside a keycap
      const keycap = prev !== null && KEYCAP_BASES.has(prev) && next === KEYCAP;
      bad[i] = !((prev !== null && isEmoji(prev)) || keycap);
    } else if (isTag(u)) {
      let j = i;
      while (j < cs.length && isTag(cs[j]!)) j++;
      const ok = prev === TAG_BASE && cs[j - 1] === TAG_END && j - i >= 2;
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

/**
 * True if NFC normalization would change `s`, by the table's rule (as kob-protocol reads it): a
 * character NFC never keeps, or two neighbours that compose. Hangul jamo compose by rule.
 */
export function changesUnderNfc(s: string): boolean {
  const cs = [...s].map((c) => c.codePointAt(0)!);
  for (let i = 0; i < cs.length; i++) {
    const c = cs[i]!;
    if (inRanges(NFC_NO, c)) return true;
    if (i === 0) continue;
    const p = cs[i - 1]!;
    const hangul = (p >= 0x1100 && p <= 0x1112 && c >= 0x1161 && c <= 0x1175) || (p >= 0xac00 && p <= 0xd7a3 && (p - 0xac00) % 28 === 0 && c >= 0x11a8 && c <= 0x11c2);
    if (hangul || NFC_COMPOSE.has(`${c}:${p}`)) return true;
  }
  return false;
}

/**
 * True if `s` has a character drawn like a Latin letter, or NFC would change it (it is drawn like
 * its normalized twin). It is a warning, never a refusal.
 */
export const hasLookalike = (s: string): boolean =>
  [...s].some((c) => LOOK_CHARS.has(c.codePointAt(0)!) || inRanges(LOOK_RANGES, c.codePointAt(0)!)) || changesUnderNfc(s);

/** The shared test cases of the table. */
export const DISPLAY_CASES: readonly { s: string; bad: boolean; lookalike: boolean }[] = table.cases;
