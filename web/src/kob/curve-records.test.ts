// A curve created in its token's genesis gets a placement record from the genesis alone (kob-wasm
// `recoverOrders` over the KOB1 payload): the order unbound (tokenCovId zero), its custody the
// genesis token output it owns. The curve's facts and the orders list read it.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { pubkeyOf } from '../testing/local-signer';
import { issueRaw } from './issue';
import { curveTable } from './curve';
import { asOrderState, curveFactsOf, describeOrder, holdsTokens, isOrderKind, sideOfKind } from './order-facts';
import { isPlacementRecord, recordsFromBuilt } from './records';

const kob = loadKobNode();
const maker = pubkeyOf('33'.repeat(32));
const SCALE = 100_000_000n;
const ZERO = '0'.repeat(64);

function genesisWithCurve() {
  const sell = 900n * SCALE;
  const t = curveTable(kob, { amount: sell, levels: 10, total: 100n * SCALE, multipleMilli: 5000n, scale: SCALE });
  return issueRaw(kob, {
    name: 'Curve Token', ticker: 'CRV', decimals: 8, supply: (1000n * SCALE).toString(),
    holders: [{ owner: ZERO, ownerScheme: 4, amount: sell.toString() }, { owner: maker, ownerScheme: 0, amount: (100n * SCALE).toString() }],
    funding: [{ transactionId: '07'.repeat(32), index: 1, amount: (500n * SCALE).toString(), blockDaaScore: '5000', pubkey: maker }],
    network: 'testnet-10',
    curve: { custody: 0, levelSize: t.levelSize.toString(), table: t.table.map(String), minFill: '1' },
  } as never);
}

describe('a curve in its token genesis', () => {
  it('is an order kind that holds tokens and sells', () => {
    expect(isOrderKind('KobCurveAsk')).toBe(true);
    expect(holdsTokens('KobCurveAsk')).toBe(true);
    expect(sideOfKind('KobCurveAsk')).toBe('sell');
  });

  it('gets a placement record from the genesis: unbound, its custody the token output it owns', () => {
    const plan = genesisWithCurve();
    const recs = recordsFromBuilt(kob, plan.built, { maker, network: 'testnet-10', placedAtUnix: 1, placedAtDaa: 2 });
    expect(recs).toHaveLength(1);
    const r = recs[0]!;
    expect(r.kind).toBe('KobCurveAsk');
    expect(r.txid).toBe(plan.built.tx.id);
    expect(r.amount).toBe((900n * SCALE).toString());
    expect(r.custody?.output).toBe(0);
    expect(r.custody?.tokenState?.owner).toBe(r.covenantId);
    // its token, from the custody output's covenant in the genesis: the orders list names it before
    // the indexer does
    expect(r.token).toBe(plan.token.covenantId);
    expect(isPlacementRecord(r)).toBe(true);
    expect(isPlacementRecord({ ...r, token: 'zz' })).toBe(false);
    const st = asOrderState(kob.decodeState('KobCurveAsk', r.state))!;
    const c = curveFactsOf(st)!;
    expect(c).toMatchObject({ level: 0, levels: 10, unbound: true, total: 900n * SCALE });
    expect(c.priceNow).toBe(c.table[0]);
    // the description's price is the current level's
    expect(describeOrder(st).price).toBe(c.table[0]);
  });
});

describe('a fresh unbound curve in the orders list (from the record, before the indexer has it)', () => {
  it('names the token from its record or the custody the node found, never the zero id', async () => {
    const { buildEntries, describeEntry } = await import('../ui/orders/orders-model');
    const plan = genesisWithCurve();
    const rec = recordsFromBuilt(kob, plan.built, { maker, network: 'testnet-10', placedAtUnix: 1, placedAtDaa: 2 })[0]!;
    const row = (r: typeof rec, resolved = new Map()) => describeEntry(buildEntries([], [r], resolved)[0]!, { kob, clock: null });
    expect(row(rec).token).toBe(plan.token.covenantId);
    // an older record without the token: nothing rather than 0000...0000
    const { token: _t, ...old } = rec;
    expect(row(old).token).toBeNull();
    // the node found its custody in the genesis: that custody's token
    const order = kob.recoverOrders(plan.built.tx)[0]!;
    const resolved = new Map([[rec.covenantId, {
      status: 'live', covenantId: rec.covenantId, strays: [], stateChanged: false, checked: 1,
      order: { transactionId: plan.built.tx.id, index: order.output, amount: order.value, state: asOrderState(order.order)! },
      custody: { transactionId: plan.built.tx.id, index: 0, amount: '1000', covenantId: plan.token.covenantId, state: order.custody!.state },
    }]]);
    expect(row(old, resolved as never).token).toBe(plan.token.covenantId);
    // live on the node, and a curve has no expiry (0): not "expired, refund pending" once the clock
    // is known
    const live = describeEntry(buildEntries([], [rec], resolved as never)[0]!, { kob, clock: { daa: 80_000_000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 } });
    expect(live.live).toBe(true);
    expect(live.expiry.kind).toBe('none');
    expect(live.expired).toBe(false);
  });
});

describe('curve facts', () => {
  const state = (amountLeft: bigint) => ({
    kind: 'KobCurveAsk' as const,
    state: { tokenCovId: '11'.repeat(32), levelSize: '30', total: '100', amountLeft: amountLeft.toString(), table: ['10', '20', '30', '40'] },
  });
  it('reads the level, the price now and sold out', () => {
    expect(curveFactsOf(state(100n))).toMatchObject({ level: 0, priceNow: 10n, unbound: false });
    expect(curveFactsOf(state(45n))).toMatchObject({ level: 1, priceNow: 20n });
    expect(curveFactsOf(state(5n))).toMatchObject({ level: 3, priceNow: 40n });
    expect(curveFactsOf(state(0n))).toMatchObject({ level: 4, priceNow: null });
    expect(curveFactsOf({ kind: 'KobAsk', state: {} })).toBeNull();
  });
});

describe('cancel of a curve', () => {
  it('returns the custody and both carriers to the maker; validates in the script engine and decodes clean', async () => {
    const { placeGolden, MAKER, keyUtxo, signAndValidate, nodeFactsOf } = await import('../testing/chain-fixtures');
    const { planCancel } = await import('./cancel');
    const { decodeSigning } = await import('./decode');
    const snap = placeGolden(kob, 'create.curve').snapshots[0]!;
    expect(snap.order.state.kind).toBe('KobCurveAsk');
    const plan = planCancel({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * SCALE, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } }, snap);
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    expect(plan.ok).toBe(true);
    signAndValidate(kob, plan.built!, [MAKER.sk]);
    const s = decodeSigning({ kob, built: plan.built!, maker: MAKER.pk, registry: null, expected: plan.expected, nodeInputs: nodeFactsOf(plan.built!) });
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('cancel');
    expect(plan.tokensReturned).toBe(BigInt(snap.custody!.state.amount));
    expect(s.net.kas.toOthers).toBe(0n);
  });
});

describe('cancel of an unbound curve (made in its token genesis)', () => {
  it('takes its token from the custody and decodes clean', async () => {
    const { MAKER, signAndValidate, nodeFactsOf, snapshotOfRecovered } = await import('../testing/chain-fixtures');
    const { planCancel } = await import('./cancel');
    const { decodeSigning } = await import('./decode');
    const sell = 900n * SCALE;
    const t = curveTable(kob, { amount: sell, levels: 10, total: 100n * SCALE, multipleMilli: 5000n, scale: SCALE });
    const plan = issueRaw(kob, {
      name: 'Curve Token', ticker: 'CRV', decimals: 8, supply: (1000n * SCALE).toString(),
      holders: [{ owner: ZERO, ownerScheme: 4, amount: sell.toString() }, { owner: MAKER.pk, ownerScheme: 0, amount: (100n * SCALE).toString() }],
      funding: [{ transactionId: '07'.repeat(32), index: 1, amount: (500n * SCALE).toString(), blockDaaScore: '5000', pubkey: MAKER.pk }],
      network: 'testnet-10',
      curve: { custody: 0, levelSize: t.levelSize.toString(), table: t.table.map(String), minFill: '1' },
    } as never);
    const rec = kob.recoverOrders(plan.built.tx)[0]!;
    const snap0 = snapshotOfRecovered(rec);
    // the node lists the custody under the token's covenant id
    const snap = { ...snap0, custody: { ...snap0.custody!, covenantId: plan.token.covenantId } };
    const c = planCancel({ kob, maker: MAKER.pk, funding: [], tokenUtxos: [], clock: { daa: 78_000_000n } }, snap);
    expect(c.issues.filter((i) => i.severity === 'error')).toEqual([]);
    expect(c.ok).toBe(true);
    signAndValidate(kob, c.built!, [MAKER.sk]);
    const s = decodeSigning({ kob, built: c.built!, maker: MAKER.pk, registry: null, expected: c.expected, nodeInputs: nodeFactsOf(c.built!) });
    expect(s.blocking).toEqual([]);
    expect(c.tokensReturned).toBe(sell);
    // its custody is its own tokens, not strays of another token (R4B-I4)
    const sp = s.spends.find((x) => x.covenantId === snap.covenantId)!;
    expect(sp.otherStrays).toEqual([]);
    expect(sp.strays).toBe(0n);
    expect(sp.tokensReleased).toBe(sell);
    expect([...s.info, ...s.warnings].map((f) => f.code)).not.toContain('other-strays-swept');

    // a stray of its own token (sent to the curve's id before its first fill) is swept by the same
    // cancel, never left behind
    const { strayFor } = await import('../testing/chain-fixtures');
    const { splitStrayViews } = await import('./cancel');
    const stray = { ...strayFor(snap, 5n * SCALE), covenantId: plan.token.covenantId };
    const view = { txid: stray.transactionId, index: stray.index, token: plan.token.covenantId, owner: snap.covenantId, amount: stray.state.amount, value: stray.amount, role: 'stray', created_daa: 1200, spent: false, spent_txid: null, state: stray.state, program: 'KCC20PublicMint', confirmations: 1, settled: true } as never;
    expect(splitStrayViews([view], snap.order.state).own).toHaveLength(0);
    expect(splitStrayViews([view], snap.order.state, plan.token.covenantId).own).toHaveLength(1);
    const withStray = planCancel({ kob, maker: MAKER.pk, funding: [], tokenUtxos: [], clock: { daa: 78_000_000n } }, { ...snap, strays: [stray] });
    expect(withStray.issues.filter((i) => i.severity !== 'info').map((i) => i.code)).toEqual([]);
    expect(withStray.tokensReturned).toBe(sell + 5n * SCALE);
    signAndValidate(kob, withStray.built!, [MAKER.sk]);
    const s2 = decodeSigning({ kob, built: withStray.built!, maker: MAKER.pk, registry: null, expected: withStray.expected, nodeInputs: nodeFactsOf(withStray.built!) });
    expect(s2.blocking).toEqual([]);

    // a stray of ANOTHER token with exactly the custody's amount does not take the custody's place:
    // the own token is the custody's, the output of the curve's own transaction (R5B-I1)
    const { parseRegistry } = await import('./registry');
    const { tradableRegistryJson, ZERO32 } = await import('../testing/chain-fixtures');
    const EIGHT = { covenantId: '80'.repeat(32), program: 'KCC20Ref_8x8' as const };
    const alien = {
      token: EIGHT,
      utxos: [{
        transactionId: 'a1'.repeat(32), index: 0, amount: (5n * SCALE).toString(), blockDaaScore: '1300', covenantId: EIGHT.covenantId,
        state: { amount: sell.toString(), owner: snap.covenantId, owner_scheme: 4, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: 'cc'.repeat(32) },
      }],
    };
    const withAlien = planCancel({ kob, maker: MAKER.pk, funding: [], tokenUtxos: [], clock: { daa: 78_000_000n } }, { ...snap, foreign: [alien] } as never);
    expect(withAlien.ok).toBe(true);
    signAndValidate(kob, withAlien.built!, [MAKER.sk]);
    const s3 = decodeSigning({ kob, built: withAlien.built!, maker: MAKER.pk, registry: parseRegistry(tradableRegistryJson(), { kob }), expected: withAlien.expected, nodeInputs: nodeFactsOf(withAlien.built!) });
    expect(s3.blocking).toEqual([]);
    const sp3 = s3.spends.find((x) => x.covenantId === snap.covenantId)!;
    expect(sp3.tokensReleased).toBe(sell);
    expect(sp3.strays).toBe(0n);
    expect(sp3.otherStrays.map((x) => [x.ref.covenantId, x.amount, x.foreign])).toEqual([[EIGHT.covenantId, sell, true]]);
    // the same with the stray put first among the inputs, where the amount alone picked it (the
    // swapped transaction is not signable; only the wording of its spend is read)
    const owned = s3.inputs.filter((i) => i.token?.escrowOf === snap.covenantId).map((i) => i.index);
    expect(owned.map((k) => s3.inputs[k]!.token!.ref.covenantId)).toEqual([plan.token.covenantId, EIGHT.covenantId]);
    const [a, b] = owned as [number, number];
    const swapped = JSON.parse(JSON.stringify(withAlien.built!)) as typeof withAlien.built & object;
    [swapped.tx.inputs[a], swapped.tx.inputs[b]] = [swapped.tx.inputs[b]!, swapped.tx.inputs[a]!];
    [swapped.roles[a], swapped.roles[b]] = [swapped.roles[b]!, swapped.roles[a]!];
    const s4 = decodeSigning({ kob, built: swapped, maker: MAKER.pk, registry: parseRegistry(tradableRegistryJson(), { kob }), expected: withAlien.expected, nodeInputs: nodeFactsOf(swapped) });
    const sp4 = s4.spends.find((x) => x.covenantId === snap.covenantId)!;
    expect(sp4.tokensReleased).toBe(sell);
    expect(sp4.otherStrays.map((x) => [x.ref.covenantId, x.amount, x.foreign])).toEqual([[EIGHT.covenantId, sell, true]]);
  });
});

describe('amend of a curve', () => {
  it('re-spreads the levels left from the price now to the last, never falling', async () => {
    const { amendCurveTable } = await import('./curve');
    const t = amendCurveTable({ total: 100n, amountLeft: 55n, levelSize: 10n, now: 1000n, last: 2000n })!;
    expect(t.table).toHaveLength(10);
    // levels 0..4 are sold (they keep the price now), 4 is current, 9 is last
    expect(t.table.slice(0, 5)).toEqual([1000n, 1000n, 1000n, 1000n, 1000n]);
    expect(t.table[9]).toBe(2000n);
    for (let i = 1; i < 10; i++) expect(t.table[i]! >= t.table[i - 1]!).toBe(true);
    expect(amendCurveTable({ total: 100_000n, amountLeft: 1n, levelSize: 1n, now: 1n, last: 2n })).toBeNull();
    expect(amendCurveTable({ total: 100n, amountLeft: 50n, levelSize: 10n, now: 5n, last: 4n })).toBeNull();
  });

  it('is in place (the custody stays), validates in the script engine, and binds an unbound curve', async () => {
    const { MAKER, keyUtxo, signAndValidate, nodeFactsOf, snapshotOfRecovered, placeGolden } = await import('../testing/chain-fixtures');
    const { planCancelReplace } = await import('./cancel');
    const { decodeSigning } = await import('./decode');
    const { buildReplacement } = await import('../ui/orders/amend');
    const env = { kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * SCALE, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } };
    // bound
    const snap = placeGolden(kob, 'create.curve').snapshots[0]!;
    const s = (snap.order.state as Extract<typeof snap.order.state, { kind: 'KobCurveAsk' }>).state;
    const a = buildReplacement(snap, 'curve', { curve: { now: BigInt(s.table[0]!) * 2n, last: BigInt(s.table[0]!) * 6n, minFill: 3n }, tip: 0n });
    expect(a.ok).toBe(true);
    const plan = planCancelReplace(env, snap, a.spec!);
    expect(plan.ok).toBe(true);
    expect(plan.issues.map((i) => i.code)).toContain('cancel.amend-in-place');
    expect(plan.tokensReturned).toBe(0n);
    signAndValidate(kob, plan.built!, [MAKER.sk]);
    expect(decodeSigning({ kob, built: plan.built!, maker: MAKER.pk, registry: null, expected: plan.expected, nodeInputs: nodeFactsOf(plan.built!) }).blocking).toEqual([]);
    // unbound: the amend names the custody's token
    const sell = 900n * SCALE;
    const t = curveTable(kob, { amount: sell, levels: 10, total: 100n * SCALE, multipleMilli: 5000n, scale: SCALE });
    const g = issueRaw(kob, {
      name: 'Curve Token', ticker: 'CRV', decimals: 8, supply: (1000n * SCALE).toString(),
      holders: [{ owner: ZERO, ownerScheme: 4, amount: sell.toString() }, { owner: MAKER.pk, ownerScheme: 0, amount: (100n * SCALE).toString() }],
      funding: [{ transactionId: '07'.repeat(32), index: 1, amount: (500n * SCALE).toString(), blockDaaScore: '5000', pubkey: MAKER.pk }],
      network: 'testnet-10',
      curve: { custody: 0, levelSize: t.levelSize.toString(), table: t.table.map(String), minFill: '1' },
    } as never);
    const u0 = snapshotOfRecovered(kob.recoverOrders(g.built.tx)[0]!);
    const u = { ...u0, custody: { ...u0.custody!, covenantId: g.token.covenantId } };
    const b = buildReplacement(u, 'curve', { curve: { levelSize: 50n * SCALE } });
    expect(b.ok).toBe(true);
    expect((b.spec!.order.state as { tokenCovId: string }).tokenCovId).toBe(g.token.covenantId);
    const p2 = planCancelReplace(env, u, b.spec!);
    expect(p2.ok).toBe(true);
    expect(p2.issues.map((i) => i.code)).toContain('cancel.amend-in-place');
    signAndValidate(kob, p2.built!, [MAKER.sk]);
    // the plan states the custody's token; the decoder checks the amend names it
    expect(p2.expected.binds).toEqual({ [u.covenantId]: g.token.covenantId });
    const dec = (binds: Record<string, string>) => decodeSigning({ kob, built: p2.built!, maker: MAKER.pk, registry: null, expected: { ...p2.expected, binds }, nodeInputs: nodeFactsOf(p2.built!) }).blocking.map((x) => x.code);
    expect(dec({ [u.covenantId]: g.token.covenantId })).not.toContain('curve-terms-invalid');
    expect(dec({ [u.covenantId]: '77'.repeat(32) })).toContain('curve-terms-invalid');
    expect(dec({})).toContain('curve-terms-invalid');
    // a minimum fill beyond one fill's reach is refused
    expect(buildReplacement(u, 'curve', { curve: { minFill: 8n * BigInt(t.levelSize) } }).issues.map((i) => i.code)).toContain('curve-min-fill-reach');
  });
});

describe('an unbound curve on the node (its custody the genesis output of its token)', () => {
  // the genesis on a fake node, and the indexer's view of its curve: the custody under the token's
  // covenant id
  async function world() {
    const { FakeChain, orderViewOf, snapshotOfRecovered, tokenUtxoView } = await import('../testing/chain-fixtures');
    const g = genesisWithCurve();
    const chain = new FakeChain();
    chain.applyTx(g.built.tx);
    const snap0 = snapshotOfRecovered(kob.recoverOrders(g.built.tx)[0]!);
    const token = g.token.covenantId;
    const custody = tokenUtxoView({ ...snap0.custody!, covenantId: token }, 'custody', { token });
    const view = orderViewOf(kob, snap0, { side: 1, token, custody: { expected_amount: snap0.custody!.state.amount, utxo: custody, ok: true } });
    return { g, chain, snap0, token, view };
  }

  it('is confirmed under its custody token and cancelled (the snapshot check named the zero token before)', async () => {
    const { testAddress, signAndValidate, nodeFactsOf } = await import('../testing/chain-fixtures');
    const { planCancel, snapshotFromOrderView } = await import('./cancel');
    const { confirmSnapshotOnNode } = await import('./node-verify');
    const { decodeSigning } = await import('./decode');
    const w = await world();
    const confirmed = await confirmSnapshotOnNode(kob, w.chain, snapshotFromOrderView(w.view), testAddress);
    expect(confirmed.custody?.covenantId).toBe(w.token);
    expect(confirmed.custody?.transactionId).toBe(w.g.built.tx.id);
    const c = planCancel({ kob, maker, funding: [], tokenUtxos: [], clock: { daa: 78_000_000n } }, confirmed);
    expect(c.issues.filter((i) => i.severity === 'error')).toEqual([]);
    expect(c.ok).toBe(true);
    expect(c.tokensReturned).toBe(900n * SCALE);
    signAndValidate(kob, c.built!, ['33'.repeat(32)]);
    const s = decodeSigning({ kob, built: c.built!, maker, registry: null, expected: c.expected, nodeInputs: nodeFactsOf(c.built!) });
    expect(s.blocking).toEqual([]);
  });

  it('a custody from another transaction (a look-alike of another token at the same script) is neither confirmed nor cancelled', async () => {
    const { testAddress } = await import('../testing/chain-fixtures');
    const { planCancel, snapshotFromOrderView } = await import('./cancel');
    const { confirmSnapshotOnNode } = await import('./node-verify');
    const w = await world();
    const other = 'cd'.repeat(32);
    const out0 = w.g.built.tx.outputs[0]!;
    w.chain.add({ transactionId: 'ab'.repeat(32), index: 0, amount: out0.value, scriptPublicKey: out0.scriptPublicKey, covenantId: other });
    const claimed = snapshotFromOrderView(w.view);
    const fake = { ...claimed, custody: { ...claimed.custody!, transactionId: 'ab'.repeat(32), index: 0, covenantId: other } };
    await expect(confirmSnapshotOnNode(kob, w.chain, fake, testAddress)).rejects.toMatchObject({ code: 'node-mismatch' });
    const c = planCancel({ kob, maker, funding: [], tokenUtxos: [], clock: { daa: 78_000_000n } }, fake);
    expect(c.ok).toBe(false);
    expect(c.issues.map((i) => i.code)).toContain('cancel.custody-missing');
  });
});

describe('an unbound curve after its first fill, from node data only (R3A-2)', () => {
  it('is found bound to the token its record keeps, with its custody (negative: an older record without the token is not)', async () => {
    const { FakeChain, testAddress } = await import('../testing/chain-fixtures');
    const { resolveRecord } = await import('./records');
    const { custodyState } = await import('./token-state');
    const g = genesisWithCurve();
    const rec = recordsFromBuilt(kob, g.built, { maker, network: 'testnet-10', placedAtUnix: 1, placedAtDaa: 2 })[0]!;
    const token = g.token.covenantId;
    const st = kob.decodeState('KobCurveAsk', rec.state);
    const left = 900n * SCALE - 1000n;
    const cont = { ...st, state: { ...st.state, tokenCovId: token, amountLeft: left.toString() } } as never;
    const chain = new FakeChain();
    chain.add({ transactionId: 'cd'.repeat(32), index: 0, amount: '100000000', scriptPublicKey: kob.scriptPublicKey(cont), covenantId: rec.covenantId });
    const placed = rec.custody!.tokenState!;
    const cst = custodyState('kcc20', left, rec.covenantId, (placed as { extension_commitment: string }).extension_commitment);
    chain.add({ transactionId: 'cd'.repeat(32), index: 1, amount: '100000000', scriptPublicKey: kob.tokenScriptPublicKey(kob.templates().find((t) => t.hash === (kob.decodeState('KobCurveAsk', rec.state).state as unknown as { tokenTplHash: string }).tokenTplHash)!.name as never, cst), covenantId: token });
    const r = await resolveRecord(kob, chain, rec, testAddress);
    expect(r.status).toBe('live');
    expect((r.order!.state.state as unknown as { tokenCovId: string; amountLeft: string })).toMatchObject({ tokenCovId: token, amountLeft: left.toString() });
    expect(r.custody).toMatchObject({ transactionId: 'cd'.repeat(32), index: 1, covenantId: token });
    const { token: _t, ...old } = rec;
    expect((await resolveRecord(kob, chain, old, testAddress)).status).toBe('unknown');
  });
});
