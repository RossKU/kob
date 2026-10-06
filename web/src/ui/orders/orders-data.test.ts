import { describe, expect, it } from 'vitest';
import type { Services } from '../../app/services';
import type { IndexerApi } from '../../data/indexer';
import type { Page, OrderView, StrayView } from '../../data/indexer-types';
import { MemoryRecordStore, recordsFromBuilt } from '../../kob/records';
import { loadKobNode } from '../../kob/wasm.node';
import type { ActionRequest } from '../../kob/types';
import { FakeChain, MAKER, goldenRequest, orderViewOf, placeGolden, signAndValidate, snapshotOfRecovered, testAddress } from '../../testing/chain-fixtures';
import { loadOrders, pool } from './orders-data';
import { describeEntry } from './orders-model';

const kob = loadKobNode();
const CLOCK = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };

function services(o: { chain?: FakeChain; indexer?: Partial<IndexerApi> | null; nodeDown?: boolean }): Services {
  const chain = o.chain ?? new FakeChain();
  return {
    kob,
    config: { network: 'testnet-10' },
    sdk: {} as never,
    node: {
      kind: 'http-mock',
      getClock: async () => {
        if (o.nodeDown) throw new Error('node down');
        return { ...CLOCK, rateMilli: 10_000 };
      },
      getUtxosByAddresses: async (a: string[]) => {
        if (o.nodeDown) throw new Error('node down');
        return chain.getUtxosByAddresses(a);
      },
    },
    indexer: o.indexer === null ? null : ({ allOrders: async () => [], strays: async () => [], ...o.indexer } as IndexerApi),
  } as unknown as Services;
}

const place = (name: string, label?: string) => {
  const p = placeGolden(kob, name);
  const [rec] = recordsFromBuilt(kob, p.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_999_000n, placedAtDaa: 900n, ...(label ? { label } : {}) });
  return { p, rec };
};

/**
 * A golden placement with a smaller amount left: the node alone proves a record spent only when its search over the continuation amounts is
 * exhaustive (every base unit up to the candidate budget, records.ts); the 10 TST golden ask (10,000 base units) is beyond it and resolves
 * 'unknown' without the indexer.
 */
const placeSmall = (name: string, amountLeft: string) => {
  const request = goldenRequest<ActionRequest>(name, MAKER.pk);
  (request as unknown as { order: { state: Record<string, string> } }).order.state.amountLeft = amountLeft;
  const built = kob.build(request);
  const signed = signAndValidate(kob, built, [MAKER.sk]);
  const recovered = kob.recoverOrders(signed.tx);
  const p: ReturnType<typeof placeGolden> = { request, built, signed, txid: signed.tx.id, recovered, snapshots: recovered.map((r) => snapshotOfRecovered(r, 1000n)) };
  const [rec] = recordsFromBuilt(kob, p.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_999_000n, placedAtDaa: 900n });
  return { p, rec };
};

const chainWith = (...txs: { p: ReturnType<typeof placeGolden> }[]) => {
  const chain = new FakeChain();
  for (const t of txs) chain.applyTx(t.p.signed.tx);
  return chain;
};

describe('pool', () => {
  it('limits concurrency and keeps the order of results', async () => {
    let running = 0;
    let peak = 0;
    const out = await pool([1, 2, 3, 4, 5, 6, 7], 3, async (x) => {
      running++;
      peak = Math.max(peak, running);
      await new Promise((r) => setTimeout(r, 5));
      running--;
      return x * 2;
    });
    expect(out).toEqual([2, 4, 6, 8, 10, 12, 14]);
    expect(peak).toBeLessThanOrEqual(3);
    expect(await pool([], 4, async (x) => x)).toEqual([]);
  });
});

describe('loadOrders', () => {
  it('shows a fresh placement the indexer has not seen yet, resolved live on the node', async () => {
    const a = place('create.ask', 'limit sell');
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    const data = await loadOrders(services({ chain: chainWith(a) }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(data.entries).toHaveLength(1);
    const row = describeEntry(data.entries[0], { kob, clock: data.clock });
    expect(row).toMatchObject({ status: 'open', live: true, canCancel: true, source: 'node', label: 'limit sell' });
    expect(data.snapshots).toHaveLength(1);
    expect(data.indexerError).toBeNull();
    expect(data.nodeError).toBeNull();
  });

  it('keeps working when the indexer is down: records still resolve, the error is reported', async () => {
    const a = place('create.bid');
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    const indexer = { allOrders: async () => { throw new Error('indexer down'); }, strays: async () => { throw new Error('indexer down'); } };
    const data = await loadOrders(services({ chain: chainWith(a), indexer }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(data.indexerError?.message).toBe('indexer down');
    expect(data.views).toEqual([]);
    expect(describeEntry(data.entries[0], { kob, clock: data.clock }).live).toBe(true);
  });

  it('does not ask the node about orders the indexer already lists', async () => {
    const a = place('create.ask');
    const view = orderViewOf(kob, a.p.snapshots[0]);
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    const chain = chainWith(a);
    const data = await loadOrders(services({ chain, indexer: { allOrders: async () => [view] } }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(chain.calls).toHaveLength(0);
    expect(data.entries).toHaveLength(1);
    expect(data.entries[0]).toMatchObject({ view, resolved: null });
    expect(data.positions).toHaveLength(1);
  });

  it('a record whose UTXO is gone is closed when the node search is exhaustive; a node failure leaves it unknown and is reported', async () => {
    const a = placeSmall('create.ask', '1000');
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    const gone = await loadOrders(services({ chain: new FakeChain() }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(describeEntry(gone.entries[0], { kob, clock: gone.clock }).status).toBe('closed');
    // the 10,000-unit golden ask cannot be searched exhaustively on the node: gone stays unknown (the indexer proves it at once)
    const bigStore = new MemoryRecordStore();
    await bigStore.put(place('create.ask').rec);
    const big = await loadOrders(services({ chain: new FakeChain() }), MAKER.pk, bigStore, { spkToAddress: testAddress });
    expect(describeEntry(big.entries[0], { kob, clock: big.clock }).status).toBe('unknown');
    const down = await loadOrders(services({ chain: chainWith(a), nodeDown: true }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(down.nodeError).not.toBeNull();
    expect(down.resolveFailures).toBe(1);
    expect(describeEntry(down.entries[0], { kob, clock: down.clock }).status).toBe('unknown');
  });

  it('merges indexer orders and records of different orders, newest first, and passes strays through', async () => {
    const a = place('create.ask');
    const b = place('create.bid');
    const stray = { token: 'aa'.repeat(32) } as unknown as StrayView;
    const viewA: OrderView = orderViewOf(kob, a.p.snapshots[0]);
    const store = new MemoryRecordStore();
    await store.put(b.rec);
    const data = await loadOrders(
      services({ chain: chainWith(b), indexer: { allOrders: async () => [viewA], strays: async () => [stray] } as Partial<IndexerApi> & { orders?: () => Promise<Page<OrderView>> } }),
      MAKER.pk,
      store,
      { spkToAddress: testAddress },
    );
    expect(data.entries.map((e) => e.id).sort()).toEqual([a.rec.covenantId, b.rec.covenantId].sort());
    expect(data.strays).toEqual([stray]);
  });

  it('works without an indexer (node + records only)', async () => {
    const a = place('create.ask');
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    const data = await loadOrders(services({ chain: chainWith(a), indexer: null }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(data.indexerError).toBeNull();
    expect(data.entries).toHaveLength(1);
  });
});

describe('records follow the order (C5-06: a cancel drops the record only once it is final)', () => {
  const mark = async (store: MemoryRecordStore, rec: ReturnType<typeof place>['rec'], spends: string, atUnix = CLOCK.unixSeconds) =>
    store.put({ ...rec, cancelling: { txid: 'cc'.repeat(32), spends, atUnix: atUnix.toString() } });

  it('keeps a marked record while the order is live at the outpoint the cancel spends, drops it once the indexer says cancelled', async () => {
    const a = place('create.ask');
    const snap = a.p.snapshots[0];
    const store = new MemoryRecordStore();
    await mark(store, a.rec, `${snap.order.transactionId}:${snap.order.index}`);
    await loadOrders(services({ indexer: { allOrders: async () => [orderViewOf(kob, snap)] } }), MAKER.pk, store, { spkToAddress: testAddress });
    expect((await store.get(a.rec.covenantId))?.cancelling?.txid).toBe('cc'.repeat(32));
    await loadOrders(services({ indexer: { allOrders: async () => [orderViewOf(kob, snap, { status: 'cancelled' })] } }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(await store.get(a.rec.covenantId)).toBeNull();
  });

  it('a fill that won the race (the order lives on at another outpoint) clears the mark and keeps the record', async () => {
    const a = place('create.ask');
    const snap = a.p.snapshots[0];
    const store = new MemoryRecordStore();
    await mark(store, a.rec, `${'ee'.repeat(32)}:0`);
    await loadOrders(services({ indexer: { allOrders: async () => [orderViewOf(kob, snap, { status: 'partial' })] } }), MAKER.pk, store, { spkToAddress: testAddress });
    const rec = await store.get(a.rec.covenantId);
    expect(rec).not.toBeNull();
    expect(rec!.cancelling ?? null).toBeNull();
  });

  it('without the indexer: dropped when the node search is exhaustive and finds nothing (spent)', async () => {
    const a = placeSmall('create.ask', '1000');
    const store = new MemoryRecordStore();
    await mark(store, a.rec, `${a.p.txid}:0`);
    await loadOrders(services({ chain: new FakeChain(), indexer: null }), MAKER.pk, store, { spkToAddress: testAddress });
    expect(await store.get(a.rec.covenantId)).toBeNull();
  });
});

describe('records for orders the wallet did not place (C5-03: exits survive an indexer outage)', () => {
  it('a live view without a record gets one with its proven state, its extension commitment and origin "indexer"; a closed one does not', async () => {
    const a = place('create.ask');
    const b = place('create.bid');
    const store = new MemoryRecordStore();
    const views = [orderViewOf(kob, a.p.snapshots[0]), orderViewOf(kob, b.p.snapshots[0], { status: 'filled' })];
    await loadOrders(services({ indexer: { allOrders: async () => views } }), MAKER.pk, store, { spkToAddress: testAddress });
    const recs = await store.list();
    expect(recs.map((r) => r.covenantId)).toEqual([a.rec.covenantId]);
    expect(recs[0]).toMatchObject({ origin: 'indexer', kind: 'KobAsk', state: a.rec.state, ext: views[0].extension_commitment });
  });
});
