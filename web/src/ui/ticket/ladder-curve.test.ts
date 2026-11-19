// A sell ladder as one curve order: the ladder's prices become the table (percent and even
// spacing), a buy or a falling ladder is not a curve. The stepper of separate orders.
import { describe, expect, it } from 'vitest';
import type { LimitIntent } from '../../kob/intent-simple';
import { expandLimitLadder } from './ladder';
import { curveOfLadder, ladderStepItems } from './ladder-curve';

const sell: LimitIntent = { type: 'limit', side: 'sell', amount: 500n, price: 1000n };

describe('curveOfLadder', () => {
  it('maps percent steps and even steps to the table', () => {
    const pct = expandLimitLadder(sell, { levels: 4, mode: 'pct', bps: 1000n }, 1n);
    expect(curveOfLadder(pct, sell)).toEqual({ type: 'curve', side: 'sell', amount: 2000n, levelSize: 500n, table: [1000n, 1100n, 1210n, 1331n] });
    const even = expandLimitLadder(sell, { levels: 3, mode: 'end', end: 2000n }, 1n);
    expect(curveOfLadder(even, sell)!.table).toEqual([1000n, 1500n, 2000n]);
  });

  it('is null for a buy, a falling sell ladder, or no ladder', () => {
    const buy: LimitIntent = { ...sell, side: 'buy' };
    expect(curveOfLadder(expandLimitLadder(buy, { levels: 3, mode: 'pct', bps: 500n }, 1n), buy)).toBeNull();
    expect(curveOfLadder(expandLimitLadder(sell, { levels: 3, mode: 'end', end: 500n }, 1n), sell)).toBeNull();
    expect(curveOfLadder({ ok: false, levels: [], errors: [], totalAmount: 0n }, sell)).toBeNull();
  });
});

describe('ladderStepItems', () => {
  it('marks the levels placed, the one being placed and a stop', () => {
    expect(ladderStepItems({ at: 1, levels: 3 }, true).map((s) => s.state)).toEqual(['done', 'current', 'waiting']);
    expect(ladderStepItems({ at: 1, levels: 3, failed: true }, false).map((s) => s.state)).toEqual(['done', 'failed', 'waiting']);
    expect(ladderStepItems({ at: 3, levels: 3 }, false).map((s) => s.state)).toEqual(['done', 'done', 'done']);
    expect(ladderStepItems({ at: 0, levels: 2 }, true)[0]!.label).toBe('Level 1');
  });
});
