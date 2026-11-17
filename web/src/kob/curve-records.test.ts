// A curve created in its token's genesis gets a placement record from the genesis alone (kob-wasm `recoverOrders` over the KOB1 payload): the
// order unbound (tokenCovId zero), its custody the genesis token output it owns. The curve's facts and the orders list read it.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { pubkeyOf } from '../testing/local-signer';
import { issueRaw } from './issue';
import { curveTable } from './curve';
import { asOrderState, curveFactsOf, describeOrder, holdsTokens, isOrderKind, sideOfKind } from './order-facts';
import { recordsFromBuilt } from './records';

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
    const st = asOrderState(kob.decodeState('KobCurveAsk', r.state))!;
    const c = curveFactsOf(st)!;
    expect(c).toMatchObject({ level: 0, levels: 10, unbound: true, total: 900n * SCALE });
    expect(c.priceNow).toBe(c.table[0]);
    // the description's price is the current level's
    expect(describeOrder(st).price).toBe(c.table[0]);
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

    // a stray of its own token (sent to the curve's id before its first fill) is swept by the same cancel, never left behind
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
  });
});
