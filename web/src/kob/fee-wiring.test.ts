// The dynamic fee policy through the real planners (real kob-wasm): which bucket each action pays, the fallback to the floor, the total-fee cap
// rebuild, the funds fallback and the remembered fee choice. Oracle: the BUILT transaction's own feeRate / fee (kob-wasm), never the planner's claim.
import { describe, expect, it } from 'vitest';
import { feeChoiceOf, normalizePolicy, type FeeContext, type FeeEstimate, type FeePolicy } from './fee-policy';
import { planOrder, orderUrgency } from './plan';
import type { Intent } from './plan';
import type { PairPlanEnv } from './plan-types';
import { makePairEnv } from './orders/pair-fixtures';
import { planCancel, planCancelReplace, planRefund, type CancelEnv, type OrderSnapshot } from './cancel';
import { planIssue, emptyIssueForm, type IssueForm } from './issue';
import { pubkeyOf } from '../testing/local-signer';
import { KAS, TOK, kob, makeEnv } from '../testing/fixtures';
import { MAKER, keyUtxo, placeGolden } from '../testing/chain-fixtures';

const estimate: FeeEstimate = { priority: 300.2, normal: 150, low: 110, seconds: { priority: 0.8, normal: 40, low: 1500 } };
const ctx = (over: Partial<FeePolicy> = {}, est: FeeEstimate | null = estimate): FeeContext => ({ policy: normalizePolicy(over), estimate: est });
const rateOf = (p: { built: { fee: { feeRate: string } } | null }): bigint => BigInt(p.built!.fee.feeRate);
const js = (v: unknown): string => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? x.toString() : x));
const flat = (n: number): FeeEstimate => ({ priority: n, normal: n, low: n });

const resting: Intent = { type: 'limit', side: 'sell', price: 300_000_000n, amount: 5n * TOK };

describe('orderUrgency', () => {
  const env = makeEnv(); // best ask 2.50, best bid 2.45 KAS per token
  it('IOC, FOK, market, streaming and close are HIGH', () => {
    expect(orderUrgency(env, { type: 'ioc', side: 'buy', price: 250_000_000n, amount: 1n * TOK })).toBe('high');
    expect(orderUrgency(env, { type: 'fok', side: 'sell', price: 245_000_000n, amount: 1n * TOK })).toBe('high');
    expect(orderUrgency(env, { type: 'market', side: 'buy', amount: 1n * TOK })).toBe('high');
    expect(orderUrgency(env, { type: 'streaming', side: 'buy', amount: 1n * TOK, displayedPrice: 250_000_000n, toleranceBps: 100n })).toBe('high');
    expect(orderUrgency(env, { type: 'close' })).toBe('high');
  });
  it('a limit that crosses the book now is HIGH (it is placed as an auction), unless refused or timed', () => {
    expect(orderUrgency(env, { type: 'limit', side: 'buy', price: 255_000_000n, amount: 1n * TOK })).toBe('high');
    expect(orderUrgency(env, { type: 'limit', side: 'sell', price: 240_000_000n, amount: 1n * TOK })).toBe('high');
    expect(orderUrgency(env, { type: 'limit', side: 'buy', price: 255_000_000n, amount: 1n * TOK, crossing: 'reject' })).toBe('normal');
    expect(orderUrgency(env, { type: 'limit', side: 'buy', price: 255_000_000n, amount: 1n * TOK, activeFrom: { unixSeconds: 1_800_000_000n } })).toBe('normal');
  });
  it('resting limits, schedules, stops, take-profit, OCO and if-done are NORMAL', () => {
    expect(orderUrgency(env, { type: 'limit', side: 'buy', price: 200_000_000n, amount: 1n * TOK })).toBe('normal');
    expect(orderUrgency(env, { type: 'limit', side: 'sell', price: 300_000_000n, amount: 1n * TOK })).toBe('normal');
    expect(orderUrgency(env, { type: 'twap', sliceAmount: 1n * TOK, interval: { seconds: 60 }, price: 300_000_000n, amount: 4n * TOK } as unknown as Intent)).toBe('normal');
    expect(orderUrgency(env, { type: 'stopMarket', side: 'sell', amount: 1n * TOK, stop: 230_000_000n })).toBe('normal');
    expect(orderUrgency(env, { type: 'takeProfit', side: 'sell', amount: 1n * TOK, price: 300_000_000n } as unknown as Intent)).toBe('normal');
    expect(orderUrgency(env, { type: 'oco', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, takeProfit: 300_000_000n })).toBe('normal');
    expect(orderUrgency(env, { type: 'ifd' } as unknown as Intent)).toBe('normal');
  });
  it('an empty book never makes a limit marketable', () => {
    expect(orderUrgency({ book: { asks: [], bids: [] } }, { type: 'limit', side: 'buy', price: 999_000_000n, amount: 1n * TOK })).toBe('normal');
  });
});

describe('planOrder pays the bucket of its urgency', () => {
  it('a resting limit: NORMAL bucket (150), built at that rate, choice remembered', () => {
    const p = planOrder({ ...makeEnv(), fees: ctx() }, resting);
    expect(p.ok, js(p.issues)).toBe(true);
    expect(rateOf(p)).toBe(150n);
    expect(p.request!.fee).toEqual({ feeRate: '150' });
    expect(BigInt(p.built!.fee.minFee)).toBe(BigInt(p.built!.fee.mass.feeMass) * 150n);
    expect(feeChoiceOf(p.built!)).toMatchObject({ urgency: 'normal', rate: 150n, source: 'estimate', estimatedSeconds: 40, bucketFeerate: 150, overCap: false });
  });

  it('IOC / FOK / market: PRIORITY bucket (ceil(300.2) = 301)', () => {
    const intents: Intent[] = [
      { type: 'ioc', side: 'buy', price: 255_000_000n, amount: 2n * TOK },
      { type: 'fok', side: 'buy', price: 255_000_000n, amount: 2n * TOK },
      { type: 'market', side: 'buy', amount: 2n * TOK },
    ];
    for (const intent of intents) {
      const p = planOrder({ ...makeEnv(), fees: ctx() }, intent);
      expect(p.ok, `${intent.type}: ${js(p.issues)}`).toBe(true);
      expect(rateOf(p), intent.type).toBe(301n);
      expect(feeChoiceOf(p.built!), intent.type).toMatchObject({ urgency: 'high', rate: 301n, source: 'estimate', estimatedSeconds: 0.8 });
    }
  });

  it('a marketable limit pays HIGH, a stop (conditional planner) pays NORMAL', () => {
    const marketable = planOrder({ ...makeEnv(), fees: ctx() }, { type: 'limit', side: 'buy', price: 255_000_000n, amount: 2n * TOK });
    expect(marketable.ok, js(marketable.issues)).toBe(true);
    expect(rateOf(marketable)).toBe(301n);
    const stop = planOrder({ ...makeEnv(), fees: ctx() }, { type: 'stopMarket', side: 'sell', amount: 2n * TOK, stop: 230_000_000n });
    expect(stop.ok, js(stop.issues)).toBe(true);
    expect(rateOf(stop)).toBe(150n);
  });

  it('no usable estimate: the floor, source floor / unavailable (never an error)', () => {
    const p = planOrder({ ...makeEnv(), fees: ctx({}, null) }, resting);
    expect(p.ok).toBe(true);
    expect(rateOf(p)).toBe(100n);
    expect(feeChoiceOf(p.built!)).toMatchObject({ source: 'floor', reason: 'unavailable', rate: 100n });
  });

  it("dynamic: false is today's behaviour: the floor, even with an estimate in hand", () => {
    const p = planOrder({ ...makeEnv(), fees: ctx({ dynamic: false }) }, resting);
    expect(rateOf(p)).toBe(100n);
    expect(feeChoiceOf(p.built!)).toMatchObject({ source: 'floor', reason: 'disabled' });
  });

  it('maxRate clamps the bucket and says so', () => {
    const p = planOrder({ ...makeEnv(), fees: ctx({ maxRate: 200n }) }, { type: 'market', side: 'buy', amount: 2n * TOK });
    expect(rateOf(p)).toBe(200n);
    expect(feeChoiceOf(p.built!)).toMatchObject({ clamped: true, clampedTo: 'maxRate', bucketFeerate: 300.2, rate: 200n });
  });

  it('an environment without a policy (or with an explicit feeRate) is untouched', () => {
    const none = planOrder(makeEnv(), resting);
    expect(none.request!.fee).toBeUndefined();
    expect(rateOf(none)).toBe(100n);
    expect(feeChoiceOf(none.built!)).toBeNull();
    const explicit = planOrder({ ...makeEnv({ feeRate: 220n }), fees: ctx() }, resting);
    expect(rateOf(explicit)).toBe(220n);
    expect(feeChoiceOf(explicit.built!)).toBeNull();
  });

  it('the total cap rebuilds ONCE at the lower rate and the fee ends within the cap', () => {
    const floorPlan = planOrder({ ...makeEnv(), fees: ctx({ dynamic: false }) }, resting);
    const floorFee = BigInt(floorPlan.built!.fee.fee);
    const cap = floorFee * 2n; // allows about rate 200
    const p = planOrder({ ...makeEnv(), fees: ctx({ maxRate: 1000n, maxFeeSompi: cap }, flat(1000)) }, resting);
    expect(p.ok, js(p.issues)).toBe(true);
    expect(BigInt(p.built!.fee.fee)).toBeLessThanOrEqual(cap);
    expect(rateOf(p)).toBeGreaterThanOrEqual(100n);
    expect(rateOf(p)).toBeLessThan(1000n);
    // floor_div(1000 x cap / fee at 1000), fee at 1000 = 1000 x mass
    const mass = BigInt(p.built!.fee.mass.feeMass);
    expect(rateOf(p)).toBe((1000n * cap) / (1000n * mass));
    expect(feeChoiceOf(p.built!)).toMatchObject({ cappedFrom: 1000n, overCap: false, rate: rateOf(p) });
    expect(p.request!.fee).toEqual({ feeRate: rateOf(p).toString() });
  });

  it('a floor-rate transaction above the cap is sent anyway and flagged overCap', () => {
    const p = planOrder({ ...makeEnv(), fees: ctx({ maxFeeSompi: 1_000n }) }, resting);
    expect(p.ok).toBe(true);
    expect(rateOf(p)).toBe(100n);
    expect(feeChoiceOf(p.built!)).toMatchObject({ overCap: true, cappedFrom: 150n });
  });

  it('maxFeeSompi 0 means no cap', () => {
    const p = planOrder({ ...makeEnv(), fees: ctx({ maxFeeSompi: 0n }, flat(5000)) }, resting);
    expect(rateOf(p)).toBe(1000n);
    expect(feeChoiceOf(p.built!)).toMatchObject({ overCap: false });
    expect(feeChoiceOf(p.built!)!.cappedFrom).toBeUndefined();
  });

  it('funds fallback: a wallet that can pay the order and the floor fee but not the estimated fee still places it, at the floor', () => {
    const base = planOrder({ ...makeEnv(), fees: ctx({ dynamic: false }) }, resting);
    const floorFee = BigInt(base.built!.fee.fee);
    const locked = base.disclosure!.kasLocked;
    const tight = locked + floorFee + 1_000n; // the order's KAS and the floor fee, and a little more
    const env = { ...makeEnv({ funding: [tight] }), fees: ctx({ maxFeeSompi: 0n }, flat(900)) };
    const p = planOrder(env, resting);
    expect(p.ok, js(p.issues)).toBe(true);
    expect(rateOf(p)).toBe(100n);
    expect(feeChoiceOf(p.built!)).toMatchObject({ source: 'floor', reason: 'funds', rate: 100n });
  });
});

describe('pair orders (planOrder with a PairPlanEnv)', () => {
  const envP = (fees: FeeContext): PairPlanEnv => ({ ...makePairEnv(), fees });
  it('a resting pair limit: NORMAL; IOC, FOK and a pair market order: HIGH; a crossing pair limit (an auction) HIGH', () => {
    const gtc = planOrder(envP(ctx()), { type: 'limit', side: 'sell', amount: 5_000n, price: 1_550n });
    expect(gtc.ok, js(gtc.issues)).toBe(true);
    expect(rateOf(gtc)).toBe(150n);
    for (const type of ['ioc', 'fok'] as const) {
      const p = planOrder(envP(ctx()), { type, side: 'sell', amount: 5_000n, price: 1_450n });
      expect(p.ok, js(p.issues)).toBe(true);
      expect(rateOf(p), type).toBe(301n);
      expect(feeChoiceOf(p.built!)).toMatchObject({ urgency: 'high' });
    }
    expect(rateOf(planOrder(envP(ctx()), { type: 'market', side: 'buy', amount: 5_000n }))).toBe(301n);
    // the KAS tip never makes a B price cross: a sell at 1451 over a 1450 bid is resting (NORMAL), at 1450 it crosses (HIGH)
    expect(orderUrgency(makePairEnv(), { type: 'limit', side: 'sell', amount: 5_000n, price: 1_451n, tip: 100n * KAS })).toBe('normal');
    expect(orderUrgency(makePairEnv(), { type: 'limit', side: 'sell', amount: 5_000n, price: 1_450n })).toBe('high');
  });
});

describe('cancel, amend and refund pay the NORMAL bucket', () => {
  const snap: OrderSnapshot = placeGolden(kob(), 'create.ask').snapshots[0];
  const cenv = (fees?: FeeContext, over: Partial<CancelEnv> = {}): CancelEnv => ({
    kob: kob(), maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201), keyUtxo(MAKER.pk, 3n * KAS, 202)], tokenUtxos: [], clock: { daa: 78_000_000n }, ...(fees ? { fees } : {}), ...over,
  });
  it('planCancel', () => {
    const plan = planCancel(cenv(ctx()), snap);
    expect(plan.ok, js(plan.issues)).toBe(true);
    expect(rateOf(plan)).toBe(150n);
    expect(plan.request).toMatchObject({ fee: { feeRate: '150' } });
    expect(feeChoiceOf(plan.built!)).toMatchObject({ urgency: 'normal', rate: 150n, source: 'estimate' });
  });
  it("the floor without an estimate, and exactly today's request without a policy", () => {
    const none = planCancel(cenv(ctx({}, null)), snap);
    expect(rateOf(none)).toBe(100n);
    expect(feeChoiceOf(none.built!)).toMatchObject({ source: 'floor', reason: 'unavailable' });
    const plain = planCancel(cenv(), snap);
    expect(plain.request).toMatchObject({ fee: { feeRate: null } });
    expect(feeChoiceOf(plain.built!)).toBeNull();
  });
  it('the cap rebuild applies to a cancel too', () => {
    const floorFee = BigInt(planCancel(cenv(ctx({ dynamic: false })), snap).built!.fee.fee);
    const cap = floorFee * 2n;
    const plan = planCancel(cenv(ctx({ maxFeeSompi: cap }, flat(1000))), snap);
    expect(plan.ok).toBe(true);
    expect(BigInt(plan.built!.fee.fee)).toBeLessThanOrEqual(cap);
    expect(feeChoiceOf(plan.built!)).toMatchObject({ cappedFrom: 1000n });
  });
  it('planRefund and the cancel-replace (amend) are NORMAL as well', () => {
    const refund = planRefund(cenv(ctx(), { clock: { daa: 90_000_000_000n } }), snap);
    if (refund.ok) expect(rateOf(refund)).toBe(150n);
    const amend = planCancelReplace(cenv(ctx()), snap, { order: snap.order.state } as never);
    if (amend.ok) expect(rateOf(amend)).toBe(150n);
  });
});

describe('planIssue pays the NORMAL bucket', () => {
  const SK = '33'.repeat(32);
  const maker = pubkeyOf(SK);
  const form = (): IssueForm => ({ ...emptyIssueForm(), name: 'Test Token', ticker: 'TEST', decimals: 8, supply: '1000' });
  const funding = (sompi: bigint) => [{ transactionId: '07'.repeat(32), index: 1, amount: sompi.toString(), blockDaaScore: '5000', pubkey: maker }];
  it('rate from the estimate, remembered, cap rebuild', () => {
    const plan = planIssue(kob(), form(), { funding: funding(500n * KAS), maker, network: 'testnet-10', fees: ctx() });
    expect(plan.built.fee.feeRate).toBe('150');
    expect(feeChoiceOf(plan.built)).toMatchObject({ urgency: 'normal', rate: 150n, source: 'estimate' });
    const floorFee = planIssue(kob(), form(), { funding: funding(500n * KAS), maker, network: 'testnet-10' }).fee;
    const capped = planIssue(kob(), form(), { funding: funding(500n * KAS), maker, network: 'testnet-10', fees: ctx({ maxFeeSompi: floorFee * 2n }, flat(1000)) });
    expect(capped.fee).toBeLessThanOrEqual(floorFee * 2n);
    expect(feeChoiceOf(capped.built)).toMatchObject({ cappedFrom: 1000n });
  });
  it('no policy: the relay minimum as before', () => {
    const plan = planIssue(kob(), form(), { funding: funding(500n * KAS), maker, network: 'testnet-10' });
    expect(plan.built.fee.feeRate).toBe('100');
  });
});
