// Claims of a mode-1 order's bought-custody (bid-delivery-spec §7), built by kob-wasm `claimOrder`,
// run in the script engine and decoded.
//
// The claim forms are CL1 (claim), CL2 (claim and merge) and CL3 (claim and place, continuing or
// ending). Then one mutant per rule is built, each refused with its own finding. A mutant
// re-derives the scripts it touches and carries its own node facts, so only the claim rule can
// catch it.
import { describe, expect, it } from 'vitest';
import { boughtOf, decodeSigning, deliveryOf, type ExpectedSigning, type SigningSummary } from './decode';
import { parseRegistry } from './registry';
import type { BuiltTx, Kcc20State, OrderState, TokenUtxo } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, OTHER, TOKEN, goldenRequest, keyUtxo, nodeFactsOf, placeGolden, tokenUtxo, tradableRegistryJson } from '../testing/chain-fixtures';
import { BOUGHT, askOf, claim, holding } from '../testing/claim-fixtures';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const codes = (xs: { code: string }[]) => xs.map((x) => x.code);

const decode = (built: BuiltTx, expected?: ExpectedSigning): SigningSummary =>
  decodeSigning({ kob, built, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(built), ...(expected ? { expected } : {}) });

describe('claims decode: the continuation keeps every field but bought, the tokens go to the maker or into the placed order', () => {
  it.each(['create.bid.acc', 'create.condBid', 'pair.create.bid'])('%s: claim (CL1)', (name) => {
    const { built, snap } = claim(name);
    const s = decode(built, { claimIds: [snap.covenantId] });
    expect(s.blocking, name).toEqual([]);
    expect(s.kind).toBe('claim');
    const sp = s.spends.find((x) => x.covenantId === snap.covenantId)!;
    expect(sp).toMatchObject({ action: 'claim', strays: 0n, tokensReleased: BOUGHT, bought: { amount: BOUGHT, input: 1, continuesAt: 0, toYou: BOUGHT, toOrder: 0n } });
    const cont = s.orders.find((o) => o.claimedFrom === 0)!;
    expect(cont).toMatchObject({ output: 0, covenantId: snap.covenantId, verified: true });
    expect(deliveryOf(cont.state)).toMatchObject({ mode: 1n, bought: 0n });
    expect(cont.value).toBe(BigInt(snap.order.amount) + boughtOf(snap.order.state)!.carrier);
    expect(codes(s.info)).toContain('bought-claimed');
    const tok = s.net.tokens.find((x) => x.ref.covenantId === boughtOf(snap.order.state)!.token)!;
    expect(tok).toMatchObject({ released: BOUGHT, toMaker: BOUGHT, toOthers: 0n, walletDelta: BOUGHT });
    expect(s.net.kas.toOthers).toBe(0n);
  });

  it('claim and merge (CL2): two plain UTXOs of the maker join the claimed tokens', () => {
    const { built } = claim('create.bid.acc', { tokens: [tokenUtxo(300n, MAKER.pk, 150), tokenUtxo(5n, MAKER.pk, 151)] });
    const s = decode(built);
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('claim-merge');
    expect(s.spends[0]!.bought).toMatchObject({ amount: BOUGHT, toYou: BOUGHT + 305n, toOrder: 0n });
  });

  it('claim and place (CL3): an ask of the claimed tokens, the rest to the maker; continuing and ending', () => {
    const { built, snap } = claim('create.bid.acc', { place: askOf(500n) });
    const s = decode(built);
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('claim-place');
    expect(s.spends[0]).toMatchObject({ action: 'claim', bought: { toYou: 200n, toOrder: 500n } });
    expect(s.orders.filter((o) => o.claimedFrom === undefined)).toHaveLength(1);
    const end = claim('create.bid.acc', { place: askOf(BOUGHT), end: true });
    const e = decode(end.built, { cancelIds: [end.snap.covenantId] });
    expect(e.blocking).toEqual([]);
    expect(e.kind).toBe('claim-place');
    expect(e.spends[0]).toMatchObject({ action: 'cancel', bought: { amount: BOUGHT, input: 1, continuesAt: null, toYou: 0n, toOrder: BOUGHT } });
    expect(e.orders.some((o) => o.covenantId === snap.covenantId)).toBe(false);
  });

  it('a plan that claims other orders, or cancels instead, is refused', () => {
    const { built, snap } = claim('create.bid.acc');
    expect(codes(decode(built, { claimIds: [] }).blocking)).toContain('expected-claim-ids');
    expect(codes(decode(built, { cancelIds: [snap.covenantId] }).blocking)).toEqual(expect.arrayContaining(['expected-cancel-ids', 'expected-claim-ids']));
  });
});

// ------------------------------------------------------------------------------------------------
// mutants

/** Re-derives a token output's script from its (edited) next state in the leader plan. */
function setTokenOutput(b: BuiltTx, j: number, edit: (st: Kcc20State) => void) {
  const k = b.plans.findIndex((p) => p.kind === 'tokenLeader');
  const p = b.plans[k] as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' }>;
  const cov = b.tx.inputs[k]!.utxo.covenantId;
  const bound = b.tx.outputs.map((o, i) => [o, i] as const).filter(([o]) => o.covenant?.covenantId === cov).map(([, i]) => i);
  const st = p.nextStates[bound.indexOf(j)]!;
  edit(st);
  b.tx.outputs[j]!.scriptPublicKey = kob.tokenScriptPublicKey(p.template, st);
}
/** Re-derives a token input's script from its (edited) state. */
function setTokenInput(b: BuiltTx, k: number, edit: (st: Kcc20State) => void) {
  const p = b.plans[k] as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' | 'tokenDelegator' }>;
  edit(p.state);
  b.tx.inputs[k]!.utxo.scriptPublicKey = kob.tokenScriptPublicKey(p.template, p.state);
}
/** Re-derives the continuation's script from an edited state. */
function setContinuation(b: BuiltTx, os: OrderState, edit: (s: Record<string, string>) => void) {
  const next = clone(os);
  (next.state as unknown as Record<string, string>).bought = '0';
  edit(next.state as unknown as Record<string, string>);
  b.tx.outputs[0]!.scriptPublicKey = kob.scriptPublicKey(next);
}
const plainOut = (b: BuiltTx) => b.tx.outputs.findIndex((o, j) => j > 0 && o.covenant && o.covenant.covenantId === TOKEN.covenantId);
const changeOut = (b: BuiltTx) => b.tx.outputs.findIndex((o) => o.scriptPublicKey === `000020${MAKER.pk}ac` && !o.covenant);
/** Moves `sompi` from output `from` to output `to` (the fee unchanged). */
const moveKas = (b: BuiltTx, from: number, to: number, sompi: bigint) => {
  b.tx.outputs[from]!.value = (BigInt(b.tx.outputs[from]!.value) - sompi).toString();
  b.tx.outputs[to]!.value = (BigInt(b.tx.outputs[to]!.value) + sompi).toString();
};

describe('claim mutants: each is refused by the claim rule', () => {
  const base = claim('create.bid.acc');
  const os = base.snap.order.state;
  const mutant = (f: (b: BuiltTx) => void): SigningSummary => {
    const b = clone(base.built);
    f(b);
    return decode(b);
  };
  const refusedBy = (s: SigningSummary, code: string, why?: RegExp) => {
    expect(codes(s.blocking)).toContain(code);
    if (why) expect(s.blocking.filter((x) => x.code === code).map((x) => x.message).join('\n')).toMatch(why);
  };

  it('the unmutated claim passes', () => expect(decode(base.built).blocking).toEqual([]));

  it('owner: the claimed tokens to another key', () => {
    const s = mutant((b) => setTokenOutput(b, plainOut(b), (st) => (st.owner = OTHER.pk)));
    refusedBy(s, 'transfer-out');
    refusedBy(s, 'claim-invalid', /go neither to your plain UTXO/);
  });

  it('destination: the claimed tokens into a covenant the tx does not create', () => {
    const s = mutant((b) => setTokenOutput(b, plainOut(b), (st) => Object.assign(st, { owner: 'ab'.repeat(32), owner_scheme: 4 })));
    refusedBy(s, 'claim-invalid', /go neither to your plain UTXO/);
  });

  it('destination: the claimed tokens onto a receive UTXO (borrow scheme 1)', () => {
    const s = mutant((b) => setTokenOutput(b, plainOut(b), (st) => Object.assign(st, { borrow_scheme: 1, borrow_guard: '01'.padEnd(64, '0') })));
    refusedBy(s, 'token-state-unplain');
  });

  it('amount: the custody input is not exactly bought', () => {
    const s = mutant((b) => {
      setTokenInput(b, 1, (st) => (st.amount = (BOUGHT + 1n).toString()));
      setTokenOutput(b, plainOut(b), (st) => (st.amount = (BOUGHT + 1n).toString()));
    });
    refusedBy(s, 'claim-invalid', /bought-custody .* is not an input/);
  });

  it('amount: the custody carries another commitment or a borrow setting', () => {
    refusedBy(mutant((b) => setTokenInput(b, 1, (st) => (st.extension_commitment = 'cd'.repeat(32)))), 'claim-invalid', /is not an input/);
    refusedBy(mutant((b) => setTokenInput(b, 1, (st) => Object.assign(st, { borrow_scheme: 1, borrow_guard: '01'.padEnd(64, '0') }))), 'claim-invalid', /is not an input/);
  });

  it('amount: the maker output holds less than bought (the rest burnt)', () => {
    const s = mutant((b) => setTokenOutput(b, plainOut(b), (st) => (st.amount = (BOUGHT - 1n).toString())));
    refusedBy(s, 'claim-invalid', /not the 700 bought/);
  });

  it('extra output: a payment to another key, and a second output bound to the order', () => {
    refusedBy(mutant((b) => {
      const c = changeOut(b);
      b.tx.outputs[c]!.value = (BigInt(b.tx.outputs[c]!.value) - KAS).toString();
      b.tx.outputs.push({ value: KAS.toString(), scriptPublicKey: `000020${OTHER.pk}ac`, covenant: null });
    }), 'payment-out');
    const s = mutant((b) => {
      const c = changeOut(b);
      b.tx.outputs[c]!.value = (BigInt(b.tx.outputs[c]!.value) - KAS).toString();
      b.tx.outputs.push({ value: KAS.toString(), scriptPublicKey: b.tx.outputs[0]!.scriptPublicKey, covenant: { ...b.tx.outputs[0]!.covenant! } });
    });
    refusedBy(s, 'claim-invalid', /2 outputs are bound/);
  });

  // every field of the continuation but bought is the order's: flip each one
  const fields = Object.keys(os.state).filter((k) => k !== 'bought');
  it.each(fields)('continuation field %s changed', (k) => {
    const s = mutant((b) =>
      setContinuation(b, os, (st) => {
        const v = st[k]!;
        st[k] = /^[0-9a-f]{64}$/.test(v) ? (v[0] === 'a' ? 'b' : 'a') + v.slice(1) : (BigInt(v) + 1n).toString();
      }),
    );
    refusedBy(s, 'claim-invalid', /not the same order with only bought set to 0/);
  });

  it('continuation keeps bought, or holds less than the order plus one carrier, or another input authorises it', () => {
    refusedBy(mutant((b) => setContinuation(b, os, (st) => (st.bought = BOUGHT.toString()))), 'claim-invalid', /only bought set to 0/);
    refusedBy(mutant((b) => moveKas(b, 0, changeOut(b), 1n)), 'claim-invalid', /plus one delivery carrier/);
    refusedBy(mutant((b) => (b.tx.outputs[0]!.covenant!.authorizingInput = 2)), 'claim-invalid', /not authorised by the order's input/);
  });

  it('a wallet token input with a borrow setting (a receive UTXO) is not merged into a claim', () => {
    const m = claim('create.bid.acc', { tokens: [tokenUtxo(300n, MAKER.pk, 150)] });
    const b = clone(m.built);
    setTokenInput(b, 2, (st) => Object.assign(st, { borrow_scheme: 1, borrow_guard: '01'.padEnd(64, '0') }));
    refusedBy(decode(b), 'claim-invalid', /not your plain UTXO/);
  });

  it('claim and place: the placed custody rebound to another covenant', () => {
    const m = claim('create.bid.acc', { place: askOf(500n) });
    const b = clone(m.built);
    const j = b.tx.outputs.findIndex((o, i) => i > 1 && o.covenant?.covenantId === TOKEN.covenantId);
    setTokenOutput(b, j, (st) => (st.owner = 'ab'.repeat(32)));
    refusedBy(decode(b), 'claim-invalid', /go neither/);
  });

  it('claim and place: the placed order holds no claimed token (a bid placed in its stead)', () => {
    const m = claim('create.bid.acc', { place: askOf(500n) });
    const b = clone(m.built);
    const [rec] = kob.recoverOrders(b.tx);
    const bid = clone(placeGolden(kob, 'create.bid').recovered[0]!.order as OrderState);
    b.tx.outputs[rec!.output]!.scriptPublicKey = kob.scriptPublicKey(bid);
    // a builder that wrote a bid's record: the record names the bid, its output carries the bid's
    // script
    const lying = { ...kob, recoverOrders: () => [{ ...rec!, order: bid, custody: null }] };
    const s = decodeSigning({ kob: lying, built: b, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(b) });
    refusedBy(s, 'claim-invalid', /does not hold only the claimed token/);
  });

  it('a second UTXO owned by the order (a sell custody, a stray) rides along', () => {
    const m = claim('create.bid.acc', { tokens: [tokenUtxo(300n, MAKER.pk, 150)] });
    const b = clone(m.built);
    setTokenInput(b, 2, (st) => Object.assign(st, { owner: m.snap.covenantId, owner_scheme: 4 }));
    refusedBy(decode(b), 'claim-invalid', /another UTXO owned by the order/);
  });

  it('a sweep in place keeps the bought-custody where it is', () => {
    const { snap, custody } = holding('create.bid.acc');
    const mode0 = clone(snap.order);
    Object.assign(mode0.state.state, { mode: '0', bought: '0' });
    // a sweep of the same order in mode 0 with a stray that is exactly the custody, then the order
    // read as mode 1 (input and output)
    const built = kob.build({ action: 'sweepOrder', order: mode0, strays: [custody], funding: [keyUtxo(MAKER.pk, 50n * KAS, 201)] });
    expect(decode(built).blocking).toEqual([]);
    const b = clone(built);
    const p = b.plans[0] as Extract<BuiltTx['plans'][number], { kind: 'entry' }>;
    p.state = kob.encodeState(snap.order.state);
    b.tx.inputs[0]!.utxo.scriptPublicKey = kob.scriptPublicKey(snap.order.state);
    b.tx.outputs[0]!.scriptPublicKey = b.tx.inputs[0]!.utxo.scriptPublicKey;
    refusedBy(decode(b), 'sweep-custody-spent');
  });

  it('an amend keeps the delivery block', () => {
    const built = kob.build(goldenRequest('amend.bid', MAKER.pk));
    expect(decode(built).blocking).toEqual([]);
    const b = clone(built);
    const p = b.plans[0] as Extract<BuiltTx['plans'][number], { kind: 'entry' }>;
    const st = kob.decodeState(p.template, p.state) as OrderState;
    Object.assign(st.state, { mode: '1' });
    p.state = kob.encodeState(st);
    b.tx.inputs[0]!.utxo.scriptPublicKey = kob.scriptPublicKey(st);
    // kob-wasm's amend recovery refuses it; the decoder's own rule refuses it without that recovery
    // check too
    expect(codes(decode(b).blocking)).toContain('recover-failed');
    const amends = kob.recoverAmends(built.tx, built.plans);
    const lax = { ...kob, recoverAmends: () => amends };
    const s = decodeSigning({ kob: lax, built: b, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(b) });
    expect(s.blocking.filter((x) => x.code === 'order-terms-invalid').map((x) => x.message).join('\n')).toMatch(/changes the order's delivery/);
  });

  it('an ending cancel that leaves the bought-custody behind', () => {
    const built = kob.build(goldenRequest('cancel.bid', MAKER.pk));
    const b = clone(built);
    const p = b.plans[0] as Extract<BuiltTx['plans'][number], { kind: 'entry' }>;
    const st = kob.decodeState(p.template, p.state) as OrderState;
    Object.assign(st.state, { mode: '1', bought: BOUGHT.toString() });
    p.state = kob.encodeState(st);
    b.tx.inputs[0]!.utxo.scriptPublicKey = kob.scriptPublicKey(st);
    refusedBy(decode(b), 'bought-custody-lost');
    // the rule's positive side: the unmutated cancel of a mode-0 bid passes
    expect(decode(built).blocking).toEqual([]);
  });
});

describe('mode-2 placement: the delivery the user chose', () => {
  const placed = kob.build(goldenRequest('create.bid.topup', MAKER.pk));
  const planned = placeGolden(kob, 'create.bid.topup').recovered[0]!.order as OrderState;
  it('the planned state passes; another tip or threshold is named', () => {
    expect(deliveryOf(planned)).toMatchObject({ mode: 2n });
    expect(decode(placed, { orders: [planned] }).blocking).toEqual([]);
    for (const k of ['topupTip', 'recvThreshold'] as const) {
      const other = clone(planned);
      const st = other.state as unknown as Record<string, string>;
      st[k] = (BigInt(st[k]!) + 1n).toString();
      const s = decode(placed, { orders: [other] });
      expect(codes(s.blocking), k).toContain('expected-delivery');
      expect(s.blocking.find((x) => x.code === 'expected-delivery')!.message).toContain(k);
    }
  });
});
