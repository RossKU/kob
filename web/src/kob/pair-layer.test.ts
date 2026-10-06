// The pair kinds (payload kinds 0x08 KobPair, 0x09 KobCondPair, 0x0a KobIfdPair) across the wallet's kob layer: placement records (custodies of
// A and / or B, a sell-first entry's prefund), backups, node resolution (a live order and both its custodies; a partly filled pair ask found by
// its continuation), node reconciliation of snapshots, balances per token, positions (an if-done pair entry with its exits) and notifications
// (a pair fill is volume only: never a price). Transactions are the golden pair vectors re-keyed to the test maker and validated by kob-wasm.
import { describe, expect, it } from 'vitest';
import type { OrderView } from '../data/indexer-types';
import { escrowedBalances, withFreeBalances } from './balances';
import { snapshotFromOrderView, type OrderSnapshot } from './cancel';
import { confirmSnapshotOnNode } from './node-verify';
import { diffOrders, snapshotMap, snapshotOf } from './notifications';
import { custodiesOf, describeOrder, isPairKind } from './order-facts';
import { groupPositions } from './positions';
import { exportBackup, exportIndexerRecovery, importBackup, importIndexerRecovery, isPlacementRecord, recordsFromBuilt, resolveRecord } from './records';
import type { OrderState, TokenUtxo } from './types';
import { loadKobNode } from './wasm.node';
import { FakeChain, MAKER, orderViewOf, placeGolden, testAddress, tokenUtxoView } from '../testing/chain-fixtures';
import { PAIR_CREATES, PAIR_MIXES, buildGolden, legState, snapshotAfter, withFields } from '../testing/pair-fixtures';
import { goldenRequest } from '../testing/chain-fixtures';

const kob = loadKobNode();
const opts = { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_790_000_000n, placedAtDaa: 1000n };
const A = '70'.repeat(32);
const B = '71'.repeat(32);

/** An indexer view of a pair order snapshot, with the `pair` object of docs/ops/executor.md 5.4 (custodies in record order). */
function pairView(snap: OrderSnapshot, over: Partial<OrderView> & Record<string, unknown> = {}): OrderView {
  const st = snap.order.state;
  const d = describeOrder(st, kob);
  const p = d.pair!;
  const utxos = [snap.custody ?? null, snap.prefund ?? null];
  const base = orderViewOf(kob, snap, { price: null, in_book: false, custody: snap.custody ? { expected_amount: snap.custody.state.amount, utxo: { ...tokenUtxoView(snap.custody, 'custody'), token: snap.custody.covenantId! }, ok: true } : { expected_amount: null, utxo: null, ok: true } });
  const pair = {
    base: A, quote: B, base_family: p.base.family, quote_family: p.quote.family, base_template_hash: p.base.tplHash, quote_template_hash: p.quote.tplHash,
    base_scale: Number(p.base.scale), quote_scale: Number(p.quote.scale), side: p.side === 'sell' ? 'ask' : 'bid', price: p.price ? p.price.toString() : null,
    quote_now: p.price.toString(), amount_left: d.amountLeft?.toString() ?? null, prefund: p.prefund.toString(), delivery_carrier: p.deliveryCarrier.toString(),
    custodies: custodiesOf(st, kob).map((c, k) => ({
      token: c.token, role: c.role, expected_amount: c.amount.toString(), utxo: utxos[k] ? { ...tokenUtxoView(utxos[k]!, 'custody'), token: c.token } : null, ok: true,
    })),
  };
  return { ...base, ...over, pair } as unknown as OrderView;
}

describe('placement records of the pair kinds (payload kinds 0x08 - 0x0a)', () => {
  for (const mix of PAIR_MIXES) {
    for (const [suffix, kind] of PAIR_CREATES) {
      const name = `pair.${mix}${suffix}`;
      it(`${name}: recorded with its kind, amount and custodies; a backup re-derives it from the placing tx`, () => {
        const p = placeGolden(kob, name);
        const [rec] = recordsFromBuilt(kob, p.built, opts);
        expect(isPlacementRecord(rec)).toBe(true);
        expect(rec).toMatchObject({ kind, maker: MAKER.pk, txid: p.txid, covenantId: p.recovered[0]!.covenantId });
        expect(rec!.templateHash).toBe(kob.templates().find((t) => t.name === kind)!.hash);
        const cs = custodiesOf(p.recovered[0]!.order as OrderState, kob).filter((c) => c.amount > 0n);
        expect(rec!.custody?.tokenState?.amount ?? null).toBe(cs[0] ? cs[0].amount.toString() : null);
        expect(rec!.prefund?.tokenState?.amount ?? null).toBe(cs[1] ? cs[1].amount.toString() : null);
        const back = importBackup(exportBackup([rec!], { [p.txid]: p.signed.tx }, 'testnet-10'), kob, { network: 'testnet-10', maker: MAKER.pk });
        expect(back.rejected).toEqual([]);
        expect(back.records[0]).toEqual(rec);
      });
    }
  }

  it('the indexer recovery format round-trips a pair entry (state and the first custody\'s extension)', () => {
    const p = placeGolden(kob, 'pair.create.ifdAsk');
    const [rec] = recordsFromBuilt(kob, p.built, opts);
    const file = exportIndexerRecovery([rec!], 'testnet-10');
    expect(file.orders[0]).toMatchObject({ template_hash: rec!.templateHash, state: rec!.state, extension_commitment: 'ee'.repeat(32) });
    const r = importIndexerRecovery(file, kob, { maker: MAKER.pk, network: 'testnet-10' });
    expect(r.rejected).toEqual([]);
    expect(r.records[0]).toMatchObject({ kind: 'KobIfdPair', covenantId: rec!.covenantId, ext: 'ee'.repeat(32) });
  });
});

describe('node resolution of pair orders', () => {
  it('a live sell-first entry: found unchanged with BOTH custodies (A and the B prefund), each on its own token\'s program', async () => {
    for (const mix of PAIR_MIXES) {
      const p = placeGolden(kob, `pair.${mix}create.ifdAsk`);
      const [rec] = recordsFromBuilt(kob, p.built, opts);
      const chain = new FakeChain();
      chain.applyTx(p.signed.tx);
      const r = await resolveRecord(kob, chain, rec!, testAddress);
      expect(r.status).toBe('live');
      expect(r.custody).toMatchObject({ covenantId: A, index: p.snapshots[0]!.custody!.index });
      expect(r.prefund).toMatchObject({ covenantId: B, index: p.snapshots[0]!.prefund!.index });
    }
  });

  it('a pair bid: its custody is its B escrow', async () => {
    const p = placeGolden(kob, 'pair.create.bid');
    const [rec] = recordsFromBuilt(kob, p.built, opts);
    const chain = new FakeChain();
    chain.applyTx(p.signed.tx);
    const r = await resolveRecord(kob, chain, rec!, testAddress);
    expect(r.status).toBe('live');
    expect(r.custody).toMatchObject({ covenantId: B });
    expect(r.prefund ?? null).toBeNull();
  });

  it('a partly filled pair ask is found at its continuation (its custody walks with its amount)', async () => {
    const { built, signed } = buildGolden(kob, 'pair.ask.rest');
    const leg = legState(goldenRequest('pair.ask.rest', MAKER.pk), 0, 'KobPair');
    const cov = built.tx.inputs[0]!.utxo.covenantId!;
    const imported = importIndexerRecovery({ version: 2, network: 'testnet-10', orders: [{ template_hash: kob.templates().find((t) => t.name === 'KobPair')!.hash, state: kob.encodeState(leg), covenant_id: cov, extension_commitment: 'ee'.repeat(32) }] }, kob, { maker: MAKER.pk, network: 'testnet-10' });
    expect(imported.rejected).toEqual([]);
    const chain = new FakeChain();
    chain.applyTx(signed.tx);
    const r = await resolveRecord(kob, chain, imported.records[0]!, testAddress);
    expect(r.status).toBe('live');
    expect(r.stateChanged).toBe(true);
    expect(r.order!.state.state).toMatchObject({ amountLeft: '6000', custody: '6000' });
    expect(r.custody).toMatchObject({ covenantId: A, state: { amount: '6000' } });
  });
});

describe('node reconciliation, snapshots from views, balances, positions and notifications of pair orders', () => {
  it('a snapshot from a pair view carries both custodies; the node confirms them (and refuses a prefund it does not hold)', async () => {
    const p = placeGolden(kob, 'pair.create.ifdAsk');
    const snap = p.snapshots[0]!;
    const view = pairView(snap);
    const fromView = snapshotFromOrderView(view);
    expect(fromView.custody).toMatchObject({ transactionId: snap.custody!.transactionId, index: snap.custody!.index, covenantId: A });
    expect(fromView.prefund).toMatchObject({ transactionId: snap.prefund!.transactionId, index: snap.prefund!.index, covenantId: B });
    const chain = new FakeChain();
    chain.applyTx(p.signed.tx);
    const confirmed = await confirmSnapshotOnNode(kob, chain, fromView, testAddress);
    expect(confirmed.prefund).toMatchObject({ covenantId: B, amount: snap.prefund!.amount });
    const forged = { ...fromView, prefund: { ...fromView.prefund!, index: 77 } } as OrderSnapshot;
    await expect(confirmSnapshotOnNode(kob, chain, forged, testAddress)).rejects.toThrow(/prefund/);
  });

  it('balances count a pair order\'s custody of A under A and of B under B, and its KAS as carriers (no KAS escrow)', () => {
    const ask = placeGolden(kob, 'pair.create.ask').snapshots[0]!;
    const bid = placeGolden(kob, 'pair.create.bid').snapshots[0]!;
    const sellFirst = placeGolden(kob, 'pair.create.ifdAsk').snapshots[0]!;
    const rows = escrowedBalances([ask, pairView(bid), sellFirst]);
    const a = rows.find((r) => r.token === A)!;
    const b = rows.find((r) => r.token === B)!;
    const cs = (s: OrderSnapshot, t: string) => custodiesOf(s.order.state, kob).filter((c) => c.token === t).reduce((x, c) => x + c.amount, 0n);
    expect(a.custody).toBe(cs(ask, A) + cs(bid, A) + cs(sellFirst, A));
    expect(b.custody).toBe(cs(ask, B) + cs(bid, B) + cs(sellFirst, B));
    expect(a.orders).toBe(3);
    expect(b.orders).toBe(0);
    expect(a.kasEscrow).toBe(0n);
    expect(withFreeBalances(rows, [] as TokenUtxo[], MAKER.pk).map((x) => x.token)).toEqual([A, B]);
  });

  it('an if-done pair entry and its exit form one position of the pair (quote B named)', () => {
    const { built, signed } = buildGolden(kob, 'pair.ifd.bid.cont');
    const entry0 = legState(goldenRequest('pair.ifd.bid.cont', MAKER.pk), 0, 'KobIfdPair');
    const n = 4000n;
    const spend = kob.ifdPairAmounts(entry0, n, (entry0.state as { price: string }).price).spend!;
    const entry = snapshotAfter(kob, built, signed.tx, built.tx.inputs[0]!.utxo.covenantId!, [
      withFields(entry0, { amountLeft: 6000n, custody: BigInt((entry0.state as { custody: string }).custody) - spend }),
    ])!;
    const exitCov = built.covenants.find((c) => c.template === 'KobCondPair')!.covenantId;
    const exit = snapshotAfter(kob, built, signed.tx, exitCov, [kob.ifdPairExitFor(entry0, n, n)])!;
    const ve = pairView(entry, { status: 'partial', filled_amount: '4000', initial_amount: '10000', amount_left: '6000', children: [exitCov] });
    const vx = pairView(exit, { parent: entry.covenantId, amount_left: '4000' });
    const [pos] = groupPositions([vx, ve]);
    expect(pos).toMatchObject({ kind: 'ifd', side: 'buy', quote: B, status: 'partial', amountLeft: 10_000n, cancelIds: [entry.covenantId, exitCov] });
    expect(isPairKind(exit.order.state.kind)).toBe(true);
  });

  it('a pair fill notifies volume of A with the quote token, never a price', () => {
    const snap = placeGolden(kob, 'pair.create.ask').snapshots[0]!;
    const v0 = pairView(snap);
    const v1 = pairView(snap, { status: 'partial', filled_amount: '4000', amount_left: '6000' });
    const s0 = snapshotOf(v0, null);
    expect(s0).toMatchObject({ price: null, quote: B });
    const ev = diffOrders(snapshotMap([v0], null), snapshotMap([v1], null), 1_000);
    expect(ev).toHaveLength(1);
    expect(ev[0]).toMatchObject({ kind: 'partial', params: { amount: '4000', quote: B } });
    expect(ev[0]!.params).not.toHaveProperty('price');
    // a re-org of that fill: still no price
    const back = diffOrders(snapshotMap([v1], null), snapshotMap([v0], null), 2_000);
    expect(back[0]).toMatchObject({ kind: 'reorg', params: { what: 'fill', amount: '4000' } });
    expect(back[0]!.params).not.toHaveProperty('price');
  });
});
