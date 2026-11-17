// The curve model: the default table from kob-wasm, the numbers beside the graph, the step line, the sale split and its validation.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import {
  curveNumbers, curveProblems, curveSteps, defaultSalePrice, tablePreview, curveTable, currentLevel, defaultSaleForm, keepPercentText, levelSizes, parsePercent, parseSale, saleSplit, stepGeometry,
} from './curve';

const kob = loadKobNode();
const SCALE = 100_000_000n;
const KAS = 100_000_000n;
const WHOLE = SCALE;

describe('curve table', () => {
  it('builds the UI default (1,000,000 tokens, 100 levels, 10,000 KAS, x5) within the target', () => {
    const amount = 1_000_000n * WHOLE;
    const t = curveTable(kob, { amount, levels: 100, total: 10_000n * KAS, multipleMilli: 5000n, scale: SCALE });
    expect(t.table).toHaveLength(100);
    expect(t.levelSize).toBe(10_000n * WHOLE);
    expect(t.realised).toBeLessThanOrEqual(10_000n * KAS);
    expect(t.realised).toBeGreaterThan(9_999n * KAS);
    // non-decreasing, last about 5x the first
    for (let i = 1; i < t.table.length; i++) expect(t.table[i]! >= t.table[i - 1]!).toBe(true);
    const ratio = Number(t.table[99]!) / Number(t.table[0]!);
    expect(ratio).toBeGreaterThan(4.99);
    expect(ratio).toBeLessThan(5.01);
    const n = curveNumbers(t, amount, SCALE);
    expect(n.first).toBe(t.table[0]);
    expect(n.last).toBe(t.table[99]);
    expect(n.total).toBe(t.realised);
    // the average is the total over the amount: 10,000 KAS / 1,000,000 tokens = 0.01 KAS
    expect(Number(n.average)).toBeCloseTo(1_000_000, -2);
  });

  it('has fewer levels when the amount is small, and refuses bad specs', () => {
    const t = curveTable(kob, { amount: 7n, levels: 100, total: 70n * KAS, multipleMilli: 5000n, scale: 1n });
    expect(t.table).toHaveLength(7);
    expect(() => curveTable(kob, { amount: 100n, levels: 0, total: KAS, multipleMilli: 5000n, scale: 1n })).toThrow(/levels/);
    expect(() => curveTable(kob, { amount: 100n, levels: 10, total: KAS, multipleMilli: 500n, scale: 1n })).toThrow(/multiple/);
  });

  it('sizes levels and finds the current level', () => {
    expect(levelSizes(30n, 4, 100n)).toEqual([30n, 30n, 30n, 10n]);
    expect(currentLevel({ total: '100', amountLeft: '100', levelSize: '30' })).toBe(0);
    expect(currentLevel({ total: '100', amountLeft: '45', levelSize: '30' })).toBe(1);
    expect(currentLevel({ total: '100', amountLeft: '10', levelSize: '30' })).toBe(3);
  });
});

describe('graph', () => {
  it('draws a step per level over the sold share, marking sold levels', () => {
    const steps = curveSteps(30n, [10n, 20n, 30n, 40n], 100n, 45n);
    expect(steps.map((s) => [s.from, s.to])).toEqual([[0, 0.3], [0.3, 0.6], [0.6, 0.9], [0.9, 1]]);
    expect(steps.map((s) => s.sold)).toEqual([true, false, false, false]);
    const g = stepGeometry(steps, 100, 50, 1);
    expect(g.maxPrice).toBe(40n);
    expect(g.path).toBe('M0 37.5 H30 L30 25 H60 L60 12.5 H90 L90 0 H100');
    expect(g.soldPath).toBe('M0 37.5 H30');
    expect(g.openPath).toBe('M30 25 H60 L60 12.5 H90 L90 0 H100');
    expect(g.soldX).toBe(30);
    expect(g.area).toBe('M30 25 H60 L60 12.5 H90 L90 0 H100 V50 H30 Z');
  });

  it('has no sold part on a fresh curve', () => {
    const g = stepGeometry(curveSteps(50n, [1n, 2n], 100n), 10, 10);
    expect(g.soldPath).toBe('');
    expect(g.soldX).toBeNull();
  });
});

describe('sale split ("Keep in my wallet")', () => {
  it('parses a percent of at most two decimals', () => {
    expect(parsePercent('0')).toEqual({ bps: 0n });
    expect(parsePercent('')).toEqual({ bps: 0n });
    expect(parsePercent('10')).toEqual({ bps: 1000n });
    expect(parsePercent('12.34')).toEqual({ bps: 1234n });
    expect(parsePercent('100')).toEqual({ bps: 10_000n });
    expect(parsePercent('100.01')).toEqual({ error: 'range' });
    expect(parsePercent('1.234')).toEqual({ error: 'syntax' });
    expect(parsePercent('-1')).toEqual({ error: 'syntax' });
  });

  it('sells supply x (1 - keep), rounded down to a base unit, and links back', () => {
    expect(saleSplit(1_000_000n, 0n)).toEqual({ sell: 1_000_000n, keep: 0n });
    expect(saleSplit(1_000_000n, 1000n)).toEqual({ sell: 900_000n, keep: 100_000n });
    expect(saleSplit(7n, 5000n)).toEqual({ sell: 3n, keep: 4n });
    expect(saleSplit(100n, 10_000n)).toEqual({ sell: 0n, keep: 100n });
    expect(keepPercentText(1_000_000n, 900_000n)).toBe('10');
    expect(keepPercentText(1_000_000n, 1_000_000n)).toBe('0');
    expect(keepPercentText(3n, 2n)).toBe('33.33');
    expect(keepPercentText(1000n, 875n)).toBe('12.5');
  });

  it('validates the sale form', () => {
    const f = { ...defaultSaleForm(), mode: 'curve' as const };
    const ok = parseSale(f, 1_000n * WHOLE, 8);
    expect(ok.issues).toEqual([]);
    expect(ok.parsed).toMatchObject({ mode: 'curve', sell: 1_000n * WHOLE, keep: 0n, levels: 100, total: 10_000n * KAS, multipleMilli: 5000n });
    // keep 10%
    expect(parseSale({ ...f, keepPct: '10' }, 1_000n * WHOLE, 8).parsed).toMatchObject({ sell: 900n * WHOLE, keep: 100n * WHOLE });
    // the amount wins over the percent
    expect(parseSale({ ...f, keepPct: '10', amount: '250' }, 1_000n * WHOLE, 8).parsed).toMatchObject({ sell: 250n * WHOLE, keep: 750n * WHOLE });
    const codes = (over: Partial<typeof f>) => parseSale({ ...f, ...over }, 1_000n * WHOLE, 8).issues.map((i) => i.code);
    expect(codes({ keepPct: '100' })).toEqual(['sale.amount.zero']);
    expect(codes({ keepPct: 'x' })).toEqual(['sale.keep.invalid']);
    expect(codes({ amount: '2000' })).toEqual(['sale.amount.too_large']);
    expect(codes({ levels: '0' })).toEqual(['sale.levels.invalid']);
    expect(codes({ levels: '1001' })).toEqual(['sale.levels.invalid']);
    expect(codes({ multiple: '0.5' })).toEqual(['sale.multiple.invalid']);
    expect(codes({ totalKas: '0' })).toEqual(['sale.total.invalid']);
    // "Keep all in my wallet": nothing to parse
    expect(parseSale(defaultSaleForm(), 1n, 0)).toEqual({ issues: [], parsed: null });
    // a one-price sale has no levels or multiple
    expect(parseSale({ ...f, mode: 'ask', levels: 'x', multiple: 'x', price: '2.5' }, 10n, 0)).toMatchObject({ issues: [], parsed: { sell: 10n, price: 250_000_000n, total: 25n * KAS } });
    expect(parseSale({ ...f, mode: 'ask', price: '' }, 10n, 0).issues.map((i) => i.code)).toEqual(['sale.price.invalid']);
    expect(defaultSalePrice(1_000_000n * WHOLE, SCALE)).toBe(1_000_000n);
  });
});

describe('curveProblems (stricter than the protocol)', () => {
  const ok = { table: ['100', '120', '120', '150'], tip: '0', levelSize: '10', total: '35', amountLeft: '35', minFill: '1', scale: '100000000', activeFrom: '0' };
  const of = (over: Partial<typeof ok>, decimals?: number) => curveProblems({ ...ok, ...over } as never, decimals);
  it('accepts a sound curve (equal neighbours allowed)', () => {
    expect(of({})).toEqual([]);
    expect(of({}, 8)).toEqual([]);
  });
  it('refuses each bad field', () => {
    expect(of({ table: [] as never })).toContain('table');
    expect(of({ table: ['100', '0', '120', '150'] })).toContain('table');
    expect(of({ table: ['100', '120', '110', '150'] })).toEqual(['falls']);
    expect(of({ tip: '100' })).toEqual(['tip']);
    expect(of({ tip: '-1' })).toEqual(['tip']);
    expect(of({ levelSize: '0' })).toContain('levelSize');
    expect(of({ total: '41' })).toEqual(['total']);
    expect(of({ total: '30', amountLeft: '30' })).toEqual(['total']);
    expect(of({ amountLeft: '36' })).toEqual(['amountLeft']);
    expect(of({ minFill: '0' })).toEqual(['minFill']);
    // at most 7 levels + 1: what one fill always reaches
    expect(of({ minFill: '71' })).toEqual([]);
    expect(of({ minFill: '72' })).toEqual(['minFill']);
    expect(of({}, 2)).toEqual(['scale']);
    expect(of({ scale: '1000000000000000000' }, 18)).toEqual(['scale']);
    expect(of({ scale: '1000000000' }, 18)).toEqual([]);
    expect(of({ activeFrom: '-1' })).toEqual(['activeFrom']);
  });
  it('previews a table as its first three and last two levels', () => {
    expect(tablePreview([1n, 2n, 3n], String)).toBe('1 · 2 · 3');
    expect(tablePreview([1n, 2n, 3n, 4n, 5n, 6n, 7n], String)).toBe('1 · 2 · 3 … 6 · 7');
  });
});
