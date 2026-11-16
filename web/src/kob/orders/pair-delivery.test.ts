// Delivery of pair BIDs (founder decision 11/16): a resting KobPair / KobCondPair bid of a KCC-20 A is Collected by default and funded with one
// delivery carrier; its default tip also pays the bought-custody input of every later fill; "Added to one UTXO" is not offered on pairs; a
// KRON A and an immediate order deliver at each fill. The plans are signed and validated by the script engine (pair-testkit pairValid).
import { describe, expect, it } from 'vitest';
import { defaultPairTip } from './pair-common';
import { kronA, makePairEnv } from './pair-fixtures';
import { pairValid, plan } from './pair-testkit';
import { PAIR_COLLECTED_EXTRA_REFERENCE, collectedPairTipExtra, pairCollectedExtra, planDelivery } from './delivery';

/** the pair state of a plan's (first) order: KobPair or KobCondPair */
const pairState = (p: { states: { state: unknown }[] }): Record<string, string> => p.states[0]!.state as Record<string, string>;
const codes = (p: { issues: { code: string }[] }): string[] => p.issues.map((i) => i.code);
const buy = { type: 'limit', side: 'buy', amount: 10_000n, price: 1_400n, minFill: 2_500n } as const;

describe('the Collected tip of a pair bid', () => {
  it('adds ceil(ceil(1.25 x extra) x scale / minFill) to the default tip (negative: no margin, no rounding up)', () => {
    expect(PAIR_COLLECTED_EXTRA_REFERENCE.KCC20Ref_8x8).toBe(1_282_600n);
    // 1.25 x 1 282 600 = 1 603 250 sompi per fill of minFill
    expect(collectedPairTipExtra(1_282_600n, 1_000n, 2_500n)).toBe(641_300n);
    expect(collectedPairTipExtra(1_282_601n, 1_000n, 3n)).toBe(534_417_334n);
    expect(collectedPairTipExtra(1_282_601n, 1_000n, 3n)).not.toBe((1_282_601n * 5n * 1_000n) / 4n / 3n);
    expect(collectedPairTipExtra(0n, 1_000n, 3n)).toBe(0n);
    // a program without a measurement uses the largest reference value
    expect(pairCollectedExtra('KCC20Ref')).toBe(1_282_600n);
  });
});

describe('pair bids with a delivery (signed and validated by the script engine)', () => {
  it('a resting pair bid is Collected by default: mode 1, one delivery carrier, the tip raised for the later fills', () => {
    const env = makePairEnv();
    const p = pairValid(env, buy);
    expect(pairState(p)).toMatchObject({ mode: '1', recvThreshold: '0', topupTip: '0', bought: '0' });
    expect(p.pair!.deliveries).toBe(1n);
    expect(planDelivery(p)).toMatchObject({ delivery: { choice: 'collected' }, fills: 1n });
    expect(BigInt(pairState(p).tip)).toBe(defaultPairTip(env, 10_000n) + collectedPairTipExtra(pairCollectedExtra(env.token.program), env.token.scale, 2_500n));
    expect(p.disclosure!.notes).toContain('deliveryCollected');
    // a tip the user typed stays as typed
    expect(pairState(pairValid(env, { ...buy, tip: 7n })).tip).toBe('7');
    // negative: Each fill funds the protocol's deliveries (at least two) with the plain default tip
    const each = pairValid(env, { ...buy, delivery: { choice: 'each' } });
    expect(pairState(each).mode).toBe('0');
    expect(each.pair!.deliveries).toBeGreaterThan(1n);
    expect(BigInt(pairState(each).tip)).toBe(defaultPairTip(env, 10_000n));
  });

  it('"Added to one UTXO" is not offered on a pair: refused when chosen, the wallet default falls back to Collected', () => {
    const env = makePairEnv();
    const chosen = plan(env, { ...buy, delivery: { choice: 'topup' } });
    expect(chosen.ok).toBe(false);
    expect(codes(chosen)).toContain('DELIVERY_TOPUP_UNAVAILABLE');
    const byDefault = pairValid(env, { ...buy, delivery: { walletDefault: 'topup' } });
    expect(pairState(byDefault).mode).toBe('1');
    expect(codes(byDefault)).toContain('DELIVERY_TOPUP_FALLBACK');
  });

  it('a KRON A, an IOC and a pair sell deliver at each fill (negative: the rules removed give them Collected)', () => {
    const kron = pairValid(makePairEnv({ a: kronA(), aAmounts: [100_000n] }), { ...buy, delivery: { choice: 'collected' } });
    expect(pairState(kron).mode).toBe('0');
    const ioc = plan(makePairEnv(), { type: 'ioc', side: 'buy', amount: 1_000n, price: 1_600n });
    expect(planDelivery(ioc)?.delivery.choice ?? 'each').toBe('each');
    const sell = pairValid(makePairEnv(), { type: 'limit', side: 'sell', amount: 10_000n, price: 1_600n });
    expect(pairState(sell).mode).toBe('0');
    expect(planDelivery(sell)).toBeNull();
  });

  it('a conditional pair bid (KobCondPair) is Collected with one carrier', () => {
    const p = pairValid(makePairEnv(), { type: 'stopLimit', side: 'buy', amount: 10_000n, stop: 1_700n, limit: 1_750n, minFill: 2_500n });
    expect(pairState(p)).toMatchObject({ mode: '1', bought: '0' });
    expect(p.pair!.deliveries).toBe(1n);
  });
});
