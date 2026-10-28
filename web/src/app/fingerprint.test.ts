import { describe, expect, it } from 'vitest';
import { hallmark } from '../third-party/hallmarks/hallmark';
import { fingerprint, FINGERPRINT_ALGORITHM } from './fingerprint';

const A = 'fee989eb' + '0'.repeat(48) + 'bae07c42';
const B = 'fee989eb' + '0'.repeat(48) + 'bae07c43';

describe('fingerprint(covenantId)', () => {
  it('is the Hallmarks v1.0 hallmark of the lowercase id', () => {
    const f = fingerprint(A);
    const h = hallmark(A);
    expect(FINGERPRINT_ALGORITHM).toBe('Hallmarks v1.0');
    expect(f.cells).toEqual(h.cells);
    expect(f.wordsText).toBe(h.wordsText);
    expect(f.primary).toBe(h.primary.hex);
    expect(f.words).toHaveLength(3);
  });
  it('is the same for the same id in any case, and cached', () => {
    expect(fingerprint(A.toUpperCase())).toBe(fingerprint(A));
    expect(fingerprint(` ${A} `).wordsText).toBe(fingerprint(A).wordsText);
  });
  it('tells ids that differ in one character apart', () => {
    const a = fingerprint(A);
    const b = fingerprint(B);
    expect(a.wordsText).not.toBe(b.wordsText);
    expect(a.cells).not.toEqual(b.cells);
  });
  it('is 5 columns by 7 rows, symmetric, with colours as #rrggbb', () => {
    const f = fingerprint(A);
    expect(f.cells).toHaveLength(7);
    for (const row of f.cells) {
      expect(row).toHaveLength(5);
      expect([row[0], row[1]]).toEqual([row[4], row[3]]);
    }
    for (const c of [f.background, f.primary, f.accent]) expect(c).toMatch(/^#[0-9a-f]{6}$/);
  });
});
