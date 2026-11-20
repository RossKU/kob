import { describe, expect, it } from 'vitest';
import { DISPLAY_CASES, changesUnderNfc, hasBadChar, hasLookalike, replaceBad } from './display';
import { plainUntrusted, sanitizeUntrusted } from './registry';

describe('display characters (finding L7; the Rust twin is kob-protocol display.rs)', () => {
  it('every shared case gives the same answer as on the Rust side', () => {
    expect(DISPLAY_CASES.length).toBeGreaterThan(70);
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

  it('refuses a joiner or a selector next to a letter, so a ticker cannot hide one', () => {
    for (const [s, kept] of [['US\u200DDT', 'USDT'], ['KAS\uFE0F', 'KAS'], ['KAS\uFE0E', 'KAS'], ['A\u200D\u{1F600}', 'A\u{1F600}'], ['1\uFE0F', '1']] as const) {
      expect(hasBadChar(s), JSON.stringify(s)).toBe(true);
      expect(replaceBad(s)).toBe(kept);
    }
    for (const s of ['\u{1F469}\u{1F3FD}\u200D\u{1F4BB}', '#\uFE0F\u20E3', '\u2764\uFE0E']) expect(hasBadChar(s), JSON.stringify(s)).toBe(false);
  });

  it('flags Lisu, Cherokee and small-capital look-alikes', () => {
    for (const s of ['\uA4D7\uA4EE\uA4E2', '\u13AA', '\u1D0B\u1D00\uA731', 'Kas\u01C0']) expect(hasLookalike(s), JSON.stringify(s)).toBe(true);
  });

  it('the table rule for NFC matches the engine: a decomposed string is flagged, its composed twin is not', () => {
    // every precomposed letter of Latin, Greek and Cyrillic, written decomposed
    let n = 0;
    for (let c = 0xc0; c < 0x530; c++) {
      const s = String.fromCodePoint(c);
      const d = s.normalize('NFD');
      if (d === s) continue;
      // a singleton (NFC never keeps it) is flagged itself
      if (s.normalize('NFC') !== s) {
        expect(changesUnderNfc(s), c.toString(16)).toBe(true);
        continue;
      }
      n++;
      expect(changesUnderNfc(d), c.toString(16)).toBe(true);
      expect(changesUnderNfc(s), c.toString(16)).toBe(false);
    }
    expect(n).toBeGreaterThan(300);
    // Hangul syllables, decomposed
    for (const s of ['\uAC00', '\uAC01', '\uD7A3']) expect(changesUnderNfc(s.normalize('NFD'))).toBe(true);
    expect(changesUnderNfc('\uAC01')).toBe(false);
    expect(hasLookalike('Cafe\u0301')).toBe(true);
    expect(hasLookalike('Caf\u00E9')).toBe(false);
  });
});
