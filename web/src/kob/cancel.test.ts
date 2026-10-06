// Every builder result is checked in the script engine: sign locally, finalize with tightened budgets, validate (consensus rules).
import { describe, expect, it } from 'vitest';
import {
  SnapshotError, planCancel, planCancelAll, planCancelReplace, planRefund, snapshotFromItem, snapshotFromOrderView, snapshotFromRecord, tokenSlotsFor,
  type CancelEnv, type CancelPlan, type OrderSnapshot,
} from './cancel';
import { decodeSigning } from './decode';
import { parseRegistry } from './registry';
import type { ResolvedRecord } from './records';
import type { CancelPositionRequest, Hex, OrderState } from './types';
import { loadKobNode } from './wasm.node';
import { familyOfKind, isKind } from './order-facts';
import { custodyState } from './token-state';
import {
  MAKER, OTHER, TOKEN, ZERO32, goldenRequest, keyUtxo, orderViewOf, placeGolden, signAndValidate, strayFor, tokenState, tokenUtxo, tokenUtxoView, tradableRegistryJson,
  nodeFactsOf,
} from '../testing/chain-fixtures';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({
  kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201), keyUtxo(MAKER.pk, 3n * KAS, 202)], tokenUtxos: [], clock: { daa: 78_000_000n }, ...over,
});
const snapOf = (name: string): OrderSnapshot => placeGolden(kob, name).snapshots[0];

/** Consensus-check a plan and run the pre-sign decoder over it with the plan's own expectations. */
function verify(plan: CancelPlan, label: string, keys: Hex[] = [MAKER.sk], reg: typeof registry | null = registry) {
  expect(plan.issues.filter((i) => i.severity === 'error'), label).toEqual([]);
  expect(plan.ok, label).toBe(true);
  const built = plan.built!;
  signAndValidate(kob, built, keys);
  const s = decodeSigning({ kob, built, maker: MAKER.pk, registry: reg, expected: plan.expected, nodeInputs: nodeFactsOf(built) });
  expect(s.blocking, label).toEqual([]);
  return s;
}

const KCC20_KINDS = ['create.ask', 'create.bid', 'create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdAsk', 'create.ifdBid.repeat', 'create.ifdAsk.repeat'];
const KINDS = [...KCC20_KINDS, ...KCC20_KINDS.map((k) => `kron.${k}`)];

describe('planCancel: one order, custody and strays swept in the same tx', () => {
  it.each(KINDS)('%s: cancels, validates in the script engine and decodes to "everything returns to the maker"', (name) => {
    const snap = snapOf(name);
    const plan = planCancel(env(), snap);
    const s = verify(plan, name);
    expect(plan.cancelIds).toEqual([snap.covenantId]);
    expect(plan.fundingUsed).toEqual([]); // the released carriers pay the fee: nothing extra to sign
    expect(plan.request!.action).toBe('cancelOrder');
    expect(s.kind).toBe('cancel');
    expect(s.spends).toHaveLength(1);
    expect(s.net.kas.toOthers).toBe(0n);
    const custody = snap.custody ? BigInt(snap.custody.state.amount) : 0n;
    expect(plan.tokensReturned).toBe(custody);
    expect(s.net.tokens.reduce((a, t) => a + t.toMaker, 0n)).toBe(custody);
    expect(plan.kasReleased).toBe(BigInt(snap.order.amount) + (snap.custody ? BigInt(snap.custody.amount) : 0n));
    // only the maker signs
    expect(plan.built!.sign.every((x) => x.pubkey === MAKER.pk)).toBe(true);
  });

  it('sweeps strays (tokens sent to the order from outside) together with the custody', () => {
    for (const name of ['create.ask', 'create.condAsk', 'create.ifdAsk']) {
      const snap = snapOf(name);
      const withStrays = { ...snap, strays: [strayFor(snap, 7n, 91), strayFor(snap, 5n, 92)] };
      const plan = planCancel(env(), withStrays);
      const s = verify(plan, name);
      expect(plan.tokensReturned, name).toBe(BigInt(snap.custody!.state.amount) + 12n);
      expect(s.spends[0], name).toMatchObject({ strays: 12n });
      expect(s.info.map((i) => i.code), name).toContain('strays-swept');
    }
    // a bid owns no tokens: every token utxo owned by its id is a stray and cancel is the only way out
    const bid = snapOf('create.bid');
    const plan = planCancel(env(), { ...bid, strays: [strayFor(bid, 42n, 93)] });
    const s = verify(plan, 'bid');
    expect(plan.tokensReturned).toBe(42n);
    expect(s.net.tokens[0]).toMatchObject({ released: 42n, toMaker: 42n });
  });

  it('refuses when strays exceed the token program slots, unless the caller accepts abandoning the smallest', () => {
    const snap = snapOf('create.ask'); // 3/3 program: custody + 2 strays fit, a third does not
    const strays = [strayFor(snap, 1n, 91), strayFor(snap, 9n, 92), strayFor(snap, 5n, 93)];
    const plan = planCancel(env(), { ...snap, strays });
    expect(plan.ok).toBe(false);
    expect(plan.issues.map((i) => i.code)).toEqual(['cancel.strays-exceed-slots']);
    const forced = planCancel(env(), { ...snap, strays }, { allowAbandonStrays: true });
    const s = verify(forced, 'forced');
    expect(forced.issues.map((i) => i.code)).toEqual(['cancel.strays-abandoned']);
    expect(forced.tokensReturned).toBe(BigInt(snap.custody!.state.amount) + 9n + 5n); // the smallest stray is the one abandoned
    expect(s.spends[0].strays).toBe(14n);
    expect(tokenSlotsFor(kob, TOKEN.templateHash)).toEqual({ inputs: 3, outputs: 3 });
  });

  it('leaves a stray with another extension commitment behind and says so', () => {
    const snap = snapOf('create.ask');
    const odd = { ...strayFor(snap, 3n, 94), state: tokenState(3n, snap.covenantId, 4, 'dd'.repeat(32)) };
    const plan = planCancel(env(), { ...snap, strays: [odd] });
    verify(plan, 'other-ext');
    expect(plan.issues.map((i) => i.code)).toEqual(['cancel.stray-other-extension']);
    expect(plan.tokensReturned).toBe(BigInt(snap.custody!.state.amount));
  });

  it('adds a maker funding UTXO only when the released KAS cannot pay the fee', () => {
    const bid = snapOf('create.bid');
    const dust = { ...bid, order: { ...bid.order, amount: '1000' } };
    const plan = planCancel(env(), dust);
    verify(plan, 'dust bid');
    expect(plan.fundingUsed).toHaveLength(1);
    expect(plan.fundingUsed[0].amount).toBe(String(3n * KAS)); // the smallest UTXO that covers the shortfall
    expect(plan.issues.map((i) => i.code)).toEqual(['cancel.funding-added']);
    expect(plan.built!.sign.map((x) => x.inputIndex).sort()).toEqual([0, 1]); // order cancel + funding input
    const broke = planCancel(env({ funding: [] }), dust);
    expect(broke.ok).toBe(false);
    expect(broke.issues.map((i) => i.code)).toEqual(['cancel.insufficient-funds']);
    // only the maker's own UTXOs are ever used
    const foreign = planCancel(env({ funding: [keyUtxo(OTHER.pk, 100n * KAS, 210)] }), dust);
    expect(foreign.ok).toBe(false);
  });

  it('refuses another key\'s order and an ask without its custody', () => {
    const snap = snapOf('create.ask');
    const theirs = planCancel(env({ maker: OTHER.pk }), snap);
    expect(theirs.ok).toBe(false);
    expect(theirs.issues.map((i) => i.code)).toEqual(['cancel.not-maker']);
    const noCustody = planCancel(env(), { ...snap, custody: null });
    expect(noCustody.issues.map((i) => i.code)).toEqual(['cancel.custody-missing']);
  });

  it('a custody that does not hold exactly amountLeft is rejected by kob-wasm and surfaced', () => {
    const snap = snapOf('create.ask');
    const wrong = { ...snap, custody: { ...snap.custody!, state: { ...snap.custody!.state, amount: '9999' } } };
    const plan = planCancel(env(), wrong);
    expect(plan.ok).toBe(false);
    expect(plan.issues[0].code).toBe('cancel.build-failed');
    expect(plan.issues[0].message).toMatch(/custody holds 9999/);
  });

  it('honours a custom change key and fee rate', () => {
    const snap = snapOf('create.bid');
    const plan = planCancel(env({ changeTo: OTHER.pk, feeRate: 200n }), snap);
    verify(plan, 'custom');
    expect(BigInt(plan.built!.fee.feeRate)).toBe(200n);
    const change = plan.built!.tx.outputs[plan.built!.fee.changeOutput!];
    expect(change.scriptPublicKey).toBe(`000020${OTHER.pk}ac`);
  });
});

describe('planCancelReplace: one atomic tx', () => {
  const same = (s: OrderSnapshot, patch: (st: any) => void): OrderState => {
    const o = clone(s.order.state);
    patch(o.state);
    return o;
  };

  it('amends an ask (new price and a smaller amount): custody re-escrowed, the rest returns', () => {
    const snap = snapOf('create.ask');
    const next = same(snap, (st) => { st.price = '260000000'; st.amountLeft = '6000'; });
    const plan = planCancelReplace(env(), snap, { order: next, value: 10n * KAS });
    const s = verify(plan, 'amend');
    expect(s.kind).toBe('cancel-replace');
    expect(s.orders[0].description).toMatchObject({ price: 260_000_000n, amountLeft: 6_000n });
    expect(s.net.tokens[0]).toMatchObject({ released: 10_000n, escrowed: 6_000n, toMaker: 4_000n });
    expect(plan.tokensReturned).toBe(4_000n);
    expect(plan.expected.tokensEscrowed).toBe(6_000n);
    expect(plan.expected.kasLocked).toBe(20n * KAS);
    // the same replacement re-signed locally is a real, consensus-valid tx and its placement record recovers
    const signed = signAndValidate(kob, plan.built!);
    expect(kob.recoverOrders(signed.tx)).toHaveLength(1);
  });

  it('amends a plain ask IN PLACE when the scale and amount stay: the custody never moves, the carrier pays the fee', () => {
    for (const name of ['create.ask', 'kron.create.ask']) {
      const snap = snapOf(name);
      const next = same(snap, (st) => { st.price = '260000000'; st.tip = '200000'; });
      const plan = planCancelReplace(env(), snap, { order: next, value: 10n * KAS });
      // the KRON fixture token shares its covenant id with the registered KCC-20 test token: decode it without the registry
      const reg = name.startsWith('kron.') ? null : registry;
      const s = verify(plan, name, [MAKER.sk], reg);
      expect(plan.request!.action, name).toBe('amendOrder');
      expect(plan.issues.map((i) => i.code)).toContain('cancel.amend-in-place');
      expect(plan.fundingUsed).toEqual([]);
      const tx = plan.built!.tx;
      expect(tx.inputs).toHaveLength(1);
      expect(tx.outputs).toHaveLength(1);
      expect(tx.outputs[0].covenant).toEqual({ authorizingInput: 0, covenantId: snap.covenantId });
      expect(BigInt(tx.outputs[0].value)).toBe(BigInt(snap.order.amount) - BigInt(plan.built!.fee.fee));
      expect(s.kind).toBe('cancel-replace');
      expect(s.orders).toHaveLength(1);
      expect(s.orders[0]).toMatchObject({ covenantId: snap.covenantId, amendedFrom: 0, custody: null, verified: true });
      expect(s.orders[0].description).toMatchObject({ price: 260_000_000n, amountLeft: 10_000n });
      expect(s.net.tokens).toEqual([]);
      expect(plan.tokensReturned).toBe(0n);
      expect(plan.kasReleased).toBe(BigInt(snap.order.amount));
      // signed, the amend recovers from its own signature scripts
      const signed = signAndValidate(kob, plan.built!);
      expect(kob.recoverAmends(signed.tx).map((a) => a.covenantId)).toEqual([snap.covenantId]);
      expect(kob.recoverOrders(signed.tx)).toEqual([]);
      // the same amendment as a cancel-replace re-escrows the custody (a token transfer) and costs more than twice as much
      const cr = planCancelReplace(env(), snap, { order: next, value: 10n * KAS }, undefined, { noInPlace: true });
      verify(cr, `${name}: cancel-replace`, [MAKER.sk], reg);
      expect(cr.request!.action).toBe('cancelOrder');
      expect(BigInt(plan.built!.fee.fee) * 2n).toBeLessThan(BigInt(cr.built!.fee.fee));
    }
  });

  it('keeps the cancel-replace where an in-place amend cannot: strays to sweep, another amount or scale, another kind', () => {
    const snap = snapOf('create.ask');
    const next = same(snap, (st) => { st.price = '260000000'; });
    const strays = planCancelReplace(env(), { ...snap, strays: [strayFor(snap, 7n, 94)] }, { order: next, value: 10n * KAS });
    verify(strays, 'strays');
    expect(strays.request!.action).toBe('cancelOrder');
    expect(strays.tokensReturned).toBe(7n);
    for (const patch of [(st: any) => { st.amountLeft = '9000'; }, (st: any) => { st.scale = '100'; st.amountLeft = '5000'; }]) {
      const plan = planCancelReplace(env(), snap, { order: same(snap, patch), value: 10n * KAS });
      verify(plan, 'quantity');
      expect(plan.request!.action).toBe('cancelOrder');
    }
    // a plain bid keeping its token and scale is amended IN PLACE (it owns no custody); with strays to sweep it stays a cancel-replace
    const bid = snapOf('create.bid');
    const b = planCancelReplace(env(), bid, { order: same(bid, (st) => { st.price = '230000000'; }), value: BigInt(bid.order.amount) });
    verify(b, 'bid');
    expect(b.request!.action).toBe('amendOrder');
    expect(b.issues.map((i) => i.code)).toContain('cancel.amend-in-place-bid');
    expect(b.built!.tx.outputs[0]!.covenant?.covenantId).toBe(bid.covenantId);
    const bs = planCancelReplace(env(), { ...bid, strays: [strayFor(bid, 3n, 95)] }, { order: same(bid, (st) => { st.price = '230000000'; }), value: BigInt(bid.order.amount) });
    verify(bs, 'bid with strays');
    expect(bs.request!.action).toBe('cancelOrder');
    const rescaled = planCancelReplace(env(), bid, { order: same(bid, (st) => { st.scale = '100'; }), value: BigInt(bid.order.amount) });
    expect(rescaled.request?.action ?? 'none').not.toBe('amendOrder');
  });

  it('the pre-sign decoder blocks an amend whose record does not prove the continuation', () => {
    const snap = snapOf('create.ask');
    const plan = planCancelReplace(env(), snap, { order: same(snap, (st) => { st.price = '260000000'; }), value: 10n * KAS });
    expect(plan.request!.action).toBe('amendOrder');
    const lie = same(snap, (st) => { st.price = '1'; });
    const built = clone(plan.built!);
    built.tx.payload = kob.encodePayload([{ type: 'amend', output: 0, input: 0, family: 1, template: lie.kind, state: kob.encodeState(lie) }]);
    const s = decodeSigning({ kob, built, maker: MAKER.pk, registry, expected: plan.expected, nodeInputs: nodeFactsOf(built) });
    expect(s.ok).toBe(false);
    expect(s.blocking.map((i) => i.code)).toEqual(expect.arrayContaining(['recover-failed', 'output-unknown']));
  });

  it('a tiny change rides on the replacement instead of a dust output (the change rule)', () => {
    // a bid amended to a lower price keeps its escrow; its released KAS less the fee would come back as a few tenths of a KAS
    const bid = snapOf('create.bid');
    const lower = same(bid, (st) => { st.price = '230000000'; });
    const value = BigInt(bid.order.amount) - 3n * KAS / 10n;
    const plan = planCancelReplace(env(), bid, { order: lower, value });
    const s = verify(plan, 'bid');
    expect(plan.built!.fee.changeOutput ?? null).toBe(null);
    expect(plan.fundingUsed).toEqual([]);
    expect(s.orders[0].value).toBe(BigInt(bid.order.amount) - BigInt(plan.built!.fee.fee));
    expect(plan.built!.fee.mass.storage).toBeLessThanOrEqual(plan.built!.fee.mass.feeMass);
  });

  it('tops up from supplied tokens or, when not supplied, picks the maker\'s free tokens', () => {
    const snap = snapOf('create.ask');
    const bigger = same(snap, (st) => { st.amountLeft = '12000'; });
    const free = tokenUtxo(3_000n, MAKER.pk, 150);
    const explicit = planCancelReplace(env(), snap, { order: bigger, value: 10n * KAS }, [free]);
    const s = verify(explicit, 'explicit');
    expect(s.net.tokens[0]).toMatchObject({ released: 10_000n, fromWallet: 3_000n, escrowed: 12_000n, toMaker: 1_000n });
    const auto = planCancelReplace(env({ tokenUtxos: [tokenUtxo(50n, MAKER.pk, 151), free, tokenUtxo(9_000n, MAKER.pk, 152)] }), snap, { order: bigger, value: 10n * KAS });
    verify(auto, 'auto');
    expect(auto.issues.map((i) => i.code)).toContain('cancel.top-up');
    // the smallest sufficient single UTXO is chosen (3000, not 9000)
    expect(auto.built!.tx.inputs.map((i) => i.transactionId)).toContain(free.transactionId);
    expect(auto.built!.tx.inputs.map((i) => i.transactionId)).not.toContain(tokenUtxo(9_000n, MAKER.pk, 152).transactionId);
    const none = planCancelReplace(env(), snap, { order: bigger, value: 10n * KAS });
    expect(none.ok).toBe(false);
    expect(none.issues.map((i) => i.code)).toEqual(['cancel.insufficient-tokens']);
    const short = planCancelReplace(env(), snap, { order: bigger, value: 10n * KAS }, [tokenUtxo(10n, MAKER.pk, 153)]);
    expect(short.issues.map((i) => i.code)).toEqual(['cancel.insufficient-tokens']);
  });

  it('turns an ask into a conditional (OCO) order and a bid into a larger bid', () => {
    const ask = snapOf('create.ask');
    const cond = placeGolden(kob, 'create.condAsk').snapshots[0].order.state;
    const asCond = { ...cond, state: { ...cond.state, amountLeft: '6000' } } as OrderState;
    const plan = planCancelReplace(env(), ask, { order: asCond, value: 10n * KAS });
    const s = verify(plan, 'oco');
    expect(s.orders[0].description.kind).toBe('KobCondAsk');
    expect(s.net.tokens[0]).toMatchObject({ escrowed: 6_000n, toMaker: 4_000n });
    const bid = snapOf('create.bid');
    const bigBid = same(bid, (st) => { st.price = '280000000'; });
    const b = planCancelReplace(env(), bid, { order: bigBid, value: 7_000_000_000n });
    const bs = verify(b, 'bid');
    expect(bs.orders[0].description.kind).toBe('KobBid');
    expect(b.tokensReturned).toBe(0n);
  });

  it('an amended armed stop restarts unarmed', () => {
    const base = placeGolden(kob, 'create.condAsk').snapshots[0];
    const armed = { ...base, order: { ...base.order, state: { ...base.order.state, state: { ...base.order.state.state, armed: '1' } } as OrderState } };
    const stillArmed = { ...armed.order.state, state: { ...armed.order.state.state, stopPrice: '210000000' } } as OrderState;
    const plan = planCancelReplace(env(), armed, { order: stillArmed, value: 10n * KAS });
    const s = verify(plan, 'restart');
    expect(plan.issues.map((i) => i.code)).toContain('cancel.replacement-restarts-unarmed');
    expect(s.orders[0].description.trigger!.armed).toBe(0n);
    expect(s.orders[0].description.trigger!.stopPrice).toBe(210_000_000n);
  });

  it('refuses a replacement of another maker or another token', () => {
    const snap = snapOf('create.ask');
    expect(planCancelReplace(env(), snap, { order: same(snap, (st) => { st.maker = OTHER.pk; }), value: 10n * KAS }).issues.map((i) => i.code)).toEqual(['cancel.replacement-wrong-maker']);
    expect(planCancelReplace(env(), snap, { order: same(snap, (st) => { st.tokenCovId = 'ab'.repeat(32); }), value: 10n * KAS }).issues.map((i) => i.code)).toEqual(['cancel.replacement-wrong-token']);
  });

  it('funds a bigger replacement from the maker\'s KAS', () => {
    const bid = snapOf('create.bid');
    const bigBid = same(bid, (st) => { st.price = '260000000'; });
    const plan = planCancelReplace(env(), bid, { order: bigBid, value: 80n * KAS });
    expect(plan.ok).toBe(true);
    expect(plan.fundingUsed).toHaveLength(1);
    signAndValidate(kob, plan.built!);
  });
});

describe('planCancelAll: positions and groups', () => {
  it('cancels a token group in one cancelPosition tx (custody inputs within the slots) and validates', () => {
    const ask = snapOf('create.ask');
    const cond = snapOf('create.condAsk');
    const bid = snapOf('create.bid');
    const plans = planCancelAll(env(), [ask, cond, bid]);
    expect(plans).toHaveLength(1);
    const s = verify(plans[0], 'group');
    expect(plans[0].request!.action).toBe('cancelPosition');
    expect(plans[0].cancelIds.sort()).toEqual([ask.covenantId, cond.covenantId, bid.covenantId].sort());
    expect(s.kind).toBe('cancel-position');
    expect(s.spends).toHaveLength(3);
    expect(s.outputs.filter((o) => o.kind === 'token-change')).toHaveLength(1); // ONE token output back to the maker
    expect(plans[0].tokensReturned).toBe(20_000n);
  });

  it('splits a group when the custody inputs exceed the token program slots, and by maxOrdersPerTx', () => {
    // three asks: 3 custody inputs fit a 3/3 program; a fourth needs a second tx
    const snaps = [snapOf('create.ask'), snapOf('create.condAsk'), snapOf('create.ifdAsk'), snapOf('create.ask.twap')];
    const plans = planCancelAll(env(), snaps);
    expect(plans.map((p) => p.cancelIds.length)).toEqual([3, 1]);
    for (const [i, p] of plans.entries()) verify(p, `chunk ${i}`);
    expect(plans[1].request!.action).toBe('cancelOrder'); // a group of one is a plain cancel
    const byCount = planCancelAll(env(), [snapOf('create.bid'), snapOf('create.condBid'), snapOf('create.ifdBid')], { maxOrdersPerTx: 2 });
    expect(byCount.map((p) => p.cancelIds.length)).toEqual([2, 1]);
    byCount.forEach((p, i) => verify(p, `bids ${i}`));
  });

  it('one tx per token: orders of different tokens never share a transaction', () => {
    const ask = snapOf('create.ask');
    const bid = snapOf('create.bid');
    expect(planCancelAll(env(), [ask, bid])).toHaveLength(1);
    // a second token (another covenant id): kob-wasm covers one token per cancelPosition, so it is planned separately
    const bid2: OrderSnapshot = { ...bid, order: { ...bid.order, state: { ...bid.order.state, state: { ...bid.order.state.state, tokenCovId: '80'.repeat(32) } } as OrderState } };
    const split = planCancelAll(env(), [ask, bid2]);
    expect(split).toHaveLength(2);
    expect(split.map((p) => p.cancelIds.length)).toEqual([1, 1]);
  });

  it('reports an order it cannot cancel without dropping the others', () => {
    const mine = snapOf('create.ask');
    const theirs = placeGolden(kob, 'create.bid', OTHER).snapshots[0];
    const plans = planCancelAll(env(), [theirs, mine]);
    expect(plans).toHaveLength(2);
    expect(plans[0].ok).toBe(false);
    expect(plans[0].issues[0].code).toBe('cancel.not-maker');
    expect(plans[1].ok).toBe(true);
    expect(planCancelAll(env(), [])[0].ok).toBe(false);
  });

  it('a repeat position (entry + booked exits, golden cancel.position vectors) cancels in one transaction and validates', () => {
    for (const name of ['cancel.position.repeatBuyFirst', 'cancel.position.repeatSellFirst']) {
      const req = goldenRequest<CancelPositionRequest>(name);
      const snaps = req.orders.map(snapshotFromItem);
      const plans = planCancelAll(env(), snaps);
      expect(plans, name).toHaveLength(1);
      const s = verify(plans[0], name);
      expect(s.spends.length, name).toBe(snaps.length);
      expect(plans[0].cancelIds.sort(), name).toEqual(snaps.map((x) => x.covenantId).sort());
      const total = snaps.reduce((a, x) => a + BigInt(x.custody?.state.amount ?? 0), 0n);
      expect(plans[0].tokensReturned, name).toBe(total);
    }
  });
});

describe('planRefund: the maker refunds their own order', () => {
  it('needs no signature: the refund tip pays the fee and the rest returns to the maker', () => {
    const snap = snapOf('create.ask'); // utxo at DAA 1000 => refundable 90 days idle = 77_761_000
    const plan = planRefund(env({ clock: { daa: 77_761_000n } }), snap);
    const s = verify(plan, 'refund ask');
    expect(plan.request!.action).toBe('refundOrder');
    expect(plan.built!.sign).toEqual([]);
    expect(s.kind).toBe('refund');
    expect(s.net.tokens[0]).toMatchObject({ released: 10_000n, toMaker: 10_000n });
    expect(s.net.kas.toOthers).toBe(0n);
    // no signature: the refund tip pays the node's relay-floor fee and the rest of the tip returns to the maker as change
    expect(plan.built!.fee.fee).toBe(plan.built!.fee.minFee);
    const tipChange = plan.built!.tx.outputs[plan.built!.fee.changeOutput!];
    expect(BigInt(tipChange.value)).toBe(3_500_000n - BigInt(plan.built!.fee.fee));
    const bid = planRefund(env({ clock: { daa: 77_761_000n } }), snapOf('create.bid'));
    const bs = verify(bid, 'refund bid');
    expect(bs.spends[0].action).toBe('refund');
  });

  it('reclaimTip adds one maker funding UTXO so the tip minus the minimum fee returns as change (one signature)', () => {
    const snap = snapOf('create.ask');
    const plan = planRefund(env({ clock: { daa: 77_761_000n } }), snap, { reclaimTip: true });
    verify(plan, 'reclaim');
    expect(plan.fundingUsed.map((f) => f.amount)).toEqual([String(3n * KAS)]); // the smallest UTXO of at least 1 KAS
    expect(plan.built!.sign).toHaveLength(1);
    expect(plan.built!.fee.fee).toBe(plan.built!.fee.minFee);
    const change = plan.built!.tx.outputs[plan.built!.fee.changeOutput!];
    expect(BigInt(change.value)).toBe(3n * KAS + 3_500_000n - BigInt(plan.built!.fee.fee));
    expect(plan.issues).toEqual([]);
    // without a usable funding UTXO it degrades to the signature-free refund
    const none = planRefund(env({ clock: { daa: 77_761_000n }, funding: [] }), snap, { reclaimTip: true });
    verify(none, 'no funding');
    expect(none.built!.sign).toEqual([]);
  });

  it('is refused by kob-wasm before the order is due, with the due DAA', () => {
    const snap = snapOf('create.ask');
    const early = planRefund(env({ clock: { daa: 5_000_000n } }), snap);
    expect(early.ok).toBe(false);
    expect(early.issues.map((i) => i.code)).toEqual(['refund.not-yet']);
    expect(early.issues[0].params).toMatchObject({ dueDaa: 77_761_000n, nowDaa: 5_000_000n });
    expect(planRefund(env({ clock: undefined }), snap).issues.map((i) => i.code)).toEqual(['refund.no-clock']);
    expect(planRefund(env({ maker: OTHER.pk, clock: { daa: 78_000_000n } }), snap).issues.map((i) => i.code)).toEqual(['cancel.not-maker']);
  });

  it('warns that a refund leaves strays behind, and refunds every order kind', () => {
    const snap = snapOf('create.ask');
    const plan = planRefund(env({ clock: { daa: 78_000_000n } }), { ...snap, strays: [strayFor(snap, 3n, 95)] });
    verify(plan, 'strays');
    expect(plan.issues.map((i) => i.code)).toEqual(['refund.strays-stay']);
    for (const name of ['create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdAsk', 'create.ifdAsk.repeat']) {
      verify(planRefund(env({ clock: { daa: 78_000_000n } }), snapOf(name)), name);
    }
    // an empty repeating sell-first entry has no custody: refunded by `close`
    const rep = snapOf('create.ifdAsk.repeat');
    const empty: OrderSnapshot = { ...rep, custody: null, order: { ...rep.order, state: { ...rep.order.state, state: { ...rep.order.state.state, amountLeft: '0' } } as OrderState } };
    const closed = planRefund(env({ clock: { daa: 78_000_000n } }), empty);
    verify(closed, 'close');
    expect(closed.built!.roles[0]).toMatch(/KobIfdAsk\.close@/);
  });
});

describe('snapshots', () => {
  it('snapshotFromOrderView reproduces the live snapshot (order, custody, strays) and cancels it', () => {
    const snap = snapOf('create.ask');
    const view = orderViewOf(kob, { ...snap, strays: [strayFor(snap, 6n, 96)] });
    const back = snapshotFromOrderView(view);
    expect(back.covenantId).toBe(snap.covenantId);
    expect(back.order).toMatchObject({ transactionId: snap.order.transactionId, index: snap.order.index, amount: snap.order.amount, covenantId: snap.covenantId });
    expect(back.order.state).toEqual(snap.order.state);
    expect(back.custody).toEqual({ ...snap.custody!, blockDaaScore: String(view.custody!.utxo!.created_daa) });
    expect(back.strays).toHaveLength(1);
    expect(back.strays[0].state).toMatchObject({ amount: '6', owner: snap.covenantId, owner_scheme: 4 });
    expect(back.source).toBe('indexer');
    verify(planCancel(env(), back), 'from view');
  });

  it('rebuilds custody / stray token states from owner + amount + extension commitment when the view has no state', () => {
    const snap = snapOf('create.ask');
    const view = orderViewOf(kob, { ...snap, strays: [strayFor(snap, 6n, 97)] });
    view.custody!.utxo = { ...view.custody!.utxo!, state: undefined };
    view.strays = view.strays!.map((s) => ({ ...s, state: undefined }));
    const back = snapshotFromOrderView(view);
    expect(back.custody!.state).toEqual(snap.custody!.state);
    expect(back.strays[0].state).toEqual(strayFor(snap, 6n, 97).state);
    verify(planCancel(env(), back), 'rebuilt');
    // a bid has no custody: strays are rebuilt with the bid state's extension commitment
    const bid = snapOf('create.bid');
    const bview = orderViewOf(kob, { ...bid, strays: [strayFor(bid, 2n, 98)] });
    bview.strays = bview.strays!.map((s) => ({ ...s, state: undefined }));
    bview.extension_commitment = undefined;
    verify(planCancel(env(), snapshotFromOrderView(bview)), 'bid stray');
  });

  it('spent strays and a spent custody are not part of the snapshot', () => {
    const snap = snapOf('create.ask');
    const view = orderViewOf(kob, { ...snap, strays: [strayFor(snap, 6n, 99)] });
    view.strays![0].spent = true;
    expect(snapshotFromOrderView(view).strays).toEqual([]);
    view.custody!.utxo!.spent = true;
    expect(snapshotFromOrderView(view).custody).toBeNull();
  });

  it('refuses views the indexer has not proven', () => {
    const snap = snapOf('create.ask');
    const code = (f: (v: ReturnType<typeof orderViewOf>) => void): string => {
      const v = orderViewOf(kob, snap);
      f(v);
      try {
        snapshotFromOrderView(v);
      } catch (e) {
        expect(e).toBeInstanceOf(SnapshotError);
        return (e as SnapshotError).code;
      }
      return 'no error';
    };
    expect(code((v) => { v.state_known = false; })).toBe('state-unknown');
    expect(code((v) => { v.state = undefined; })).toBe('state-unknown');
    expect(code((v) => { v.state = { kind: 'KobUnknown', state: {} } as never; })).toBe('state-unknown');
    expect(code((v) => { v.current = null; })).toBe('no-current-utxo');
    expect(code((v) => { v.custody!.utxo = { ...tokenUtxoView(snap.custody!, 'custody'), state: undefined }; v.extension_commitment = null; })).toBe('no-extension-commitment');
  });

  // If-done exits (booked by an entry's fill, no placement record): the indexer serves the entry's extension commitment and the proven
  // token state of the exit's custody and strays (it served neither, so the cancel of a buy-first exit threw `no-extension-commitment`).
  describe.each([
    'cancel.position.repeatBuyFirst', 'cancel.position.repeatSellFirst', 'kron.cancel.position.repeatBuyFirst', 'kron.cancel.position.repeatSellFirst',
  ])('an if-done exit view (%s)', (name) => {
    const req = goldenRequest<CancelPositionRequest>(name);
    const entry = snapshotFromItem(req.orders[0]);
    const exitItem = snapshotFromItem(req.orders[1]);
    const family = familyOfKind(exitItem.order.state.kind);
    const st = exitItem.order.state.state as unknown as Record<string, string>;
    const es = entry.order.state.state as unknown as Record<string, string>;
    // what the indexer stores for the exit: its entry's commitment (buy-first: the entry state's; sell-first: the exit state's, which
    // is the entry custody's); KRON has none (zero)
    const ext: Hex = family === 'kron' ? ZERO32 : (es.extensionCommitment ?? st.extensionCommitment);
    const stray = { ...strayFor(exitItem, 5n, 94), state: custodyState(family, 5n, exitItem.covenantId, ext) };
    const exit: OrderSnapshot = { ...exitItem, strays: [stray] };
    /** The view exactly as the fixed indexer serves it. */
    const served = () => orderViewOf(kob, exit, { parent: entry.covenantId, extension_commitment: ext });

    it('is booked to its entry', () => {
      expect(st.parent).toBe(entry.covenantId);
      expect(isKind(exitItem.order.state, 'KobCondAsk')).toBe(!!exitItem.custody);
    });

    it('snapshots with the proven custody and stray states and cancels (alone and with its entry) and refunds', () => {
      const back = snapshotFromOrderView(served());
      expect(back.order.state).toEqual(exit.order.state);
      expect(back.custody?.state ?? null).toEqual(exit.custody?.state ?? null);
      expect(back.strays.map((s) => s.state)).toEqual([stray.state]);
      verify(planCancel(env(), back), `${name}: cancel`);
      const all = planCancelAll(env(), [snapshotFromOrderView(orderViewOf(kob, entry)), back]);
      expect(all.length).toBeGreaterThan(0);
      all.forEach((p, i) => verify(p, `${name}: cancel all #${i}`));
      expect(all.flatMap((p) => p.cancelIds).sort()).toEqual([entry.covenantId, exit.covenantId].sort());
      verify(planRefund(env(), back), `${name}: refund`);
    });

    it('rebuilds the token states from the extension commitment alone, and from the proven states alone', () => {
      const noStates = served();
      noStates.custody!.utxo = noStates.custody!.utxo ? { ...noStates.custody!.utxo, state: undefined } : null;
      noStates.strays = noStates.strays!.map((s) => ({ ...s, state: undefined }));
      const rebuilt = snapshotFromOrderView(noStates);
      expect(rebuilt.custody?.state ?? null).toEqual(exit.custody?.state ?? null);
      expect(rebuilt.strays.map((s) => s.state)).toEqual([stray.state]);
      verify(planCancel(env(), rebuilt), `${name}: rebuilt`);
      const noExt = served();
      noExt.extension_commitment = null;
      verify(planCancel(env(), snapshotFromOrderView(noExt)), `${name}: proven states only`);
    });

    it('the view an earlier indexer served (no commitment, no states) cannot rebuild a KCC-20 exit custody', () => {
      const old = served();
      old.extension_commitment = null;
      old.custody!.utxo = old.custody!.utxo ? { ...old.custody!.utxo, state: undefined } : null;
      old.strays = [];
      const kcc20Custody = family === 'kcc20' && !!exit.custody;
      if (kcc20Custody) {
        expect(() => snapshotFromOrderView(old)).toThrow(SnapshotError);
        try { snapshotFromOrderView(old); } catch (e) { expect((e as SnapshotError).code).toBe('no-extension-commitment'); }
      } else {
        // KRON has no commitment; a sell-first exit holds KAS and carries its commitment in its state
        verify(planCancel(env(), snapshotFromOrderView(old)), `${name}: old view`);
      }
    });
  });

  it('snapshotFromRecord accepts only a live resolved record', () => {
    const snap = snapOf('create.bid');
    const live: ResolvedRecord = { status: 'live', covenantId: snap.covenantId, order: snap.order, custody: null, strays: [], stateChanged: false, checked: 1 };
    const back = snapshotFromRecord(live, [], 1_790_726_400n);
    expect(back).toMatchObject({ covenantId: snap.covenantId, source: 'record', deadline: 1_790_726_400n, custody: null });
    verify(planCancel(env(), back), 'from record');
    expect(() => snapshotFromRecord({ ...live, status: 'spent', order: null })).toThrow(SnapshotError);
    expect(() => snapshotFromRecord({ ...live, status: 'unknown', order: null })).toThrow(/unknown/);
  });
});
