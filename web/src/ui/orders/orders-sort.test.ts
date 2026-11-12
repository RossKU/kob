import { describe, expect, it } from 'vitest';
import type { OrderRowModel } from './orders-model';
import { bidSpentByFills, filledAmount, filledPermyriad, orderEnded, orderSortValue, orderTotalKas } from './orders-sort';

const SCALE = 100_000_000n;
interface Over {
  status?: OrderRowModel['status'];
  live?: boolean;
  total?: bigint | null;
  left?: bigint | null;
  filled?: string | null;
  price?: bigint | null;
  pair?: boolean;
  budget?: boolean;
}
/** The fields of an order line these helpers read (a cast: the rest of the model is not their business). */
const row = (o: Over = {}): OrderRowModel =>
  ({
    status: o.status ?? 'open',
    live: o.live ?? (o.status === undefined || o.status === 'open' || o.status === 'partial'),
    amountTotal: o.total === undefined ? 494_580_000n : o.total,
    amountLeft: o.left === undefined ? 0n : o.left,
    price: o.price === undefined ? 300_000_000n : o.price,
    scale: SCALE,
    pair: o.pair ? {} : null,
    budget: o.budget ? { placed: null, left: null } : null,
    entry: { view: o.filled === null ? null : { filled_amount: o.filled ?? '0' } },
  }) as unknown as OrderRowModel;

describe('the filled part of an order', () => {
  it('reads the indexer filled_amount: a cancelled order that filled 1 of 4.9458 is 20 %, not 100 % (amount_left is 0 once it ended)', () => {
    const r = row({ status: 'cancelled', total: 494_580_000n, left: 0n, filled: '100000000' });
    expect(filledAmount(r)).toBe(100_000_000n);
    expect(filledPermyriad(r)).toBe(2021);
  });
  it('an ended order that filled nothing is 0 %', () => {
    expect(filledPermyriad(row({ status: 'refunded', total: 1000n, left: 0n, filled: '0' }))).toBe(0);
  });
  it('falls back to total - left only where the field is absent', () => {
    expect(filledAmount(row({ status: 'partial', total: 1000n, left: 400n, filled: null }))).toBe(600n);
    expect(filledAmount(row({ status: 'open', total: null, left: null, filled: null }))).toBeNull();
  });
  it('sorts the Filled column by the real filled amount', () => {
    const ctx = { clock: null, marketText: () => '', decimalsOf: () => 8, avgOf: () => null };
    const a = orderSortValue(row({ status: 'cancelled', total: 494_580_000n, left: 0n, filled: '100000000' }), 'filled', ctx);
    const b = orderSortValue(row({ status: 'filled', total: 494_580_000n, left: 0n, filled: '494580000' }), 'filled', ctx);
    expect(Number(a)).toBeLessThan(Number(b));
  });
});

describe('the Total column', () => {
  it('an open order: its initial amount at its limit price', () => {
    // 4.9458 tokens at 3 KAS = 14.8374 KAS
    expect(orderTotalKas(row({ status: 'open', filled: '0' }), null)).toBe(1_483_740_000n);
  });
  it('an ended order: the filled amount at the average fill price, not the initial amount at the limit', () => {
    const r = row({ status: 'cancelled', total: 494_580_000n, left: 0n, filled: '100000000' });
    expect(orderEnded(r)).toBe(true);
    // 1 token filled at an average of 2.5 KAS
    expect(orderTotalKas(r, 250_000_000n)).toBe(250_000_000n);
    // the fills are not read yet: no figure rather than a wrong one
    expect(orderTotalKas(r, undefined)).toBeNull();
    expect(orderTotalKas(r, null)).toBeNull();
  });
  it('an ended order that traded nothing: 0', () => {
    expect(orderTotalKas(row({ status: 'cancelled', total: 494_580_000n, left: 0n, filled: '0' }), null)).toBe(0n);
  });
  it('a pair order has none; the sort follows the same figure', () => {
    expect(orderTotalKas(row({ pair: true }), null)).toBeNull();
    const r = row({ status: 'cancelled', total: 494_580_000n, left: 0n, filled: '100000000' });
    expect(orderSortValue(r, 'total', { clock: null, marketText: () => '', decimalsOf: () => 8, avgOf: () => 250_000_000n })).toBe(250_000_000n);
  });
});

describe('what a bid placed in another browser spent', () => {
  it('is shown: the filled tokens at the average fill price', () => {
    const r = row({ status: 'filled', total: null, left: null, filled: '250000000', budget: true });
    expect(bidSpentByFills(r, 200_000_000n)).toBe(500_000_000n);
  });
  it('is not invented without the average price, without fills or for an ask', () => {
    expect(bidSpentByFills(row({ status: 'filled', total: null, left: null, filled: '250000000', budget: true }), null)).toBeNull();
    expect(bidSpentByFills(row({ status: 'cancelled', total: null, left: null, filled: '0', budget: true }), 200_000_000n)).toBeNull();
    expect(bidSpentByFills(row({ status: 'filled', filled: '250000000' }), 200_000_000n)).toBeNull();
  });
});
