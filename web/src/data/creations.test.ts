import { describe, expect, it } from 'vitest';
import { DECLARATION_WARN_MS, creationIsOld, genesisSigned, listCreations, markPrepSent, removeCreation, reservedCreationOutpoints, saveCreation, type CreationRecord } from './creations';
import type { StorageLike } from '../config';

function memory(): StorageLike & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v), removeItem: (k) => void data.delete(k) };
}
const tx = (id: string) => ({ id, version: 1, inputs: [], outputs: [], subnetworkId: '00'.repeat(20), lockTime: '0', gas: '0', storageMass: '0', payload: '' });
/** a v2 record: the genesis as an unsigned plan (`signed`: also signed) */
const rec = (cov: string, over: Partial<CreationRecord> = {}, signed = false): CreationRecord =>
  ({
    network: 'testnet-10', maker: 'aa'.repeat(32), createdAt: 1, covenantId: cov, prepTxid: '01'.repeat(32), genesisTxid: '02'.repeat(32),
    prep: tx('01'.repeat(32)), genesisBuilt: { tx: tx('02'.repeat(32)), sign: [], plans: [], roles: [], covenants: [], fee: {} },
    ...(signed ? { genesis: tx('02'.repeat(32)) } : {}), token: { covenantId: cov }, declaration: { json: '{}', payload: '007b7d', funding: '1' },
    docs: {}, prepSent: false, ...over,
  }) as unknown as CreationRecord;
/** a v1 record: always a signed genesis, no plan */
const v1 = (cov: string): Record<string, unknown> => {
  const { genesisBuilt: _plan, ...r } = rec(cov, {}, true) as unknown as Record<string, unknown>;
  return r;
};

describe('creation records (a creation whose genesis may not have gone out)', () => {
  it('saves, marks the preparation sent, lists by network and wallet, and forgets', () => {
    const s = memory();
    expect(saveCreation(rec('c1'.repeat(32)), s)).toBe(true);
    saveCreation(rec('c2'.repeat(32), { maker: 'bb'.repeat(32) }), s);
    saveCreation(rec('c3'.repeat(32), { network: 'mainnet' }), s);
    expect(listCreations('testnet-10', 'aa'.repeat(32), s).map((r) => r.covenantId)).toEqual(['c1'.repeat(32)]);
    markPrepSent('testnet-10', 'c1'.repeat(32), s, 42);
    expect(listCreations('testnet-10', 'AA'.repeat(32), s)[0]).toMatchObject({ prepSent: true, prepSentAt: 42 });
    // the same creation saved again replaces its record (here: with the genesis signed)
    saveCreation(rec('c1'.repeat(32), { createdAt: 9 }, true), s);
    const [one] = listCreations('testnet-10', 'aa'.repeat(32), s);
    expect(one!.createdAt).toBe(9);
    expect(genesisSigned(one!)).toBe(true);
    removeCreation('testnet-10', 'c1'.repeat(32), s);
    expect(listCreations('testnet-10', 'aa'.repeat(32), s)).toEqual([]);
    expect(listCreations('mainnet', 'aa'.repeat(32), s)).toHaveLength(1);
    // only the v2 key is written: a page of an older build (v1 only) never sees a record without a signed genesis
    expect(s.data.has('kob.creations.v1')).toBe(false);
    expect(s.data.has('kob.creations.v2')).toBe(true);
  });

  it('keeps a record without a signed genesis, and one with only a signed genesis (v1)', () => {
    const s = memory();
    saveCreation(rec('c1'.repeat(32)), s);
    const [r] = listCreations('testnet-10', 'aa'.repeat(32), s);
    expect(genesisSigned(r!)).toBe(false);
    expect(r!.genesisBuilt!.tx.id).toBe('02'.repeat(32));
  });

  it('moves v1 records to v2 on the first read and drops the v1 key; a v2 record of the same creation wins', () => {
    const s = memory();
    saveCreation(rec('c2'.repeat(32), { createdAt: 7 }), s);
    s.setItem('kob.creations.v1', JSON.stringify([v1('c1'.repeat(32)), { ...v1('c2'.repeat(32)), createdAt: 3 }]));
    const all = listCreations('testnet-10', 'aa'.repeat(32), s);
    expect(all.map((r) => [r.covenantId, r.createdAt])).toEqual([['c1'.repeat(32), 1], ['c2'.repeat(32), 7]]);
    expect(genesisSigned(all[0]!)).toBe(true);
    expect(s.data.has('kob.creations.v1')).toBe(false);
    expect(JSON.parse(s.data.get('kob.creations.v2')!)).toHaveLength(2);
  });

  it('drops malformed records and survives a broken or missing storage', () => {
    const s = memory();
    s.setItem('kob.creations.v2', JSON.stringify([
      rec('c1'.repeat(32)),
      { ...rec('c2'.repeat(32)), prepTxid: 'nothex' },
      { ...rec('c4'.repeat(32), {}, true), genesis: tx('ff'.repeat(32)) },
      { ...rec('c5'.repeat(32)), genesisBuilt: { tx: tx('ff'.repeat(32)), sign: [] } },
      { ...v1('c6'.repeat(32)), genesis: undefined },
      42,
    ]));
    expect(listCreations('testnet-10', 'aa'.repeat(32), s).map((r) => r.covenantId)).toEqual(['c1'.repeat(32)]);
    s.setItem('kob.creations.v2', '{not json');
    expect(listCreations('testnet-10', 'aa'.repeat(32), s)).toEqual([]);
    expect(saveCreation(rec('c1'.repeat(32)), null)).toBe(false);
    expect(listCreations('testnet-10', 'aa'.repeat(32), null)).toEqual([]);
    const throwing: StorageLike = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); } };
    expect(saveCreation(rec('c1'.repeat(32)), throwing)).toBe(false);
    expect(listCreations('testnet-10', 'aa'.repeat(32), throwing)).toEqual([]);
  });

  it("reserves every record's preparation output 0, and says when the declaration window is closing", () => {
    const s = memory();
    saveCreation(rec('c1'.repeat(32), { prepTxid: 'AB'.repeat(32).toLowerCase(), prep: tx('ab'.repeat(32)) }), s);
    saveCreation(rec('c2'.repeat(32), { maker: 'bb'.repeat(32) }), s);
    expect([...reservedCreationOutpoints(s)].sort()).toEqual([`${'01'.repeat(32)}:0`, `${'ab'.repeat(32)}:0`]);
    expect(reservedCreationOutpoints(null).size).toBe(0);
    expect(creationIsOld({ createdAt: 0 }, DECLARATION_WARN_MS)).toBe(false);
    expect(creationIsOld({ createdAt: 0 }, DECLARATION_WARN_MS + 1)).toBe(true);
    expect(creationIsOld({ createdAt: 0, prepSentAt: 10 }, DECLARATION_WARN_MS + 1)).toBe(false);
  });
});
