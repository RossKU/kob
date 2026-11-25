// The founder's TN10 market buy (7.41309779 TUSD at 3 % slippage): the budget shown is what the
// order's fills may spend, so at its worst price it buys the amount typed. A bid's refund tip is
// paid from what is left at a refund (KobBid.sil), not out of the budget.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { goldenRequest, MAKER } from '../testing/chain-fixtures';
import { describeOrder } from './order-facts';
import { splitOrderValue } from './decode';
import type { OrderState } from './types';

const kob = loadKobNode();

describe('a buy order\'s budget', () => {
  it('7.41309779 at a worst all-in price of 39.97324765 KAS: budget 296.32560... and it buys the amount typed', () => {
    const b = kob.build(goldenRequest('create.bid.market', MAKER.pk) as never);
    const st = JSON.parse(JSON.stringify(kob.recoverOrders(b.tx)[0]!.order)) as OrderState & { state: Record<string, string> };
    // the founder's terms: 8 decimals, start 38.84291872, worst all-in 39.97324765 (tip included),
    // one fill, a 1 KAS delivery carrier, a 0.036 KAS refund tip
    Object.assign(st.state, { scale: '100000000', minFill: '1', price: '3884291872', tip: '0', priceEnd: '3997324765', slope: '565164', decayStep: '1', refundTip: '3600000', deliveryCarrier: '100000000', reserve: '0' });
    const amount = 741_309_779n;
    const value = kob.bidEscrow(st, amount, 1n)!;
    const locked = splitOrderValue(describeOrder(st, kob), value);
    // the budget is what the fills may spend: the value less the carrier, the refund tip inside it
    expect(locked.escrow).toBe(value - 100_000_000n);
    // at the worst price it buys the amount typed
    expect((locked.escrow * 100_000_000n) / 3_997_324_765n).toBeGreaterThanOrEqual(amount);
    // as the founder's frame showed: the order output 297.32559382 KAS, the budget no longer 0.036 short
    expect(value).toBe(29_732_559_382n);
    expect(locked.escrow).toBe(29_632_559_382n);
  });
});
