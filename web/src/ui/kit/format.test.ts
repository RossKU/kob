import { describe, expect, it } from 'vitest';
import { amountText, CELL_CHARS, columnFraction, fitAmountColumn, fixedPricePerToken, fixedUnits, groupDigits, kasText, pricePerTokenText, scaleOfDecimals, shortAddress, shortId, signedKasText, tickKasFraction, tickPriceFraction, tokenText, tokenTextShort, trailingZeros, unitsFraction } from './format';

describe('kit format', () => {
  it('shortens ids without touching short strings', () => {
    const id = 'abcd' + '0'.repeat(56) + '1234';
    expect(shortId(id)).toBe('abcd…1234');
    expect(shortId(id, 6, 6)).toBe('abcd00…001234');
    expect(shortId('abc')).toBe('abc');
    expect(shortId('123456789')).toBe('123456789'); // 9 chars: nothing to hide with 4 + 4
  });

  it('keeps the network prefix of an address', () => {
    const a = 'kaspatest:qqabcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnop';
    const s = shortAddress(a);
    expect(s.startsWith('kaspatest:qqabcd')).toBe(true);
    expect(s.endsWith(a.slice(-6))).toBe(true);
    expect(s).toContain('…');
    expect(shortAddress('kaspa:qq')).toBe('kaspa:qq');
  });

  it('formats KAS exactly from sompi, trimming zeros', () => {
    expect(kasText(0n)).toBe('0');
    expect(kasText(100_000_000n)).toBe('1');
    expect(kasText(1n)).toBe('0.00000001');
    expect(kasText(123_456_789_012_345_678n)).toBe('1234567890.12345678'); // beyond Number.MAX_SAFE_INTEGER
    expect(signedKasText(150_000_000n)).toBe('+1.5');
    expect(signedKasText(-25_000_000n)).toBe('-0.25');
  });

  it('formats token amounts with the token decimals and an optional cap', () => {
    expect(tokenText(150_000_000n, 8)).toBe('1.5');
    expect(tokenText(5n, 0)).toBe('5');
    expect(tokenTextShort(123_456_789n, 8, 4)).toBe('1.2346');
    expect(tokenTextShort(100_000_000n, 8, 4)).toBe('1');
  });

  it('formats a state price (sompi per scale base units) as KAS per whole token', () => {
    // 8 decimals, scale 1e8 (one whole token): the state price is sompi per token
    expect(pricePerTokenText(25_100n, 8, 100_000_000n)).toBe('0.000251');
    // 12 decimals, scale capped at 1e9 (a thousandth of a token): the per-token price is 1000 x the state price
    expect(pricePerTokenText(100_000n, 12, 1_000_000_000n)).toBe('1');
    // a price too small for 8 digits still shows as non-zero (an open-list order scale of 1000 base units of a 0-decimal token)
    expect(pricePerTokenText(1n, 0, 1n)).toBe('0.00000001');
    expect(pricePerTokenText(1n, 0, 1_000n)).not.toBe('0');
    expect(pricePerTokenText(0n, 8, 100_000_000n)).toBe('0');
  });

  it('renders an Amount text per kind and a dash for missing values', () => {
    expect(amountText({ kind: 'kas', value: 250_000_000n })).toBe('2.5');
    expect(amountText({ kind: 'token', value: 12_345_678n, decimals: 4 })).toBe('1234.5678');
    expect(amountText({ kind: 'price', value: 25_100n, decimals: 8, scale: 100_000_000n })).toBe('0.000251');
    // the scale defaults to the token's (10^decimals, capped at 10^9)
    expect(amountText({ kind: 'price', value: 25_100n, decimals: 8 })).toBe('0.000251');
    expect(amountText({ kind: 'kas', value: null })).toBe('—');
    expect(amountText({ kind: 'price', value: undefined })).toBe('—');
  });

  it('groups digits of the integer part only', () => {
    expect(groupDigits('1234567.891234')).toBe('1,234,567.891234');
    expect(groupDigits('-1000')).toBe('-1,000');
    expect(groupDigits('999')).toBe('999');
    expect(groupDigits('—')).toBe('—');
  });
});

describe('fixed-decimal column helpers', () => {
  it('counts trailing zeros and the fraction an amount needs', () => {
    expect(trailingZeros(0n)).toBe(0);
    expect(trailingZeros(1_000n)).toBe(3);
    expect(trailingZeros(1_230n)).toBe(1);
    expect(unitsFraction(100_000_000n, 8)).toBe(0); // 1 token
    expect(unitsFraction(2_365n, 8)).toBe(8);
    expect(unitsFraction(1_130n, 8)).toBe(7);
    expect(unitsFraction(0n, 8)).toBe(0);
    expect(columnFraction([600n, 1130n, 2365n, 21_240n, null], 8)).toBe(8);
    expect(columnFraction([], 8)).toBe(0);
  });

  it('fixedUnits keeps trailing zeros, groups thousands, rounds half up and pads past the token decimals', () => {
    expect(fixedUnits(600n, 8, 8)).toBe('0.00000600');
    expect(fixedUnits(150_000_000n, 8, 3)).toBe('1.500');
    expect(fixedUnits(123_456_789_012n, 8, 2)).toBe('1,234.57');
    expect(fixedUnits(5n, 0, 0)).toBe('5');
    expect(fixedUnits(5n, 0, 2)).toBe('5.00'); // a column shared with finer tokens
    expect(fixedUnits(250n, 2, 4)).toBe('2.5000');
    expect(fixedUnits(-150_000_000n, 8, 2)).toBe('-1.50');
  });

  it('derives the price decimals from the tick and the scale, never from a row', () => {
    // 8 decimals, scale 1e8, tick 100 000 sompi per token: 0.001 KAS per token
    expect(tickPriceFraction(100_000n, 8, 100_000_000n)).toBe(3);
    // a coarse tick still shows at least 2 decimals
    expect(tickPriceFraction(100_000_000n, 8, 100_000_000n)).toBe(2);
    // 1 sompi per whole token: 1e-8 KAS per token
    expect(tickPriceFraction(1n, 8, 100_000_000n)).toBe(8);
    // 12 decimals, scale 1e9: 1 sompi per 1e9 base units = 1e-5 KAS per token
    expect(tickPriceFraction(1n, 12, 1_000_000_000n)).toBe(5);
    // never past the cap (a tick that is not a terminating decimal)
    expect(tickPriceFraction(1n, 8, 3n)).toBe(10);
    expect(tickPriceFraction(null, 8, 1n)).toBeNull();
    expect(tickPriceFraction(1n, 8, null)).toBeNull();
  });

  it('derives the wallet scale from the decimals and KAS decimals from the tick', () => {
    expect(scaleOfDecimals(8)).toBe(100_000_000n);
    expect(scaleOfDecimals(0)).toBe(1n);
    expect(scaleOfDecimals(12)).toBe(1_000_000_000n); // capped at 10^9
    expect(tickKasFraction(100n)).toBe(6);
    expect(tickKasFraction(100_000_000n)).toBe(0);
    expect(tickKasFraction(null)).toBe(8);
  });

  it('formats a state price at a fixed number of decimals, and Amount specs honour `fraction`', () => {
    expect(fixedPricePerToken(25_100n, 8, 100_000_000n, 8, '')).toBe('0.00025100');
    expect(fixedPricePerToken(2_500_000_000_000n, 8, 100_000_000n, 2)).toBe('25,000.00');
    expect(amountText({ kind: 'kas', value: 250_000_000n, fraction: 4 })).toBe('2.5000');
    expect(amountText({ kind: 'token', value: 12_340_000n, decimals: 4, fraction: 3 })).toBe('1234.000');
    expect(amountText({ kind: 'price', value: 25_100n, decimals: 8, scale: 100_000_000n, fraction: 6 })).toBe('0.000251');
    expect(amountText({ kind: 'kas', value: null, fraction: 4 })).toBe('—');
  });
});

describe('amount columns never cut a digit', () => {
  const texts = (cells: { text: string }[]) => cells.map((c) => c.text);
  const fits = (cells: { text: string }[]) => cells.every((c) => c.text.length <= CELL_CHARS);

  it('a column that fits keeps its fixed decimals and needs no tooltip', () => {
    const cells = fitAmountColumn([700_000n, 3_000_000n, 12_345_600_000n], 8);
    expect(texts(cells)).toEqual(['0.007', '0.030', '123.456']);
    expect(cells.every((c) => c.exact === null)).toBe(true);
  });

  it('0 decimals: whole numbers, grouped; a huge amount is compact with the exact amount in the tooltip', () => {
    expect(texts(fitAmountColumn([1n, 42n, 999_999n], 0))).toEqual(['1', '42', '999,999']);
    const cells = fitAmountColumn([56_609_953_123n, 7n, 1_500n], 0, { unit: 'ZERO' });
    expect(texts(cells)).toEqual(['56.61B', '7', '1.50K']);
    expect(cells.map((c) => c.exact)).toEqual(['56,609,953,123 ZERO', null, '1,500 ZERO']);
    expect(fits(cells)).toBe(true);
    // a column of 10 whole digits still fits as it is
    expect(texts(fitAmountColumn([1_234_567n, 8n], 0))).toEqual(['1,234,567', '8']);
  });

  it('18 decimals: one base unit no longer forces 18 decimals on the column (the old "56,609,95…" / "0.0000000…")', () => {
    const one = 10n ** 18n;
    const cells = fitAmountColumn([56_609_953n * one + 1n, 1n, 25n * one / 10n], 18, { unit: 'EIGHTEEN' });
    expect(texts(cells)).toEqual(['56.61M', '<0.01', '2.50']);
    expect(cells[0]!.exact).toBe('56,609,953.000000000000000001 EIGHTEEN');
    expect(cells[1]!.exact).toBe('0.000000000000000001 EIGHTEEN');
    expect(cells[2]!.exact).toBeNull();
    expect(fits(cells)).toBe(true);
    // whole digits that fit: the decimals shrink for the whole column, a tiny amount reads "<0.0001", never 0
    const mid = fitAmountColumn([12_345n * one, 1n, one / 3n], 18);
    expect(texts(mid)).toEqual(['12,345.000', '<0.001', '0.333']);
    // a narrower cell (the trade tape): two decimals
    expect(texts(fitAmountColumn([12_345n * one, 1n, one / 3n], 18, { width: 9 }))).toEqual(['12,345.00', '<0.01', '0.33']);
    expect(fits(mid)).toBe(true);
    // every amount of 18 decimals exactly at 10 characters or less is shown as it is
    expect(texts(fitAmountColumn([one / 1000n], 18))).toEqual(['0.001']);
  });

  it('huge amounts of every size fit; nothing above a thousand reads as a small number', () => {
    for (const decimals of [0, 8, 18]) {
      const u = 10n ** BigInt(decimals);
      const values = [1n, u, 999n * u, 1_000n * u, 123_456_789n * u, 98_765_432_101n * u, 5_000_000_000_000n * u];
      const cells = fitAmountColumn(values, decimals);
      expect(fits(cells), `decimals ${decimals}: ${texts(cells).join(' ')}`).toBe(true);
      expect(cells.at(-1)!.text).toBe('5.00T');
      expect(cells[4]!.text).toBe('123.46M');
      expect(cells.map((c) => c.text).every((s) => !s.includes('…'))).toBe(true);
    }
    expect(texts(fitAmountColumn([null, 5n], 0))).toEqual(['—', '5']);
  });
});
