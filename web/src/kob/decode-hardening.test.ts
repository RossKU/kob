// Regressions on the pre-sign path: what decodeSigning can and cannot be lied to about.
//   KAS amounts of UNSIGNED covenant inputs are node-confirmed, never taken from the indexer
//   builder-declared facts (covenant ids, token state plainness, fee) are re-derived / bounded
import { describe, expect, it } from 'vitest';
import { planCancel, type CancelEnv, type OrderSnapshot } from './cancel';
import { FEE_ABSOLUTE_MAX, decodeSigning } from './decode';
import { parseRegistry } from './registry';
import type { ActionRequest, BuiltTx, Hex, Kcc20State } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, OTHER, goldenRequest, keyUtxo, nodeFactsOf, placeGolden, tradableRegistryJson } from '../testing/chain-fixtures';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({
  kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 78_000_000n }, ...over,
});

describe('decodeSigning flags every input the node does not confirm', () => {
  const snap: OrderSnapshot = placeGolden(kob, 'create.ask').snapshots[0];
  const real = BigInt(snap.custody!.amount);
  const honest = planCancel(env(), snap);
  const lied: OrderSnapshot = { ...snap, custody: { ...snap.custody!, amount: (real - 4n * KAS).toString() } }; // what a hostile /v1/orders/:id says
  const evil = planCancel(env(), lied);
  const custodyInput = (b: BuiltTx) => b.tx.inputs.findIndex((i) => i.utxo.covenantId === snap.custody!.covenantId);

  it('an honest cancel with node-confirmed inputs decodes cleanly (no unverified warning)', () => {
    const s = decodeSigning({ kob, built: honest.built!, maker: MAKER.pk, registry, expected: honest.expected, nodeInputs: nodeFactsOf(honest.built!) });
    expect(s.blocking).toEqual([]);
    expect(s.warnings.map((w) => w.code)).not.toContain('inputs-unverified');
  });

  it('a custody carrier the indexer understated by 4 KAS is BLOCKING when compared with the node facts (the surplus would be a miner fee)', () => {
    expect(honest.ok && evil.ok).toBe(true);
    // the node holds the real value: facts come from the honest transaction's inputs
    const nodeFacts = nodeFactsOf(honest.built!);
    const s = decodeSigning({ kob, built: evil.built!, maker: MAKER.pk, registry, expected: evil.expected, nodeInputs: nodeFacts });
    expect(s.ok).toBe(false);
    const hit = s.blocking.find((b) => b.code === 'input-unconfirmed');
    expect(hit).toBeTruthy();
    expect(hit!.input).toBe(custodyInput(evil.built!));
  });

  it('an input the node does not know, or knows with another script / covenant id, is blocking', () => {
    const facts = new Map(nodeFactsOf(honest.built!));
    const i = custodyInput(honest.built!);
    const key = `${honest.built!.tx.inputs[i].transactionId}:${honest.built!.tx.inputs[i].index}`;
    const missing = new Map(facts);
    missing.delete(key);
    expect(decodeSigning({ kob, built: honest.built!, maker: MAKER.pk, registry, nodeInputs: missing }).blocking.map((b) => b.code)).toContain('input-unconfirmed');
    const otherScript = new Map(facts);
    otherScript.set(key, { ...facts.get(key)!, scriptPublicKey: '0000aa20' + 'ee'.repeat(32) + '87' });
    expect(decodeSigning({ kob, built: honest.built!, maker: MAKER.pk, registry, nodeInputs: otherScript }).blocking.map((b) => b.code)).toContain('input-unconfirmed');
    const otherCov = new Map(facts);
    otherCov.set(key, { ...facts.get(key)!, covenantId: 'cd'.repeat(32) });
    expect(decodeSigning({ kob, built: honest.built!, maker: MAKER.pk, registry, nodeInputs: otherCov }).blocking.map((b) => b.code)).toContain('input-unconfirmed');
  });

  it('without node facts the decode still works but says the amounts were not checked', () => {
    const s = decodeSigning({ kob, built: honest.built!, maker: MAKER.pk, registry, expected: honest.expected });
    expect(s.warnings.map((w) => w.code)).toContain('inputs-unverified');
  });
});

describe('decodeSigning does not trust builder-declared facts it can recompute', () => {
  const cancelBuilt = (): { built: BuiltTx; expected: ReturnType<typeof planCancel>['expected'] } => {
    const plan = planCancel(env(), placeGolden(kob, 'create.ask').snapshots[0]);
    return { built: clone(plan.built!), expected: plan.expected };
  };
  const dec = (built: BuiltTx, expected?: ReturnType<typeof planCancel>['expected']) => decodeSigning({ kob, built, maker: MAKER.pk, registry, expected, nodeInputs: nodeFactsOf(built) });
  const tokenLeader = (b: BuiltTx) => b.plans.findIndex((p) => p.kind === 'tokenLeader');

  it('tokens sent to a covenant id that only `built.covenants` (self-declared) vouches for are BLOCKED (cancel flows create no order)', () => {
    const { built, expected } = cancelBuilt();
    const li = tokenLeader(built);
    const plan = built.plans[li] as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' }>;
    const outIdx = built.tx.outputs.findIndex((o) => o.covenant?.covenantId === built.tx.inputs[li].utxo.covenantId);
    const dead = 'de'.repeat(32); // a covenant id nobody controls
    const st = clone(plan.nextStates[0]);
    st.owner = dead;
    st.owner_scheme = 4;
    plan.nextStates[0] = st;
    built.tx.outputs[outIdx].scriptPublicKey = kob.tokenScriptPublicKey(plan.template, st);
    built.covenants.push({ outputs: [outIdx], authorizingInput: li, covenantId: dead, template: null });
    const s = dec(built, expected);
    expect(s.ok).toBe(false);
    expect(s.blocking.map((b) => b.code)).toContain('token-to-unknown-owner');
    expect(s.outputs[outIdx].kind).not.toBe('custody');
  });

  it('a maker-owned token output with a borrow guard (someone else may borrow it) is blocking, not plain token change', () => {
    const { built, expected } = cancelBuilt();
    const li = tokenLeader(built);
    const plan = built.plans[li] as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' }>;
    const outIdx = built.tx.outputs.findIndex((o) => o.covenant?.covenantId === built.tx.inputs[li].utxo.covenantId);
    const st = clone(plan.nextStates[0]) as Kcc20State;
    st.borrow_scheme = 1;
    st.borrow_guard = OTHER.pk;
    plan.nextStates[0] = st;
    built.tx.outputs[outIdx].scriptPublicKey = kob.tokenScriptPublicKey(plan.template, st);
    const s = dec(built, expected);
    expect(s.ok).toBe(false);
    expect(s.blocking.map((b) => b.code)).toContain('token-state-unplain');
  });

  it('a plain honest token output is not flagged', () => {
    const { built, expected } = cancelBuilt();
    expect(dec(built, expected).blocking).toEqual([]);
  });

  it('the fee is bounded in absolute terms too: a 1 KAS "fee" on a huge trade is no longer a legal skim', () => {
    const b = clone(goldenBuilt('create.ask'));
    const i = b.tx.outputs.findIndex((o) => o.scriptPublicKey === `000020${MAKER.pk}ac` && !o.covenant && BigInt(o.value) > 2n * KAS);
    expect(i).toBeGreaterThanOrEqual(0);
    b.tx.outputs[i].value = (BigInt(b.tx.outputs[i].value) - KAS).toString();
    b.fee.fee = (BigInt(b.fee.fee) + KAS).toString();
    const s = dec(b);
    expect(s.blocking.map((x) => x.code)).toContain('fee-excessive');
    expect(FEE_ABSOLUTE_MAX < KAS).toBe(true);
  });
});

function goldenBuilt(name: string): BuiltTx {
  return kob.build(goldenRequest<ActionRequest>(name, MAKER.pk));
}

describe('an if-done entry committed exit with another maker (negative result kept as a regression)', () => {
  it('the Rust builder refuses it, so the TS decoder gap (no nested exit-maker check) is not reachable through kob.build', () => {
    const req = goldenRequest<ActionRequest & { order: { state: { exitState: string; maker: Hex } } }>('create.ifdBid', MAKER.pk);
    const exit = req.order.state.exitState;
    const evilReq = clone(req);
    evilReq.order.state.exitState = exit.split(MAKER.pk).join(OTHER.pk);
    expect(() => kob.build(evilReq)).toThrow(/exit must be the maker's order of the same token/);
  });
});
