// Conformance: every vector of the reference's test-vectors.json (Hallmarks v1.0, CC0) must be reproduced exactly (spec section 7), plus the pins of
// UPSTREAM.md: the vectors file and the wordlist are the bytes the reference release carries.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { BIP39_ENGLISH } from './bip39-english';
import { hallmark, hex, sha256 } from './hallmark';

interface Color { L: number; C: number; h: number; hex: string }
interface Vector {
  label: string;
  input: string;
  sha256: string;
  cells: number[][];
  words: string[];
  wordsText: string;
  colors: { standard: { background: Color; primary: Color; accent: Color } };
}
const file = new URL('./test-vectors.json', import.meta.url);
const parsed = JSON.parse(readFileSync(file, 'utf8')) as { spec_version: string; vectors: Vector[] };
const sha256Hex = (data: Uint8Array | string): string => createHash('sha256').update(data).digest('hex');

describe('hallmarks v1.0 pins', () => {
  it('test-vectors.json is the file of hallmarks 1.0.0', () => {
    expect(sha256Hex(readFileSync(file))).toBe('661e880331cd2d98f3103b870d84deb2d8daaa454978fd14a465e1a507678879');
    expect(parsed.spec_version).toBe('1.0');
    expect(parsed.vectors.length).toBeGreaterThanOrEqual(10);
  });
  it('the wordlist is the 2048 BIP-39 English words', () => {
    expect(BIP39_ENGLISH).toHaveLength(2048);
    expect(new Set(BIP39_ENGLISH).size).toBe(2048);
    expect(sha256Hex(`${BIP39_ENGLISH.join('\n')}\n`)).toBe('2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda');
  });
});

describe('sha256', () => {
  it('matches node:crypto for lengths around the block boundaries', () => {
    for (const n of [0, 1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 1000]) {
      const data = Uint8Array.from({ length: n }, (_, i) => (i * 7 + n) & 0xff);
      expect(hex(sha256(data))).toBe(sha256Hex(data));
    }
  });
});

describe('hallmark conformance vectors', () => {
  for (const v of parsed.vectors) {
    it(v.label, () => {
      expect(hex(sha256(new TextEncoder().encode(v.input)))).toBe(v.sha256);
      const h = hallmark(v.input);
      expect(h.cells).toEqual(v.cells);
      expect(h.words).toEqual(v.words);
      expect(h.wordsText).toBe(v.wordsText);
      const std = v.colors.standard;
      for (const k of ['background', 'primary', 'accent'] as const) {
        expect(h[k].hex).toBe(std[k].hex);
        expect(h[k].L).toBe(std[k].L);
        expect(h[k].C).toBe(std[k].C);
        expect(h[k].h).toBeCloseTo(std[k].h, 9);
      }
    });
  }
  it('is symmetric and changes with one character', () => {
    const a = hallmark('fee989ebf4bcd31ed0c1d4d7e2e3de3c0bd54f1b9a1b3a9a0000000000bae07c');
    for (const row of a.cells) expect([row[0], row[1]]).toEqual([row[4], row[3]]);
    expect(hallmark('fee989eb').wordsText).not.toBe(hallmark('fee989ec').wordsText);
  });
});
