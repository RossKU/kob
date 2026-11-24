// The order actions against real kob-wasm plans: every built transaction is signed locally,
// finalized and validated by the script engine.
import { describe, expect, it } from 'vitest';
import type { Services } from '../../app/services';
import type { IndexerApi } from '../../data/indexer';
import type { StrayView } from '../../data/indexer-types';
import { SnapshotError, planCancel, planCancelReplace, type CancelEnv, type CancelPlan } from '../../kob/cancel';
import type { AskState, Kcc20State, TokenUtxo } from '../../kob/types';
import { MemoryRecordStore, recordsFromBuilt } from '../../kob/records';
import { parseRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import type { ActionRequest } from '../../kob/types';
import { FakeChain, MAKER, TOKEN, goldenRequest, keyUtxo, orderViewOf, placeGolden, signAndValidate, snapshotOfRecovered, strayFor, testAddress, tokenUtxoView, tradableRegistryJson } from '../../testing/chain-fixtures';
import { abandoningOrders, abandonsStrays, describeActionError, dropRecords, lacksHeldUtxo, lostStrays, markCancelling, planAmendFor, planCancelFor, planCancelMany, planRefundFor, planSweepFor, snapshotFor, strandedCount, straysUnchecked, straysCutLeft, strayToTokenUtxo } from './actions';
import { buildEntries, describeEntry, type OrderEntry } from './orders-model';

const kob = loadKobNode();
const KAS = 100_000_000n;
const registry = parseRegistry(tradableRegistryJson(), { kob });

// The actions resolve records through the SDK address helper: swap it for the tests' address
// stand-in.
import * as data from './orders-data';
import { vi } from 'vitest';
vi.spyOn(data, 'spkToAddressFor').mockReturnValue(testAddress);

function services(o: { chain?: FakeChain; indexer?: Partial<IndexerApi> | null; daa?: bigint }): Services {
  const chain = o.chain ?? new FakeChain();
  return {
    kob,
    config: { network: 'testnet-10' },
    sdk: {} as never,
    registry,
    node: {
      kind: 'http-mock',
      getClock: async () => ({ daa: o.daa ?? 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 }),
      getUtxosByAddresses: (a: string[]) => chain.getUtxosByAddresses(a),
    },
    utxos: { fundingFor: async () => [keyUtxo(MAKER.pk, 100n * KAS, 201)] },
    tracker: { tokenUtxosFor: async () => [] },
    indexer: o.indexer === null ? null : ({ order: async () => null, ...o.indexer } as IndexerApi),
  } as unknown as Services;
}

const place = (name: string) => {
  const p = placeGolden(kob, name);
  const [rec] = recordsFromBuilt(kob, p.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_999_000n, placedAtDaa: 900n });
  return { p, rec, snap: p.snapshots[0] };
};
/**
 * A golden placement with a smaller amount left: the node alone proves a record spent only when its
 * search over the continuation amounts is exhaustive (every base unit up to the candidate budget,
 * records.ts). The 10 TST golden ask (10,000 base units) is beyond it and resolves 'unknown'
 * without the indexer.
 */
const placeSmall = (name: string, amountLeft: string) => {
  const request = goldenRequest<ActionRequest>(name, MAKER.pk);
  (request as unknown as { order: { state: Record<string, string> } }).order.state.amountLeft = amountLeft;
  const built = kob.build(request);
  const signed = signAndValidate(kob, built, [MAKER.sk]);
  const recovered = kob.recoverOrders(signed.tx);
  const p: ReturnType<typeof placeGolden> = { request, built, signed, txid: signed.tx.id, recovered, snapshots: recovered.map((r) => snapshotOfRecovered(r, 1000n)) };
  const [rec] = recordsFromBuilt(kob, p.built, { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_789_999_000n, placedAtDaa: 900n });
  return { p, rec, snap: p.snapshots[0] };
};

const chainOf = (...ps: ReturnType<typeof place>[]) => {
  const c = new FakeChain();
  ps.forEach((x) => c.applyTx(x.p.signed.tx));
  return c;
};
const withStrays = (c: FakeChain, strays: TokenUtxo[]): FakeChain => {
  for (const x of strays) c.add({ transactionId: x.transactionId, index: x.index, amount: x.amount, scriptPublicKey: kob.tokenScriptPublicKey('KCC20Ref', x.state as Kcc20State), covenantId: x.covenantId });
  return c;
};
const entryOf = (x: ReturnType<typeof place>, view = true, record = true): OrderEntry => buildEntries(view ? [orderViewOf(kob, x.snap)] : [], record ? [x.rec] : [])[0];

function validate(plan: CancelPlan) {
  expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
  expect(plan.ok).toBe(true);
  signAndValidate(kob, plan.built!, [MAKER.sk]);
}

describe('snapshotFor', () => {
  it('uses the indexer single-order view (custody + strays included)', async () => {
    const a = place('create.ask');
    const stray = strayFor(a.snap, 5n);
    const view = orderViewOf(kob, { ...a.snap, strays: [stray] });
    const s = services({ chain: withStrays(chainOf(a), [stray]), indexer: { order: async () => view } });
    const snap = await snapshotFor(s, entryOf(a));
    expect(snap.source).toBe('indexer');
    expect(snap.custody).not.toBeNull();
    expect(snap.strays).toHaveLength(1);
  });

  it('falls back to the placement record resolved on the node when the indexer does not know the order', async () => {
    const a = place('create.ask');
    const s = services({ chain: chainOf(a), indexer: { order: async () => null } });
    const snap = await snapshotFor(s, entryOf(a, false, true));
    expect(snap.source).toBe('record');
    expect(snap.custody).not.toBeNull();
  });

  it('takes strays for a record-resolved order from the indexer stray list', async () => {
    const a = place('create.ask');
    const stray = strayFor(a.snap, 9n);
    const sv: StrayView = { ...tokenUtxoView(stray, 'stray'), order_status: 'open', maker: MAKER.pk, lost: false, owner: a.rec.covenantId };
    const snap = await snapshotFor(services({ chain: withStrays(chainOf(a), [stray]), indexer: null }), entryOf(a, false, true), [sv]);
    expect(snap.strays).toHaveLength(1);
    expect(snap.strays[0].state.amount).toBe('9');
  });

  it('a per-order token list cut at its page bound marks the strays unknown (R5B-I3)', async () => {
    const a = place('create.ask');
    const stray = strayFor(a.snap, 9n);
    const view = { ...tokenUtxoView(stray, 'stray'), owner: a.rec.covenantId };
    const listed = (truncated: boolean) => async () => Object.assign([view], truncated ? { truncated: true as const } : {});
    const whole = await snapshotFor(services({ chain: withStrays(chainOf(a), [stray]), indexer: { tokenUtxos: listed(false) } }), entryOf(a, false, true), []);
    expect(whole.strays).toHaveLength(1);
    expect(whole.straysUnknown).toBeFalsy();
    const cut = await snapshotFor(services({ chain: withStrays(chainOf(a), [stray]), indexer: { tokenUtxos: listed(true) } }), entryOf(a, false, true), []);
    expect(cut.strays).toHaveLength(1);
    expect(cut.straysUnknown).toBe(true);
  });

  it('refuses an order that is gone or that nothing can locate', async () => {
    // gone: the node search over every continuation amount finds nothing (an order small enough to
    // search exhaustively).
    const a = placeSmall('create.ask', '1000');
    await expect(snapshotFor(services({}), entryOf(a, false, true))).rejects.toMatchObject({ code: 'not-live' });
    // a larger order the node cannot search exhaustively: its state is unknown, never claimed gone
    await expect(snapshotFor(services({}), entryOf(place('create.ask'), false, true))).rejects.toMatchObject({ code: 'state-unknown' });
    await expect(snapshotFor(services({}), { id: 'ab'.repeat(32), view: null, record: null, resolved: null })).rejects.toBeInstanceOf(SnapshotError);
    expect(describeActionError(new SnapshotError('not-live', 'x'))).toContain('no longer on chain');
  });

  it('rebuilds a stray token UTXO only when the extension commitment is known', () => {
    const a = place('create.ask');
    const sv = { ...tokenUtxoView(strayFor(a.snap, 1n), 'stray'), state: undefined } as unknown as StrayView;
    expect(strayToTokenUtxo(sv, null)).toBeNull();
    expect(strayToTokenUtxo(sv, TOKEN.ext)?.state).toMatchObject({ owner_scheme: 4, extension_commitment: TOKEN.ext, amount: '1' });
  });
});

describe('snapshotFor: every UTXO is re-read from the node', () => {
  it('takes the KAS value of custody and strays from the node, whatever the indexer says', async () => {
    const a = place('create.ask');
    const stray = strayFor(a.snap, 5n);
    const realCustody = BigInt(a.snap.custody!.amount);
    const lied = { ...a.snap, custody: { ...a.snap.custody!, amount: (realCustody - 4n * KAS).toString() }, strays: [{ ...stray, amount: '1' }] };
    const s = services({ chain: withStrays(chainOf(a), [stray]), indexer: { order: async () => orderViewOf(kob, lied) } });
    const snap = await snapshotFor(s, entryOf(a));
    expect(snap.source).toBe('indexer');
    expect(BigInt(snap.custody!.amount)).toBe(realCustody);
    expect(snap.strays[0].amount).toBe(stray.amount);
    expect(BigInt(snap.order.amount)).toBe(BigInt(a.snap.order.amount));
  });

  it('the cancel built from a lying indexer pays the maker what the node holds (no surplus turned into a miner fee)', async () => {
    const a = place('create.ask');
    const real = BigInt(a.snap.custody!.amount);
    const lied = { ...a.snap, custody: { ...a.snap.custody!, amount: (real - 4n * KAS).toString() } };
    const honest = await planCancelFor(services({ chain: chainOf(a), indexer: { order: async () => orderViewOf(kob, a.snap) } }), MAKER.pk, entryOf(a));
    const evil = await planCancelFor(services({ chain: chainOf(a), indexer: { order: async () => orderViewOf(kob, lied) } }), MAKER.pk, entryOf(a));
    const out = (p: CancelPlan) => p.built!.tx.outputs.reduce((x, o) => x + BigInt(o.value), 0n);
    expect(out(evil.plan)).toBe(out(honest.plan));
    expect(evil.plan.built!.tx.inputs.find((i) => i.utxo.covenantId === a.snap.custody!.covenantId)!.utxo.amount).toBe(real.toString());
  });

  it('an indexer that names a wrong outpoint falls back to the node-resolved record; without a record nothing is built', async () => {
    const a = place('create.ask');
    const bad = orderViewOf(kob, { ...a.snap, order: { ...a.snap.order, transactionId: 'ee'.repeat(32) } });
    const withRec = await snapshotFor(services({ chain: chainOf(a), indexer: { order: async () => bad } }), entryOf(a, true, true));
    expect(withRec.source).toBe('record');
    expect(withRec.order.transactionId).toBe(a.snap.order.transactionId);
    await expect(snapshotFor(services({ chain: chainOf(a), indexer: { order: async () => bad } }), entryOf(a, true, false))).rejects.toMatchObject({ code: 'node-mismatch' });
  });

  it('a custody UTXO the node does not hold with that script blocks the action; a stray it does not hold is dropped', async () => {
    const a = place('create.ask');
    const noCustody = new FakeChain();
    noCustody.add({ transactionId: a.snap.order.transactionId, index: a.snap.order.index, amount: a.snap.order.amount, scriptPublicKey: kob.scriptPublicKey(a.snap.order.state), covenantId: a.snap.covenantId });
    await expect(snapshotFor(services({ chain: noCustody, indexer: { order: async () => orderViewOf(kob, a.snap) } }), entryOf(a, true, false))).rejects.toMatchObject({ code: 'node-mismatch' });
    const ghostStray = strayFor(a.snap, 3n, 77);
    const snap = await snapshotFor(services({ chain: chainOf(a), indexer: { order: async () => orderViewOf(kob, { ...a.snap, strays: [ghostStray] }) } }), entryOf(a));
    expect(snap.strays).toEqual([]);
  });
});

describe('a decoy of another class named as the custody is never spent (R8F4-1)', () => {
  /**
   * The golden ask with a decoy on the chain: the same amount, owned by the order, of another class
   * (extension commitment). A hostile indexer names the decoy as the custody and lists the real
   * custody as a stray.
   */
  const decoyed = () => {
    const a = place('create.ask');
    const real = a.snap.custody!;
    const decoy: TokenUtxo = { ...real, transactionId: 'dd'.repeat(32), index: 7, state: { ...(real.state as Kcc20State), extension_commitment: '99'.repeat(32) } };
    const chain = withStrays(chainOf(a), [decoy]);
    const view = orderViewOf(kob, { ...a.snap, custody: decoy, strays: [real] });
    return { a, real, decoy, chain, view };
  };
  const spends = (p: CancelPlan, u: TokenUtxo): boolean => p.built!.tx.inputs.some((i) => i.transactionId === u.transactionId && i.index === u.index);

  it('the node check does not confirm the decoy as the custody: without a record nothing is built', async () => {
    const d = decoyed();
    await expect(snapshotFor(services({ chain: d.chain, indexer: { order: async () => d.view } }), entryOf(d.a, true, false))).rejects.toMatchObject({ code: 'node-mismatch' });
  });

  it('cancel: built with the real custody (the node-resolved record), never with the decoy', async () => {
    const d = decoyed();
    const { plan } = await planCancelFor(services({ chain: d.chain, indexer: { order: async () => d.view } }), MAKER.pk, entryOf(d.a));
    validate(plan);
    expect(spends(plan, d.real)).toBe(true);
    expect(spends(plan, d.decoy)).toBe(false);
  });

  it('cancel-all: the same', async () => {
    const d = decoyed();
    const many = await planCancelMany(services({ chain: d.chain, indexer: { order: async () => d.view } }), MAKER.pk, [entryOf(d.a)]);
    expect(many.plans.length).toBe(1);
    validate(many.plans[0]!);
    expect(spends(many.plans[0]!, d.real)).toBe(true);
    expect(spends(many.plans[0]!, d.decoy)).toBe(false);
    // without a record it stops
    const none = await planCancelMany(services({ chain: d.chain, indexer: { order: async () => d.view } }), MAKER.pk, [entryOf(d.a, true, false)]);
    expect(none.plans).toEqual([]);
    expect(none.failed.map((f) => (f.error as SnapshotError).code)).toEqual(['node-mismatch']);
  });

  it('cancel-replace: the same', async () => {
    const d = decoyed();
    const out = await planAmendFor(services({ chain: d.chain, indexer: { order: async () => d.view } }), MAKER.pk, entryOf(d.a), 'limit', { price: 260_000_000n, amount: 6_000n });
    expect(out.plan).not.toBeNull();
    validate(out.plan!);
    expect(spends(out.plan!, d.real)).toBe(true);
    expect(spends(out.plan!, d.decoy)).toBe(false);
    await expect(planAmendFor(services({ chain: d.chain, indexer: { order: async () => d.view } }), MAKER.pk, entryOf(d.a, true, false), 'limit', { price: 260_000_000n, amount: 6_000n })).rejects.toMatchObject({ code: 'node-mismatch' });
  });
});

describe('strays a cancel would leave behind for good (R8F4-2)', () => {
  it('a stray list the indexer cut never stops the cancel: it goes on, and says strays are left behind (R9B-L1)', async () => {
    const a = place('create.ask');
    const stray = strayFor(a.snap, 9n);
    const view = { ...tokenUtxoView(stray, 'stray'), owner: a.rec.covenantId };
    const cut = async () => Object.assign([view], { truncated: true as const });
    const { plan } = await planCancelFor(services({ chain: withStrays(chainOf(a), [stray]), indexer: { tokenUtxos: cut } }), MAKER.pk, entryOf(a, false, true), []);
    validate(plan);
    expect(straysCutLeft(plan)).not.toBeNull();
    // the stray list read in full: the cancel goes on and sweeps the stray
    const whole = async () => [view];
    validate((await planCancelFor(services({ chain: withStrays(chainOf(a), [stray]), indexer: { tokenUtxos: whole } }), MAKER.pk, entryOf(a, false, true), [])).plan);
  });

  it('a stray of another class: the cancel offers "Sweep first" for it (a sweep moves it while the order lives)', async () => {
    const a = place('create.ask');
    const other: TokenUtxo = { ...strayFor(a.snap, 4n, 93), state: { ...(strayFor(a.snap, 4n, 93).state as Kcc20State), extension_commitment: '77'.repeat(32) } };
    const view = orderViewOf(kob, { ...a.snap, strays: [other] });
    const { plan } = await planCancelFor(services({ chain: withStrays(chainOf(a), [other]), indexer: { order: async () => view } }), MAKER.pk, entryOf(a));
    expect(plan.ok).toBe(true);
    expect(abandonsStrays(plan)).toBe(true);
    expect(abandoningOrders([plan])).toEqual([a.snap.covenantId]);
    expect(strandedCount([plan])).toBe(1);
    expect(lostStrays(plan)).toBe(0);
  });

  it('a stray its maker cannot move at all is counted for the confirmation, which states it before signing', () => {
    const a = place('create.ask');
    const env: CancelEnv = { kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1000n } };
    const plan = planCancel(env, { ...a.snap, foreignUnproven: [{ outpoint: `${'ee'.repeat(32)}:0`, token: 'ef'.repeat(32), amount: 5n }] });
    expect(plan.ok).toBe(true);
    expect(lostStrays(plan)).toBe(1);
    expect(abandonsStrays(plan)).toBe(false);
  });
});

describe('with the indexer down, a maker gets an order back from its record and the node alone (R8B-M1)', () => {
  const down = () => Promise.reject(new TypeError('fetch failed'));
  for (const name of ['create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdAsk', 'create.curve', 'pair.create.ask', 'pair.create.bid']) {
    it(name + ': the cancel is built, and its confirmation says the strays could not be checked', async () => {
      const a = place(name);
      const { plan } = await planCancelFor(services({ chain: chainOf(a), indexer: { order: down, tokenUtxos: down } }), MAKER.pk, entryOf(a, false, true), null);
      validate(plan);
      expect(straysUnchecked(plan)).toBe(true);
      expect(plan.issues.find((i) => i.code === 'cancel.strays-unknown')?.severity).toBe('warning');
    });
  }

  it('a stray list the indexer answered but cut goes on too (nothing an outsider sends stops the maker, R9B-L1)', async () => {
    const a = place('create.ask');
    const stray = strayFor(a.snap, 9n);
    const cut = async () => Object.assign([{ ...tokenUtxoView(stray, 'stray'), owner: a.rec.covenantId }], { truncated: true as const });
    const { plan } = await planCancelFor(services({ chain: withStrays(chainOf(a), [stray]), indexer: { tokenUtxos: cut } }), MAKER.pk, entryOf(a, false, true), []);
    validate(plan);
    expect(straysUnchecked(plan)).toBe(false);
    expect(straysCutLeft(plan)).not.toBeNull();
  });
});

describe('the record path is tried whenever the view lacks a UTXO the state holds (R8FX-1, R8FX2-I1)', () => {
  it('a missing custody, and the missing bought-custody of a Collected bid, send it to the record; a whole view does not', () => {
    const a = place('create.ask');
    expect(lacksHeldUtxo(a.snap)).toBe(false);
    expect(lacksHeldUtxo({ ...a.snap, custody: null })).toBe(true);
    const bid = place('create.bid').snap;
    const st = JSON.parse(JSON.stringify(bid.order.state));
    Object.assign(st.state, { mode: '1', bought: '700' });
    const collected = { ...bid, order: { ...bid.order, state: st } };
    expect(lacksHeldUtxo(collected)).toBe(true);
    expect(lacksHeldUtxo({ ...collected, boughtCustody: strayFor(bid, 700n, 77) })).toBe(false);
  });
});

describe('a sweep never moves the custody of the order it keeps (R8FX-1)', () => {
  it('the indexer leaves the custody out, lists it as a stray and says side 2: the record path finds the custody, and the sweep moves only true strays', async () => {
    const a = place('create.ask');
    const real = a.snap.custody!;
    const stray = strayFor(a.snap, 4n, 92);
    const evil = { ...orderViewOf(kob, { ...a.snap, custody: null, strays: [real, stray] }), side: 2 } as never;
    // the hostile per-owner list names the custody as a stray too
    const listed = [real, stray].map((u) => ({ ...tokenUtxoView(u, 'stray'), owner: a.rec.covenantId }));
    const s = services({ chain: withStrays(chainOf(a), [stray]), indexer: { order: async () => evil, tokenUtxos: async () => listed } });
    const { plan } = await planSweepFor(s, MAKER.pk, entryOf(a));
    validate(plan);
    const spent = plan.built!.tx.inputs.map((i) => `${i.transactionId}:${i.index}`);
    expect(spent).toContain(`${stray.transactionId}:${stray.index}`);
    expect(spent).not.toContain(`${real.transactionId}:${real.index}`);
  });

  it('without a record the snapshot has no custody and the sweep stops', async () => {
    const a = place('create.ask');
    const real = a.snap.custody!;
    const evil = { ...orderViewOf(kob, { ...a.snap, custody: null, strays: [real] }), side: 2 } as never;
    const { plan } = await planSweepFor(services({ chain: chainOf(a), indexer: { order: async () => evil } }), MAKER.pk, entryOf(a, true, false));
    expect(plan.ok).toBe(false);
    expect(plan.issues.map((i) => i.code)).toContain('cancel.custody-missing');
  });

  it('a twin of the custody (the same state at another outpoint) stays; the custody stays; other strays are swept', async () => {
    const a = place('create.ask');
    const real = a.snap.custody!;
    const twin: TokenUtxo = { ...real, transactionId: 'ab'.repeat(32), index: 3 };
    const stray = strayFor(a.snap, 4n, 92);
    const view = orderViewOf(kob, { ...a.snap, strays: [twin, stray] });
    const { plan } = await planSweepFor(services({ chain: withStrays(chainOf(a), [twin, stray]), indexer: { order: async () => view } }), MAKER.pk, entryOf(a));
    validate(plan);
    const spent = plan.built!.tx.inputs.map((i) => `${i.transactionId}:${i.index}`);
    expect(spent).toContain(`${stray.transactionId}:${stray.index}`);
    expect(spent).not.toContain(`${twin.transactionId}:${twin.index}`);
    expect(spent).not.toContain(`${real.transactionId}:${real.index}`);
    expect(plan.issues.map((i) => i.code)).toContain('sweep.twin-stays');
  });
});

describe('snapshotFor: a hostile order state from the indexer', () => {
  it('a view whose committed exit is malformed (wasm trap) does not block the cancel: the node-resolved record path takes over', async () => {
    const a = place('create.ifdBid');
    const evil = JSON.parse(JSON.stringify(a.snap.order.state));
    evil.state.exitState = '000000';
    const view = orderViewOf(kob, { ...a.snap, order: { ...a.snap.order, state: evil } });
    const snap = await snapshotFor(services({ chain: chainOf(a), indexer: { order: async () => view } }), entryOf(a, true, true));
    expect(snap.source).toBe('record');
    // the module keeps working afterwards
    expect(kob.encodeState(a.snap.order.state)).toBe(a.rec.state);
  });
});

describe('cancel', () => {
  it('cancels one order (indexer data) into a consensus-valid transaction that returns tokens and KAS to the maker', async () => {
    const a = place('create.ask');
    const view = orderViewOf(kob, a.snap);
    const { plan } = await planCancelFor(services({ chain: chainOf(a), indexer: { order: async () => view } }), MAKER.pk, entryOf(a));
    validate(plan);
    expect(plan.cancelIds).toEqual([a.rec.covenantId]);
    expect(plan.tokensReturned).toBeGreaterThan(0n);
  });

  it('cancels from the record alone when the indexer is gone', async () => {
    const a = place('create.bid');
    const { plan } = await planCancelFor(services({ chain: chainOf(a), indexer: null }), MAKER.pk, entryOf(a, false, true));
    validate(plan);
  });

  it('cancel-all: several orders of one token in as few transactions as possible; unlocatable ones are reported', async () => {
    const a = place('create.ask');
    const b = place('create.bid');
    const ghost = place('create.condAsk');
    const s = services({
      chain: chainOf(a, b),
      indexer: {
        order: async (id) => (id === a.rec.covenantId ? orderViewOf(kob, a.snap) : id === b.rec.covenantId ? orderViewOf(kob, b.snap) : null),
      },
    });
    const many = await planCancelMany(s, MAKER.pk, [entryOf(a), entryOf(b), { id: ghost.rec.covenantId, view: null, record: null, resolved: null }]);
    expect(many.failed.map((f) => f.id)).toEqual([ghost.rec.covenantId]);
    expect(many.snapshots).toHaveLength(2);
    expect(many.plans.length).toBeGreaterThanOrEqual(1);
    for (const p of many.plans) validate(p);
    expect(many.plans.flatMap((p) => p.cancelIds).sort()).toEqual([a.rec.covenantId, b.rec.covenantId].sort());
  });

  it('nothing to cancel: no plans, no error', async () => {
    const many = await planCancelMany(services({}), MAKER.pk, []);
    expect(many).toEqual({ plans: [], snapshots: [], failed: [] });
  });
});

describe('refund', () => {
  it('is refused with the due DAA before the order is refundable, and valid once it is', async () => {
    const a = place('create.ask');
    const view = orderViewOf(kob, a.snap);
    const early = await planRefundFor(services({ chain: chainOf(a), indexer: { order: async () => view }, daa: 1000n }), MAKER.pk, entryOf(a));
    expect(early.plan.ok).toBe(false);
    expect(early.plan.issues.map((i) => i.code)).toContain('refund.not-yet');
    const due = await planRefundFor(services({ chain: chainOf(a), indexer: { order: async () => view }, daa: 400_000_001n }), MAKER.pk, entryOf(a));
    validate(due.plan);
  });
});

describe('amend', () => {
  it('builds the replacement and the atomic plan (consensus valid): a plain ask keeping its amount is amended in place', async () => {
    const a = place('create.ask');
    const view = orderViewOf(kob, a.snap);
    const s = services({ chain: chainOf(a), indexer: { order: async () => view } });
    const out = await planAmendFor(s, MAKER.pk, entryOf(a), 'limit', { tip: 200_000n });
    expect(out.amend.ok).toBe(true);
    validate(out.plan!);
    expect(out.plan!.request!.action).toBe('amendOrder');
    expect(out.plan!.cancelIds).toEqual([a.snap.covenantId]);
    expect(out.plan!.built!.tx.outputs[0].covenant?.covenantId).toBe(a.snap.covenantId);
  });

  it('returns the coded issues without building when the input is invalid', async () => {
    const a = place('create.ask');
    const s = services({ chain: chainOf(a), indexer: { order: async () => orderViewOf(kob, a.snap) } });
    const out = await planAmendFor(s, MAKER.pk, entryOf(a), 'limit', { amount: 0n });
    expect(out.plan).toBeNull();
    expect(out.amend.issues.map((i) => i.code)).toEqual(['amount-not-positive']);
  });

  it('describes the row of the amended order for the dialog (row model agrees with the state)', async () => {
    const a = place('create.ask');
    const row = describeEntry(entryOf(a), { kob, clock: { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 } });
    expect(row.canAmend).toBe(true);
  });
});

describe('dropRecords', () => {
  it('removes records and swallows storage failures', async () => {
    const a = place('create.ask');
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    await dropRecords(store, [a.rec.covenantId, 'ff'.repeat(32)]);
    expect(await store.list()).toEqual([]);
    const broken = { ...store, remove: async () => { throw new Error('blocked'); } } as unknown as MemoryRecordStore;
    await expect(dropRecords(broken, ['x'])).resolves.toBeUndefined();
  });
});

describe('markCancelling (C5-06, and an in-place amend is not a cancel)', () => {
  const env = (): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1000n } });
  it('marks the record of a cancelled order with the tx and the outpoint it spends', async () => {
    const a = place('create.ask');
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    const plan = planCancel(env(), a.snap);
    await markCancelling(store, plan, 'cc'.repeat(32), 5n);
    expect((await store.get(a.rec.covenantId))!.cancelling).toEqual({ txid: 'cc'.repeat(32), spends: `${a.snap.order.transactionId}:${a.snap.order.index}`, atUnix: '5' });
  });
  it('leaves the record of an order the tx amends in place alone (same covenant id continues)', async () => {
    const a = place('create.ask');
    const store = new MemoryRecordStore();
    await store.put(a.rec);
    const st = a.snap.order.state.state as AskState;
    const plan = planCancelReplace(env(), a.snap, { order: { kind: 'KobAsk', state: { ...st, price: (BigInt(st.price) + 1n).toString() } }, value: BigInt(a.snap.order.amount) });
    expect(plan.request!.action).toBe('amendOrder');
    await markCancelling(store, plan, 'cc'.repeat(32), 5n);
    expect((await store.get(a.rec.covenantId))!.cancelling ?? null).toBeNull();
  });
});

describe('nothing an outsider sends to an order stops its maker from ending it (R9B-L1)', () => {
  it('a view of listed twins with strays_total above the list still cancels, with the note; a sweep has nothing to move', async () => {
    const a = place('create.ask');
    const amount = BigInt((a.snap.custody!.state as Kcc20State).amount);
    const twins = Array.from({ length: 12 }, (_, k) => strayFor(a.snap, amount, 100 + k));
    const view = { ...orderViewOf(kob, { ...a.snap, strays: twins }), strays_total: 101 };
    const s = services({ chain: withStrays(chainOf(a), twins), indexer: { order: async () => view } });
    const { plan } = await planCancelFor(s, MAKER.pk, entryOf(a));
    validate(plan);
    expect(straysCutLeft(plan)).toBe(101 - 12);
    // the twins that fit go with the cancel; the rest are counted apart and no "Sweep first" is offered for them
    expect(plan.issues.some((i) => i.code === 'cancel.twins-abandoned')).toBe(true);
    expect(abandonsStrays(plan)).toBe(false);
    const sweep = await planSweepFor(s, MAKER.pk, entryOf(a));
    expect(sweep.plan.ok).toBe(false);
  });

  it('a refund says it leaves the strays (a cancel returns them, R9B-L2)', async () => {
    const a = place('create.ask');
    const stray = strayFor(a.snap, 4n, 92);
    const { planRefund } = await import('../../kob/cancel');
    const { refundLeavesStrays } = await import('./actions');
    const env: CancelEnv = { kob, maker: MAKER.pk, funding: [], tokenUtxos: [], clock: { daa: 10_000_000_000n } };
    const plan = planRefund(env, { ...a.snap, strays: [stray], straysUnknown: true, straysCut: true, straysBeyond: 200 });
    expect(plan.ok, JSON.stringify(plan.issues)).toBe(true);
    expect(refundLeavesStrays(plan)).toBe(1);
  });
});

describe('standalone: the stream book reads no strays, and a cancel says so (R9B-L3)', () => {
  it('a view marked strays_unread gives a cancel that states the strays could not be checked', async () => {
    const { snapshotFromOrderView } = await import('../../kob/cancel');
    const a = place('create.ask');
    const view = { ...orderViewOf(kob, a.snap), strays: [], strays_unread: true };
    const snap = snapshotFromOrderView(view as never);
    expect(snap.straysUnknown).toBe(true);
    expect(snap.straysCut).toBeUndefined();
    const plan = planCancel({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1000n } }, snap);
    expect(straysUnchecked(plan)).toBe(true);
  });
});
