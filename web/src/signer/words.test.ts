// The signer's numbers at a reader's precision: exact when short, a few significant digits otherwise, and a bound never rounded the kind way.
import { describe, expect, it } from 'vitest';
import { decimalText, kasPerTokenWords, kasWords, tokenWords, tokensPerKasWords } from './words';

describe('decimalText', () => {
  it('keeps short values exact and cuts long ones to significant digits, grouped', () => {
    expect(decimalText(1_000n, 8)).toBe('0.00001');
    expect(decimalText(2_000_000_00000000n, 8)).toBe('2,000,000');
    expect(decimalText(123_456_789n, 8, { sig: 4 })).toBe('1.235');
    expect(decimalText(-5n, 0)).toBe('−5');
  });

  it('rounds a bound down or up, never the kind way', () => {
    expect(decimalText(123_456_789n, 8, { sig: 4, mode: 'down' })).toBe('1.234');
    expect(decimalText(123_401_000n, 8, { sig: 4, mode: 'up' })).toBe('1.235');
    expect(decimalText(199_999_999n, 8, { fixed: 2, mode: 'nearest' })).toBe('2.00');
  });
});

describe('the words of the sentence', () => {
  it('KAS: two decimals from 1 KAS on, two significant digits below', () => {
    expect(kasWords(2_400_223_001n)).toBe('24.00');
    expect(kasWords(223_001n)).toBe('0.0022');
    expect(kasWords(223_001n, 'up')).toBe('0.0023');
  });

  it('tokens: exact when short, six significant digits otherwise', () => {
    expect(tokenWords(1_000n, 8)).toBe('0.00001');
    expect(tokenWords(72_345_678n, 8)).toBe('0.723457');
  });

  it('a price per whole token, and the same price as tokens per KAS', () => {
    // 2,000,000 KAS per TBTC (8 decimals, scale 1e8)
    expect(kasPerTokenWords(2_000_000n * 100_000_000n, 100_000_000n, 8)).toBe('2,000,000');
    expect(tokensPerKasWords(2_000_000n * 100_000_000n, 100_000_000n, 8)).toBe('0.0000005');
    // 27.66 KAS per TUSD: about 0.0361533 TUSD per KAS
    expect(tokensPerKasWords(2_766_000_000n, 100_000_000n, 8)).toBe('0.0361533');
  });
});
