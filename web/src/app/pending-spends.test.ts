import { describe, expect, it } from 'vitest';
import { p2pkScriptPublicKey } from '../data/utxos';
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

  it('counts the KAS change of a submitted transaction as funding at once, until the node lists it or the TTL ends (TN10 2026-10-17: Available fell to the unspent coins for ~10 s)', () => {
    let now = 0;
    const p = new PendingSpends(() => now);
    const pk = 'ab'.repeat(32);
    const key = (transactionId: string, index: number, amount: string) => ({ transactionId, index, amount, covenantId: null, pubkey: pk });
    // the wallet holds 126.5 KAS: one coin of 120 and one of 6.5; an order spends the 120 and returns 113 as change (7 KAS locked and fee)
    const listed = [key('c1', 0, '12000000000'), key('c2', 0, '650000000')];
    const tx = {
      outputs: [
        { value: '700000000', scriptPublicKey: '0000aa', covenant: { authorizingInput: 0, covenantId: 'ee'.repeat(32) } },
        { value: '11300000000', scriptPublicKey: p2pkScriptPublicKey(pk), covenant: null },
        { value: '100', scriptPublicKey: p2pkScriptPublicKey('cd'.repeat(32)), covenant: null },
      ],
    };
    p.add([{ transactionId: 'c1', index: 0 }]);
    p.addChange(tx, 'TX1', pk);
    const total = (us: { amount: string }[]) => us.reduce((a, u) => a + BigInt(u.amount), 0n);
    // a read made before the transaction reached a block still lists the spent coin and not the change: 6.5 + 113 = 119.5 KAS
    const f = p.funding(listed, pk);
    expect(total(f)).toBe(11950000000n);
    expect(f[0]).toMatchObject({ transactionId: 'tx1', index: 1, amount: '11300000000', covenantId: null, pubkey: pk });
    // only this key's plain outputs: not the order output, not a payment to another key, not for another key
    expect(p.funding([], 'cd'.repeat(32))).toEqual([]);
    // the next order may spend it: its node facts are the ones the page built
    expect(p.factOf('tx1', 1)).toEqual({ amount: 11300000000n, scriptPublicKey: p2pkScriptPublicKey(pk), covenantId: null });
    expect(p.factOf('tx1', 0)).toBeNull();
    // once the node lists it, it is counted once
    expect(total(p.funding([key('c2', 0, '650000000'), key('tx1', 1, '11300000000')], pk))).toBe(11950000000n);
    // a later transaction that spends the change takes it out again
    p.add([{ transactionId: 'tx1', index: 1 }]);
    expect(total(p.funding(listed, pk))).toBe(650000000n);
    now += PENDING_SPEND_TTL_MS;
    expect(p.factOf('tx1', 1)).toBeNull();
    expect(p.funding(listed, pk)).toBe(listed);
  });
});
