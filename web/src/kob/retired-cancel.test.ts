// Orders under a RETIRED template of the cancel-only table (kob-wasm `retiredTemplates`, docs/spec/template-retirement.md): the wallet's record
// of such an order is resolved on the node under the retired template's own script, its maker's cancel is built (alone or in a cancel-all),
// validated in the script engine and decoded as "Cancel an order under an older contract version (cancel only)"; every other planner refuses it
// and the decoder refuses any other spend of it.
import { describe, expect, it } from 'vitest';
import { planCancel, planCancelAll, planCancelReplace, planRefund, planSweep, snapshotFromRecord, type CancelEnv, type OrderSnapshot } from './cancel';
import { decodeSigning } from './decode';
import { recordCodec, recordsFromBuilt, resolveRecord, retiredOf, type PlacementRecord } from './records';
import type { BuiltTx, Hex, OrderState } from './types';
import { loadKobNode } from './wasm.node';
import { summarize } from '../signer/summary';
import { FakeChain, MAKER, keyUtxo, nodeFactsOf, placeGolden, signAndValidate, strayFor, testAddress } from '../testing/chain-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
const opts = { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_790_000_000n, placedAtDaa: 1000n };
const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 78_000_000n }, ...over });
const retiredHash = (label: string): Hex => {
  const r = kob.retiredTemplates().find((x) => x.label === label);
  if (!r) throw new Error(`no retired template ${label}`);
  return r.hash;
};
const BID = 'KobBid b995661f';
const COND_ASK_F1 = 'KobCondAsk 02a34664';

/**
 * A placed golden order as the wallet recorded it under the retired template `label` (same kind and state layout), its order UTXO on the fake
 * node at the retired template's script (outpoint `n`), its custody (if any) where the placement put it.
 */
function retiredOrder(golden: string, label: string, n = 0x5e) {
  const p = placeGolden(kob, golden);
  const rec0 = recordsFromBuilt(kob, p.built, opts)[0]!;
  const rec: PlacementRecord = { ...rec0, templateHash: retiredHash(label) };
  const state = kob.decodeState(rec.kind as OrderState['kind'], rec.state) as OrderState;
  const chain = new FakeChain();
  chain.applyTx(p.signed.tx);
  const spk = kob.retiredScriptPublicKey(rec.templateHash, state);
  chain.add({ transactionId: n.toString(16).padStart(2, '0').repeat(32), index: 0, amount: rec.value, scriptPublicKey: spk, covenantId: rec.covenantId });
  return { p, rec, state, chain, spk };
}

async function snapshotOf(golden: string, label: string, n?: number): Promise<{ snap: OrderSnapshot; rec: PlacementRecord; spk: string }> {
  const { rec, chain, spk } = retiredOrder(golden, label, n);
  const r = await resolveRecord(kob, chain, rec, testAddress);
  expect(r.status).toBe('live');
  expect(r.retired).toBe(rec.templateHash);
  return { snap: snapshotFromRecord(r), rec, spk };
}

describe('the cancel-only table in kob-wasm', () => {
  it('lists the retired templates of template-retirement.md, each with the cancel entry only', () => {
    const t = kob.retiredTemplates();
    expect(t).toHaveLength(31);
    for (const label of [BID, 'KobBidKron 6ec1a3dd', 'KobBidKronD0 436fbbbb', 'KobCondBid 26042aff', COND_ASK_F1]) expect(t.map((x) => x.label)).toContain(label);
    expect(t.every((x) => x.entries.length === 1 && x.entries[0] === 'cancel')).toBe(true);
    // none is a template this build pins
    const pinned = new Set(kob.templates().map((x) => x.hash));
    expect(t.some((x) => pinned.has(x.hash))).toBe(false);
  });

  it('derives the retired script of a state, never today\'s, and refuses a hash outside the table or of another kind', () => {
    const { state, spk } = retiredOrder('create.bid', BID);
    expect(spk).not.toBe(kob.scriptPublicKey(state));
    expect(() => kob.retiredScriptPublicKey('5a'.repeat(32), state)).toThrow();
    expect(() => kob.retiredScriptPublicKey(retiredHash(COND_ASK_F1), state)).toThrow();
  });
});

describe('records of an order under a retired template', () => {
  it('decode with today\'s codec and resolve live under the retired script', async () => {
    const { rec, chain, spk } = retiredOrder('create.bid', BID);
    const codec = recordCodec(kob, rec)!;
    expect(codec.retired).toBe(rec.templateHash);
    expect(codec.spanSpk(rec.state)).toBe(spk);
    expect(retiredOf(kob, rec)?.label).toBe(BID);
    const r = await resolveRecord(kob, chain, rec, testAddress);
    expect(r).toMatchObject({ status: 'live', retired: rec.templateHash });
    expect(r.order!.transactionId).toBe('5e'.repeat(32));
  });

  it('a template neither pinned nor in the table stays old-template', async () => {
    const { rec, chain } = retiredOrder('create.bid', BID);
    const unknown = { ...rec, templateHash: '5a'.repeat(32) };
    expect(recordCodec(kob, unknown)).toBeNull();
    expect((await resolveRecord(kob, chain, unknown, testAddress)).status).toBe('old-template');
  });
});

/** Engine-validates a plan's tx and decodes it as the signer frame does. */
function checkCancel(built: BuiltTx, expected?: Parameters<typeof decodeSigning>[0]['expected']) {
  signAndValidate(kob, built, [MAKER.sk]);
  const s = decodeSigning({ kob, built, maker: MAKER.pk, registry: null, expected, nodeInputs: nodeFactsOf(built) });
  expect(s.blocking).toEqual([]);
  return s;
}

describe('the maker\'s cancel of an order under a retired template', () => {
  it('a bid: its retired cancel validates in the engine and the signer says what returns to the wallet', async () => {
    const { snap, spk } = await snapshotOf('create.bid', BID);
    const plan = planCancel(env(), snap);
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(plan.request).toMatchObject({ action: 'cancelOrder', retired: snap.retired });
    const built = plan.built!;
    expect(built.plans[0]).toMatchObject({ kind: 'retiredCancel', template: 'KobBid', retired: snap.retired, maker: MAKER.pk });
    expect(built.tx.inputs[0].utxo.scriptPublicKey).toBe(spk);
    const s = checkCancel(built, plan.expected);
    expect(s.kind).toBe('cancel');
    expect(s.spends[0]).toMatchObject({ action: 'cancel', retired: snap.retired, makerIsWallet: true });
    expect(s.net.kas.walletDelta).toBeGreaterThan(0n);
    const sum = summarize(kob, built, MAKER.pk, []);
    expect(sum.title).toBe('Cancel an order under an older contract version (cancel only)');
    expect(sum.lines.map((l) => l.label)).toContain('Closes order (older contract version)');
    expect(sum.orders).toEqual([]);
    expect(sum.problems).toBe(0);
  });

  it('a conditional ask of the F1 generation: its custody and strays return to the maker', async () => {
    const { snap } = await snapshotOf('create.condAsk', COND_ASK_F1);
    expect(snap.custody).not.toBeNull();
    const plan = planCancel(env(), { ...snap, strays: [strayFor(snap, 7n, 91)] });
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    const s = checkCancel(plan.built!, plan.expected);
    expect(plan.tokensReturned).toBe(BigInt(snap.custody!.state.amount) + 7n);
    expect(s.net.tokens.reduce((a, t) => a + t.toMaker, 0n)).toBe(plan.tokensReturned);
  });

  it('cancel all takes several of them in one transaction', async () => {
    const a = (await snapshotOf('create.bid', BID, 0x61)).snap;
    const b = (await snapshotOf('create.bid', BID, 0x62)).snap;
    const two = { ...b, covenantId: 'c2'.repeat(32), order: { ...b.order, covenantId: 'c2'.repeat(32) } };
    const plans = planCancelAll(env(), [a, two]);
    expect(plans).toHaveLength(1);
    expect(plans[0]!.ok, JSON.stringify(plans[0]!.issues)).toBe(true);
    expect(plans[0]!.request).toMatchObject({ action: 'cancelPosition' });
    expect(plans[0]!.built!.plans.filter((p) => p.kind === 'retiredCancel')).toHaveLength(2);
    const s = checkCancel(plans[0]!.built!, plans[0]!.expected);
    expect(s.kind).toBe('cancel-position');
    expect(summarize(kob, plans[0]!.built!, MAKER.pk, []).title).toBe('Cancel an order under an older contract version (cancel only)');
  });

  it('nothing else is planned for it: no replacement, sweep or refund', async () => {
    const { snap, rec } = await snapshotOf('create.bid', BID);
    const st = kob.decodeState('KobBid', rec.state) as OrderState;
    for (const plan of [planCancelReplace(env(), snap, { order: st, value: BigInt(rec.value) }), planSweep(env(), { ...snap, strays: [strayFor(snap, 1n, 93)] }), planRefund(env(), snap)]) {
      expect(plan.ok).toBe(false);
      expect(plan.issues.map((i) => i.code)).toContain('cancel.retired-cancel-only');
    }
  });

  it('the decoder refuses any other spend of it, and a plan whose script is not the spent one', async () => {
    const { snap } = await snapshotOf('create.bid', BID);
    const built = planCancel(env(), snap).built!;
    const asFill: BuiltTx = { ...built, roles: built.roles.map((r, i) => (i === 0 ? r.replace('.cancel', '.fill') : r)) };
    const s = decodeSigning({ kob, built: asFill, maker: MAKER.pk, registry: null, nodeInputs: nodeFactsOf(asFill) });
    expect(s.blocking.map((b) => b.code)).toContain('retired-not-cancel');
    // another retired template of the same kind (none here) or of another kind: its script derives nothing that matches
    const p0 = built.plans[0]!;
    if (p0.kind !== 'retiredCancel') throw new Error('not a retired cancel');
    const other: BuiltTx = { ...built, plans: [{ ...p0, retired: retiredHash(COND_ASK_F1) }, ...built.plans.slice(1)] };
    const s2 = decodeSigning({ kob, built: other, maker: MAKER.pk, registry: null, nodeInputs: nodeFactsOf(other) });
    expect(s2.blocking.map((b) => b.code)).toContain('input-script-mismatch');
  });
});
