// Review plans the order again from freshly read data; a warning only that fresh plan has is passed to the confirmation.
import { describe, expect, it } from 'vitest';
import { planOrder } from '../../kob/plan';
import { TOK, level, makeEnv } from '../../testing/fixtures';
import { newWarnings } from './plan-expected';

const REAL = 250_000_000n;
const buy = { type: 'market', side: 'buy', amount: 5n * TOK } as never;

describe('warnings that appear only in the plan made at Review', () => {
  it('a depth warning raised by the fresh book is new; one the ticket already showed is not', () => {
    const before = planOrder(makeEnv({ book: { asks: [level(REAL, 1_000n * TOK, 1)], bids: [level(REAL - 1_000n, 1_000n * TOK, 1)] } }), buy);
    expect(before.issues.map((i) => i.code)).not.toContain('MARKET_DEPTH_INSUFFICIENT');
    // the book thinned out between the ticket and Review
    const fresh = planOrder(makeEnv({ book: { asks: [level(REAL, 1n * TOK, 1)], bids: [level(REAL - 1_000n, 1_000n * TOK, 1)] } }), buy);
    const added = newWarnings(before, fresh).map((i) => i.code);
    expect(added).toContain('MARKET_DEPTH_INSUFFICIENT');
    // every warning the ticket already showed stays out (only the new ones need a second look)
    for (const i of before.issues) expect(added).not.toContain(i.code);
    expect(newWarnings(fresh, fresh)).toEqual([]);
    // no plan shown at all: every warning is new; errors and info lines are never passed
    expect(newWarnings(null, fresh).every((i) => i.severity === 'warning')).toBe(true);
    expect(newWarnings(null, fresh).length).toBe(fresh.issues.filter((i) => i.severity === 'warning').length);
  });
});
