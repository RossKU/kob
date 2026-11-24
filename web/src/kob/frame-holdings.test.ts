// What the private-key frame can judge without trusting the app (R9FB-1, R9FB-2): every hostile
// transaction here is made from an honest build at the transaction level and decoded as the frame
// decodes it, with no node facts, no planner claims and no registry. Each must block in the frame's
// decode and in its summary; the honest shapes must not.
import { describe, expect, it } from 'vitest';
import { planCancel, planCancelAll, planCancelReplace, planSweep, type CancelEnv, type OrderSnapshot } from './cancel';
import { decodeSigning, type SigningSummary } from './decode';
import type { BuiltTx, Kcc20State, OrderState } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, keyUtxo, placeGolden, strayFor } from '../testing/chain-fixtures';
import { dropTokenInputs, restateMakerOutput, sweepTakingCustody } from '../testing/hostile-tx';
import { summarize } from '../signer/summary';

const kob = loadKobNode();
const KAS = 100_000_000n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const env = (): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } });
const snapOf = (name: string): OrderSnapshot => placeGolden(kob, name).snapshots[0]!;
/** The frame's decode: no node facts, no planner claims, no registry. */
const frame = (b: BuiltTx): SigningSummary => decodeSigning({ kob, built: b, maker: MAKER.pk, registry: null });
const codes = (s: SigningSummary): string[] => s.blocking.map((i) => i.code);
const blocksInFrame = (b: BuiltTx, code: string) => {
  expect(codes(frame(b))).toContain(code);
  const sum = summarize(kob, b, MAKER.pk, []);
  expect(sum.problems).toBeGreaterThan(0);
  expect(sum.problemCodes).toContain(code);
};
const passesInFrame = (b: BuiltTx) => {
  const c = codes(frame(b));
  expect(c.filter((x) => ['custody-unreleased', 'sweep-custody-spent', 'malformed-built', 'sweep-invalid', 'curve-terms-invalid'].includes(x))).toEqual([]);
};
const ownedBy = (cov: string) => (st: Kcc20State) => st.owner === cov;

describe('the action of an order input is its signed entry, not the app\'s label (R9FB-1)', () => {
  it('a cancel without its custody, labelled a fill, blocks: the label disagrees with the entry, and what the order held does not come back', () => {
    const snap = snapOf('create.ask');
    const plan = planCancel(env(), snap);
    const b = dropTokenInputs(kob, plan.built!, MAKER.pk, ownedBy(snap.covenantId));
    const at = b.plans.findIndex((p) => p.kind === 'entry');
    b.roles[at] = 'KobAsk.settle';
    blocksInFrame(b, 'malformed-built');
    blocksInFrame(b, 'custody-unreleased');
  });

  it('two orders cancelled together, the second one\'s custody left out and its input labelled a fill: blocks', () => {
    const a = snapOf('create.ask');
    const c = snapOf('create.condAsk');
    const plans = planCancelAll(env(), [a, c]);
    const both = plans.find((p) => p.ok && p.cancelIds.length === 2);
    expect(both).toBeTruthy();
    const b = dropTokenInputs(kob, both!.built!, MAKER.pk, ownedBy(c.covenantId));
    b.roles = b.roles.map((r, i) => ((b.plans[i] as { kind: string }).kind === 'entry' && r.startsWith('KobCondAsk') ? 'KobCondAsk.settle' : r));
    blocksInFrame(b, 'custody-unreleased');
  });
});

describe('what an ended order held must come back to the wallet, judged on the outputs (R9FB-1)', () => {
  it('a custody of another class than the state pins (the output re-stated under it) blocks', () => {
    const snap = snapOf('create.ask');
    const plan = planCancel(env(), snap);
    blocksInFrame(restateMakerOutput(kob, plan.built!, MAKER.pk, '99'.repeat(32)), 'custody-unreleased');
  });

  it('the wallet\'s own tokens standing in for the custody block', () => {
    const snap = snapOf('create.ask');
    const plan = planCancel(env(), snap);
    const b = clone(plan.built!);
    const k = b.plans.findIndex((p) => (p as { state?: Kcc20State }).state?.owner === snap.covenantId);
    const p = b.plans[k] as unknown as { state: Kcc20State; template: 'KCC20Ref' };
    // the custody input becomes a plain unit of the wallet of the same token, class and amount, signed
    p.state = { ...p.state, owner: MAKER.pk, owner_scheme: 0 };
    b.tx.inputs[k]!.utxo.scriptPublicKey = kob.tokenScriptPublicKey(p.template, p.state);
    b.sign.push({ ...b.sign[0]!, inputIndex: k });
    blocksInFrame(b, 'custody-unreleased');
  });

  it('a cancel-replace keeps the released custody in the new order: it passes', () => {
    const snap = snapOf('create.ask');
    const next = clone(snap.order.state) as OrderState & { state: Record<string, string> };
    next.state.price = '260000000';
    next.state.amountLeft = '6000';
    passesInFrame(planCancelReplace(env(), snap, { order: next, value: 10n * KAS }).built!);
  });
});

describe('an order that continues keeps its custody, judged on the outputs (R9FB-1)', () => {
  it('a sweep that moves the custody, its class claimed as another, blocks in the frame', () => {
    const snap = snapOf('create.ask');
    const stray = strayFor(snap, 4n, 92);
    const honest = planSweep(env(), { ...snap, strays: [stray] });
    passesInFrame(honest.built!);
    const b = sweepTakingCustody(kob, honest.built!, snap, stray);
    // the app claims the custody input is of another class, so a check of the inputs alone misses it
    const k = b.tx.inputs.findIndex((x) => x.transactionId === snap.custody!.transactionId && x.index === snap.custody!.index);
    const p = b.plans[k] as unknown as { state: Kcc20State; template: 'KCC20Ref' };
    p.state = { ...p.state, extension_commitment: '99'.repeat(32) };
    b.tx.inputs[k]!.utxo.scriptPublicKey = kob.tokenScriptPublicKey(p.template, p.state);
    blocksInFrame(b, 'sweep-custody-spent');
  });
});

describe('the honest shapes pass in the frame', () => {
  it('cancel one, cancel-all, cancel-replace (above), sweep (above), a KRON cancel', () => {
    passesInFrame(planCancel(env(), snapOf('create.ask')).built!);
    passesInFrame(planCancel(env(), snapOf('kron.create.ask')).built!);
    passesInFrame(planCancel(env(), snapOf('create.bid')).built!);
    passesInFrame(planCancel(env(), snapOf('pair.create.ask')).built!);
    for (const p of planCancelAll(env(), [snapOf('create.ask'), snapOf('create.condAsk')])) if (p.ok) passesInFrame(p.built!);
  });
});

describe('an unbound curve (placed at its token creation) is neither swept nor bound blind in the frame (R9FB-2)', () => {
  /** The build with its order input re-stated as the same curve unbound (token 0): input script and continuation follow. */
  const unbind = (built: BuiltTx, snap: OrderSnapshot): BuiltTx => {
    const b = clone(built);
    const st = clone(snap.order.state) as OrderState & { state: Record<string, string> };
    st.state.tokenCovId = '00'.repeat(32);
    const at = b.plans.findIndex((p) => p.kind === 'entry');
    (b.plans[at] as unknown as { state: string }).state = kob.encodeState(st);
    const spk = kob.scriptPublicKey(st);
    b.tx.inputs[at]!.utxo.scriptPublicKey = spk;
    return b;
  };

  it('an in-place amend of it blocks in the frame (the token of its custody is the word of the app there)', () => {
    const snap = snapOf('create.curve');
    const next = clone(snap.order.state) as OrderState & { state: Record<string, string> };
    next.state.minFill = String(BigInt(next.state.minFill ?? '1') + 1n);
    const plan = planCancelReplace(env(), snap, { order: next, value: BigInt(snap.order.amount) });
    expect(plan.ok).toBe(true);
    passesInFrame(plan.built!);
    const s = frame(unbind(plan.built!, snap));
    expect(s.blocking.map((x) => x.code)).toContain('curve-terms-invalid');
  });

  it('a sweep of it blocks', () => {
    const snap = snapOf('create.curve');
    const stray = strayFor(snap, 4n, 92);
    const sweep = planSweep(env(), { ...snap, strays: [stray] });
    expect(sweep.ok).toBe(true);
    const b = unbind(sweep.built!, snap);
    const cont = b.tx.outputs.find((o) => o.covenant?.authorizingInput === b.plans.findIndex((p) => p.kind === 'entry'))!;
    cont.scriptPublicKey = b.tx.inputs[b.plans.findIndex((p) => p.kind === 'entry')]!.utxo.scriptPublicKey;
    expect(frame(b).blocking.map((x) => x.code)).toContain('sweep-invalid');
    expect(summarize(kob, b, MAKER.pk, []).problems).toBeGreaterThan(0);
  });
});
