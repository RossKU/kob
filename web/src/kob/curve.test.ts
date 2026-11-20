// The curve model: the default table from kob-wasm, the numbers beside the graph, the step line,
// the sale split and its validation.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import {
  CHROMATIC_MAX_OCTAVES, curveNumbers, curveProblems, curveSpecOfSale, curveSteps, defaultSalePrice, tablePreview, curveTable, currentLevel, defaultSaleForm, keepPercentText, levelSizes, parsePercent, parseSale, saleSplit, stepGeometry,
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

  it('merges levels narrower than one unit of the graph, keeping its shape, scale and sold boundary', () => {
    const amount = 1_000_000n * WHOLE;
    const t = curveTable(kob, { amount, levels: 1000, total: 10_000n * KAS, multipleMilli: 5000n, scale: SCALE });
    expect(t.table).toHaveLength(1000);
    const left = amount - 333n * t.levelSize - t.levelSize / 2n; // mid level 333
    const all = curveSteps(t.levelSize, t.table, amount, left, 1e9);
    const merged = curveSteps(t.levelSize, t.table, amount, left);
    expect(all).toHaveLength(1000);
    expect(merged.length).toBeLessThanOrEqual(2 * 320 + 4);
    expect(merged.length).toBeLessThan(400);
    // contiguous from 0 to 1; each merged step keeps its first level's price
    expect(merged[0]!.from).toBe(0);
    expect(merged[merged.length - 1]!.to).toBe(1);
    merged.forEach((s, i) => {
      if (i > 0) expect(s.from).toBe(merged[i - 1]!.to);
      expect(s.price).toBe(t.table[s.level]);
    });
    const [ga, gm] = [stepGeometry(all, 320, 96), stepGeometry(merged, 320, 96)];
    expect(gm.maxPrice).toBe(ga.maxPrice);
    expect(gm.soldX).toBe(ga.soldX);
    expect(merged.find((s) => s.price === t.table[0])).toBeTruthy();
    // at the graph's own size the line moves by less than one unit anywhere
    const yAt = (st: typeof all, x: number) => {
      const s = st.find((v) => v.from <= x && x < v.to) ?? st[st.length - 1]!;
      return (Number(s.price) / Number(ga.maxPrice)) * 96 * 0.9;
    };
    for (let i = 0; i < 3200; i++) expect(Math.abs(yAt(merged, i / 3200) - yAt(all, i / 3200))).toBeLessThan(1);
    // a curve of wide levels is drawn one step per level
    expect(curveSteps(30n, [10n, 20n, 30n, 40n], 100n, 45n)).toHaveLength(4);
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

describe('chromatic curve (one semitone per level)', () => {
  const chromatic = (amount: bigint, scale: bigint, octaves: number, total = 10_000n * KAS) =>
    curveTable(kob, { amount, levels: 12 * octaves + 1, total, multipleMilli: 5000n, scale, shape: 'chromatic', octaves });

  it('is the protocol table: the same vector as crates/kob-protocol/tests/curve.rs and the CLI', () => {
    const t = chromatic(2_500_000_000n, 100n, 1);
    expect(t.table).toEqual([27634n, 29278n, 31018n, 32863n, 34817n, 36888n, 39081n, 41405n, 43867n, 46475n, 49239n, 52167n, 55268n]);
    expect(t.realised).toBeLessThanOrEqual(10_000n * KAS);
  });

  it('doubles every 12 levels over 1 to 8 octaves, the top 2^octaves times the start', () => {
    for (let n = 1; n <= CHROMATIC_MAX_OCTAVES; n++) {
      const t = chromatic(1_000_000n * WHOLE, SCALE, n);
      expect(t.table).toHaveLength(12 * n + 1);
      for (let i = 12; i < t.table.length; i++) expect(t.table[i]).toBe(2n * t.table[i - 12]!);
      for (let i = 1; i < t.table.length; i++) expect(t.table[i]! > t.table[i - 1]!).toBe(true);
      expect(t.table[12 * n]).toBe(t.table[0]! << BigInt(n));
      expect(t.realised).toBeLessThanOrEqual(10_000n * KAS);
    }
  });

  it('refuses a start too low for a higher price at every semitone, which the create form says plainly', async () => {
    // 10^9 whole tokens for 50 KAS: rounded on their own, the levels would step down at the octave
    let msg = '';
    try {
      chromatic(10n ** 17n, SCALE, 2, 50n * KAS);
    } catch (e) {
      msg = e instanceof Error ? e.message : String(e);
    }
    expect(msg).toMatch(/too low for a higher price at every semitone/);
    const { curveErrorKey } = await import('../ui/issue/issue-model');
    expect(curveErrorKey(msg)).toBe('issue.sale.lowStart');
    expect(curveErrorKey('curve: amount, total and scale must be > 0, tip >= 0')).toBeNull();
    // a start of 17 sompi and up rises at every level
    for (const total of [60n, 100n, 1_000n]) {
      let t: bigint[] | null = null;
      try {
        t = chromatic(10n ** 17n, SCALE, 2, total * KAS).table;
      } catch {
        t = null;
      }
      if (t) for (let i = 1; i < t.length; i++) expect(t[i]! > t[i - 1]!, `${total} KAS, level ${i}`).toBe(true);
    }
  });

  it('has fewer levels when the amount is small, and refuses 0 or 9 octaves', () => {
    expect(chromatic(7n, 1n, 1, 700n * KAS).table).toHaveLength(7);
    expect(() => chromatic(100n, 1n, 0)).toThrow(/octaves/);
    expect(() => chromatic(100n, 1n, 9)).toThrow(/octaves/);
  });

  it('validates the octaves of the sale form and builds its spec (levels and multiple not used)', () => {
    const f = { ...defaultSaleForm(), mode: 'curve' as const, shape: 'chromatic' as const };
    const ok = parseSale(f, 1_000n * WHOLE, 8);
    expect(ok.issues).toEqual([]);
    expect(ok.parsed).toMatchObject({ mode: 'curve', shape: 'chromatic', octaves: 2, levels: 25, total: 10_000n * KAS });
    expect(parseSale({ ...f, octaves: '8', levels: 'x', multiple: 'x' }, 1_000n * WHOLE, 8).parsed).toMatchObject({ octaves: 8, levels: 97 });
    const codes = (octaves: string) => parseSale({ ...f, octaves }, 1_000n * WHOLE, 8).issues.map((i) => [i.code, i.field]);
    for (const bad of ['0', '9', 'x', '', '1.5', '-1']) expect(codes(bad)).toEqual([['sale.octaves.invalid', 'sale.octaves']]);
    // the spec gives the same table as a direct call
    const spec = curveSpecOfSale(ok.parsed!, SCALE);
    expect(spec).toMatchObject({ shape: 'chromatic', octaves: 2, levels: 25 });
    expect(curveTable(kob, spec).table).toEqual(chromatic(1_000n * WHOLE, SCALE, 2).table);
    // a linear sale ignores the octaves; a one-price sale is never chromatic
    expect(parseSale({ ...f, shape: 'linear', octaves: 'x' }, 1_000n * WHOLE, 8).parsed).toMatchObject({ shape: 'linear', octaves: 0, levels: 100 });
    expect(parseSale({ ...f, mode: 'ask', octaves: 'x', price: '1' }, 10n, 0).parsed).toMatchObject({ shape: 'linear' });
    expect(curveSpecOfSale(parseSale({ ...f, shape: 'linear' }, 1_000n * WHOLE, 8).parsed!, SCALE).shape).toBeUndefined();
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
