import { describe, expect, it } from 'vitest';
import type { OrderView } from '../../data/indexer-types';
import { planRefund, type CancelEnv, type OrderSnapshot } from '../../kob/cancel';
import { parseRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER, keyUtxo, placeGolden, tradableRegistryJson } from '../../testing/chain-fixtures';
import { AUTO_REFUND_KEY, loadAutoRefund, refundDue, saveAutoRefund, submittableWithoutSignature } from './auto-refund';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const KAS = 100_000_000n;
const env = (over: Partial<CancelEnv> = {}): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 3n * KAS, 201)], tokenUtxos: [], clock: { daa: 77_761_000n }, ...over });
const snapOf = (name: string): OrderSnapshot => placeGolden(kob, name).snapshots[0];

describe('automatic refund (C5 W-15)', () => {
  it('is opt-in and survives a throwing storage', () => {
    const m = new Map<string, string>();
    const st = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
    expect(loadAutoRefund(st)).toBe(false);
    saveAutoRefund(true, st);
    expect(m.get(AUTO_REFUND_KEY)).toBe('1');
    expect(loadAutoRefund(st)).toBe(true);
    const bad = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); } };
    expect(loadAutoRefund(bad)).toBe(false);
    expect(() => saveAutoRefund(true, bad)).not.toThrow();
  });

  it('picks the live own orders whose refund time has come on the indexer clock', () => {
    const v = (over: Partial<OrderView>): OrderView => ({ covenant_id: 'a'.repeat(64), status: 'open', state_known: true, refund_due_daa: 100, current_daa: 100, ...over }) as OrderView;
    expect(refundDue([v({}), v({ current_daa: 99 }), v({ status: 'filled' }), v({ refund_due_daa: null }), v({ state_known: false })])).toHaveLength(1);
  });

  it('sends only a refund that needs no signature and pays nothing away', () => {
    const snap = snapOf('create.ask');
    const plain = planRefund(env({ funding: [] }), snap);
    expect(plain.built!.sign).toEqual([]);
    expect(submittableWithoutSignature({ kob, registry }, MAKER.pk, plain)).toBe(true);
    // the tip reclaim adds a funding input: one signature, so it is left to the Refund button
    const reclaim = planRefund(env(), snap, { reclaimTip: true });
    expect(reclaim.built!.sign.length).toBe(1);
    expect(submittableWithoutSignature({ kob, registry }, MAKER.pk, reclaim)).toBe(false);
    // not yet due: no plan
    expect(submittableWithoutSignature({ kob, registry }, MAKER.pk, planRefund(env({ clock: { daa: 5_000_000n }, funding: [] }), snap))).toBe(false);
    // the decoder must find nothing blocking for THIS wallet: another key is refused
    expect(submittableWithoutSignature({ kob, registry }, 'ab'.repeat(32), plain)).toBe(false);
  });
});
