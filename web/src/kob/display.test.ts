import { describe, expect, it } from 'vitest';
import { DISPLAY_CASES, hasBadChar, hasLookalike, replaceBad } from './display';
import { plainUntrusted, sanitizeUntrusted } from './registry';

describe('display characters (finding L7; the Rust twin is kob-protocol display.rs)', () => {
  it('every shared case gives the same answer as on the Rust side', () => {
    expect(DISPLAY_CASES.length).toBeGreaterThan(40);
    for (const c of DISPLAY_CASES) {
      expect(hasBadChar(c.s), `bad: ${JSON.stringify(c.s)}`).toBe(c.bad);
      expect(hasLookalike(c.s), `lookalike: ${JSON.stringify(c.s)}`).toBe(c.lookalike);
    }
  });

  it('refuses the listed code points', () => {
    for (const cp of [0x61c, 0x2028, 0x2029, 0xad, 0x115f, 0x1160, 0x3164, 0xffa0, 0x2800, 0x200b, 0xe0001, 0xe0100, 0xfe00, 0x85, 0x7f]) {
      const s = `A${String.fromCodePoint(cp)}B`;
      expect(hasBadChar(s), cp.toString(16)).toBe(true);
      expect(replaceBad(s), cp.toString(16)).toBe('AB');
    }
  });

  it('keeps emoji, joiner sequences and flag tags', () => {
    for (const s of ['\u{1F469}\u200D\u{1F4BB}', '\u{1F469}\u200D\u2764\uFE0F\u200D\u{1F468}', '\u2764\uFE0F', '\u{1F3F4}\u{E0067}\u{E0062}\u{E007F}']) {
      expect(hasBadChar(s), JSON.stringify(s)).toBe(false);
      expect(replaceBad(s)).toBe(s);
    }
    expect(replaceBad('\u200DA\u200D')).toBe('A');
    expect(sanitizeUntrusted('A\u061CB')).toBe('A\uFFFDB');
    expect(plainUntrusted('a\u2028b\u00ADc')).toBe('a bc');
  });

  it('flags look-alikes without refusing them', () => {
    expect(hasLookalike('p\u0430ypal')).toBe(true);
    expect(hasBadChar('p\u0430ypal')).toBe(false);
    expect(hasLookalike('Kaspa 123 \u{1F600}')).toBe(false);
  });
});
