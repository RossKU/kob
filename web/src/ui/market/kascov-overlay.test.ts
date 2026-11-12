import { describe, expect, it } from 'vitest';
import type { KascovCandle } from '../../data/kascov-candles';
import { ratToNumber, type Rational } from '../../kob/pair';
import { kascovChartGate, kascovTitleLine, kasPerWholeToken, laidOnBars, lineData, overlayPriceText, pairPoints, spotPairPrice, spotPrice, tokenPoints, withKascovLine } from './kascov-overlay';

const c = (t: number, quoteSompi: bigint, baseAmount: bigint): KascovCandle => ({ t, quoteSompi, baseAmount });
const R = (num: bigint, den: bigint): Rational => ({ num, den });
const same = (a: Rational | null | undefined, b: Rational) => expect(!!a && a.num * b.den === b.num * a.den).toBe(true);

describe('units: exactly as KOB prices its own candles (KAS per whole token with the market decimals)', () => {
  it('quote_sompi / base_amount is sompi per base unit; a whole token is 10^decimals of them; a KAS is 10^8 sompi', () => {
    // 2 KAS for 1000 base units of a 2-decimals token: 10 tokens for 2 KAS = 0.2 KAS per token
    same(kasPerWholeToken(c(1, 200_000_000n, 1000n), 2), R(1n, 5n));
    // the real THEN sample (3 display decimals): 484325962 sompi for 19716 units
    expect(ratToNumber(kasPerWholeToken(c(1, 484_325_962n, 19_716n), 3))).toBeCloseTo(0.245651, 6);
    // the same close read with 8 decimals: the decimals are the market's, never kascov's
    expect(ratToNumber(kasPerWholeToken(c(1, 484_325_962n, 19_716n), 8))).toBeCloseTo(24565.1, 1);
  });

  it('a flipped market (KAS/TOKEN) shows the reciprocal: tokens per KAS', () => {
    const p = tokenPoints({ candles: [c(10, 200_000_000n, 1000n)], decimals: 2 }, false);
    const f = tokenPoints({ candles: [c(10, 200_000_000n, 1000n)], decimals: 2 }, true);
    same(p[0]!.price, R(1n, 5n));
    same(f[0]!.price, R(5n, 1n));
    expect(f[0]!.t).toBe(10);
  });

  it('a pair is BASE KAS price over QUOTE KAS price at the buckets both have; a bucket one leg lacks is skipped; the decimals are each token\'s own', () => {
    const base = { candles: [c(1, 200_000_000n, 1000n), c(2, 200_000_000n, 1000n), c(3, 400_000_000n, 1000n)], decimals: 2 }; // 0.2, 0.2, 0.4
    const quote = { candles: [c(2, 400_000_000n, 1000n), c(3, 400_000_000n, 1000n), c(4, 1n, 1n)], decimals: 2 }; // -, 0.4, 0.4, other
    const pts = pairPoints(base, quote);
    expect(pts.map((x) => x.t)).toEqual([2, 3]);
    same(pts[0]!.price, R(1n, 2n));
    same(pts[1]!.price, R(1n, 1n));
    // different decimals: BASE 0.2 KAS per token (2 decimals); QUOTE 4 KAS per token (0 decimals: 4e8 sompi per unit)
    same(pairPoints({ candles: [c(1, 200_000_000n, 1000n)], decimals: 2 }, { candles: [c(1, 400_000_000n, 1n)], decimals: 0 })[0]!.price, R(1n, 20n));
    // the market list's spots give the same exact figures
    same(spotPairPrice({ num: 13n, den: 800n }, 8, { num: 50n, den: 1n }, 6), R(13n, 400n));
    same(spotPrice({ num: 13n, den: 800n }, 8), R(13n, 800n));
  });
});

describe('the line on the KOB bars', () => {
  const pts = [{ t: 900_000, price: R(1n, 1n) }, { t: 1_800_000, price: R(2n, 1n) }];
  const whole = (r: Rational | null) => (r ? Number(r.num / r.den) : null);

  it('is, per bar, the latest point that starts before the bar end; none before the first point; carried after the last', () => {
    // 5 minute bars (open times in seconds): the bar at 600 s ends at 900 s, before the first 15 minute point
    expect(laidOnBars(pts, [600, 900, 1200, 1500, 1800, 2100, 9000], 300_000).map(whole)).toEqual([null, 1, 1, 1, 2, 2, 2]);
  });

  it('an equal bucket (1h under 1H bars) maps one to one; no points, no line', () => {
    const hourly = [{ t: 3_600_000, price: R(3n, 1n) }, { t: 7_200_000, price: R(4n, 1n) }];
    expect(laidOnBars(hourly, [3600, 7200], 3_600_000).map(whole)).toEqual([3, 4]);
    expect(laidOnBars([], [3600], 3_600_000)).toEqual([null]);
  });

  it('the chart data has a point only where there is a price, at the bar own time', () => {
    expect(lineData([null, R(1n, 4n), R(1n, 2n)], [10, 20, 30])).toEqual([{ time: 20, value: 0.25 }, { time: 30, value: 0.5 }]);
  });
});

describe('the price text: about 5 significant digits, at most 6 decimals, trailing zeros trimmed, no zero run', () => {
  it('reads', () => {
    expect(overlayPriceText(R(13n, 800n))).toBe('0.01625');
    expect(overlayPriceText(R(1234567891n, 100000000000n))).toBe('0.012346');
    expect(overlayPriceText(R(334567n, 10000n))).toBe('33.457');
    expect(overlayPriceText(R(123n, 2n))).toBe('61.5');
    expect(overlayPriceText(R(123456789n, 1000n))).toBe('123,457');
    expect(overlayPriceText(R(1n, 2_500_000n))).toBe('<0.000001');
  });
});

describe('the market list tooltip line', () => {
  it('"kascov: price unit (percent)" against the KOB price, signed in whole percent; no percent without one; nothing without a kascov price', () => {
    expect(kascovTitleLine(R(13n, 800n), R(251n, 10000n), 'KAS')).toBe('kascov: 0.01625 KAS (-35%)');
    expect(kascovTitleLine(R(1n, 25n), R(251n, 10000n), 'KAS')).toBe('kascov: 0.04 KAS (+59%)');
    expect(kascovTitleLine(R(251n, 10000n), R(251n, 10000n), 'KAS')).toBe('kascov: 0.0251 KAS (0%)');
    expect(kascovTitleLine(R(13n, 800n), null, 'KAS')).toBe('kascov: 0.01625 KAS');
    expect(kascovTitleLine(R(13n, 800n), R(0n, 1n), 'KAS')).toBe('kascov: 0.01625 KAS');
    expect(kascovTitleLine(null, R(1n, 1n), 'KAS')).toBeNull();
    expect(kascovTitleLine(R(0n, 1n), R(1n, 1n), 'KAS')).toBeNull();
  });

  it('is the last line of a cell tooltip; a cell with none gets it alone; no line leaves the cell as it was', () => {
    const cell = { text: '1', title: 'one' };
    expect(withKascovLine(cell, 'kascov: 2 KAS')).toEqual({ text: '1', title: 'one\nkascov: 2 KAS' });
    expect(withKascovLine({ text: null, title: null }, 'kascov: 2 KAS')).toEqual({ text: null, title: 'kascov: 2 KAS' });
    expect(withKascovLine(cell, null)).toBe(cell);
  });
});

describe('the chart toggle gate (kascovChartGate)', () => {
  const a = { token: 'aa', decimals: 8, priced: true };
  const b = { token: 'bb', decimals: 2, priced: true };
  it('kascov off for the deployment: no toggle at all', () => {
    expect(kascovChartGate('off', [a])).toEqual({});
  });
  it('every leg priced: the working toggle with its legs, in order', () => {
    expect(kascovChartGate('ok', [a])).toEqual({ kascov: [{ token: 'aa', decimals: 8 }] });
    expect(kascovChartGate('ok', [a, b])).toEqual({ kascov: [{ token: 'aa', decimals: 8 }, { token: 'bb', decimals: 2 }] });
  });
  it('a token without a price (or not in the list): disabled, "token" for one leg and "pair" when either leg of a pair lacks one', () => {
    expect(kascovChartGate('ok', [{ ...a, priced: false }])).toEqual({ kascovWhy: 'token' });
    expect(kascovChartGate('ok', [a, { ...b, priced: false }])).toEqual({ kascovWhy: 'pair' });
    expect(kascovChartGate('ok', [{ ...a, priced: false }, b])).toEqual({ kascovWhy: 'pair' });
    expect(kascovChartGate('ok', [])).toEqual({ kascovWhy: 'token' });
  });
  it('the list not loaded yet or failed: disabled with the list reason, whatever the legs', () => {
    expect(kascovChartGate('loading', [a])).toEqual({ kascovWhy: 'list' });
    expect(kascovChartGate('unavailable', [a, b])).toEqual({ kascovWhy: 'list' });
  });
});
