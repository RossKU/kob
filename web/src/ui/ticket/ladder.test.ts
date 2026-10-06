import { describe, expect, it } from 'vitest';
import { errors } from '../../kob/plan-types';
import { planOrder } from '../../kob/plan';
import { CTX, TOKEN, form, ticketEnv } from './ticket-fixtures';
import { buildIntent } from './form-state';
import { expandLadder, ladderOfForm, ladderTotals } from './ladder';

const buy = (extra: Record<string, string> = {}) =>
  form('repeatIfd', 'buy', { amount: '2', price: '2.3', 'exit.takeProfit': '2.6', 'ladder.levels': '3', 'ladder.step': '0.1', ...extra });
const intentOf = (f: ReturnType<typeof buy>) => buildIntent(f, CTX).intent!;

describe('ladder expansion', () => {
  it('reads the ladder from the form (off for 0 / 1 / empty levels, a bad step or a non-repeat type)', () => {
    expect(ladderOfForm(buy(), CTX)).toEqual({ levels: 3, step: 10_000_000n });
    expect(ladderOfForm(buy({ 'ladder.levels': '1' }), CTX)).toBeNull();
    expect(ladderOfForm(buy({ 'ladder.levels': '' }), CTX)).toBeNull();
    expect(ladderOfForm(buy({ 'ladder.levels': '21' }), CTX)).toBeNull();
    expect(ladderOfForm(buy({ 'ladder.step': '' }), CTX)).toBeNull();
    expect(ladderOfForm(form('ifd', 'buy', { 'ladder.levels': '3', 'ladder.step': '0.1' }), CTX)).toBeNull();
  });

  it('buy-first levels go down, every price of a level shifts equally, the exit keeps its distance', () => {
    const r = expandLadder(intentOf(buy()), 3, 10_000_000n, 100n);
    expect(r.ok).toBe(true);
    expect(r.levels.map((l) => l.entryPrice)).toEqual([230_000_000n, 220_000_000n, 210_000_000n]);
    expect(r.levels.map((l) => l.exitTakeProfit)).toEqual([260_000_000n, 250_000_000n, 240_000_000n]);
    expect(r.levels.map((l) => l.shift)).toEqual([0n, -10_000_000n, -20_000_000n]);
    expect(r.totalAmount).toBe(6n * TOKEN);
    // level 1 is the typed intent itself
    expect(r.levels[0]!.intent).toEqual(intentOf(buy()));
  });

  it('sell-first levels go up; IFO shifts the stop and the entry stop too', () => {
    const f = form('repeatIfo', 'sell', { amount: '1', price: '2.7', 'entry.stop': '2.65', 'exit.takeProfit': '2.4', 'exit.stop': '2.9', 'exit.stopLimit': '2.95' });
    const r = expandLadder(intentOf(f), 2, 5_000_000n, 100n);
    expect(r.ok).toBe(true);
    const l2 = r.levels[1]!.intent;
    expect(l2.entry).toMatchObject({ price: 275_000_000n, stop: 270_000_000n });
    expect(l2.exit).toMatchObject({ takeProfit: 245_000_000n, stop: 295_000_000n, stopLimit: 300_000_000n });
  });

  it('refuses prices that reach zero, steps off the tick, more than 20 levels and non-repeat intents', () => {
    expect(expandLadder(intentOf(buy()), 3, 120_000_000n, 100n).errors).toContainEqual({ code: 'price', level: 3, field: 'price' });
    expect(expandLadder(intentOf(buy()), 3, 10_000_050n, 100n).errors[0]!.code).toBe('tick');
    expect(expandLadder(intentOf(buy()), 21, 1_000n, 100n).errors[0]!.code).toBe('levels');
    expect(expandLadder(intentOf(buy()), 20, 1_000n, 100n).ok).toBe(true);
    expect(expandLadder(intentOf(buy()), 2, 0n, 100n).errors[0]!.code).toBe('step');
    expect(expandLadder(buildIntent(form('limit', 'buy', { amount: '1', price: '2.3' }), CTX).intent!, 2, 1_000n, 100n).errors[0]!.code).toBe('notRepeat');
  });

  it('buildIntent reports a bad ladder', () => {
    expect(buildIntent(buy({ 'ladder.levels': '21' }), CTX).errors.map((e) => e.field)).toContain('ladder.levels');
    expect(buildIntent(buy({ 'ladder.step': '' }), CTX).errors.map((e) => e.field)).toContain('ladder.step');
    expect(buildIntent(buy({ 'ladder.levels': '1', 'ladder.step': '' }), CTX).errors).toEqual([]);
  });

  it('every level plans, and the budget estimate equals the sum of the exact level plans', () => {
    for (const side of ['buy', 'sell'] as const) {
      const f = side === 'buy' ? buy() : form('repeatIfd', 'sell', { amount: '2', price: '2.7', 'exit.takeProfit': '2.4', 'ladder.levels': '3', 'ladder.step': '0.1' });
      const r = expandLadder(intentOf(f), 3, 10_000_000n, 100n);
      const plans = r.levels.map((l) => planOrder(ticketEnv(), l.intent));
      for (const p of plans) expect(errors(p)).toEqual([]);
      const totals = ladderTotals(plans[0]!, r)!;
      expect(totals.kasLocked).toBe(plans.reduce((a, p) => a + p.disclosure!.kasLocked, 0n));
      expect(totals.tokens).toBe(plans.reduce((a, p) => a + p.disclosure!.tokensEscrowed, 0n));
      expect(totals.amount).toBe(6n * TOKEN);
    }
  });
});
