// A Collected (mode-1) bid holding its bought-custody (bid-delivery-spec §5, §7): claim, claim and merge, claim and sell, and the cancel /
// refund that carry the custody back. Every plan is signed and validated in the script engine. Each rule has a negative case.
import { describe, expect, it } from 'vitest';
import { claimMergeRoom, planCancel, planCancelAll, planCancelReplace, planClaim, planRefund, snapshotFromOrderView, type CancelEnv, type OrderSnapshot } from './cancel';
import { boughtOf } from './bought';
import { escrowedBalances, withFreeBalances } from './balances';
import type { OrderState, TokenUtxo } from './types';
import { loadKobNode } from './wasm.node';
import { FakeChain, MAKER, OTHER, TOKEN, keyUtxo, orderViewOf, placeGolden, signAndValidate, strayFor, testAddress, tokenUtxo, tokenUtxoView } from '../testing/chain-fixtures';
import { confirmSnapshotOnNode } from './node-verify';
import { decodeSigning } from './decode';
import { parseRegistry } from './registry';
import type { CancelPlan } from './cancel';
import { nodeFactsOf, tradableRegistryJson } from '../testing/chain-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
const registry = parseRegistry(tradableRegistryJson(), { kob });
const BOUGHT = 4_000n;

const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({
  kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201), keyUtxo(MAKER.pk, 3n * KAS, 202)], tokenUtxos: [], clock: { daa: 78_000_000n }, ...over,
});

/** The golden bid as a Collected order that bought `bought` (its custody owned by the order id, exactly `bought`). */
function collected(bought = BOUGHT): { snap: OrderSnapshot; custody: TokenUtxo } {
  const base = placeGolden(kob, 'create.bid').snapshots[0]!;
  const state = { ...base.order.state, state: { ...base.order.state.state, mode: '1', bought: bought.toString() } } as OrderState;
  const snap0: OrderSnapshot = { ...base, order: { ...base.order, state } };
  const custody = strayFor(snap0, bought, 91, 10n * KAS);
  return { snap: { ...snap0, boughtCustody: custody }, custody };
}
/** The pre-sign decoder (W3) accepts the plan as its own expectations say: nothing blocking. */
function expectSigns(plan: CancelPlan) {
  const s = decodeSigning({ kob, built: plan.built!, maker: MAKER.pk, registry, expected: plan.expected, nodeInputs: nodeFactsOf(plan.built!) });
  expect(s.blocking.map((b) => `${b.code}: ${b.message}`)).toEqual([]);
}
const spends = (built: { tx: { inputs: { transactionId: string; index: number }[] } }, u: TokenUtxo) => built.tx.inputs.some((i) => i.transactionId === u.transactionId && i.index === u.index);

describe('bought-custody facts', () => {
  it('boughtOf names the bid token and amount in mode 1 only (negative: mode 0 or bought 0 hold none)', () => {
    const { snap } = collected();
    expect(boughtOf(snap.order.state)).toMatchObject({ token: TOKEN.covenantId, amount: BOUGHT, ext: TOKEN.ext });
    const s = snap.order.state;
    expect(boughtOf({ ...s, state: { ...s.state, mode: '0' } } as OrderState)).toBeNull();
    expect(boughtOf({ ...s, state: { ...s.state, bought: '0' } } as OrderState)).toBeNull();
  });

  it("a bid view's custody is its bought-custody: the snapshot keeps it apart from sell custody (negative: read as custody)", () => {
    const { snap, custody } = collected();
    const view = { ...orderViewOf(kob, snap), delivery_mode: 1, bought: BOUGHT.toString() };
    const cv = { expected_amount: BOUGHT.toString(), utxo: tokenUtxoView(custody, 'custody', { role: 'bought' }), ok: true };
    const fromBoth = snapshotFromOrderView({ ...view, custody: cv, bought_custody: cv });
    expect(fromBoth.custody).toBeNull();
    expect(fromBoth.boughtCustody).toMatchObject({ transactionId: custody.transactionId, index: custody.index });
    // an older view with only `custody` still finds it
    expect(snapshotFromOrderView({ ...view, custody: cv }).boughtCustody?.index).toBe(custody.index);
  });

  it('balances count it as the maker\'s bought tokens, not as sell custody (negative: the sum of both)', () => {
    const { snap, custody } = collected();
    const view = { ...orderViewOf(kob, snap), delivery_mode: 1, bought: BOUGHT.toString() };
    const cv = { expected_amount: BOUGHT.toString(), utxo: tokenUtxoView(custody, 'custody', { role: 'bought' }), ok: true };
    for (const input of [{ ...view, custody: cv, bought_custody: cv }, snap]) {
      const [e] = escrowedBalances([input]);
      expect(e).toMatchObject({ token: TOKEN.covenantId, custody: 0n, bought: BOUGHT, boughtOrders: 1 });
      // the custody's KAS is locked with the order
      expect(e!.kasCarriers).toBeGreaterThanOrEqual(10n * KAS);
    }
    const [b] = withFreeBalances(escrowedBalances([snap]), [tokenUtxo(700n, MAKER.pk)], MAKER.pk);
    expect(b).toMatchObject({ free: 700n, escrowed: 0n, bought: BOUGHT, boughtOrders: 1 });
  });
});

describe('planClaim (CL1 to CL3)', () => {
  it('claim: the order continues with bought = 0 and one more carrier, the tokens reach the wallet as one plain UTXO', () => {
    const { snap, custody } = collected();
    const plan = planClaim(env(), snap);
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    signAndValidate(kob, plan.built!);
    expect(plan.request!.action).toBe('claimOrder');
    expect(spends(plan.built!, custody)).toBe(true);
    const cont = plan.built!.tx.outputs.find((o) => o.covenant?.covenantId === snap.covenantId)!;
    expect(BigInt(cont.value)).toBe(BigInt(snap.order.amount) + 10n * KAS);
    expect(plan.claim).toMatchObject({ claimed: BOUGHT, toWallet: BOUGHT, merged: 0n, placed: 0n, carrier: 10n * KAS, ends: false });
    expect(plan.claim!.fee).toBe(BigInt(plan.built!.fee.fee));
    expect(plan.expected).toMatchObject({ claimIds: [snap.covenantId], cancelIds: [], orders: [] });
    expectSigns(plan);
  });

  it('claim and merge takes the wallet UTXOs up to the program\'s inputs (negative: one more is refused)', () => {
    const { snap } = collected();
    const room = claimMergeRoom(kob, snap.order.state);
    expect(room).toBeGreaterThan(0);
    const wallet = Array.from({ length: room + 1 }, (_, k) => tokenUtxo(100n + BigInt(k), MAKER.pk, 160 + k));
    const plan = planClaim(env(), snap, { merge: wallet.slice(0, room) });
    expect(plan.ok).toBe(true);
    signAndValidate(kob, plan.built!);
    expectSigns(plan);
    expect(plan.claim).toMatchObject({ mergedUtxos: room, toWallet: BOUGHT + wallet.slice(0, room).reduce((a, u) => a + BigInt(u.state.amount), 0n) });
    const over = planClaim(env(), snap, { merge: wallet });
    expect(over.ok).toBe(false);
    expect(over.issues.map((i) => i.code)).toContain('claim.merge-too-many');
    // another key's UTXO never joins
    expect(planClaim(env(), snap, { merge: [tokenUtxo(5n, OTHER.pk, 170)] }).issues.map((i) => i.code)).toContain('claim.merge-not-plain');
  });

  it('claim and sell places an ask on the claimed tokens in the same transaction; with end the bid ends', () => {
    const { snap } = collected();
    const st = snap.order.state.state as unknown as Record<string, string>;
    const ask = {
      kind: 'KobAsk', state: {
        maker: MAKER.pk, tokenCovId: st.tokenCovId, tokenTplHash: st.tokenTplHash, tplPrefixLen: st.tplPrefixLen, tplSuffixLen: st.tplSuffixLen, scale: st.scale,
        minFill: '1000', price: (BigInt(st.price) * 2n).toString(), tip: '0', tif: '0', activeFrom: '0', expiryDaa: st.expiryDaa, refundTip: st.refundTip,
        interval: '0', maxFill: '0', slope: '0', priceEnd: '0', decayStep: '1', amountLeft: BOUGHT.toString(), extensionCommitment: st.extensionCommitment,
      },
    } as unknown as OrderState;
    for (const end of [false, true]) {
      const plan = planClaim(env(), snap, { place: { order: ask, value: KAS }, end });
      expect(plan.issues.filter((i) => i.severity === 'error'), String(end)).toEqual([]);
      signAndValidate(kob, plan.built!);
      expect(plan.claim).toMatchObject({ placed: BOUGHT, toWallet: 0n, ends: end });
      expect(plan.built!.tx.outputs.some((o) => o.covenant?.covenantId === snap.covenantId)).toBe(!end);
      expect(plan.expected.tokensEscrowed).toBe(BOUGHT);
      expectSigns(plan);
    }
    // negative: end without a placement is a cancel
    expect(planClaim(env(), snap, { end: true }).issues.map((i) => i.code)).toContain('claim.end-without-place');
  });

  it('refuses without the bought-custody, without bought tokens, without funding, or for another maker', () => {
    const { snap } = collected();
    expect(planClaim(env(), { ...snap, boughtCustody: null }).issues.map((i) => i.code)).toEqual(['cancel.bought-missing']);
    const zero = { ...snap, order: { ...snap.order, state: { ...snap.order.state, state: { ...snap.order.state.state, bought: '0' } } as OrderState } };
    expect(planClaim(env(), zero).issues.map((i) => i.code)).toEqual(['claim.nothing']);
    expect(planClaim(env({ funding: [] }), snap).issues.map((i) => i.code)).toEqual(['claim.needs-funding']);
    expect(planClaim(env({ maker: OTHER.pk }), snap).issues.map((i) => i.code)).toEqual(['cancel.not-maker']);
  });
});

describe('cancel, refund and replace carry the bought-custody back', () => {
  it('cancel: the custody is an input and its tokens return (negative: without it the planner refuses, never loses them)', () => {
    const { snap, custody } = collected();
    const plan = planCancel(env(), snap);
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    signAndValidate(kob, plan.built!);
    expect(spends(plan.built!, custody)).toBe(true);
    expect(plan.tokensReturned).toBe(BOUGHT);
    expect(plan.kasReleased).toBe(BigInt(snap.order.amount) + 10n * KAS);
    expectSigns(plan);
    const lost = planCancel(env(), { ...snap, boughtCustody: null });
    expect(lost.ok).toBe(false);
    expect(lost.issues.map((i) => i.code)).toContain('cancel.bought-missing');
  });

  it('a position cancel of a Collected bid and another bid returns the custody in one transaction; a cancel-replace returns it and the new bid starts at 0', () => {
    const a = collected();
    const b = placeGolden(kob, 'create.condBid').snapshots[0]!;
    const plans = planCancelAll(env(), [a.snap, b]);
    expect(plans.flatMap((p) => p.issues.filter((i) => i.severity === 'error'))).toEqual([]);
    for (const p of plans) signAndValidate(kob, p.built!);
    expect(plans.map((p) => p.request!.action)).toEqual(['cancelPosition']);
    expect(plans.reduce((s, p) => s + p.tokensReturned, 0n)).toBe(BOUGHT);
    const st = a.snap.order.state;
    const rep = planCancelReplace(env(), a.snap, { order: { ...st, state: { ...st.state, price: (BigInt((st.state as { price: string }).price) - 1000n).toString() } } as OrderState, value: BigInt(a.snap.order.amount) }, undefined, { noInPlace: true });
    expect(rep.issues.filter((i) => i.severity === 'error')).toEqual([]);
    signAndValidate(kob, rep.built!);
    expect(rep.tokensReturned).toBe(BOUGHT);
    expect(rep.expected.orders?.[0]?.state).toMatchObject({ bought: '0' });
    expectSigns(rep);
    for (const p of plans) expectSigns(p);
  });

  it('refund releases it to the maker (negative: without it the planner refuses)', () => {
    const { snap, custody } = collected();
    const due = { ...snap, order: { ...snap.order, blockDaaScore: '0' } };
    const plan = planRefund(env(), due);
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    signAndValidate(kob, plan.built!, []);
    expect(spends(plan.built!, custody)).toBe(true);
    expect(plan.tokensReturned).toBe(BOUGHT);
    expectSigns(plan);
    expect(planRefund(env(), { ...due, boughtCustody: null }).issues.map((i) => i.code)).toContain('cancel.bought-missing');
  });
});

describe('the node confirms the bought-custody', () => {
  it('a snapshot keeps it only when the node holds it at its script (negative: a forged outpoint is a node-mismatch)', async () => {
    const { snap, custody } = collected();
    const chain = new FakeChain();
    chain.add({ transactionId: snap.order.transactionId, index: snap.order.index, amount: snap.order.amount, scriptPublicKey: kob.scriptPublicKey(snap.order.state), covenantId: snap.covenantId });
    chain.add({ transactionId: custody.transactionId, index: custody.index, amount: custody.amount, scriptPublicKey: kob.tokenScriptPublicKey('KCC20Ref', custody.state), covenantId: custody.covenantId });
    const ok = await confirmSnapshotOnNode(kob, chain, snap, testAddress);
    expect(ok.boughtCustody).toMatchObject({ transactionId: custody.transactionId, amount: custody.amount, covenantId: TOKEN.covenantId });
    await expect(confirmSnapshotOnNode(kob, chain, { ...snap, boughtCustody: { ...custody, index: 77 } }, testAddress)).rejects.toThrow(/bought-custody/);
  });
});
