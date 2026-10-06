import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { planCancel, planCancelReplace } from './cancel';
import {
  LocalStorageRecordStore, MemoryRecordStore, amendedRecords, exportBackup, exportIndexerRecovery, importBackup, importIndexerRecovery, isPlacementRecord,
  recordsFromBuilt, resolveRecord, type PlacementRecord,
} from './records';
import type { AskState, OrderState, TxJson } from './types';
import { FakeChain, MAKER, OTHER, TOKEN, ZERO32, placeGolden, signAndValidate, testAddress, tokenState } from '../testing/chain-fixtures';

const kob = loadKobNode();
const opts = { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_790_000_000n, placedAtDaa: 1000n };

function recordOf(name: string, extra: Partial<typeof opts & { note: string; label: string }> = {}) {
  const p = placeGolden(kob, name);
  const recs = recordsFromBuilt(kob, p.built, { ...opts, ...extra });
  return { p, recs, rec: recs[0] };
}

class FakeStorage {
  m = new Map<string, string>();
  getItem(k: string) { return this.m.get(k) ?? null; }
  setItem(k: string, v: string) { this.m.set(k, v); }
  removeItem(k: string) { this.m.delete(k); }
}
const throwingStorage = {
  getItem(): string | null { throw new Error('blocked'); },
  setItem(): void { throw new Error('quota'); },
  removeItem(): void { throw new Error('blocked'); },
};

describe('recordsFromBuilt', () => {
  it('create.ask: ask record with custody and no deadline', () => {
    const { p, rec } = recordOf('create.ask', { note: 'n', label: 'Sell 10 tokens' });
    expect(isPlacementRecord(rec)).toBe(true);
    expect(rec).toMatchObject({
      version: 1, network: 'testnet-10', maker: MAKER.pk, txid: p.txid, output: 0, kind: 'KobAsk', amount: '10000', value: '1000000000', deadline: null,
      placedAtUnix: '1790000000', placedAtDaa: '1000', note: 'n', label: 'Sell 10 tokens',
    });
    expect(rec.covenantId).toBe(p.recovered[0].covenantId);
    expect(rec.templateHash).toBe(kob.templates().find((t) => t.name === 'KobAsk')!.hash);
    expect(rec.custody).toMatchObject({ output: 1, value: '1000000000', tokenState: { amount: '10000', owner: rec.covenantId, owner_scheme: 4 } });
    expect(kob.decodeState('KobAsk', rec.state)).toEqual(p.recovered[0].order);
  });

  it('create.bid has no custody and no amount (its quantity is its escrow); create.condAsk and create.ifdAsk.repeat are recorded with custody', () => {
    const bid = recordOf('create.bid').rec;
    expect(bid).toMatchObject({ kind: 'KobBid', amount: null, custody: null });
    const cond = recordOf('create.condAsk').rec;
    expect(cond.kind).toBe('KobCondAsk');
    expect(cond.custody).not.toBeNull();
    const ifd = recordOf('create.ifdAsk.repeat').rec;
    expect(ifd.kind).toBe('KobIfdAsk');
    expect(ifd.amount).toBe('10000');
    expect(ifd.custody).not.toBeNull();
  });

  it('a record stored by an older build (a `lots` count, no amount) is still a valid record', () => {
    const { rec } = recordOf('create.ask');
    const { amount: _a, ...old } = rec;
    expect(isPlacementRecord({ ...old, lots: 10 })).toBe(true);
    expect(isPlacementRecord({ ...rec, amount: 'x' })).toBe(false);
  });

  it('day orders keep their deadline', () => {
    expect(recordOf('create.ask.day').rec.deadline).toBe('1790726400');
  });

  it('skips orders of another maker', () => {
    const p = placeGolden(kob, 'create.ask');
    expect(recordsFromBuilt(kob, p.built, { ...opts, maker: OTHER.pk })).toEqual([]);
  });

  it('works on the unsigned built tx (same tx id as the signed one)', () => {
    const { p, rec } = recordOf('create.bid');
    expect(rec.txid).toBe(p.signed.tx.id);
  });
});

describe('stores', () => {
  it('MemoryRecordStore round trip', async () => {
    const { rec } = recordOf('create.ask');
    const s = new MemoryRecordStore();
    expect(await s.get(rec.covenantId)).toBeNull();
    await s.put(rec);
    expect(await s.get(rec.covenantId)).toEqual(rec);
    expect(await s.list()).toEqual([rec]);
    await s.remove(rec.covenantId);
    expect(await s.list()).toEqual([]);
  });

  it('LocalStorageRecordStore persists per network+maker and survives a reload', async () => {
    const { rec } = recordOf('create.ask');
    const st = new FakeStorage();
    const a = new LocalStorageRecordStore(st, 'testnet-10', MAKER.pk);
    await a.put(rec);
    const key = [...st.m.keys()][0];
    expect(key).toContain('testnet-10');
    expect(key).toContain(MAKER.pk);
    expect(await new LocalStorageRecordStore(st, 'testnet-10', MAKER.pk).list()).toEqual([rec]);
    expect(await new LocalStorageRecordStore(st, 'mainnet', MAKER.pk).list()).toEqual([]);
    expect(await new LocalStorageRecordStore(st, 'testnet-10', OTHER.pk).list()).toEqual([]);
    await a.remove(rec.covenantId);
    expect(st.m.size).toBe(0);
    await expect(new LocalStorageRecordStore(st, 'mainnet', MAKER.pk).put(rec)).rejects.toThrow(/another network/);
  });

  it('falls back to memory when storage throws, and ignores corrupt or foreign data', async () => {
    const { rec } = recordOf('create.ask');
    const t = new LocalStorageRecordStore(throwingStorage, 'testnet-10', MAKER.pk);
    await t.put(rec);
    expect(await t.list()).toEqual([rec]);
    // C5 W-14: the UI is told the records live in memory only (and asks for a backup)
    expect(t.persistent()).toBe(false);
    expect(new LocalStorageRecordStore(null, 'testnet-10', MAKER.pk).persistent()).toBe(false);
    const ok = new LocalStorageRecordStore(new FakeStorage(), 'testnet-10', MAKER.pk);
    await ok.put(rec);
    expect(ok.persistent()).toBe(true);
    await t.remove(rec.covenantId);
    expect(await t.list()).toEqual([]);

    const st = new FakeStorage();
    const s = new LocalStorageRecordStore(st, 'testnet-10', MAKER.pk);
    st.setItem(s.key, '{not json');
    expect(await s.list()).toEqual([]);
    st.setItem(s.key, JSON.stringify({ [rec.covenantId]: rec, bad: { version: 1 }, x: 5 }));
    expect(await s.list()).toEqual([rec]);
    st.setItem(s.key, '[1,2]');
    expect(await s.list()).toEqual([rec]); // last good copy kept in memory
    expect(await new LocalStorageRecordStore(null, 'testnet-10', MAKER.pk).list()).toEqual([]);
  });
});

describe('indexer recovery format', () => {
  it('exports and re-imports every order kind', () => {
    const recs = ['create.ask', 'create.bid', 'create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdAsk'].map((n) => recordOf(n).rec);
    const file = exportIndexerRecovery(recs, 'testnet-10');
    // version 2 (docs/ops/executor.md 7.5): an ask carries its custody token's extension commitment, else its custody cannot be rebuilt (C5 W-7)
    expect(file.version).toBe(2);
    expect(file.orders[0]).toEqual({ template_hash: recs[0].templateHash, state: recs[0].state, covenant_id: recs[0].covenantId, extension_commitment: TOKEN.ext });
    const back = importIndexerRecovery(JSON.parse(JSON.stringify(file)), kob, { maker: MAKER.pk, network: 'testnet-10' });
    expect(back.rejected).toEqual([]);
    expect(back.records.map((r) => [r.covenantId, r.kind, r.state, r.amount, r.txid, r.output])).toEqual(
      recs.map((r) => [r.covenantId, r.kind, r.state, r.amount, null, null]),
    );
    expect(back.records.every(isPlacementRecord)).toBe(true);
  });

  it('rejects wrong maker, bad hash, bad state, bad covenant id, wrong network', () => {
    const rec = recordOf('create.ask').rec;
    const good = exportIndexerRecovery([rec], 'testnet-10');
    const e = good.orders[0];
    const run = (orders: object[], network = 'testnet-10', maker = MAKER.pk) => importIndexerRecovery({ version: 1, network, orders }, kob, { maker, network: 'testnet-10' }).rejected.map((r) => r.reason);
    expect(run([e], 'testnet-10', OTHER.pk)[0]).toMatch(/another maker/);
    expect(run([{ ...e, template_hash: '00'.repeat(32) }])[0]).toMatch(/pinned/);
    expect(run([{ ...e, template_hash: 'zz' }])[0]).toMatch(/64/);
    expect(run([{ ...e, covenant_id: 'abcd' }])[0]).toMatch(/covenant_id/);
    expect(run([{ ...e, state: e.state.slice(0, -2) }])[0]).toMatch(/does not decode|canonical/);
    expect(run([{ ...e, state: e.state.slice(0, 4) + 'ff' + e.state.slice(6) }]).length).toBe(1);
    expect(run([e], 'mainnet')[0]).toMatch(/network/);
    expect(importIndexerRecovery({ version: 2 }, kob, { maker: MAKER.pk, network: 'testnet-10' }).rejected).toHaveLength(1);
    // a KCC-20 token template hash is not an order template
    expect(run([{ ...e, template_hash: TOKEN.templateHash }])[0]).toMatch(/pinned/);
  });
});

describe('backup', () => {
  it('round trips and re-derives from the placed tx', () => {
    const a = recordOf('create.ask.day');
    const b = recordOf('create.bid');
    const txs: Record<string, TxJson> = { [a.p.txid]: a.p.signed.tx, [b.p.txid]: b.p.signed.tx };
    const file = exportBackup([a.rec, b.rec], txs, 'testnet-10', new Date('2026-09-29T00:00:00Z'));
    expect(file.exportedAt).toBe('2026-09-29T00:00:00.000Z');
    const back = importBackup(JSON.parse(JSON.stringify(file)), kob, { network: 'testnet-10', maker: MAKER.pk });
    expect(back.rejected).toEqual([]);
    expect(back.records).toEqual([a.rec, b.rec]);
  });

  it('rejects tampered state, covenant id, output and missing txs', () => {
    const a = recordOf('create.ask');
    const txs = { [a.p.txid]: a.p.signed.tx };
    const other = recordOf('create.ask.day').rec; // a different valid state, same maker
    const run = (rec: PlacementRecord, t: Record<string, TxJson> = txs) => importBackup({ ...exportBackup([rec], t, 'testnet-10'), records: [rec] }, kob, { network: 'testnet-10' }).rejected.map((r) => r.reason);
    expect(run(a.rec)).toEqual([]);
    expect(run({ ...a.rec, state: other.state })[0]).toMatch(/state does not match/);
    // a state whose amountLeft differs by one base unit: still canonical, but not what the tx placed
    const s = kob.decodeState('KobAsk', a.rec.state) as OrderState;
    const changed = kob.encodeState({ kind: 'KobAsk', state: { ...(s.state as AskState), amountLeft: '9999' } });
    expect(run({ ...a.rec, state: changed })[0]).toMatch(/state does not match/);
    expect(run({ ...a.rec, covenantId: 'ab'.repeat(32) })[0]).toMatch(/does not create this covenant id/);
    expect(run({ ...a.rec, output: 3 })[0]).toMatch(/output index/);
    expect(run(a.rec, {})[0]).toMatch(/missing/);
    expect(run({ ...a.rec, maker: OTHER.pk })[0]).toMatch(/another maker/);
    // tx tampered (payload cut): does not verify
    const bad = { ...a.p.signed.tx, payload: a.p.signed.tx.payload.slice(0, 40) };
    expect(run(a.rec, { [a.p.txid]: bad })[0]).toMatch(/does not verify|does not create/);
    // wrong maker filter and wrong network
    expect(importBackup(exportBackup([a.rec], txs, 'testnet-10'), kob, { network: 'testnet-10', maker: OTHER.pk }).records).toEqual([]);
    expect(importBackup(exportBackup([a.rec], txs, 'testnet-10'), kob, { network: 'mainnet' }).rejected[0].reason).toMatch(/network/);
    expect(importBackup({ format: 'x' }, kob, { network: 'testnet-10' }).rejected).toHaveLength(1);
  });

  it('a record without a tx must pass the indexer checks', () => {
    const a = recordOf('create.ask').rec;
    const lone = { ...a, txid: null, output: null };
    expect(importBackup(exportBackup([lone], {}, 'testnet-10'), kob, { network: 'testnet-10' }).records).toEqual([lone]);
    const bad = { ...lone, templateHash: '11'.repeat(32) };
    expect(importBackup({ ...exportBackup([bad], {}, 'testnet-10') }, kob, { network: 'testnet-10' }).rejected).toHaveLength(1);
  });
});

describe('resolveRecord', () => {
  const addr = testAddress;
  /** a record of a small ask (minimum fill 1): every amount below it is a candidate the search can try */
  const askWithAmount = (base: PlacementRecord, amount: number): PlacementRecord => {
    const s = kob.decodeState('KobAsk', base.state) as OrderState;
    return { ...base, state: kob.encodeState({ kind: 'KobAsk', state: { ...(s.state as AskState), amountLeft: String(amount), minFill: '1' } }), amount: String(amount) };
  };

  it('live and unchanged: found at the original address, with its custody', async () => {
    const { p, rec } = recordOf('create.ask');
    const chain = new FakeChain();
    chain.applyTx(p.signed.tx);
    const r = await resolveRecord(kob, chain, rec, addr);
    expect(r.status).toBe('live');
    expect(r.stateChanged).toBe(false);
    expect(r.order).toMatchObject({ transactionId: p.txid, index: 0, amount: '1000000000', covenantId: rec.covenantId });
    expect(kob.encodeState(r.order!.state)).toBe(rec.state);
    expect(r.custody).toMatchObject({ transactionId: p.txid, index: 1, covenantId: TOKEN.covenantId, state: { amount: '10000', owner: rec.covenantId, owner_scheme: 4 } });
    expect(r.strays).toEqual([]);
    expect(r.checked).toBe(2);
  });

  it('an in-place amend moves the record to the amended state; the node then finds the order and its untouched custody', async () => {
    const { p, rec } = recordOf('create.ask', { label: 'Sell 10 tokens' });
    const chain = new FakeChain();
    chain.applyTx(p.signed.tx);
    const store = new MemoryRecordStore();
    await store.put(rec);
    const snap = p.snapshots[0];
    const amended: OrderState = { kind: 'KobAsk', state: { ...(snap.order.state.state as AskState), price: '260000000' } };
    const plan = planCancelReplace({ kob, maker: MAKER.pk, funding: [], clock: { daa: 2000n } }, snap, { order: amended, value: 10n * 100_000_000n });
    expect(plan.request!.action).toBe('amendOrder');
    const signed = signAndValidate(kob, plan.built!);
    chain.applyTx(signed.tx, '2000');
    // a record that had a later proven state and a submit-time cancel mark: the amend replaces both (C5 merge follow-up)
    await store.put({ ...rec, last: { state: rec.state, txid: p.txid, index: 0, daa: '1000' }, cancelling: { txid: signed.tx.id, spends: `${p.txid}:0`, atUnix: '1' } });
    const [next] = await amendedRecords(kob, plan.built!, store, MAKER.pk);
    expect(next.last ?? null).toBeNull();
    expect(next.cancelling ?? null).toBeNull();
    expect(next).toMatchObject({ covenantId: rec.covenantId, kind: 'KobAsk', txid: null, output: null, amount: '10000', label: 'Sell 10 tokens', custody: rec.custody });
    expect(kob.decodeState('KobAsk', next.state)).toEqual(amended);
    expect(BigInt(next.value)).toBe(BigInt(signed.tx.outputs[0].value));
    expect(isPlacementRecord(next)).toBe(true);
    // the old record no longer finds the order (its script is gone; a search over every amount of it decides it); the moved one does, with the
    // custody the order had all along
    expect((await resolveRecord(kob, chain, rec, addr, { maxCandidates: 10_001 })).status).toBe('spent');
    const r = await resolveRecord(kob, chain, next, addr);
    expect(r.status).toBe('live');
    expect(r.order).toMatchObject({ transactionId: signed.tx.id, index: 0, covenantId: rec.covenantId });
    expect(r.custody).toMatchObject({ transactionId: p.txid, index: 1 });
    // a backup keeps the moved record (no placing tx: it passes the indexer-import checks)
    const back = importBackup(exportBackup([next], {}, 'testnet-10'), kob, { network: 'testnet-10', maker: MAKER.pk });
    expect(back.rejected).toEqual([]);
    expect(back.records).toHaveLength(1);
    // another maker's store, or no record: nothing to move
    expect(await amendedRecords(kob, plan.built!, new MemoryRecordStore(), MAKER.pk)).toEqual([]);
    expect(await amendedRecords(kob, plan.built!, store, OTHER.pk)).toEqual([]);
  });

  it('live after a partial fill: re-derives the continuation state and the custody (a large amount walks in steps of the minimum fill)', async () => {
    const { rec } = recordOf('create.ask');
    const st = kob.decodeState('KobAsk', rec.state) as OrderState;
    expect((st.state as AskState).minFill).toBe('1000');
    const cont: OrderState = { kind: 'KobAsk', state: { ...(st.state as AskState), amountLeft: '6000' } };
    const chain = new FakeChain();
    chain.add({ transactionId: 'ab'.repeat(32), index: 0, amount: '1000000000', scriptPublicKey: kob.scriptPublicKey(cont), covenantId: rec.covenantId, blockDaaScore: '2000' });
    const cust = tokenState(6000n, rec.covenantId, 4);
    chain.add({ transactionId: 'ab'.repeat(32), index: 1, amount: '1000000000', scriptPublicKey: kob.tokenScriptPublicKey(TOKEN.program, cust), covenantId: TOKEN.covenantId });
    const r = await resolveRecord(kob, chain, rec, addr);
    expect(r.status).toBe('live');
    expect(r.stateChanged).toBe(true);
    expect((r.order!.state.state as AskState).amountLeft).toBe('6000');
    expect(r.order!.blockDaaScore).toBe('2000');
    expect(r.custody!.state.amount).toBe('6000');
    expect(r.custody!.index).toBe(1);
  });

  it('a live ask whose custody is not on the node resolves live with custody null', async () => {
    const { p, rec } = recordOf('create.ask');
    const chain = new FakeChain();
    chain.applyTx(p.signed.tx);
    chain['utxos'].delete(`${p.txid}:1`);
    const r = await resolveRecord(kob, chain, rec, addr);
    expect(r.status).toBe('live');
    expect(r.custody).toBeNull();
  });

  it('spent after a real cancel: KobAsk is decidable (spent) when every amount fits the search, KobCondAsk is not (unknown)', async () => {
    for (const [name, want] of [['create.ask', 'spent'], ['create.condAsk', 'unknown']] as const) {
      const { p, rec } = recordOf(name);
      const chain = new FakeChain();
      chain.applyTx(p.signed.tx);
      const plan = planCancel({ kob, maker: MAKER.pk }, p.snapshots[0]);
      expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
      chain.applyTx(signAndValidate(kob, plan.built!).tx);
      expect((await resolveRecord(kob, chain, rec, addr, { maxCandidates: 10_001 })).status, name).toBe(want);
    }
    // with the default budget the 10_000 base units are walked in steps of the minimum fill only: not exhaustive, so unknown rather than spent
    const { p, rec } = recordOf('create.ask');
    const chain = new FakeChain();
    chain.applyTx(p.signed.tx);
    chain.applyTx(signAndValidate(kob, planCancel({ kob, maker: MAKER.pk }, p.snapshots[0]).built!).tx);
    expect((await resolveRecord(kob, chain, rec, addr)).status).toBe('unknown');
  });

  it('a bid resolves spent when gone (no mutable fields) and live while present', async () => {
    const { p, rec } = recordOf('create.bid');
    const chain = new FakeChain();
    expect((await resolveRecord(kob, chain, rec, addr)).status).toBe('spent');
    chain.applyTx(p.signed.tx);
    const r = await resolveRecord(kob, chain, rec, addr);
    expect(r.status).toBe('live');
    expect(r.custody).toBeNull();
  });

  it('rejects a forged utxo: right covenant id, wrong script', async () => {
    const rec = askWithAmount(recordOf('create.ask').rec, 30);
    const node = {
      async getUtxosByAddresses(a: string[]) {
        return a.map((address) => ({
          address, transactionId: 'cd'.repeat(32), index: 0, amount: '1', scriptPublicKey: '0000aa20' + 'ee'.repeat(32) + '87', blockDaaScore: '1', isCoinbase: false, covenantId: rec.covenantId,
        }));
      },
    };
    expect((await resolveRecord(kob, node, rec, addr)).status).toBe('spent');
    // and a genuine script that carries ANOTHER covenant id is ignored as well
    const chain = new FakeChain();
    chain.add({ transactionId: 'cd'.repeat(32), index: 0, amount: '1', scriptPublicKey: kob.scriptPublicKey(kob.decodeState('KobAsk', rec.state)), covenantId: 'ff'.repeat(32) });
    expect((await resolveRecord(kob, chain, rec, addr)).status).toBe('spent');
  });

  it('asks the node in chunks and gives up (unknown) beyond maxCandidates', async () => {
    const rec = askWithAmount(recordOf('create.ask').rec, 30);
    const chain = new FakeChain();
    const r = await resolveRecord(kob, chain, rec, addr, { chunk: 10 });
    expect(r.status).toBe('spent');
    expect(chain.calls.map((c) => c.length)).toEqual([1, 10, 10, 9]);
    expect(r.checked).toBe(30);

    const chain2 = new FakeChain();
    const t = await resolveRecord(kob, chain2, rec, addr, { chunk: 10, maxCandidates: 5 });
    expect(t.status).toBe('unknown');
    expect(chain2.calls.map((c) => c.length)).toEqual([1, 5]);
  });

  it('a truncated search still finds a state inside the window', async () => {
    const rec = askWithAmount(recordOf('create.ask').rec, 30);
    const st = kob.decodeState('KobAsk', rec.state) as OrderState;
    const chain = new FakeChain();
    chain.add({ transactionId: 'ab'.repeat(32), index: 0, amount: '5', scriptPublicKey: kob.scriptPublicKey({ kind: 'KobAsk', state: { ...(st.state as AskState), amountLeft: '28' } }), covenantId: rec.covenantId });
    const r = await resolveRecord(kob, chain, rec, addr, { maxCandidates: 5 });
    expect(r.status).toBe('live');
    expect((r.order!.state.state as AskState).amountLeft).toBe('28');
  });

  it('hints are accepted only when they match a derivable state', async () => {
    const rec = askWithAmount(recordOf('create.ask').rec, 12);
    const st = kob.decodeState('KobAsk', rec.state) as OrderState;
    const spk = kob.scriptPublicKey({ kind: 'KobAsk', state: { ...(st.state as AskState), amountLeft: '7' } });
    const hint = { address: addr(spk), transactionId: 'ab'.repeat(32), index: 4, amount: '9', scriptPublicKey: spk, blockDaaScore: '3', isCoinbase: false, covenantId: rec.covenantId };
    const empty = new FakeChain();
    const r = await resolveRecord(kob, empty, rec, addr, { hints: [hint] });
    expect(r.status).toBe('live');
    expect(r.order).toMatchObject({ transactionId: 'ab'.repeat(32), index: 4 });
    expect((r.order!.state.state as AskState).amountLeft).toBe('7');
    const forged = { ...hint, scriptPublicKey: '0000aa20' + '12'.repeat(32) + '87' };
    expect((await resolveRecord(kob, new FakeChain(), rec, addr, { hints: [forged] })).status).toBe('spent');
    expect((await resolveRecord(kob, new FakeChain(), rec, addr, { hints: [{ ...hint, covenantId: ZERO32 }] })).status).toBe('spent');
  });

  it('follows a repeating if-done entry through amount / repeat changes and node errors propagate', async () => {
    const { rec } = recordOf('create.ifdAsk.repeat');
    const st = kob.decodeState('KobIfdAsk', rec.state) as OrderState;
    const s = st.state as unknown as Record<string, string>;
    // one fill of the entry's minimum fill: amountLeft and rptAmount both drop by it
    const n = BigInt(s.minFill);
    const cont = { kind: 'KobIfdAsk', state: { ...s, amountLeft: String(BigInt(s.amountLeft) - n), rptAmount: String(BigInt(s.rptAmount) - n) } } as unknown as OrderState;
    const chain = new FakeChain();
    chain.add({ transactionId: 'ab'.repeat(32), index: 0, amount: '7', scriptPublicKey: kob.scriptPublicKey(cont), covenantId: rec.covenantId });
    const r = await resolveRecord(kob, chain, rec, addr);
    expect(r.status).toBe('live');
    expect((r.order!.state.state as unknown as Record<string, string>).rptAmount).toBe(String(BigInt(s.rptAmount) - n));
    const boom = { getUtxosByAddresses: async () => { throw new Error('node down'); } };
    await expect(resolveRecord(kob, boom, rec, addr)).rejects.toThrow('node down');
  });
});
