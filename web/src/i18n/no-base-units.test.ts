// Founder decision 2026-10-17: "base units" never appears in front of a user. Every amount of a token is said in whole tokens (app/token-display.ts:
// the registry's decimals, this browser's issuance of it, the scale of its orders, else 8 assumed, marked); a raw integer only inside the collapsed
// technical details, labelled as the chain's own amount.
import { describe, expect, it } from 'vitest';
import { allKeys, t, tokenAmountText } from './index';

describe('no "base units" in any English string', () => {
  it('no string of the dictionary says "base unit"', () => {
    const bad = allKeys().filter((k) => /base[ -]?units?/i.test(t(k)));
    expect(bad).toEqual([]);
  });

  it('an amount of a token nothing names is said in whole tokens on the assumed 8 decimals', () => {
    expect(tokenAmountText(null, 4_500_000_000_000n)).toBe('45,000 tokens (decimals assumed 8)');
  });
});
