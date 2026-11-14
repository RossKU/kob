import { describe, expect, it } from 'vitest';
import { p2pkScriptPublicKey } from '../data/utxos';
import type { TokenUtxo } from '../kob/types';
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

  it('never offers a reserved outpoint as funding (the preparation output 0 of an unfinished creation), listed or as pending change', () => {
    const pk = 'ab'.repeat(32);
    const reserved = new Set(['p1:0']);
    const p = new PendingSpends(() => 0, () => reserved);
    const coin = (transactionId: string, index: number) => ({ transactionId, index, amount: '100', covenantId: null, pubkey: pk });
    expect(p.funding([coin('P1', 0), coin('c1', 0)], pk).map((x) => x.transactionId)).toEqual(['c1']);
    // the preparation's change (its output 0 back to the key) is reserved too while it is pending
    p.addChange({ outputs: [{ value: '500', scriptPublicKey: p2pkScriptPublicKey(pk), covenant: null }] }, 'p1', pk);
    expect(p.funding([coin('c1', 0)], pk).map((x) => x.transactionId)).toEqual(['c1']);
    reserved.clear();
    expect(p.funding([coin('c1', 0)], pk).map((x) => x.transactionId)).toEqual(['p1', 'c1']);
    // a reservation that cannot be read reserves nothing
    const broken = new PendingSpends(() => 0, () => { throw new Error('storage'); });
    expect(broken.funding([coin('p1', 0)], pk)).toHaveLength(1);
  });
  it("counts the token change of a submitted transaction as the key's token UTXO until the node lists it (TN10 2026-11-14: a sell ladder's level 3 held 0)", () => {
    let now = 0;
    const p = new PendingSpends(() => now);
    const pk = 'ab'.repeat(32);
    const tok = 'ee'.repeat(32);
    const tu = (transactionId: string, index: number, amount: string, covenantId = tok, owner = pk) =>
      ({ transactionId, index, amount: '100000000', covenantId, state: { owner, amount, extension_commitment: 'cc'.repeat(32) } }) as unknown as TokenUtxo;
    // level 1 spent the wallet's one token UTXO (100) and returned 99 as change; level 2 spends that change and returns 98
    p.add([{ transactionId: 'a0', index: 0 }]);
    p.addTokens([{ utxo: tu('L1', 2, '99'), scriptPublicKey: '0000aa' }], pk);
    // the reader lists only what the node verifies: neither the spent UTXO (still listed by an old read) nor the change counts wrong
    expect(p.tokens([tu('a0', 0, '100')], pk, tok).map((u) => [u.transactionId, u.state.amount])).toEqual([['l1', '99']]);
    expect(p.factOf('L1', 2)).toEqual({ amount: 100000000n, scriptPublicKey: '0000aa', covenantId: tok });
    p.add([{ transactionId: 'l1', index: 2 }]);
    p.addTokens([{ utxo: tu('L2', 2, '98'), scriptPublicKey: '0000bb' }], pk);
    expect(p.tokens([], pk, tok).map((u) => [u.transactionId, u.state.amount])).toEqual([['l2', '98']]);
    // only this key's outputs of this token; once the node lists one, it is counted once
    expect(p.tokens([], 'cd'.repeat(32), tok)).toEqual([]);
    expect(p.tokens([], pk, '71'.repeat(32))).toEqual([]);
    expect(p.tokens([], pk, undefined)).toEqual([]);
    expect(p.tokens([tu('l2', 2, '98'), tu('b0', 0, '5')], pk, tok).map((u) => u.state.amount)).toEqual(['98', '5']);
    now += PENDING_SPEND_TTL_MS;
    expect(p.tokens([], pk, tok)).toEqual([]);
    expect(p.factOf('l2', 2)).toBeNull();
  });
});
