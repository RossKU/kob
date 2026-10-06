// The pair book / own-order helpers of kob/pair.ts (the exact rational helpers are covered by the UI models that use them).
import { describe, expect, it } from 'vitest';
import type { OrderView, PairBookView } from '../data/indexer-types';
import { impliedPairRate, isPairOrderView, ownPairOrderRefs, pairBookToBookView, wholePriceOf } from './pair';

const A = 'a1'.repeat(32);
const B = 'b2'.repeat(32);

describe('wholePriceOf', () => {
  it('per whole A, asks rounded up and bids down; malformed terms are null', () => {
    expect(wholePriceOf(1n, 3n, 1000n, 'up')).toBe(334n);
    expect(wholePriceOf(1n, 3n, 1000n, 'down')).toBe(333n);
    expect(wholePriceOf(3n, 2n, 1000n, 'up')).toBe(1500n);
    expect(wholePriceOf(0n, 2n, 1000n, 'up')).toBeNull();
    expect(wholePriceOf(1n, 0n, 1000n, 'up')).toBeNull();
  });
});

describe('pairBookToBookView', () => {
  const lvl = (source: string, num: string, den: string, amount: string, orders = 1) => ({ source, price_num: num, price_den: den, amount, orders });
  it('every source is liquidity; levels that round to one price merge; the order stays asks ascending, bids descending', () => {
    const v = {
      asks: [lvl('direct', '3', '2', '100'), lvl('entry', '1501', '1000', '50'), lvl('route', '301', '200', '7', 3)],
      bids: [lvl('route', '29', '20', '10', 2), lvl('direct', '1449', '1000', '5'), lvl('entry', '1', '1', '1')],
    } as unknown as PairBookView;
    expect(pairBookToBookView(v, 1000n)).toEqual({
      asks: [{ price: 1500n, amount: 100n, orders: 1 }, { price: 1501n, amount: 50n, orders: 1 }, { price: 1505n, amount: 7n, orders: 3 }],
      bids: [{ price: 1450n, amount: 10n, orders: 2 }, { price: 1449n, amount: 5n, orders: 1 }, { price: 1000n, amount: 1n, orders: 1 }],
    });
    // 1/3 and 1/3 + a hair round up to one ask price: one level
    const merged = pairBookToBookView({ asks: [lvl('direct', '1', '3', '1'), lvl('route', '1000001', '3000000', '2')], bids: [] } as unknown as PairBookView, 1000n);
    expect(merged.asks).toEqual([{ price: 334n, amount: 3n, orders: 2 }]);
  });

  it('skips malformed levels and a scale of 1 keeps exact prices', () => {
    const v = { asks: [lvl('direct', 'x', '1', '1'), lvl('direct', '5', '1', '0'), lvl('direct', '5', '1', '2', -1)], bids: [] } as unknown as PairBookView;
    expect(pairBookToBookView(v, 1n).asks).toEqual([{ price: 5n, amount: 2n, orders: 1 }]);
  });
});

describe('ownPairOrderRefs / isPairOrderView / impliedPairRate', () => {
  const view = (id: string, pair: object | null, over: object = {}): OrderView =>
    ({ covenant_id: id.repeat(32), contract: 'KobPair', status: 'open', expired: false, amount_left: '9', pair, ...over }) as unknown as OrderView;
  it('oriented live pair orders with a price, at their quote now, tip 0', () => {
    const refs = ownPairOrderRefs(
      [
        view('01', { base: A, quote: B, side: 'ask', price: '100', quote_now: '90', amount_left: '3' }),
        view('02', { base: A, quote: B, side: 'bid', price: '80', quote_now: null, amount_left: null }, { expired: true }),
        view('03', { base: B, quote: A, side: 'ask', price: '1', quote_now: '1' }),
        view('04', { base: A, quote: B, side: 'ask', price: null, quote_now: null }),
        view('05', { base: A, quote: B, side: 'ask', price: '7', quote_now: '7' }, { status: 'cancelled' }),
        view('06', null),
      ],
      A,
      B,
    );
    expect(refs).toEqual([
      { covenantId: '01'.repeat(32), side: 'sell', price: 90n, tip: 0n, amountLeft: 3n, active: true },
      { covenantId: '02'.repeat(32), side: 'buy', price: 80n, tip: 0n, amountLeft: 9n, active: false },
    ]);
  });

  it('pair views by object or contract; implied rate floor(kasA x scale(B) / kasB)', () => {
    expect(isPairOrderView({ contract: 'KobIfdPair' })).toBe(true);
    expect(isPairOrderView({ contract: 'KobAsk', pair: { base: A } })).toBe(true);
    expect(isPairOrderView({ contract: 'KobAsk' })).toBe(false);
    expect(impliedPairRate(300_000_000n, 20_000_000n, 100n)).toBe(1_500n);
    expect(impliedPairRate(1n, 3n, 10n)).toBe(3n);
    expect(impliedPairRate(null, 3n, 10n)).toBeNull();
    expect(impliedPairRate(1n, 0n, 10n)).toBeNull();
  });
});
