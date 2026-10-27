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
      receive: null,
    });
    expect(fillsNote(facts({ budgeted: 4n, counterparty: 'bids', carrierDeposit: 0n, fillable: 20n * TOK }), c).hint).toMatch(/3 bids per transaction\)\. A sell deposits no carrier per fill\.$/);
    const ioc = fillsNote(facts(), { ...c, box: false });
    expect(ioc.hint).toBeNull();
    expect(ioc.short).toBe('One transaction takes at most 3 asks: only about 6 TST of 20 TST fills now, the rest returns.');
  });

  it('the market shown as KAS/TOKEN: the counterparties are named as that view shows them (a chain ask is a bid for KAS)', () => {
    const inv = { ...c, inverted: true };
    expect(fillsNote(facts(), inv)).toEqual({
      hint: 'The visible book needs about 4 fill(s) for this amount (3 bids per transaction). 1 fill(s) deposit 2 KAS in carriers; unused ones come back.',
      short: 'One transaction takes at most 3 bids: only about 6 TST of 20 TST fills now, the rest returns. Raise Fills (Advanced) to trade more.',
      receive: null,
    });
    expect(fillsNote(facts({ budgeted: 4n, counterparty: 'bids', carrierDeposit: 0n, fillable: 20n * TOK }), inv).hint).toMatch(/3 asks per transaction\)\. Paying with TST deposits no carrier per fill\.$/);
  });

  it('a resting buy speaks of what crosses now; short of fills it says how much can fill', () => {
    const n = fillsNote(facts({ scope: 'crossing', budgeted: 2n, fillable: 12n * TOK, carrierDeposit: 400_000_000n }), c);
    expect(n.hint).toMatch(/^What crosses now needs about 4 fill\(s\) \(3 asks per transaction\)\./);
    expect(n.short).toBe('With 2 fill(s) only about 12 TST of 20 TST can fill now: raise Fills to trade more.');
    expect(fillsNote(facts({ needed: 0, fillable: 0n }), c)).toMatchObject({ hint: expect.stringMatching(/^Nothing in the visible book is within reach now\./), short: null });
  });

  it('a multi-fill market buy: says the Fills value was cut by the minimum fill, and the most the escrow may deliver above the typed amount', () => {
    const n = fillsNote(facts({ budgeted: 2n, requested: 3n, minFill: 338n, maxReceive: 667n, fillable: 20n * TOK, carrierDeposit: 400_000_000n }), { ...c, amount: 600n });
    expect(n.receive).toContain('Fills limited to 2 (you asked for 3): with a minimum fill of 0.338 TST, 0.6 TST cannot be split into more.');
    expect(n.receive).toContain('You may receive up to 0.667 TST, not only 0.6 TST: a buy is a KAS budget');
    expect(n.receive).toContain('You never pay more than the budget.');
    // the line next to the Fills box stays the estimate and the deposit
    expect(n.hint).not.toContain('You may receive');
    // no clamp and no excess: neither sentence
    const plain = fillsNote(facts({ budgeted: 2n, requested: undefined, minFill: 338n, maxReceive: 600n, fillable: 20n * TOK }), { ...c, amount: 600n });
    expect(plain.receive).toBeNull();
    // an order without a Fills box (an IOC) says nothing of it either
    expect(fillsNote(facts({ requested: 3n, minFill: 338n, maxReceive: 667n }), { ...c, amount: 600n, box: false }).receive).toBeNull();
  });
});
