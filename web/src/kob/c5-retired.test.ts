// C5-02 (web): live orders of a RETIRED template (docs/spec/template-retirement.md) are found from their record and cancelled by the maker through
// kob-wasm `buildCancelRetired`; the signing screen accepts and describes the `retired` plan. The retired templates of protocol v2.6 hold LEGACY lot
// states (kob-wasm `decodeRetired`: `lotsLeft` lots of `lotUnits x unit` base units). Fixture: a v2.6 KobAsk keeps today's KobAsk byte layout
// (unit / lotUnits / tipLot / maxLots / lotsLeft sit where scale / minFill / tip / maxFill / amountLeft sit now), so today's encoder writes its span:
// scale 1000, minFill 1, amountLeft 10 is the legacy state unit 1000, lotUnits 1, lotsLeft 10, a custody of 10 x 1 x 1000 = 10_000 base units.
import { describe, expect, it } from 'vitest';
import { planCancel, planCancelReplace, planRefund, snapshotFromRecord, type CancelEnv } from './cancel';
import { decodeSigning } from './decode';
import { confirmSnapshotOnNode } from './node-verify';
import { custodyAmountOf, describeLegacy, isLegacyState, retiredAmountLeft } from './order-facts';
import { exportBackup, gapOf, importBackup, isPlacementRecord, recordCodec, recordsFromBuilt, resolveRecord, type PlacementRecord } from './records';
import type { AskState, OrderState } from './types';
import { loadKobNode } from './wasm.node';
import { buildEntries, describeEntry } from '../ui/orders/orders-model';
import { FakeChain, MAKER, keyUtxo, nodeFactsOf, placeGolden, signAndValidate, testAddress } from '../testing/chain-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
const env: CancelEnv = { kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1000n } };
const CLOCK = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };

const v26 = (kindName: string) => kob.retiredTemplates().find((t) => t.kindName === kindName && t.note.includes('protocol v2.6 lot templates'))!;
const retired = v26('KobAsk');

function fixture(lotsLeft = '10') {
  const p = placeGolden(kob, 'create.ask');
  const snap = p.snapshots[0];
  // today's encoding of these values IS the v2.6 lot span (same layout): unit 1000, lotUnits 1, lotsLeft as given
  const st = { ...snap.order.state, state: { ...(snap.order.state.state as AskState), minFill: '1', amountLeft: lotsLeft } } as OrderState;
  const span = kob.encodeState(st);
  const [placed] = recordsFromBuilt(kob, p.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_999_000n, placedAtDaa: 900n });
  const rec: PlacementRecord = { ...placed, templateHash: retired.hash, state: kob.encodeState({ ...st, state: { ...(st.state as AskState), amountLeft: '10' } } as OrderState) };
  return { snap, span, rec };
}

/** A node holding the retired order (its span) and its custody of `custodyAmount` base units. */
function chainWith(f: ReturnType<typeof fixture>, span: string, custodyAmount: bigint): FakeChain {
  const c = new FakeChain();
  c.add({ transactionId: 'ab'.repeat(32), index: 0, amount: f.snap.order.amount, scriptPublicKey: kob.retiredScriptPublicKey(retired.hash, span), covenantId: f.rec.covenantId, blockDaaScore: '1000' });
  const cs = { ...f.snap.custody!.state, amount: custodyAmount.toString() };
  const program = kob.templates().find((t) => t.hash === (f.snap.order.state.state as AskState).tokenTplHash)!.name as Parameters<typeof kob.tokenScriptPublicKey>[0];
  c.add({ transactionId: 'ab'.repeat(32), index: 1, amount: f.snap.custody!.amount, scriptPublicKey: kob.tokenScriptPublicKey(program, cs), covenantId: f.snap.custody!.covenantId, blockDaaScore: '1000' });
  return c;
}

describe('C5-02: an order of a retired template is found and cancelled by its maker', () => {
  it('the build knows the retired v2.6 ask template and decodes its span as the LEGACY lot state', () => {
    expect(retired).toMatchObject({ kind: 'KobAsk', kindName: 'KobAsk', family: 1, stateLen: 243 });
    const f = fixture();
    expect(f.span.length).toBe(243 * 2);
    const legacy = kob.decodeRetired(retired.hash, f.span);
    expect(legacy.kind).toBe('KobAsk');
    expect(legacy.state).toMatchObject({ maker: MAKER.pk, unit: '1000', lotUnits: '1', lotsLeft: '10' });
    expect(isLegacyState(legacy)).toBe(true);
    expect(retiredAmountLeft(legacy.state)).toBe(10_000n);
    expect(custodyAmountOf(legacy as unknown as OrderState)).toBe(10_000n);
    // the description of an older contract version: price per `unit` base units, its lot was its minimum fill
    expect(describeLegacy(legacy)).toMatchObject({ kind: 'KobAsk', side: 'sell', maker: MAKER.pk, scale: 1_000n, minFill: 1_000n, amountLeft: 10_000n, tokenAmount: 10_000n, price: 250_000_000n });
    const codec = recordCodec(kob, f.rec)!;
    expect(codec.retired?.hash).toBe(retired.hash);
    expect(codec.encode(codec.decode(f.rec.state))).toBe(f.rec.state);
    // a today's state is never re-encoded into a retired span
    expect(() => codec.encode(f.snap.order.state)).toThrow(/no known span/);
  });

  it('resolves live with its custody, plans a cancel that validates in the script engine, and the signing screen describes it', async () => {
    const f = fixture();
    const chain = chainWith(f, f.span, 10_000n);
    const r = await resolveRecord(kob, chain, f.rec, testAddress);
    expect(r.status).toBe('live');
    expect(r.retired).toEqual({ templateHash: retired.hash, state: f.span });
    expect(r.custody).not.toBeNull();
    expect(r.custody!.state.amount).toBe('10000');
    const snap = await confirmSnapshotOnNode(kob, chain, snapshotFromRecord(r), testAddress);
    expect(snap.retired?.templateHash).toBe(retired.hash);
    const plan = planCancel(env, snap);
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    expect(plan.ok).toBe(true);
    expect(plan.built!.plans[0]).toMatchObject({ kind: 'retired', templateHash: retired.hash, state: f.span, entry: 'cancel' });
    signAndValidate(kob, plan.built!, [MAKER.sk]);
    const s = decodeSigning({ kob, built: plan.built!, maker: MAKER.pk, expected: plan.expected, nodeInputs: nodeFactsOf(plan.built!) });
    expect(s.blocking).toEqual([]);
    expect(s.warnings.map((w) => w.code)).toContain('retired-cancel');
    expect(s.inputs[0].order).toMatchObject({ kind: 'KobAsk', action: 'cancel', makerIsWallet: true });
    expect(s.inputs[0].order!.description).toMatchObject({ amountLeft: 10_000n, scale: 1_000n });
  });

  it('a partly filled retired order is found through its last proven state (never "spent" without it)', async () => {
    const f = fixture();
    const span2 = fixture('9').span; // lotsLeft 9: a custody of 9000
    const chain = chainWith(f, span2, 9_000n);
    // without a proven state the record cannot derive the new span: unknown, never spent (the order is still the maker's)
    expect((await resolveRecord(kob, chain, f.rec, testAddress)).status).toBe('unknown');
    const withLast: PlacementRecord = { ...f.rec, last: { state: span2, txid: 'ab'.repeat(32), index: 0, daa: '1000' } };
    const r = await resolveRecord(kob, chain, withLast, testAddress);
    expect(r.status).toBe('live');
    expect(r.retired?.state).toBe(span2);
    expect(r.custody!.state.amount).toBe('9000');
    const plan = planCancel(env, snapshotFromRecord(r));
    expect(plan.ok).toBe(true);
    signAndValidate(kob, plan.built!, [MAKER.sk]);
  });

  it('My orders: cancel only (no amend, no refund); backups keep the record; amend / refund refuse', async () => {
    const f = fixture();
    const r = await resolveRecord(kob, chainWith(f, f.span, 10_000n), f.rec, testAddress);
    const row = describeEntry(buildEntries([], [f.rec], new Map([[f.rec.covenantId, r]]))[0], { kob, clock: CLOCK });
    expect(row).toMatchObject({ live: true, canCancel: true, canAmend: false, canRefund: false, retiredTemplate: true, oldTemplate: false });
    const back = importBackup(exportBackup([{ ...f.rec, txid: null, output: null }], {}, 'testnet-10'), kob, { network: 'testnet-10', maker: MAKER.pk });
    expect(back.rejected).toEqual([]);
    const snap = snapshotFromRecord(r);
    expect(planRefund(env, snap).issues.map((i) => i.code)).toEqual(['cancel.retired-cancel-only']);
    expect(planCancelReplace(env, snap, { order: f.snap.order.state, value: 1n }).issues.map((i) => i.code)).toEqual(['cancel.retired-cancel-only']);
  });

  it('a template that is neither pinned nor retired stays "older contract version" (never spent, no cancel)', async () => {
    const f = fixture();
    const unknown = { ...f.rec, templateHash: 'aa'.repeat(32) };
    const r = await resolveRecord(kob, chainWith(f, f.span, 10_000n), unknown, testAddress);
    expect(r.status).toBe('old-template');
    const row = describeEntry(buildEntries([], [unknown], new Map([[unknown.covenantId, r]]))[0], { kob, clock: CLOCK });
    expect(row).toMatchObject({ oldTemplate: true, retiredTemplate: false, canCancel: false });
  });
});

describe('the retired cross limit templates (KobCross, KobCrossKron: replaced by the pair kinds)', () => {
  // a v2.6 cross limit state span, written field by field (32-byte pushes 0x20, 8-byte little-endian pushes 0x08): the cross limit has no encoder
  // in this build any more, only its retired templates decode it
  const push32 = (h: string) => '20' + h;
  const push8 = (v: bigint) => '08' + Buffer.from(new BigInt64Array([v]).buffer).toString('hex');
  const A = '70'.repeat(32);
  const B = '71'.repeat(32);
  const span = [
    push32(MAKER.pk), push32(A), push32('f4ac029d2c3c74dd3dcaeb64245f7d0a0977e27c2956f3977540a11dc7c45b1f'), push8(1n), push8(2977n),
    // unit, lotUnits, bFamily, B, its template, prefix / suffix, extension
    push8(1000n), push8(1n), push8(1n), push32(B), push32('f4ac029d2c3c74dd3dcaeb64245f7d0a0977e27c2956f3977540a11dc7c45b1f'), push8(1n), push8(2977n), push32('ee'.repeat(32)),
    // bLot, tipLot, tif, activeFrom, expiryDaa, refundTip, deliveryCarrier, bLotEnd, auctionDaa, lotsLeft
    push8(52_000n), push8(100n), push8(0n), push8(0n), push8(400_000_000n), push8(3_500_000n), push8(1_000_000_000n), push8(0n), push8(0n), push8(4n),
  ].join('');

  it('a record of a retired cross limit (either family) is kept, decodes through its retired template and describes as a pair sell order', () => {
    expect(span.length).toBe(351 * 2);
    for (const kindName of ['KobCross', 'KobCrossKron']) {
      const t = v26(kindName);
      expect(t).toMatchObject({ kind: 'KobCross', kindName, stateLen: 351 });
      const legacy = kob.decodeRetired(t.hash, span);
      expect(legacy).toMatchObject({ kind: 'KobCross', state: { maker: MAKER.pk, tokenCovId: A, unit: '1000', lotUnits: '1', bLot: '52000', lotsLeft: '4', bCovId: B } });
      const rec: PlacementRecord = {
        version: 1, network: 'testnet-10', maker: MAKER.pk, txid: null, output: null, covenantId: 'c1'.repeat(32), kind: kindName as PlacementRecord['kind'], templateHash: t.hash, state: span,
        custody: null, value: '2000000000', placedAtUnix: '1', placedAtDaa: '1',
      };
      expect(isPlacementRecord(rec)).toBe(true);
      expect(recordCodec(kob, rec)!.retired?.kindName).toBe(kindName);
      // the retired order: 4 lots of 1 x 1000 base units of A in custody; it is shown as a pair sell order of A for B, cancel only
      const d = describeLegacy(legacy);
      expect(d).toMatchObject({ kind: 'KobCross', side: 'sell', tokenCovId: A, amountLeft: 4000n, tokenAmount: 4000n, pair: { side: 'sell', base: { covId: A }, quote: { covId: B }, price: 52_000n } });
      expect(custodyAmountOf(legacy as unknown as OrderState)).toBe(4000n);
    }
  });
});

// The if-done entries retired on 2026-10-02 hold lot states of the same byte layout as today's: their spans decode as legacy states.
describe('gapOf (a retired span shorter than a wider encoding)', () => {
  it('finds an empty run for a same-length span and refuses a different one', () => {
    expect(gapOf('aabbcc', 'aabbcc')).toEqual({ at: 0, n: 0 });
    expect(gapOf('aabbcc', 'aabbdd')).toBeNull();
    expect(gapOf('aa11bbcc', 'aabbcc')).toEqual({ at: 2, n: 2 });
  });
});

describe('the retired protocol v3 cross limit (no lots)', () => {
  it('reads in its own layout: custody = amountLeft, prices per whole A, an older contract version', async () => {
    const { readFileSync } = await import('node:fs');
    const j = JSON.parse(readFileSync(new URL('../../../contracts/retired/KobCross-ea23f1fe.json', import.meta.url), 'utf8'));
    const c = j.contracts[0] ?? Object.values(j.contracts)[0];
    const { offset, len } = c.compiled.state_span as { offset: number; len: number };
    const span = Buffer.from((c.compiled.bytecode as number[]).slice(offset, offset + len)).toString('hex');
    const t = kob.retiredTemplates().find((x) => x.hash.startsWith('ea23f1fe'))!;
    expect(t).toMatchObject({ kind: 'KobCross', stateLen: 360 });
    const st = kob.decodeRetired(t.hash, span);
    expect(st.kind).toBe('KobCross');
    expect(isLegacyState(st)).toBe(true);
    const s = st.state as unknown as Record<string, string>;
    expect(retiredAmountLeft(st.state)).toBe(BigInt(s.amountLeft));
    const d = describeLegacy(st);
    expect(d.pair?.kind).toBe('KobCross');
    expect(d.scale).toBe(BigInt(s.scale) > 0n ? BigInt(s.scale) : 1n);
    expect(d.minFill).toBe(BigInt(s.minFill));
    expect(d.price).toBe(BigInt(s.price));
  });
});
