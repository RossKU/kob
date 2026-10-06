import { describe, expect, it } from 'vitest';
import {
  I64_MAX, UnitsError, assertI64, bpsCeil, bpsFloor, ceilDiv, floorDiv, formatBps, formatKas, formatPricePerToken, formatTokenAmount, formatUnits,
  isOnTick, parseKas, parsePricePerToken, parseTokenAmount, parseUnits, requireTick, roundDiv, roundToTick, roundToTickSafe,
  safeRounding, statePriceToTokenPrice, tokenPriceToStatePrice, tryParseKas,
} from './units';

describe('parse / format', () => {
  it('parses KAS exactly without floating point', () => {
    expect(parseKas('1.5')).toBe(150_000_000n);
    expect(parseKas('0.00000001')).toBe(1n);
    expect(parseKas('  12 ')).toBe(1_200_000_000n);
    expect(parseKas('.5')).toBe(50_000_000n);
    expect(parseKas('7.')).toBe(700_000_000n);
    // a value a double cannot represent: 2^53 + 1 sompi
    expect(parseKas('90071992.54740993')).toBe(9_007_199_254_740_993n);
    expect(parseKas('92233720368.54775807')).toBe(I64_MAX);
  });

  it('refuses malformed, negative and over-precise input instead of rounding money', () => {
    const code = (s: string, d = 8): string => {
      try {
        parseUnits(s, d);
        return 'ok';
      } catch (e) {
        return (e as UnitsError).code;
      }
    };
    expect(code('')).toBe('empty');
    expect(code('   ')).toBe('empty');
    expect(code('-1')).toBe('negative');
    expect(code('abc')).toBe('format');
    expect(code('1.2.3')).toBe('format');
    expect(code('.')).toBe('format');
    expect(code('1e5')).toBe('format');
    expect(code('1,5')).toBe('format');
    expect(code('0.000000001')).toBe('too_many_decimals');
    expect(code('1.5', 0)).toBe('too_many_decimals');
    // trailing zeros beyond the precision lose nothing
    expect(code('1.500000000')).toBe('ok');
    expect(tryParseKas('x')).toBeNull();
    expect(tryParseKas('1')).toBe(100_000_000n);
  });

  it('formats exactly, trimming zeros by default', () => {
    expect(formatKas(150_000_000n)).toBe('1.5');
    expect(formatKas(100_000_000n)).toBe('1');
    expect(formatKas(1n)).toBe('0.00000001');
    expect(formatKas(0n)).toBe('0');
    expect(formatKas(-150_000_000n)).toBe('-1.5');
    expect(formatKas(150_000_000n, { trim: false })).toBe('1.50000000');
    expect(formatKas(150_000_000n, { minFraction: 2 })).toBe('1.50');
    expect(formatKas(123_456_789_012_345_678n, { group: ',' })).toBe('1,234,567,890.12345678');
    expect(formatKas(150_000_000n, { point: ',' })).toBe('1,5');
    expect(formatKas(199_999_999n, { maxFraction: 2 })).toBe('2');
    expect(formatKas(125_000_000n, { maxFraction: 1 })).toBe('1.3');
    expect(formatBps(300n)).toBe('3.00');
  });

  it('round-trips parse(format(x)) for edge values', () => {
    for (const v of [0n, 1n, 9n, 10n, 99_999_999n, 100_000_000n, 9_007_199_254_740_993n, I64_MAX]) {
      expect(parseKas(formatKas(v))).toBe(v);
      expect(parseKas(formatKas(v, { trim: false }))).toBe(v);
    }
    for (const d of [0, 1, 3, 8, 18]) {
      const v = 123_456_789_123_456_789n;
      expect(parseTokenAmount(formatTokenAmount(v, d), d)).toBe(v);
    }
    expect(formatTokenAmount(1_234_567n, 3)).toBe('1234.567');
    expect(formatUnits(5n, 0)).toBe('5');
  });
});

describe('integer helpers', () => {
  it('ceilDiv / floorDiv / roundDiv', () => {
    expect(ceilDiv(10n, 3n)).toBe(4n);
    expect(ceilDiv(9n, 3n)).toBe(3n);
    expect(ceilDiv(0n, 3n)).toBe(0n);
    expect(floorDiv(10n, 3n)).toBe(3n);
    expect(roundDiv(5n, 2n)).toBe(3n);
    expect(roundDiv(4n, 3n)).toBe(1n);
    expect(() => ceilDiv(1n, 0n)).toThrow(RangeError);
    expect(() => ceilDiv(-1n, 2n)).toThrow(RangeError);
  });

  it('bps math floors like the covenants and ceilings on request', () => {
    expect(bpsFloor(260_000_000n, 300n)).toBe(7_800_000n);
    expect(bpsFloor(101n, 300n)).toBe(3n);
    expect(bpsCeil(101n, 300n)).toBe(4n);
    expect(bpsFloor(9_007_199_254_740_993n, 10_000n)).toBe(9_007_199_254_740_993n);
  });

  it('assertI64 bounds', () => {
    expect(assertI64(I64_MAX)).toBe(I64_MAX);
    expect(() => assertI64(I64_MAX + 1n)).toThrow(RangeError);
    expect(() => assertI64(-1n)).toThrow(RangeError);
  });
});

describe('state price (per scale base units) <-> per whole token', () => {
  // the wallet's scale is 10^decimals: the state price is the price per whole token
  it('is the identity when the scale is 10^decimals', () => {
    expect(statePriceToTokenPrice(250_000_000n, 8, 100_000_000n)).toBe(250_000_000n);
    expect(tokenPriceToStatePrice(250_000_000n, 8, 100_000_000n, 'down')).toBe(250_000_000n);
    expect(formatPricePerToken(250_000_000n, 8, 100_000_000n)).toBe('2.5');
  });

  it('converts a foreign scale and rounds in the requested direction', () => {
    // a 12-decimal token quotes per 10^9 base units (the scale cap): 3 sompi per whole token is 0.003 sompi per 10^9 base units
    expect(tokenPriceToStatePrice(3_000n, 12, 1_000_000_000n, 'down')).toBe(3n);
    expect(tokenPriceToStatePrice(3_001n, 12, 1_000_000_000n, 'down')).toBe(3n);
    expect(tokenPriceToStatePrice(3_001n, 12, 1_000_000_000n, 'up')).toBe(4n);
    expect(statePriceToTokenPrice(3n, 12, 1_000_000_000n)).toBe(3_000n);
    // a scale of 500 base units on a 3-decimal token (not a power of ten: only to exercise the rounding)
    expect(tokenPriceToStatePrice(3n, 3, 500n, 'nearest')).toBe(2n);
    expect(statePriceToTokenPrice(1n, 3, 500n, 'up')).toBe(2n);
  });

  it('formats and parses KAS per token exactly', () => {
    // a 3-decimal order quoted per 10^3 base units: 2.5 KAS per token
    expect(formatPricePerToken(250_000_000n, 3, 1000n)).toBe('2.5');
    expect(parsePricePerToken('2.5', 3, 1000n, 'up')).toBe(250_000_000n);
    expect(parsePricePerToken('0.00000001', 3, 1000n, 'down')).toBe(1n);
    // a 12-decimal token quoted per 10^9 base units: 1 sompi per 10^9 base units is 1000 sompi per token
    expect(formatPricePerToken(1n, 12, 1_000_000_000n, { maxFraction: 8 })).toBe('0.00001');
    expect(formatPricePerToken(1n, 3, 3n, { maxFraction: 8 })).toBe('0.00000333');
  });
});

describe('ticks', () => {
  it('rounds a sell up and a buy down (never worse than typed)', () => {
    expect(safeRounding('sell')).toBe('up');
    expect(safeRounding('buy')).toBe('down');
    expect(roundToTickSafe(1_234n, 100n, 'sell')).toBe(1_300n);
    expect(roundToTickSafe(1_234n, 100n, 'buy')).toBe(1_200n);
    expect(roundToTickSafe(1_200n, 100n, 'sell')).toBe(1_200n);
    expect(roundToTick(1_250n, 100n, 'nearest')).toBe(1_300n);
    expect(roundToTick(77n, 0n, 'up')).toBe(77n);
  });

  it('isOnTick and requireTick (the refuse policy)', () => {
    expect(isOnTick(1_200n, 100n)).toBe(true);
    expect(isOnTick(1_201n, 100n)).toBe(false);
    expect(isOnTick(7n, 0n)).toBe(true);
    expect(requireTick(1_201n, 100n)).toBeNull();
    expect(requireTick(1_200n, 100n)).toBe(1_200n);
  });
});
