// Pair planner tests of the conditionals (KobCondPair): stop-market, stop-limit, trailing (with a take-profit), take-profit and OCO on a pair A/B,
// both sides, every KCC-20 / KRON mix. Same oracle as pair-simple.test.ts; prices B per whole A, tips and keeper tips KAS; the trigger rule and
// the arming of the planned state are checked against kob-wasm (`pairTriggerRule`, `pairArms`, `condPairTrailK`).
import { describe, expect, it } from 'vitest';
import type { CondPairState } from '../types';
import type { TokenMarket } from '../plan-types';
import { KAS, ceilDivM, ceilQ, floorQ, kronA, kronB, makePairEnv, tokenA, tokenB } from './pair-fixtures';
import { K, codes, errorCodes, pairValid, plan } from './pair-testkit';

const DC = 10n * KAS;
const cs = (p: { states: { kind: string; state: unknown }[] }): CondPairState => {
  expect(p.states[0].kind).toBe('KobCondPair');
  return p.states[0].state as CondPairState;
};
const defMin = (amount: bigint, scaleA: bigint): bigint => {
  const want = ceilDivM(10n * KAS * scaleA, 3n * KAS);
  return want > amount ? amount : want;
};
const worstOf = (side: 'sell' | 'buy', stop: bigint, bps: bigint): bigint => (side === 'sell' ? stop - (stop * bps) / 10_000n : stop + (stop * bps) / 10_000n);
const minB = (x: bigint, y: bigint): bigint => (x < y ? x : y);

const combos: [string, () => TokenMarket, () => TokenMarket][] = [
  ['KCC-20 / KCC-20', tokenA, tokenB],
  ['KRON / KCC-20', kronA, tokenB],
  ['KCC-20 / KRON', tokenA, kronB],
  ['KRON / KRON', kronA, kronB],
];

describe.each(combos)('KobCondPair on %s', (_name, mkA, mkB) => {
  const a = mkA();
  const b = mkB();
  const env = () => makePairEnv({ a, b });
  const keeperTip = BigInt(K.pairTips(a.program, b.program).keeperTip);
  const amount = 9_001n;
  const tip = 77_777n;

  it('stop-market sell: side ASK holds the A amount, 3% band over 300 DAA, minTouch = minFill, keeper reserve on the order UTXO, falls-to trigger', () => {
    const p = pairValid(env(), { type: 'stopMarket', side: 'sell', amount, stop: 1_401n, tip });
    const s = cs(p);
    const mf = defMin(amount, a.scale);
    expect(s).toMatchObject({
      side: '1', sCovId: a.covenantId, tCovId: b.covenantId, stopPrice: '1401', tpPrice: '0', slipBps: '300', bandDaa: '300', minRestDaa: '50', armed: '0',
      trailStep: '0', trailGap: '0', trailWait: '0', custody: String(amount), amountLeft: String(amount), keeperTip: String(keeperTip), tip: String(tip),
      parent: '0'.repeat(64), rptPrice: '0', rptPre: '0', rptUntil: '0',
    });
    expect(BigInt(s.minFill)).toBe(mf);
    expect(BigInt(s.minTouch)).toBe(mf);
    const worst = worstOf('sell', 1_401n, 300n);
    const fills = minB(ceilDivM(amount, mf), 64n); // one delivery carrier per possible fill
    const x = p.pair!;
    expect(x.receiveMinB).toBe(ceilQ(amount, worst, a.scale));
    expect(x.expectedB).toBe(ceilQ(amount, 1_401n, a.scale));
    expect(x.tipKasTotal).toBe(floorQ(amount, tip, a.scale));
    expect(x.orderValue).toBe(fills * DC + floorQ(amount, tip, a.scale) + keeperTip);
    expect(p.disclosure).toMatchObject({ limitPrice: worst, worstPrice: worst, expectedPrice: 1_401n, minTouch: mf, keeperTip });
    expect(x.trigger).toMatchObject({ kind: 'KobCondPair', stop: '1401', direction: 'fallsTo', minRestDaa: '50', arm: { kasBooks: { a: 'ask', b: 'bid' }, pair: 'ask' }, trail: null });
    expect(BigInt(x.trigger!.minTouch)).toBe(mf);
    expect(x.trigger!.minTouchB === null ? null : BigInt(x.trigger!.minTouchB)).toBe(ceilQ(mf, 1_401n, a.scale));
    expect(p.cond?.legs).toMatchObject({ stop: 1_401n, stopWorst: worst, slipBps: 300, bandDaa: 300n, minTouch: mf, minRestDaa: 50n });
    expect(p.cond?.keeper).toMatchObject({ tip: keeperTip, expectedUpdates: 1, reserve: keeperTip });
    expect(p.disclosure!.notes).toEqual(expect.arrayContaining(['stopTrigger', 'stopAuction', 'triggerExposure', 'keeperReserve']));
    expect(x.notes).toContain('pairTrigger');
  });

  it('stop-market buy: side BID holds the B escrow at the band ceiling (rounded down) plus one unit, rises-to trigger', () => {
    const p = pairValid(env(), { type: 'stopMarket', side: 'buy', amount, stop: 1_601n });
    const s = cs(p);
    const worst = worstOf('buy', 1_601n, 300n);
    const possible = ceilDivM(amount, defMin(amount, a.scale));
    expect(s).toMatchObject({ side: '2', sCovId: b.covenantId, tCovId: a.covenantId });
    expect(BigInt(s.custody)).toBe(floorQ(amount, worst, a.scale) + 1n);
    expect(BigInt(s.custody)).toBe(K.condPairBidEscrow(p.states[0], possible));
    expect(p.pair!.payMaxB).toBe(floorQ(amount, worst, a.scale));
    expect(p.pair!.escrowB).toBe(BigInt(s.custody));
    expect(p.pair!.trigger).toMatchObject({ direction: 'risesTo', arm: { kasBooks: { a: 'bid', b: 'ask' }, pair: 'bid' } });
  });

  it('stop-limit: the largest band within the limit; take-profit; OCO with both legs', () => {
    const sl = pairValid(env(), { type: 'stopLimit', side: 'sell', amount, stop: 1_400n, limit: 1_380n });
    expect(cs(sl).slipBps).toBe(String(((20n + 1n) * 10_000n - 1n) / 1_400n));
    expect(sl.pair!.receiveMinB).toBe(ceilQ(amount, 1_380n, a.scale));
    expect(sl.disclosure!.notes).toContain('stopLimitMayNotFill');
    const tp = pairValid(env(), { type: 'takeProfit', side: 'buy', amount, price: 1_300n });
    expect(cs(tp)).toMatchObject({ tpPrice: '1300', stopPrice: '0', keeperTip: '0', slipBps: '0', bandDaa: '0' });
    expect(tp.pair!.trigger).toBeNull();
    expect(tp.pair!.payMaxB).toBe(floorQ(amount, 1_300n, a.scale));
    expect(BigInt(cs(tp).custody)).toBe(floorQ(amount, 1_300n, a.scale) + 1n);
    const oco = pairValid(env(), { type: 'oco', side: 'sell', amount, stop: 1_400n, takeProfit: 1_700n });
    const w = worstOf('sell', 1_400n, 300n);
    expect(cs(oco)).toMatchObject({ tpPrice: '1700', stopPrice: '1400' });
    expect(oco.pair!.receiveMinB).toBe(ceilQ(amount, w, a.scale));
    expect(oco.disclosure).toMatchObject({ limitPrice: 1_700n, worstPrice: w, expectedPrice: null });
    expect(oco.pair!.minFillB).toBe(ceilQ(defMin(amount, a.scale), 1_700n, a.scale));
    expect(oco.disclosure!.notes).toEqual(expect.arrayContaining(['oco', 'partialFillsKeepLegs']));
  });

  it('trailing stop with a take-profit: step / gap / wait, the pre-funded updates, the trailing rule', () => {
    const p = pairValid(env(), { type: 'trailingStop', side: 'sell', amount, stop: 1_400n, trail: { step: 10n, gap: 30n }, takeProfit: 1_800n });
    expect(cs(p)).toMatchObject({ trailStep: '10', trailGap: '30', trailWait: '6000', tpPrice: '1800' });
    expect(p.cond?.keeper).toMatchObject({ expectedUpdates: 21, reserve: keeperTip * 21n });
    expect(p.disclosure!.carriers.find((c) => c.kind === 'keeperReserve')).toMatchObject({ amount: keeperTip, count: 21 });
    expect(p.pair!.trigger!.trail).toMatchObject({ direction: 'up', kasBooks: { a: 'bid', b: 'ask' }, pair: 'bid', step: '10', gap: '30' });
    // a resting pair bid at 1450 justifies k = floor((1450 - 30 - 1400) / 10) = 2 steps (maximal, below the take-profit)
    expect(K.condPairTrailK(p.states[0], { mode: 'pair', price: '1450' })).toBe(2n);
    const buy = pairValid(env(), { type: 'trailingStop', side: 'buy', amount, stop: 1_600n, trail: { step: 5n, gap: 0n, wait: 600n, expectedUpdates: 3 } });
    expect(buy.pair!.trigger!.trail).toMatchObject({ direction: 'down', pair: 'ask' });
    expect(buy.cond?.keeper?.reserve).toBe(keeperTip * 4n);
  });

  it('the planned stop arms exactly on the evidence the rule names (kob-wasm pairArms, both modes)', () => {
    const p = pairValid(env(), { type: 'stopMarket', side: 'sell', amount, stop: 1_400n });
    const s = p.states[0];
    expect(K.pairArms(s, { mode: 'pair', price: '1400' })).toBe(true);
    expect(K.pairArms(s, { mode: 'pair', price: '1401' })).toBe(false);
    // two KAS books: A at a sompi per whole A, B at bq sompi per whole B: the implied rate a * scale(B) / bq B per whole A
    const bq = 20_000_000n;
    const aAt = (1_400n * bq) / b.scale;
    expect(K.pairArms(s, { mode: 'kasBooks', a: String(aAt), b: String(bq) })).toBe(true);
    expect(K.pairArms(s, { mode: 'kasBooks', a: String(aAt + bq), b: String(bq) })).toBe(false);
  });
});

describe('KobCondPair presets, warnings and refusals', () => {
  it('minTouch presets (50% / 100% of the amount) and the rest time are the state\'s', () => {
    const p = pairValid(makePairEnv(), { type: 'stopMarket', side: 'sell', amount: 8_000n, stop: 1_400n, minTouch: 4_000n, minRestDaa: 100n, bandDaa: 0n, slipBps: 150 });
    expect(cs(p)).toMatchObject({ minTouch: '4000', minRestDaa: '100', bandDaa: '0', slipBps: '150' });
    expect(p.pair!.trigger).toMatchObject({ minTouch: '4000', minRestDaa: '100' });
  });

  it('a custom keeper tip and maxFills of either side', () => {
    const p = pairValid(makePairEnv(), { type: 'stopMarket', side: 'sell', amount: 20_000n, stop: 1_400n, keeperTip: 5_000_000n, maxFills: 5n });
    expect(cs(p).keeperTip).toBe('5000000');
    expect(p.pair!.deliveries).toBe(5n);
    expect(p.pair!.orderValue).toBe(5n * DC + 5_000_000n);
    expect(errorCodes(plan(makePairEnv(), { type: 'stopMarket', side: 'buy', amount: 20_000n, stop: 1_600n, maxFills: 99n }))).toEqual(['MAX_FILLS_INVALID']);
  });

  it('the intent\'s carrier is the KAS of every delivery and of the custody', () => {
    const p = pairValid(makePairEnv(), { type: 'stopMarket', side: 'buy', amount: 5_000n, stop: 1_600n, carrier: 5n * KAS });
    expect(cs(p).deliveryCarrier).toBe(String(5n * KAS));
    expect(p.disclosure!.carriers.find((c) => c.kind === 'quoteTokenCarrier')).toMatchObject({ amount: 5n * KAS, count: 1 });
    expect(p.request!.tokenCarrier).toBe(String(5n * KAS));
  });

  it('warnings: the stop already reached, a take-profit through the pair book', () => {
    expect(codes(pairValid(makePairEnv(), { type: 'stopMarket', side: 'sell', amount: 5_000n, stop: 1_500n }))).toContain('COND_STOP_ALREADY_REACHED');
    const tp = pairValid(makePairEnv(), { type: 'takeProfit', side: 'buy', amount: 5_000n, price: 1_510n });
    expect(codes(tp)).toContain('PAIR_TP_CROSSES');
    expect(tp.issues.find((i) => i.code === 'PAIR_TP_CROSSES')!.params).toMatchObject({ touch: 1_500n, ticker: 'BBB' });
  });

  it('refusals: leg order, stop-limit beyond the stop, trailing step, carrier, not enough B, the KRON escrow cap, the B notional', () => {
    const env = makePairEnv();
    expect(errorCodes(plan(env, { type: 'oco', side: 'sell', amount: 5_000n, stop: 1_400n, takeProfit: 1_300n }))).toEqual(['COND_TP_STOP_ORDER']);
    expect(errorCodes(plan(env, { type: 'stopLimit', side: 'sell', amount: 5_000n, stop: 1_400n, limit: 1_450n }))).toEqual(['COND_STOP_LIMIT_BEYOND_STOP']);
    expect(errorCodes(plan(env, { type: 'trailingStop', side: 'sell', amount: 5_000n, stop: 1_400n, trail: { step: 0n, gap: 0n } }))).toEqual(['COND_TRAIL_STEP_INVALID']);
    expect(errorCodes(plan(env, { type: 'stopMarket', side: 'sell', amount: 5_000n, stop: 1_400n, carrier: 0n }))).toEqual(['COND_CARRIER_INVALID']);
    const noB = plan(makePairEnv({ bAmounts: [1_000n] }), { type: 'stopMarket', side: 'buy', amount: 5_000n, stop: 1_600n });
    expect(errorCodes(noB)).toEqual(['PAIR_INSUFFICIENT_TOKENS']);
    expect(noB.issues.find((i) => i.code === 'PAIR_INSUFFICIENT_TOKENS')!.params).toMatchObject({ ticker: 'BBB' });
    const kron = plan(makePairEnv({ a: kronA(), b: kronB() }), { type: 'stopMarket', side: 'buy', amount: 1_000_000n, stop: 2_000_000n });
    expect(errorCodes(kron)).toEqual(['PAIR_KRON_CUSTODY_TOO_LARGE']);
    // a stop above the covenant's MAX_STOP (stop x slipBps must fit 63 bits) is too large as a price; a large amount at a high stop breaks the 2^62 bound
    expect(errorCodes(plan(env, { type: 'stopMarket', side: 'sell', amount: 5_000n, stop: 1n << 61n }))).toEqual(['PRICE_TOO_LARGE']);
    expect(errorCodes(plan(env, { type: 'stopMarket', side: 'sell', amount: 10_000_000_000_000n, stop: 900_000_000_000_000n }))).toEqual(['PAIR_NOTIONAL_TOO_LARGE']);
    expect(errorCodes(plan(env, { type: 'stopMarket', side: 'sell', amount: 5_000n, stop: 0n }))).toEqual(['PRICE_NOT_POSITIVE']);
  });

  it('self-trade: a sell stop whose band reaches an own resting pair bid', () => {
    const env = makePairEnv({ ownOrders: [{ covenantId: 'cd'.repeat(32), side: 'buy', price: 1_390n, tip: 0n, amountLeft: 100n, active: true }] });
    expect(errorCodes(plan(env, { type: 'stopMarket', side: 'sell', amount: 5_000n, stop: 1_400n }))).toEqual(['SELF_TRADE']);
    expect(codes(plan(env, { type: 'stopMarket', side: 'sell', amount: 5_000n, stop: 1_400n, slipBps: 0 }))).not.toContain('SELF_TRADE');
  });
});
