// The ticket's line next to the Fills box (the visible book's estimate and the carriers a buy deposits) and its warning when the fills cannot take
// the amount (one transaction takes at most 3 asks: a large market buy against many small asks fills only part).
import { describe, expect, it } from 'vitest';
import type { PlanFills } from '../../kob/plan-types';
import { fillsNote } from './fills-note';

const TOK = 1000n;
const facts = (o: Partial<PlanFills> = {}): PlanFills => ({
  budgeted: 1n, needed: 4, perTx: 3, counterparty: 'asks', fillable: 6n * TOK, enough: true, carrierDeposit: 200_000_000n, scope: 'amount', ...o,
});
const c = { decimals: 3, ticker: 'TST', box: true, amount: 20n * TOK };

describe('fillsNote', () => {
  it('a one-transaction market buy against more asks than one transaction takes: the estimate, the deposit, the warning to raise Fills', () => {
    const n = fillsNote(facts(), c);
    expect(n.hint).toBe('The visible book needs about 4 fill(s) for this amount (3 asks per transaction). 1 fill(s) deposit 2 KAS in carriers; unused ones come back.');
    expect(n.short).toBe('One transaction takes at most 3 asks: only about 6 TST of 20 TST fills now, the rest returns. Raise Fills (Advanced) to trade more.');
  });

  it('enough fills: no warning; a sell deposits no carrier; an IOC has no box, only the warning', () => {
    expect(fillsNote(facts({ budgeted: 4n, fillable: 20n * TOK, carrierDeposit: 800_000_000n }), c)).toEqual({
      hint: 'The visible book needs about 4 fill(s) for this amount (3 asks per transaction). 4 fill(s) deposit 8 KAS in carriers; unused ones come back.',
      short: null,
    });
    expect(fillsNote(facts({ budgeted: 4n, counterparty: 'bids', carrierDeposit: 0n, fillable: 20n * TOK }), c).hint).toMatch(/3 bids per transaction\)\. A sell deposits no carrier per fill\.$/);
    const ioc = fillsNote(facts(), { ...c, box: false });
    expect(ioc.hint).toBeNull();
    expect(ioc.short).toBe('One transaction takes at most 3 asks: only about 6 TST of 20 TST fills now, the rest returns.');
  });

  it('a resting buy speaks of what crosses now; short of fills it says how much can fill', () => {
    const n = fillsNote(facts({ scope: 'crossing', budgeted: 2n, fillable: 12n * TOK, carrierDeposit: 400_000_000n }), c);
    expect(n.hint).toMatch(/^What crosses now needs about 4 fill\(s\) \(3 asks per transaction\)\./);
    expect(n.short).toBe('With 2 fill(s) only about 12 TST of 20 TST can fill now: raise Fills to trade more.');
    expect(fillsNote(facts({ needed: 0, fillable: 0n }), c)).toMatchObject({ hint: expect.stringMatching(/^Nothing in the visible book is within reach now\./), short: null });
  });
});
