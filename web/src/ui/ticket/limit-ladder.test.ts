import { describe, expect, it } from 'vitest';
import { planOrder } from '../../kob/plan';
import { errors } from '../../kob/plan-types';
import { CTX, TOKEN, form, ticketEnv } from './ticket-fixtures';
import { buildIntent, layoutOf, switchType } from './form-state';
import { expandLimitLadder, ladderTotals, limitLadderOfForm, limitLadderPrices, limitLadderValues } from './ladder';

const on = { 'ladder.on': 'true', 'ladder.levels': '4', 'ladder.pct': '2' };
const buy = (extra: Record<string, string> = {}) => form('limit', 'buy', { amount: '2', price: '2.3', ...on, ...extra });
const sell = (extra: Record<string, string> = {}) => form('limit', 'sell', { amount: '2', price: '2.7', ...on, ...extra });
const intentOf = (f: ReturnType<typeof buy>) => buildIntent(f, CTX).intent!;

describe('limit ladder prices', () => {
  it('reads the ladder from the form: off by default, 2 to 20 levels, a percent step or an end price', () => {
    expect(limitLadderOfForm(buy(), CTX)).toEqual({ levels: 4, mode: 'pct', bps: 200n });
    expect(limitLadderOfForm(buy({ 'ladder.end': '2.0' }), CTX)).toEqual({ levels: 4, mode: 'end', end: 200_000_000n });
    expect(limitLadderOfForm(buy({ 'ladder.on': '' }), CTX)).toBeNull();
    expect(limitLadderOfForm(buy({ 'ladder.levels': '1' }), CTX)).toBeNull();
    expect(limitLadderOfForm(buy({ 'ladder.levels': '21' }), CTX)).toBeNull();
    expect(limitLadderOfForm(buy({ 'ladder.pct': '' }), CTX)).toBeNull();
    expect(limitLadderOfForm(buy({ 'ladder.pct': '100' }), CTX)).toBeNull();
    expect(limitLadderOfForm(form('ioc', 'buy', on), CTX)).toBeNull();
  });

  it('a percent step compounds: buys go down, sells go up, on the tick, never to a worse limit', () => {
    const b = limitLadderPrices(230_000_000n, 'buy', { levels: 4, mode: 'pct', bps: 200n }, 100n);
    expect(b).toEqual({ prices: [230_000_000n, 225_400_000n, 220_892_000n, 216_474_100n] });
    const s = limitLadderPrices(270_000_000n, 'sell', { levels: 3, mode: 'pct', bps: 150n }, 100n);
    expect(s).toEqual({ prices: [270_000_000n, 274_050_000n, 278_160_800n] });
    // an off-tick result rounds down for a buy and up for a sell
    const odd = limitLadderPrices(100_000_000n, 'buy', { levels: 2, mode: 'pct', bps: 33n }, 100n) as { prices: bigint[] };
    expect(odd.prices[1]).toBe(99_670_000n);
    const odd2 = limitLadderPrices(100_000_000n, 'sell', { levels: 2, mode: 'pct', bps: 33n }, 100n) as { prices: bigint[] };
    expect(odd2.prices[1]).toBe(100_330_000n);
    const off = limitLadderPrices(100_000_100n, 'sell', { levels: 2, mode: 'pct', bps: 1n }, 100n) as { prices: bigint[] };
    expect(off.prices[1]! % 100n).toBe(0n);
    expect(off.prices[1]!).toBeGreaterThanOrEqual((100_000_100n * 10_001n) / 10_000n);
  });

  it('an end price spaces the levels evenly, either way, and ends exactly there', () => {
    expect(limitLadderPrices(230_000_000n, 'buy', { levels: 4, mode: 'end', end: 200_000_000n }, 100n)).toEqual({
      prices: [230_000_000n, 220_000_000n, 210_000_000n, 200_000_000n],
    });
    expect(limitLadderPrices(230_000_000n, 'buy', { levels: 3, mode: 'end', end: 250_000_000n }, 100n)).toEqual({ prices: [230_000_000n, 240_000_000n, 250_000_000n] });
  });

  it('refuses an end equal to the start, a price at zero and levels that collapse onto one tick', () => {
    expect(limitLadderPrices(230_000_000n, 'buy', { levels: 3, mode: 'end', end: 230_000_000n }, 100n)).toEqual({ error: { code: 'same', field: 'ladder.end' } });
    expect(limitLadderPrices(300n, 'buy', { levels: 20, mode: 'pct', bps: 9_000n }, 100n)).toMatchObject({ error: { code: 'price' } });
    expect(limitLadderPrices(1_000n, 'sell', { levels: 3, mode: 'pct', bps: 1n }, 100n)).toMatchObject({ error: { code: 'collapse', level: 2 } });
  });

  it('expands a limit into independent limit orders of the same size and side', () => {
    const r = expandLimitLadder(intentOf(buy()), { levels: 4, mode: 'pct', bps: 200n }, 100n);
    expect(r.ok).toBe(true);
    expect(r.levels.map((l) => l.entryPrice)).toEqual([230_000_000n, 225_400_000n, 220_892_000n, 216_474_100n]);
    expect(r.levels.map((l) => l.shift)).toEqual([0n, -4_600_000n, -9_108_000n, -13_525_900n]);
    for (const l of r.levels) expect(l.intent).toMatchObject({ type: 'limit', side: 'buy', amount: 2n * TOKEN });
    expect(r.levels[0]!.intent).toEqual(intentOf(buy()));
    expect(r.totalAmount).toBe(8n * TOKEN);
    expect(expandLimitLadder(intentOf(form('ioc', 'buy', { amount: '1', price: '2.5' })), { levels: 3, mode: 'pct', bps: 100n }, 100n).errors[0]!.code).toBe('notRepeat');
    expect(expandLimitLadder(intentOf(buy()), { levels: 21, mode: 'pct', bps: 100n }, 100n).errors[0]!.code).toBe('levels');
  });

  it('lists the value of every level and their sum', () => {
    const r = expandLimitLadder(intentOf(buy()), { levels: 3, mode: 'end', end: 210_000_000n }, 100n);
    const v = limitLadderValues(r, 'buy', 100_000_000n);
    expect(v.values).toEqual([460_000_000n, 440_000_000n, 420_000_000n]);
    expect(v.total).toBe(1_320_000_000n);
  });

  it('every level plans and the budget estimate matches the sum of the exact level plans', () => {
    for (const side of ['buy', 'sell'] as const) {
      const r = expandLimitLadder(intentOf(side === 'buy' ? buy() : sell()), { levels: 4, mode: 'pct', bps: 200n }, 100n);
      const plans = r.levels.map((l) => planOrder(ticketEnv(), l.intent));
      for (const p of plans) expect(errors(p), side).toEqual([]);
      const totals = ladderTotals(plans[0]!, r)!;
      expect(totals.tokens).toBe(plans.reduce((a, p) => a + p.disclosure!.tokensEscrowed, 0n));
      const exact = plans.reduce((a, p) => a + p.disclosure!.kasLocked, 0n);
      // the estimate rounds each level's escrow on its own: within a few sompi per level
      const diff = totals.kasLocked > exact ? totals.kasLocked - exact : exact - totals.kasLocked;
      expect(diff, side).toBeLessThan(10n);
    }
  });
});

describe('limit ladder in the form', () => {
  it('shows the ladder fields only while Ladder is on, and only for a limit', () => {
    expect(layoutOf(form('limit', 'buy', {})).main).toContain('ladder.on');
    expect(layoutOf(form('limit', 'buy', {})).main).not.toContain('ladder.levels');
    expect(layoutOf(buy()).main).toEqual(expect.arrayContaining(['ladder.on', 'ladder.levels', 'ladder.pct', 'ladder.end']));
    expect(layoutOf(form('ioc', 'buy', {})).main).not.toContain('ladder.on');
  });

  it('keeps the ladder when the type is picked again', () => {
    expect(switchType(buy(), 'limit').values['ladder.on']).toBe('true');
    expect(switchType(buy(), 'ioc').values['ladder.on']).toBeUndefined();
  });

  it('asks for 2 to 20 levels and a step or an end price', () => {
    const errs = (f: ReturnType<typeof buy>) => buildIntent(f, CTX).errors.map((e) => `${e.field}:${e.code}`);
    expect(errs(buy())).toEqual([]);
    expect(errs(buy({ 'ladder.levels': '1' }))).toEqual(['ladder.levels:range']);
    expect(errs(buy({ 'ladder.levels': '21' }))).toEqual(['ladder.levels:range']);
    expect(errs(buy({ 'ladder.levels': '' }))).toEqual(['ladder.levels:required']);
    expect(errs(buy({ 'ladder.pct': '' }))).toEqual(['ladder.pct:required']);
    expect(errs(buy({ 'ladder.pct': '0' }))).toEqual(['ladder.pct:range']);
    expect(errs(buy({ 'ladder.pct': '', 'ladder.end': '2.0' }))).toEqual([]);
    expect(errs(buy({ 'ladder.end': '2.3' }))).toEqual(['ladder.end:range']);
    // off: nothing of it is checked
    expect(errs(buy({ 'ladder.on': '', 'ladder.levels': '99' }))).toEqual([]);
  });
});
