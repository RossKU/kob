// An order-ending spend must release what the order holds (R8F2b-1, R8F2-I4): a cancel or refund
// that ends an order and leaves its custody unspent blocks, in the app (with node facts) and in the
// private-key frame (from the bytes and the order state alone).
import { describe, expect, it } from 'vitest';
import { planCancel, type CancelEnv, type OrderSnapshot } from './cancel';
import { decodeSigning, type SigningSummary } from './decode';
import { parseRegistry } from './registry';
import type { BuiltTx } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, keyUtxo, nodeFactsOf, placeGolden, tradableRegistryJson } from '../testing/chain-fixtures';
import { summarize } from '../signer/summary';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const env = (): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } });
const snapOf = (name: string): OrderSnapshot => placeGolden(kob, name).snapshots[0]!;
const codes = (s: SigningSummary): string[] => s.blocking.map((i) => i.code);

/**
 * The built cancel without the order's custody input(s) and the token outputs that returned them:
 * the shape a hostile app could hand the frame. The fee field follows the change in value.
 */
function withoutCustody(built: BuiltTx, cov: string): BuiltTx {
  const b = clone(built);
  const drop = b.tx.inputs.map((x, i) => [x, i] as const).filter(([, i]) => {
    const st = (b.plans[i] as { state?: unknown }).state;
    return typeof st === 'object' && st !== null && (st as { owner?: string }).owner === cov;
  }).map(([, i]) => i);
  expect(drop.length).toBeGreaterThan(0);
  const keep = <T,>(xs: T[]): T[] => xs.filter((_, i) => !drop.includes(i));
  const outIn = drop.reduce((x, i) => x + BigInt(b.tx.inputs[i]!.utxo.amount), 0n);
  b.tx.inputs = keep(b.tx.inputs);
  b.plans = keep(b.plans);
  b.roles = keep(b.roles);
  // re-index the sign requests of the inputs left
  const map = new Map<number, number>();
  let k = 0;
  built.tx.inputs.forEach((_, i) => { if (!drop.includes(i)) map.set(i, k++); });
  b.sign = b.sign.filter((r) => map.has(r.inputIndex)).map((r) => ({ ...r, inputIndex: map.get(r.inputIndex)! }));
  // the token outputs go too (a token output names its covenant)
  const tokenOuts = b.tx.outputs.filter((o) => o.covenant && o.covenant.covenantId !== cov);
  const outOut = tokenOuts.reduce((x, o) => x + BigInt(o.value), 0n);
  b.tx.outputs = b.tx.outputs.filter((o) => !tokenOuts.includes(o));
  // keep the fee as it was: the KAS of the dropped inputs and outputs nets out on the change
  const change = b.tx.outputs.find((o) => !o.covenant)!;
  change.value = (BigInt(change.value) + outIn - outOut).toString();
  return b;
}

describe('an order-ending cancel must spend the custody its state holds', () => {
  for (const name of ['create.ask', 'kron.create.ask', 'create.curve', 'pair.create.ask', 'pair.create.bid', 'pair.create.ifdAsk']) {
    it(`${name}: the cancel without its custody blocks (custody-unreleased), the honest one passes`, () => {
      const snap = snapOf(name);
      const plan = planCancel(env(), snap);
      expect(plan.ok).toBe(true);
      const honest = decodeSigning({ kob, built: plan.built!, maker: MAKER.pk, registry, expected: plan.expected, nodeInputs: nodeFactsOf(plan.built!) });
      expect(codes(honest)).toEqual([]);
      const evil = withoutCustody(plan.built!, snap.covenantId);
      // the app path, with node facts
      expect(codes(decodeSigning({ kob, built: evil, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(evil) }))).toContain('custody-unreleased');
      // the frame: no node facts, no registry, no expectations
      expect(codes(decodeSigning({ kob, built: evil, maker: MAKER.pk, registry: null }))).toContain('custody-unreleased');
      // the frame's summary: today it said only 'Closes order' with no token line; now it names the problem and Approve stays off
      const frame = summarize(kob, evil, MAKER.pk, []);
      expect(frame.problemCodes).toContain('custody-unreleased');
      expect(frame.problems).toBeGreaterThan(0);
      expect(summarize(kob, plan.built!, MAKER.pk, []).problems).toBe(0);
    });
  }
});

describe('a custody of another class does not release the order', () => {
  it('a cancel spending a UTXO owned by the order of another class in place of its custody blocks', () => {
    const snap = snapOf('create.ask');
    const plan = planCancel(env(), snap);
    const b = clone(plan.built!);
    const k = b.tx.inputs.findIndex((_, i) => ((b.plans[i] as { state?: { owner?: string } }).state?.owner ?? '') === snap.covenantId);
    const st = (b.plans[k] as unknown as { state: Record<string, string> }).state;
    st.extension_commitment = '99'.repeat(32);
    b.tx.inputs[k]!.utxo.scriptPublicKey = kob.tokenScriptPublicKey('KCC20Ref', st as never);
    expect(codes(decodeSigning({ kob, built: b, maker: MAKER.pk, registry: null }))).toContain('custody-unreleased');
  });
});

describe('the fee ceiling of the private-key frame does not follow the builder labels (R8F2b-8)', () => {
  it('a "priority" label with a large minFee does not lift the absolute ceiling without a fee policy', () => {
    const plan = planCancel(env(), snapOf('create.ask'));
    const b = clone(plan.built!);
    const extra = 60_000_000n; // 0.6 KAS more fee, above FEE_ABSOLUTE_MAX
    const change = b.tx.outputs.find((o) => !o.covenant)!;
    change.value = (BigInt(change.value) - extra).toString();
    b.fee = { ...b.fee, fee: (BigInt(b.fee.fee) + extra).toString(), minFee: (30_000_000n).toString(), feeMode: 'priority' };
    expect(codes(decodeSigning({ kob, built: b, maker: MAKER.pk, registry: null }))).toContain('fee-excessive');
  });
});
