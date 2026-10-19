import { describe, expect, it } from 'vitest';
import { listCreations, markPrepSent, removeCreation, saveCreation, type CreationRecord } from './creations';
import type { StorageLike } from '../config';

function memory(): StorageLike & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v), removeItem: (k) => void data.delete(k) };
}
const tx = (id: string) => ({ id, version: 1, inputs: [], outputs: [], subnetworkId: '00'.repeat(20), lockTime: '0', gas: '0', storageMass: '0', payload: '' });
const rec = (cov: string, over: Partial<CreationRecord> = {}): CreationRecord =>
  ({
    network: 'testnet-10', maker: 'aa'.repeat(32), createdAt: 1, covenantId: cov, prepTxid: '01'.repeat(32), genesisTxid: '02'.repeat(32),
    prep: tx('01'.repeat(32)), genesis: tx('02'.repeat(32)), token: { covenantId: cov }, declaration: { json: '{}', payload: '007b7d', funding: '1' },
    docs: {}, prepSent: false, ...over,
  }) as unknown as CreationRecord;

describe('creation records (a creation whose genesis may not have gone out)', () => {
  it('saves, marks the preparation sent, lists by network and wallet, and forgets', () => {
    const s = memory();
    expect(saveCreation(rec('c1'.repeat(32)), s)).toBe(true);
    saveCreation(rec('c2'.repeat(32), { maker: 'bb'.repeat(32) }), s);
    saveCreation(rec('c3'.repeat(32), { network: 'mainnet' }), s);
    expect(listCreations('testnet-10', 'aa'.repeat(32), s).map((r) => r.covenantId)).toEqual(['c1'.repeat(32)]);
    markPrepSent('testnet-10', 'c1'.repeat(32), s);
    expect(listCreations('testnet-10', 'AA'.repeat(32), s)[0]!.prepSent).toBe(true);
    // the same creation saved again replaces its record
    saveCreation(rec('c1'.repeat(32), { createdAt: 9 }), s);
    expect(listCreations('testnet-10', 'aa'.repeat(32), s).map((r) => r.createdAt)).toEqual([9]);
    removeCreation('testnet-10', 'c1'.repeat(32), s);
    expect(listCreations('testnet-10', 'aa'.repeat(32), s)).toEqual([]);
    expect(listCreations('mainnet', 'aa'.repeat(32), s)).toHaveLength(1);
  });

  it('drops malformed records and survives a broken or missing storage', () => {
    const s = memory();
    s.setItem('kob.creations.v1', JSON.stringify([rec('c1'.repeat(32)), { ...rec('c2'.repeat(32)), prepTxid: 'nothex' }, { ...rec('c4'.repeat(32)), genesis: tx('ff'.repeat(32)) }, 42]));
    expect(listCreations('testnet-10', 'aa'.repeat(32), s).map((r) => r.covenantId)).toEqual(['c1'.repeat(32)]);
    s.setItem('kob.creations.v1', '{not json');
    expect(listCreations('testnet-10', 'aa'.repeat(32), s)).toEqual([]);
    expect(saveCreation(rec('c1'.repeat(32)), null)).toBe(false);
    expect(listCreations('testnet-10', 'aa'.repeat(32), null)).toEqual([]);
    const throwing: StorageLike = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); } };
    expect(saveCreation(rec('c1'.repeat(32)), throwing)).toBe(false);
  });
});
