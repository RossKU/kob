// The sell custody the contracts accept (token-state.ts acceptsAsSellCustody): the borrow guard is
// ignored by a retired template only (the current ones pin a zero guard, TPLGEN row 2); everything
// else must match.
import { describe, expect, it } from 'vitest';
import { acceptsAsSellCustody, custodyState } from './token-state';
import { snapshotCheck, outpointKey, type NodeInputFact } from './node-verify';
import type { Kcc20State, TokenState } from './types';
import { loadKobNode } from './wasm.node';
import { placeGolden } from '../testing/chain-fixtures';

const kob = loadKobNode();
const O = 'ab'.repeat(32);
const EXT = 'cd'.repeat(32);
const exact = custodyState('kcc20', 100n, O, EXT) as Kcc20State;

describe('acceptsAsSellCustody', () => {
  it('a retired template ignores the borrow guard only; a current one requires a zero guard', () => {
    expect(acceptsAsSellCustody(exact, { owner: O, amount: 100n, ext: EXT, retired: false })).toBe(true);
    expect(acceptsAsSellCustody({ ...exact, borrow_guard: '11'.repeat(32) }, { owner: O, amount: 100n, ext: EXT, retired: false })).toBe(false);
    expect(acceptsAsSellCustody(custodyState('kron', 5n, O, null), { owner: O, amount: 5n, ext: null, retired: false })).toBe(true);
    expect(acceptsAsSellCustody(exact, { owner: O, amount: 100n, ext: EXT, retired: true })).toBe(true);
    expect(acceptsAsSellCustody({ ...exact, borrow_guard: '11'.repeat(32) }, { owner: O, amount: 100n, ext: EXT, retired: true })).toBe(true);
    expect(acceptsAsSellCustody({ ...exact, borrow_scheme: 1 }, { owner: O, amount: 100n, ext: EXT, retired: true })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, extension_commitment: '99'.repeat(32) }, { owner: O, amount: 100n, ext: EXT, retired: true })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, amount: '99' }, { owner: O, amount: 100n, ext: EXT, retired: true })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, owner_scheme: 0 }, { owner: O, amount: 100n, ext: EXT, retired: true })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, owner: '00'.repeat(32) }, { owner: O, amount: 100n, ext: EXT, retired: true })).toBe(false);
    expect(acceptsAsSellCustody(custodyState('kron', 5n, O, null), { owner: O, amount: 5n, ext: null, retired: true })).toBe(true);
  });

  it('the node check of a current order takes only the exact custody: a claimed guarded one is checked as the exact one', () => {
    const snap = placeGolden(kob, 'create.ask').snapshots[0]!;
    const guarded = { ...(snap.custody!.state as Kcc20State), borrow_guard: '11'.repeat(32) } as TokenState;
    const withGuard = { ...snap, custody: { ...snap.custody!, state: guarded } };
    const check = snapshotCheck(kob, withGuard);
    const facts = new Map<string, NodeInputFact>(check.refs.map((r) => [outpointKey(r.transactionId, r.index), { amount: 1_000n, scriptPublicKey: r.scriptPublicKey, covenantId: r === check.refs[0] ? snap.covenantId : snap.custody!.covenantId! }]));
    const done = check.finish(facts);
    // a current template pins a zero guard: the exact state is the one the node confirms
    expect((done.custody!.state as Kcc20State).borrow_guard).toBe('00'.repeat(32));
    // the node holding the guarded unit under its own script is no custody of a current order
    const c1 = snapshotCheck(kob, withGuard);
    const f1 = new Map<string, NodeInputFact>(c1.refs.map((r) => [outpointKey(r.transactionId, r.index), { amount: 1_000n, scriptPublicKey: r.scriptPublicKey, covenantId: r === c1.refs[0] ? snap.covenantId : snap.custody!.covenantId! }]));
    const k1 = c1.refs.find((r) => r.transactionId === snap.custody!.transactionId && r.index === snap.custody!.index)!;
    f1.set(outpointKey(k1.transactionId, k1.index), { amount: 1_000n, scriptPublicKey: kob.tokenScriptPublicKey('KCC20Ref', guarded), covenantId: snap.custody!.covenantId! });
    expect(() => c1.finish(f1)).toThrow(/custody/);
    // a custody of another class is still no custody
    const other = { ...snap, custody: { ...snap.custody!, state: { ...guarded, extension_commitment: '99'.repeat(32) } as TokenState } };
    const c2 = snapshotCheck(kob, other);
    const f2 = new Map<string, NodeInputFact>(c2.refs.map((r) => [outpointKey(r.transactionId, r.index), { amount: 1_000n, scriptPublicKey: r.scriptPublicKey, covenantId: r === c2.refs[0] ? snap.covenantId : snap.custody!.covenantId! }]));
    const k = c2.refs.find((r) => r.transactionId === snap.custody!.transactionId && r.index === snap.custody!.index)!;
    // the node holds the unit of the other class under its own script, not the exact one's
    f2.set(outpointKey(k.transactionId, k.index), { amount: 1_000n, scriptPublicKey: kob.tokenScriptPublicKey('KCC20Ref', other.custody.state), covenantId: snap.custody!.covenantId! });
    expect(() => c2.finish(f2)).toThrow(/custody/);
  });
});

// Since the pre-mainnet templates (TPLGEN row 2) a current order pins a zero guard: kob-protocol
// refuses to release a guarded unit as its custody. A retired order's cancel still takes one
// (kob-protocol tests/custody_class.rs).
describe('a cancel releasing a custody with a borrow guard', () => {
  it('is refused for an order of the current templates', async () => {
    const { planCancel } = await import('./cancel');
    const { MAKER, keyUtxo } = await import('../testing/chain-fixtures');
    const snap = placeGolden(kob, 'create.ask').snapshots[0]!;
    const guarded = { ...(snap.custody!.state as Kcc20State), borrow_guard: '11'.repeat(32) } as TokenState;
    const p = planCancel({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10_000_000_000n, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } }, { ...snap, custody: { ...snap.custody!, state: guarded } });
    expect(p.ok).toBe(false);
    expect(JSON.stringify(p.issues)).toMatch(/not exactly the one its order pins/);
  });
});

describe('both an exact and a guarded unit: the exact one is the custody, whatever the indexer names', () => {
  it('the indexer names the guarded one: the node check takes the exact one, the guarded one leaves as a twin', () => {
    const snap = placeGolden(kob, 'create.ask').snapshots[0]!;
    const exact = snap.custody!;
    const guarded = { ...exact, transactionId: '5a'.repeat(32), index: 3, state: { ...(exact.state as Kcc20State), borrow_guard: '11'.repeat(32) } as TokenState };
    const named = { ...snap, custody: guarded, strays: [exact] };
    const check = snapshotCheck(kob, named);
    const facts = new Map<string, NodeInputFact>(check.refs.map((r) => [outpointKey(r.transactionId, r.index), { amount: 1_000n, scriptPublicKey: r.scriptPublicKey, covenantId: r === check.refs[0] ? snap.covenantId : exact.covenantId! }]));
    const done = check.finish(facts);
    expect(done.custody!.transactionId).toBe(exact.transactionId);
    expect(done.strays.map((u) => u.transactionId)).toEqual([guarded.transactionId]);
  });
});
