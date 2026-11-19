// Delivery of pair BIDs. A resting KobPair / KobCondPair bid of a KCC-20 A is Collected by default
// and funded with one delivery carrier. Its default tip also pays the bought-custody input of every
// later fill. "Added to one UTXO" is not offered on pairs. A KRON A and an immediate order deliver
// at each fill. The plans are signed and validated by the script engine (pair-testkit pairValid).
import { describe, expect, it } from 'vitest';
import { defaultPairTip } from './pair-common';
import { kronA, kronB, makePairEnv } from './pair-fixtures';
import { pairValid, plan } from './pair-testkit';
import { ceilDiv } from '../units';
import { PAIR_COLLECTED_EXTRA_REFERENCE, collectedPairTipExtra, pairCollectedExtra, planDelivery } from './delivery';

/** the pair state of a plan's (first) order: KobPair or KobCondPair */
const pairState = (p: { states: { state: unknown }[] }): Record<string, string> => p.states[0]!.state as Record<string, string>;
const codes = (p: { issues: { code: string }[] }): string[] => p.issues.map((i) => i.code);
const buy = { type: 'limit', side: 'buy', amount: 10_000n, price: 1_400n, minFill: 2_500n } as const;

describe('the Collected tip of a pair bid', () => {
  it('adds ceil(ceil(1.25 x extra) x L x scale / amount), L = later fills (at most 63), to the default tip (negative: no margin, no rounding up, no cap)', () => {
    expect(PAIR_COLLECTED_EXTRA_REFERENCE.KCC20Ref_8x8).toBe(1_282_600n);
    // 10 000 base units, minFill 2 500: 3 later fills of 1.25 x 1 282 600 = 1 603 250 sompi, over
    // 10 whole A
    expect(collectedPairTipExtra(1_282_600n, 1_000n, 10_000n, 2_500n)).toBe(480_975n);
    // a later fill of minFill pays its filler at least 1.25 x the extra less the first fill's share
    expect((480_975n * 2_500n) / 1_000n).toBeGreaterThanOrEqual((1_603_250n * 3n) / 4n);
    // rounded up
    expect(collectedPairTipExtra(1_282_601n, 1_000n, 7n, 3n)).toBe(458_072_000n);
    expect(collectedPairTipExtra(1_282_601n, 1_000n, 7n, 3n)).not.toBe((((1_282_601n * 5n) / 4n) * 2n * 1_000n) / 7n);
    // at most 63 later fills: a minFill of 1 over 10 000 does not pay 9 999 of them
    expect(collectedPairTipExtra(1_282_600n, 1_000n, 10_000n, 1n)).toBe(ceilDiv(1_603_250n * 63n * 1_000n, 10_000n));
    // one possible fill: no custody input ever
    expect(collectedPairTipExtra(1_282_600n, 1_000n, 10_000n, 10_000n)).toBe(0n);
    expect(collectedPairTipExtra(0n, 1_000n, 7n, 3n)).toBe(0n);
    // a program without a measurement uses the largest reference value
    expect(pairCollectedExtra('KCC20Ref')).toBe(1_282_600n);
  });
});

describe('pair bids with a delivery (signed and validated by the script engine)', () => {
  it('a resting pair bid is Each fill by default (the plain default tip); Collected is opt-in: mode 1, one delivery carrier, the tip raised for the later fills', () => {
    const env = makePairEnv();
    const col = { delivery: { choice: 'collected' } } as const;
    const p = pairValid(env, { ...buy, ...col });
    expect(pairState(p)).toMatchObject({ mode: '1', recvThreshold: '0', topupTip: '0', bought: '0' });
    expect(p.pair!.deliveries).toBe(1n);
    expect(planDelivery(p)).toMatchObject({ delivery: { choice: 'collected' }, fills: 1n });
    expect(BigInt(pairState(p).tip)).toBe(defaultPairTip(env, 10_000n) + collectedPairTipExtra(pairCollectedExtra(env.token.program), env.token.scale, 10_000n, 2_500n));
    expect(p.disclosure!.notes).toContain('deliveryCollected');
    // a tip the user typed stays as typed
    expect(pairState(pairValid(env, { ...buy, ...col, tip: 7n })).tip).toBe('7');
    // the default: Each fill funds the protocol's deliveries (at least two) with the plain default
    // tip
    const each = pairValid(env, buy);
    expect(pairState(each).mode).toBe('0');
    expect(each.pair!.deliveries).toBeGreaterThan(1n);
    expect(BigInt(pairState(each).tip)).toBe(defaultPairTip(env, 10_000n));
  });

  it('"Added to one UTXO" is not offered on a pair: refused when chosen, the wallet default falls back to Each fill', () => {
    const env = makePairEnv();
    const chosen = plan(env, { ...buy, delivery: { choice: 'topup' } });
    expect(chosen.ok).toBe(false);
    expect(codes(chosen)).toContain('DELIVERY_TOPUP_UNAVAILABLE');
    const byDefault = pairValid(env, { ...buy, delivery: { walletDefault: 'topup' } });
    expect(pairState(byDefault).mode).toBe('0');
    expect(codes(byDefault)).toContain('DELIVERY_TOPUP_FALLBACK');
  });

  it('a KRON A, an IOC and a sell of a KRON B deliver at each fill (negative: the rules removed give them Collected)', () => {
    const kron = pairValid(makePairEnv({ a: kronA(), aAmounts: [100_000n] }), { ...buy, delivery: { choice: 'collected' } });
    expect(pairState(kron).mode).toBe('0');
    const ioc = plan(makePairEnv(), { type: 'ioc', side: 'buy', amount: 1_000n, price: 1_600n });
    expect(planDelivery(ioc)?.delivery.choice ?? 'each').toBe('each');
    // a sell whose B is KRON
    const kronSell = pairValid(makePairEnv({ b: kronB(), bAmounts: [100_000n] }), { type: 'limit', side: 'sell', amount: 10_000n, price: 1_600n, delivery: { choice: 'collected' } });
    expect(pairState(kronSell).mode).toBe('0');
  });

  it('a conditional pair bid (KobCondPair) is Each fill by default; Collected has one carrier', () => {
    const stop = { type: 'stopLimit', side: 'buy', amount: 10_000n, stop: 1_700n, limit: 1_750n, minFill: 2_500n } as const;
    expect(pairState(pairValid(makePairEnv(), stop)).mode).toBe('0');
    const p = pairValid(makePairEnv(), { ...stop, delivery: { choice: 'collected' } });
    expect(pairState(p)).toMatchObject({ mode: '1', bought: '0' });
    expect(p.pair!.deliveries).toBe(1n);
  });

  it('a resting pair sell (it receives B) is Each fill by default; Collected: one carrier, the tip raised with the extra of B', () => {
    const env = makePairEnv();
    const sell = { type: 'limit', side: 'sell', amount: 10_000n, price: 1_600n, minFill: 2_500n } as const;
    const col = { delivery: { choice: 'collected' } } as const;
    const p = pairValid(env, { ...sell, ...col });
    expect(pairState(p)).toMatchObject({ side: '1', mode: '1', recvThreshold: '0', topupTip: '0', bought: '0' });
    expect(p.pair!.deliveries).toBe(1n);
    expect(BigInt(pairState(p).tip)).toBe(defaultPairTip(env, 10_000n) + collectedPairTipExtra(pairCollectedExtra(env.pair.quote.program), env.token.scale, 10_000n, 2_500n));
    // the default: Each fill funds a carrier per possible fill, the plain tip
    const each = pairValid(env, sell);
    expect(pairState(each).mode).toBe('0');
    expect(each.pair!.deliveries).toBe(4n);
    expect(BigInt(pairState(each).tip)).toBe(defaultPairTip(env, 10_000n));
    // a TWAP sell and a stop sell: Each fill by default, Collected when picked; an IOC sell
    // delivers at each fill
    const twap = { type: 'twap', amount: 10_000n, sliceAmount: 2_500n, interval: { daa: 600n }, price: 1_500n } as const;
    const stopSell = { type: 'stopMarket', side: 'sell', amount: 8_000n, stop: 1_400n } as const;
    expect(pairState(pairValid(env, twap)).mode).toBe('0');
    expect(pairState(pairValid(env, stopSell)).mode).toBe('0');
    expect(pairState(pairValid(env, { ...twap, ...col })).mode).toBe('1');
    expect(pairState(pairValid(env, { ...stopSell, ...col })).mode).toBe('1');
    const ioc = plan(env, { type: 'ioc', side: 'sell', amount: 1_000n, price: 1_400n });
    expect(planDelivery(ioc)?.delivery.choice ?? 'each').toBe('each');
  });
});
