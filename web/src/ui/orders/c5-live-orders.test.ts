// C5 liveness ("every state can be exited"): My-orders states the wallet must show and act on. Uncompiled when written (2026-10-02);
// tests marked `C5: expected to fail` pin the fixed behaviour of a finding in research/2026-09-29/planning/core/c5_liveness.md.
import { describe, expect, it, vi } from 'vitest';
import type { Services } from '../../app/services';
import type { IndexerApi } from '../../data/indexer';
import type { OrderView } from '../../data/indexer-types';
import { MemoryRecordStore, recordsFromBuilt } from '../../kob/records';
import { isLiveStatus, isTerminalStatus, groupPositions } from '../../kob/positions';
import { parseRegistry } from '../../kob/registry';
import type { Kcc20State, TokenUtxo } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { FakeChain, MAKER, keyUtxo, orderViewOf, placeGolden, signAndValidate, strayFor, testAddress, tradableRegistryJson } from '../../testing/chain-fixtures';
import * as data from './orders-data';
import { loadOrders } from './orders-data';
import { planCancelFor, planCancelMany } from './actions';
import { buildEntries, countByStatus, describeEntry } from './orders-model';

const kob = loadKobNode();
const KAS = 100_000_000n;
const CLOCK = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const registry = parseRegistry(tradableRegistryJson(), { kob });
vi.spyOn(data, 'spkToAddressFor').mockReturnValue(testAddress);

function services(o: { chain?: FakeChain; indexer?: Partial<IndexerApi> | null }): Services {
  const chain = o.chain ?? new FakeChain();
  return {
    kob,
    config: { network: 'testnet-10' },
    sdk: {} as never,
    registry,
    node: { kind: 'http-mock', getClock: async () => CLOCK, getUtxosByAddresses: (a: string[]) => chain.getUtxosByAddresses(a) },
    utxos: { fundingFor: async () => [keyUtxo(MAKER.pk, 100n * KAS, 201)] },
    tracker: { tokenUtxosFor: async () => [] },
    indexer: o.indexer === null ? null : ({ allOrders: async () => [], strays: async () => [], order: async () => null, ...o.indexer } as IndexerApi),
  } as unknown as Services;
}

const place = (name: string) => {
  const p = placeGolden(kob, name);
  const [rec] = recordsFromBuilt(kob, p.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_999_000n, placedAtDaa: 900n });
  return { p, rec, snap: p.snapshots[0] };
};

describe('C5-L1: a killed IOC / FOK / market order is a terminal status, not "unknown"', () => {
  // The indexer closes an IOC whose unfilled rest is returned (and every IOC / FOK kill) with status `killed`
  // (crates/kob-executor/src/indexer/processor.rs: `(None, Some("kill")) => "killed"`); the web knows only
  // open / partial / filled / cancelled / refunded / closed, so every partly filled market order reads "unknown".
  // C5: expected to fail until orders-model.ts ORDER_STATUSES / statusOf and positions.ts TERMINAL learn `killed`.
  it('describeEntry maps killed to a done status and counts it as done', () => {
    const { snap } = place('create.ask.market');
    const view = orderViewOf(kob, snap, { status: 'killed', filled_amount: '3', amount_left: '0' } as Partial<OrderView>);
    const row = describeEntry(buildEntries([view], [])[0], { kob, clock: CLOCK });
    expect(row.status).not.toBe('unknown');
    expect(row.live).toBe(false);
    expect(row.canCancel).toBe(false);
    expect(countByStatus([row])).toEqual({ live: 0, done: 1, unknown: 0 });
  });

  it('a position whose only member was killed is closed', () => {
    expect(isTerminalStatus('killed')).toBe(true);
    expect(isLiveStatus('killed')).toBe(false);
    const { snap } = place('create.bid.market');
    const [pos] = groupPositions([orderViewOf(kob, snap, { status: 'killed' } as Partial<OrderView>)]);
    expect(pos.status).toBe('closed');
  });
});

describe('C5-L2: a live order older than the newest 20 pages of history is still listed', () => {
  // loadOrders asks `allOrders({ maker, limit: 200 })` without a status filter; the indexer pages newest first and allOrders stops after
  // 20 pages (4,000 orders). A repeat IFD books one exit per entry fill, so an active trap repeat reaches that in weeks, and its ENTRY,
  // the oldest order of the position (and the one the booked exits need for every take-profit), is the first to fall off the list:
  // no row, no cancel, position cancel misses it. The indexer already serves `status=active`.
  // C5: expected to fail until loadOrders also loads `allOrders({ maker, status: 'active' })` (all pages) and merges.
  it('the old live order appears even when the unfiltered history is truncated', async () => {
    const old = place('create.ifdBid.repeat');
    const live = orderViewOf(kob, old.snap);
    const recent = place('create.ask');
    // 4,000 newer, closed orders: what the unfiltered, truncated listing returns (same view object reused, ids differ)
    const history: OrderView[] = Array.from({ length: 4000 }, (_, i) => ({
      ...orderViewOf(kob, recent.snap, { status: 'filled' } as Partial<OrderView>),
      covenant_id: (i + 1).toString(16).padStart(64, '0'),
    }));
    const allOrders = async (q: { status?: string } = {}) => (q.status === 'active' ? [live] : history);
    const d = await loadOrders(services({ indexer: { allOrders } as Partial<IndexerApi> }), MAKER.pk, new MemoryRecordStore(), { spkToAddress: testAddress });
    const ids = d.entries.map((e) => e.id);
    expect(ids).toContain(old.snap.covenantId);
    const row = describeEntry(d.entries.find((e) => e.id === old.snap.covenantId)!, { kob, clock: d.clock });
    expect(row.live).toBe(true);
    expect(row.canCancel).toBe(true);
  });
});

describe('C5-L3: dust strays must not make an order uncancellable from the UI', () => {
  // Anyone can send token UTXOs of the order's token to its covenant id. With more strays than the token program's input room
  // (3/3 program: custody + 2), planCancel refuses (`cancel.strays-exceed-slots`) unless `allowAbandonStrays`, and NO UI caller passes it
  // (ui/orders/actions.ts planCancelFor / planCancelMany): the maker can no longer cancel the order (nor its position) at all; only a fill
  // or the expiry refund ends it (90 days for a GTC, never for a stop-loss exit the user wants to pull). A griefer pays 3 dust strays.
  // C5: expected to fail until the actions fall back to abandoning the smallest strays (a warning the confirm screen shows) or sweep
  // them in a separate step first.
  const withStrays = (c: FakeChain, strays: TokenUtxo[]): FakeChain => {
    for (const x of strays) {
      c.add({ transactionId: x.transactionId, index: x.index, amount: x.amount, scriptPublicKey: kob.tokenScriptPublicKey('KCC20Ref', x.state as Kcc20State), covenantId: x.covenantId });
    }
    return c;
  };

  it('planCancelFor still yields a signable cancel (abandoning the smallest stray, with a warning)', async () => {
    const a = place('create.ask');
    const strays = [strayFor(a.snap, 1n, 91), strayFor(a.snap, 1n, 92), strayFor(a.snap, 1n, 93)];
    const chain = new FakeChain();
    chain.applyTx(a.p.signed.tx);
    withStrays(chain, strays);
    const view = orderViewOf(kob, { ...a.snap, strays });
    const s = services({ chain, indexer: { order: async () => view } });
    const entry = buildEntries([view], [a.rec])[0];
    const { plan } = await planCancelFor(s, MAKER.pk, entry);
    expect(plan.ok).toBe(true);
    expect(plan.issues.map((i) => i.code)).toContain('cancel.strays-abandoned');
    signAndValidate(kob, plan.built!, [MAKER.sk]);
  });

  it('planCancelMany (position / cancel-all) does not drop the griefed order', async () => {
    const a = place('create.condAsk');
    const strays = [strayFor(a.snap, 1n, 94), strayFor(a.snap, 1n, 95), strayFor(a.snap, 1n, 96)];
    const chain = new FakeChain();
    chain.applyTx(a.p.signed.tx);
    withStrays(chain, strays);
    const view = orderViewOf(kob, { ...a.snap, strays });
    const s = services({ chain, indexer: { order: async () => view } });
    const out = await planCancelMany(s, MAKER.pk, buildEntries([view], [a.rec]));
    expect(out.plans.filter((p) => p.ok).flatMap((p) => p.cancelIds)).toContain(a.snap.covenantId);
  });
});

describe('C5-L4: if-done exits survive an indexer outage (the wallet keeps a record of them)', () => {
  // An exit is created by its entry's fill: it has no placement record. The wallet's records (localStorage, backup files) therefore hold
  // only the ENTRY; when the indexer is down or gone, `loadOrders` lists only records, so every exit (which holds the bought tokens of a
  // buy-first IFD / repeat, or the proceeds + prefund of a sell-first one) disappears from My orders and cannot be cancelled. The indexer
  // serves everything a record needs (state, extension commitment); the wallet just never stores it.
  // C5: expected to fail until loadOrders (or the position view) upserts a record for every own live order the indexer shows without one.
  it('loadOrders stores a record for an exit the indexer shows', async () => {
    const { goldenRequest } = await import('../../testing/chain-fixtures');
    const { snapshotFromItem } = await import('../../kob/cancel');
    const req = goldenRequest<import('../../kob/types').CancelPositionRequest>('cancel.position.repeatBuyFirst');
    const entry = snapshotFromItem(req.orders[0]);
    const exit = snapshotFromItem(req.orders[1]);
    const views = [orderViewOf(kob, entry), orderViewOf(kob, exit, { parent: entry.covenantId } as Partial<OrderView>)];
    const store = new MemoryRecordStore();
    await loadOrders(services({ indexer: { allOrders: async () => views } as Partial<IndexerApi> }), MAKER.pk, store, { spkToAddress: testAddress });
    const ids = (await store.list()).map((r) => r.covenantId);
    expect(ids).toContain(exit.covenantId);
  });
});
