// The sell custody the contracts accept (token-state.ts acceptsAsSellCustody): the borrow guard is
// ignored, as the order covenants ignore it; everything else must match.
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
  it('ignores the borrow guard only', () => {
    expect(acceptsAsSellCustody(exact, { owner: O, amount: 100n, ext: EXT })).toBe(true);
    expect(acceptsAsSellCustody({ ...exact, borrow_guard: '11'.repeat(32) }, { owner: O, amount: 100n, ext: EXT })).toBe(true);
    expect(acceptsAsSellCustody({ ...exact, borrow_scheme: 1 }, { owner: O, amount: 100n, ext: EXT })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, extension_commitment: '99'.repeat(32) }, { owner: O, amount: 100n, ext: EXT })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, amount: '99' }, { owner: O, amount: 100n, ext: EXT })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, owner_scheme: 0 }, { owner: O, amount: 100n, ext: EXT })).toBe(false);
    expect(acceptsAsSellCustody({ ...exact, owner: '00'.repeat(32) }, { owner: O, amount: 100n, ext: EXT })).toBe(false);
    expect(acceptsAsSellCustody(custodyState('kron', 5n, O, null), { owner: O, amount: 5n, ext: null })).toBe(true);
  });

  it('the node check takes a custody with a borrow guard, confirmed under its own script', () => {
    const snap = placeGolden(kob, 'create.ask').snapshots[0]!;
    const guarded = { ...(snap.custody!.state as Kcc20State), borrow_guard: '11'.repeat(32) } as TokenState;
    const withGuard = { ...snap, custody: { ...snap.custody!, state: guarded } };
    const check = snapshotCheck(kob, withGuard);
    const facts = new Map<string, NodeInputFact>(check.refs.map((r) => [outpointKey(r.transactionId, r.index), { amount: 1_000n, scriptPublicKey: r.scriptPublicKey, covenantId: r === check.refs[0] ? snap.covenantId : snap.custody!.covenantId! }]));
    const done = check.finish(facts);
    expect((done.custody!.state as Kcc20State).borrow_guard).toBe('11'.repeat(32));
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

describe('a cancel releasing a custody with a borrow guard', () => {
  it('builds and decodes with no custody finding (the maker gets a plain unit)', async () => {
    const { planCancel } = await import('./cancel');
    const { decodeSigning } = await import('./decode');
    const { MAKER, keyUtxo } = await import('../testing/chain-fixtures');
    const snap = placeGolden(kob, 'create.ask').snapshots[0]!;
    const guarded = { ...(snap.custody!.state as Kcc20State), borrow_guard: '11'.repeat(32) } as TokenState;
    const p = planCancel({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 10_000_000_000n, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } }, { ...snap, custody: { ...snap.custody!, state: guarded } });
    expect(p.ok, JSON.stringify(p.issues)).toBe(true);
    const s = decodeSigning({ kob, built: p.built!, maker: MAKER.pk, registry: null });
    expect(s.blocking.map((b) => b.code)).toEqual([]);
  });
});
