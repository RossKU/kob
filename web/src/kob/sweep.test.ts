// The maker's SWEEP of an order's strays IN PLACE (kob-protocol `SweepOrder`, SWEEP record 0x83): planned by `planSweep`, built by kob-wasm,
// signed locally and validated in the script engine for every order kind of both families, then decoded on the confirmation model; and the
// hostile variants the decoder must block (another script on the continuation, a record that does not verify, a token output to another key,
// the custody swept along, a plan that claims another sweep). Foreign strays (other tokens owned by the order id) ride on sweeps and cancels.
import { describe, expect, it } from 'vitest';
import { planCancel, planSweep, snapshotFromOrderView, type CancelEnv, type CancelPlan, type OrderSnapshot } from './cancel';
import { decodeSigning, type ExpectedSigning, type SigningSummary } from './decode';
import { familyOfKind, tokenCovIdOf } from './order-facts';
import { MemoryRecordStore, recordsFromBuilt, sweptRecords } from './records';
import { parseRegistry } from './registry';
import { recoverSweeps } from './sweep';
import { custodyState, extensionOfState } from './token-state';
import type { BuiltTx, ForeignStrays, Hex, Kcc20State, SweepOrderRequest, TokenUtxo } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, OTHER, TOKEN, ZERO32, keyUtxo, nodeFactsOf, orderViewOf, placeGolden, signAndValidate, tokenUtxoView, tradableRegistryJson } from '../testing/chain-fixtures';
import { buildConfirmModel } from '../ui/confirm/confirm-model';
import { t } from '../i18n';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
/** A listed foreign token of the registry (EIGHT, 8/8 program) and one outside it. */
const EIGHT = { covenantId: '80'.repeat(32), program: 'KCC20Ref_8x8' as const };
const ALIEN = { covenantId: '81'.repeat(32), program: 'KCC20Ref_8x8' as const };

const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201), keyUtxo(MAKER.pk, 3n * KAS, 202)], clock: { daa: 78_000_000n }, ...over });
const snapOf = (name: string): OrderSnapshot => placeGolden(kob, name).snapshots[0]!;
const txid = (n: number): Hex => n.toString(16).padStart(2, '0').repeat(32);

/** A stray of the order's own token (its family; the custody's extension commitment, else the golden token's). */
function ownStray(snap: OrderSnapshot, amount: bigint, n: number): TokenUtxo {
  const family = familyOfKind(snap.order.state.kind);
  const ext = (snap.custody ? extensionOfState(snap.custody.state) : null) ?? TOKEN.ext;
  return {
    transactionId: txid(n), index: n, amount: (10n * KAS).toString(), blockDaaScore: '1200', covenantId: tokenCovIdOf(snap.order.state),
    state: custodyState(family, amount, snap.covenantId, family === 'kron' ? null : ext),
  };
}
/** Foreign strays of a KCC-20 token owned by the order id. */
function foreignGroup(snap: OrderSnapshot, token: typeof EIGHT, amounts: bigint[], n: number): ForeignStrays {
  return {
    token,
    utxos: amounts.map((a, k) => ({
      transactionId: txid(n + k), index: k, amount: (5n * KAS).toString(), blockDaaScore: '1300', covenantId: token.covenantId,
      state: { amount: a.toString(), owner: snap.covenantId, owner_scheme: 4, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: 'cc'.repeat(32) } satisfies Kcc20State,
    })),
  };
}
const withStrays = (snap: OrderSnapshot, foreign: ForeignStrays[] = []): OrderSnapshot => ({ ...snap, strays: [ownStray(snap, 7n, 0x91), ownStray(snap, 5n, 0x92)], foreign });

const decode = (built: BuiltTx, expected?: ExpectedSigning, reg: typeof registry | null = registry): SigningSummary =>
  decodeSigning({ kob, built, maker: MAKER.pk, registry: reg, nodeInputs: nodeFactsOf(built), ...(expected ? { expected } : {}) });
const codes = (xs: { code: string }[]) => xs.map((x) => x.code);

function verified(plan: CancelPlan, label: string, reg: typeof registry | null = registry): SigningSummary {
  expect(plan.issues.filter((i) => i.severity === 'error'), label).toEqual([]);
  expect(plan.ok, label).toBe(true);
  signAndValidate(kob, plan.built!, [MAKER.sk]);
  const s = decode(plan.built!, plan.expected, reg);
  expect(s.blocking, label).toEqual([]);
  return s;
}

const KCC20_KINDS = ['create.ask', 'create.bid', 'create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdAsk', 'create.ifdBid.repeat', 'create.ifdAsk.repeat'];
const KINDS = [...KCC20_KINDS, ...KCC20_KINDS.map((k) => `kron.${k}`)];

describe('planSweep: the order continues under the same script, its strays return to the maker', () => {
  it.each(KINDS)('%s: 2 own strays + 1 foreign stray, engine-valid, decoded as a sweep with no blocking finding', (name) => {
    const snap = withStrays(snapOf(name), [foreignGroup(snapOf(name), EIGHT, [300n], 0xa1)]);
    const plan = planSweep(env(), snap);
    const reg = name.startsWith('kron.') ? null : registry;
    const s = verified(plan, name, reg);
    const req = plan.request as SweepOrderRequest;
    expect(req.action).toBe('sweepOrder');
    expect(req.strays).toHaveLength(2);
    expect(req.foreign).toEqual([foreignGroup(snap, EIGHT, [300n], 0xa1)]);
    // a plain ask's carrier pays; every other kind is funded by one wallet coin
    const ask = /create\.ask$/.test(name);
    expect(plan.fundingUsed.length, name).toBe(ask ? 0 : 1);
    // the continuation: output 0, the SAME script as the order input, bound to the same covenant id from input 0
    const tx = plan.built!.tx;
    expect(tx.outputs[0]!.scriptPublicKey).toBe(tx.inputs[0]!.utxo.scriptPublicKey);
    expect(tx.outputs[0]!.covenant).toEqual({ authorizingInput: 0, covenantId: snap.covenantId });
    expect(s.kind).toBe('sweep');
    expect(s.orders).toHaveLength(1);
    expect(s.orders[0]).toMatchObject({ output: 0, covenantId: snap.covenantId, sweptFrom: 0, verified: true });
    expect(s.spends).toHaveLength(1);
    expect(s.spends[0]).toMatchObject({ action: 'sweep', strays: 12n, sweptTo: { output: 0, utxos: 3, kas: 25n * KAS } });
    expect(s.spends[0]!.otherStrays.map((x) => [x.ref.covenantId, x.amount, x.foreign])).toEqual([[EIGHT.covenantId, 300n, true]]);
    expect(codes(s.info)).toEqual(expect.arrayContaining(['swept-in-place', 'strays-swept', 'other-strays-swept']));
    // the tokens come back to the maker; nothing goes to anyone else
    const byToken = new Map(s.net.tokens.map((x) => [x.ref.covenantId, x]));
    expect(byToken.get(tokenCovIdOf(snap.order.state))).toMatchObject({ released: 12n, toMaker: 12n, toOthers: 0n });
    expect(byToken.get(EIGHT.covenantId)).toMatchObject({ released: 300n, toMaker: 300n, toOthers: 0n });
    expect(s.net.kas.toOthers).toBe(0n);
    expect(plan.sweep).toMatchObject({ covenantId: snap.covenantId, utxos: 3, kas: 25n * KAS, later: 0, unproven: 0 });
    expect(plan.foreignReturned).toEqual([{ covenantId: EIGHT.covenantId, amount: 300n, utxos: 1 }]);
    // the custody, when the order holds one, stays where it is
    if (snap.custody) expect(tx.inputs.some((i) => i.transactionId === snap.custody!.transactionId && i.index === snap.custody!.index)).toBe(false);
  });

  it('the confirmation model states what returns, that the order continues unchanged and that its idle window restarts', () => {
    const base = snapOf('create.ask');
    const snap = withStrays(base, [foreignGroup(base, ALIEN, [42n], 0xb1)]);
    const plan = planSweep(env(), snap);
    const s = verified(plan, 'ask');
    expect(codes(s.warnings)).toContain('token-unlisted'); // the foreign token outside the registry
    const en = buildConfirmModel(s, { registry, tr: (k, p) => t(k, p) });
    expect(en.kind).toBe('sweep');
    expect(en.heading).toBe('Sweep stray tokens');
    expect(en.intro).toBe(
      '3 stray token UTXO(s) (0.012 TST, 42 base units of unknown token (8181...8181)) and their 25 KAS return to you. The order continues unchanged (same price, amount and custody); as a new UTXO, its 90-day idle window restarts.',
    );
    expect(en.sections.map((x) => x.id)).toEqual(['spend', 'sweep', 'back', 'fee', 'net']);
    const card = en.sections.find((x) => x.id === 'sweep')!.cards[0]!;
    expect(card.title).toBe('Sweep strays: Sell limit order');
    const rows = Object.fromEntries(card.rows.map((r) => [r.id, r]));
    expect(rows.strays!.value).toBe('0.012 TST');
    expect(rows[`strays-${ALIEN.covenantId}`]).toMatchObject({ tone: 'bad', label: 'Stray unknown token (8181...8181) (another token) swept' });
    expect(rows.continues!.value).toBe('continues unchanged at output 0');
    expect(rows.idle!.value).toBe('restarts');
    // only the strays' carriers are "released": the order's own value stays on it (less the fee its carrier paid)
    expect(en.sections.find((x) => x.id === 'spend')!.rows.find((r) => r.id === 'kas-released')!.value).toBe('25 KAS');
  });

  it('a bid needs a wallet coin; without one the plan says so', () => {
    const snap = withStrays(snapOf('create.bid'));
    const plan = planSweep(env({ funding: [] }), snap);
    expect(plan.ok).toBe(false);
    expect(codes(plan.issues)).toContain('sweep.needs-funding');
  });

  it('takes at most the program inputs of one extension commitment; the rest stays for a later sweep; unproven foreign strays are reported', () => {
    const base = snapOf('create.ask'); // 3/3 program
    const odd: TokenUtxo = { ...ownStray(base, 2n, 0x96), state: custodyState('kcc20', 2n, base.covenantId, 'dd'.repeat(32)) };
    const snap: OrderSnapshot = {
      ...base, strays: [ownStray(base, 1n, 0x91), ownStray(base, 9n, 0x92), ownStray(base, 5n, 0x93), ownStray(base, 4n, 0x94), odd],
      foreignUnproven: [{ outpoint: `${txid(0xc1)}:0`, token: ALIEN.covenantId, amount: 3n }],
    };
    const plan = planSweep(env(), snap);
    verified(plan, 'ask');
    expect((plan.request as SweepOrderRequest).strays!.map((x) => x.state.amount)).toEqual(['9', '5', '4']); // the largest first
    expect(codes(plan.issues)).toEqual(expect.arrayContaining(['sweep.in-place', 'sweep.later-extension', 'sweep.later-slots', 'sweep.stray-unproven']));
    expect(plan.sweep).toMatchObject({ utxos: 3, later: 2, unproven: 1 });
    // a second sweep (after the first) takes the next ones
    expect(planSweep(env(), { ...base, strays: [ownStray(base, 1n, 0x91), odd] }).ok).toBe(true);
  });

  it('nothing to sweep, or another maker: refused', () => {
    expect(codes(planSweep(env(), snapOf('create.ask')).issues)).toEqual(['sweep.nothing']);
    expect(codes(planSweep(env({ maker: OTHER.pk }), withStrays(snapOf('create.ask'))).issues)).toEqual(['cancel.not-maker']);
  });
});

describe('the pre-sign decoder blocks a sweep that is not one', () => {
  const plan = planSweep(env(), withStrays(snapOf('create.ask'), [foreignGroup(snapOf('create.ask'), EIGHT, [300n], 0xa1)]));
  const built = plan.built!;
  const blocking = (b: BuiltTx, expected: ExpectedSigning | undefined = plan.expected) => codes(decode(b, expected).blocking);

  it('the continuation under another script (the order would change)', () => {
    const b = clone(built);
    const st = clone(plan.expected.orders![0]!);
    (st.state as { price: string }).price = '1';
    b.tx.outputs[0]!.scriptPublicKey = kob.scriptPublicKey(st);
    const bl = blocking(b);
    expect(bl).toContain('sweep-invalid');
    expect(bl).toContain('output-unknown');
  });

  it('a record naming another output or input, a missing record, a record on another kind of spend', () => {
    const other = clone(built);
    other.tx.payload = kob.encodePayload([{ type: 'sweep', output: 1, input: 0 }]);
    expect(blocking(other)).toContain('sweep-invalid');
    const input = clone(built);
    input.tx.payload = kob.encodePayload([{ type: 'sweep', output: 0, input: 1 }]);
    expect(blocking(input)).toContain('sweep-invalid');
    // no record: the continuation is an unknown covenant output and the plan's sweep is missing
    const none = clone(built);
    none.tx.payload = '';
    expect(blocking(none)).toEqual(expect.arrayContaining(['output-unknown', 'expected-sweep-ids']));
    // the order input spent by another entry than its maker's cancel
    const entry = clone(built);
    const p0 = entry.plans[0]!;
    if (p0.kind === 'entry') p0.entry = 'refund';
    expect(blocking(entry)).toContain('sweep-invalid');
    expect(recoverSweeps(kob, entry.tx, entry.plans).failed[0]!.reason).toMatch(/not spent by its maker's cancel/);
  });

  it('a token output to another key', () => {
    const b = clone(built);
    const leader = b.plans.findIndex((p) => p.kind === 'tokenLeader' && b.tx.inputs[b.plans.indexOf(p)]!.utxo.covenantId === EIGHT.covenantId);
    const lp = b.plans[leader] as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' }>;
    const next = { ...lp.nextStates[0]!, owner: OTHER.pk };
    lp.nextStates[0] = next;
    const j = b.tx.outputs.findIndex((o) => o.covenant?.covenantId === EIGHT.covenantId);
    b.tx.outputs[j]!.scriptPublicKey = kob.tokenScriptPublicKey(EIGHT.program, next);
    expect(blocking(b)).toContain('transfer-out');
  });

  it('the custody swept along, and a plan that claims another sweep or a cancel', () => {
    const snap = withStrays(snapOf('create.ask'));
    const ok = planSweep(env(), snap);
    // the builder accepts the custody as one more stray of the order's token: the plan's `untouched` outpoint catches it
    const req = { ...(ok.request as SweepOrderRequest), strays: [snap.custody!, ...(ok.request as SweepOrderRequest).strays!] };
    const greedy = kob.build({ ...req, ownKeys: [MAKER.pk] });
    expect(codes(decode(greedy, ok.expected).blocking)).toContain('sweep-custody-spent');
    // a cancel plan's expectations against a sweep, and a sweep plan's against another order id
    expect(blocking(built, { cancelIds: [snap.covenantId] })).toEqual(expect.arrayContaining(['expected-cancel-ids', 'expected-sweep-ids']));
    expect(blocking(built, { sweepIds: ['ab'.repeat(32)] })).toContain('expected-sweep-ids');
  });
});

describe('foreign strays in snapshots and cancels', () => {
  it('snapshotFromOrderView keeps foreign strays apart (with their program); planCancel returns them, unproven ones stay behind with a warning', () => {
    const snap = snapOf('create.ask');
    const own = ownStray(snap, 6n, 0x96);
    const fg = foreignGroup(snap, EIGHT, [300n, 200n], 0xa1);
    const view = orderViewOf(kob, { ...snap, strays: [own] }, {
      strays: [
        tokenUtxoView(own, 'stray'),
        ...fg.utxos.map((u) => tokenUtxoView(u, 'stray', { token: EIGHT.covenantId, program: EIGHT.program, foreign: true })),
        // a foreign stray the indexer could not prove: no state, no program
        { ...tokenUtxoView(foreignGroup(snap, ALIEN, [9n], 0xc1).utxos[0]!, 'stray', { token: ALIEN.covenantId, foreign: true }), state: undefined },
      ],
    });
    const back = snapshotFromOrderView(view);
    expect(back.strays.map((s) => s.state.amount)).toEqual(['6']);
    expect(back.foreign).toEqual([fg]);
    expect(back.foreignUnproven).toEqual([{ outpoint: `${txid(0xc1)}:0`, token: ALIEN.covenantId, amount: 9n }]);
    const plan = planCancel(env(), back);
    const s = verified(plan, 'cancel');
    expect(plan.request).toMatchObject({ action: 'cancelOrder', foreign: [fg] });
    expect(codes(plan.issues)).toEqual(['cancel.stray-other-token']); // only the unproven one
    expect(plan.foreignReturned).toEqual([{ covenantId: EIGHT.covenantId, amount: 500n, utxos: 2 }]);
    expect(s.kind).toBe('cancel');
    expect(s.spends[0]!.otherStrays.map((x) => [x.ref.covenantId, x.amount, x.utxos, x.foreign])).toEqual([[EIGHT.covenantId, 500n, 2, true]]);
    expect(new Map(s.net.tokens.map((x) => [x.ref.covenantId, x.toMaker])).get(EIGHT.covenantId)).toBe(500n);
  });
});

describe('records after a sweep', () => {
  it('the record stays and its last state points at the continuation', async () => {
    const placed = placeGolden(kob, 'create.ask');
    const store = new MemoryRecordStore();
    for (const r of recordsFromBuilt(kob, placed.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1n, placedAtDaa: 1000n })) await store.put(r);
    const snap = withStrays(placed.snapshots[0]!);
    const plan = planSweep(env(), snap);
    const signed = signAndValidate(kob, plan.built!, [MAKER.sk]);
    const before = await store.get(snap.covenantId);
    await store.put({ ...before!, cancelling: { txid: signed.tx.id, spends: `${snap.order.transactionId}:${snap.order.index}`, atUnix: '1' } });
    const out = await sweptRecords(kob, plan.built!, store, MAKER.pk, 2000n);
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ covenantId: snap.covenantId, txid: before!.txid, state: before!.state, cancelling: null });
    expect(out[0]!.last).toMatchObject({ txid: plan.built!.tx.id, index: 0, daa: '2000' });
    // another maker's store entry is never touched
    expect(await sweptRecords(kob, plan.built!, store, OTHER.pk, 2000n)).toEqual([]);
  });
});
