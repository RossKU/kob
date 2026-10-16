import { describe, expect, it } from 'vitest';
import { PENDING_SPEND_TTL_MS, PendingSpends } from './pending-spends';

const u = (transactionId: string, index: number) => ({ transactionId, index, amount: '1' });

describe('PendingSpends', () => {
  it('leaves the inputs of a submitted transaction out of every later read until the TTL (an old environment cannot pick them)', () => {
    let now = 0;
    const p = new PendingSpends(() => now);
    const coins = [u('AA', 0), u('aa', 1), u('bb', 0)];
    expect(p.without(coins)).toBe(coins);
    p.add([{ transactionId: 'aa', index: 0 }]);
    // outpoints are compared case-insensitively
    expect(p.without(coins)).toEqual([u('aa', 1), u('bb', 0)]);
    now += PENDING_SPEND_TTL_MS - 1;
    expect(p.has('AA', 0)).toBe(true);
    now += 1;
    // a transaction the network dropped frees its coins
    expect(p.has('aa', 0)).toBe(false);
    expect(p.without(coins)).toBe(coins);
  });
});
