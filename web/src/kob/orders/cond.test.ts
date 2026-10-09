// Conditional orders without an entry: stop-market, stop-limit, trailing stop, take-profit, OCO, on both sides. Every plan is run through
// the oracle chain of cond-testkit (sign locally, finalize with tightened budgets, script-engine validate, recover the placement record).
import { describe, expect, it } from 'vitest';
import type { CondIntent } from '../intent-cond';
import { CLOCK, KAS, MAKER_PK, OTHER_PK, TOK, makeEnv, market8x8, tokenUtxo } from '../../testing/fixtures';
import { ceilDiv } from '../units';
import { planCond } from './cond';
import { askState, bidState, codes, errorCodes, planOk } from './cond-testkit';

const CARRIER = 1_000_000_000n;
const GTC = CLOCK.daa + 77_760_000n;
const SCALE = 1_000n;
/** the wallet default minimum fill: the amount worth 10 KAS at the leg price, clamped to 1..amount */
const defMin = (amount: bigint, price: bigint): bigint => {
  const want = ceilDiv(10n * KAS * SCALE, price);
  return want > amount ? amount : want;
};

describe('stop-market', () => {
  it('sell: KobCondAsk stop leg with the wallet defaults, custody in 0x04, tokens escrowed', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n });
    const s = askState(plan.states[0]!);
    expect(s).toMatchObject({
      maker: MAKER_PK, tokenCovId: env.token.covenantId, tokenTplHash: env.token.templateHash, scale: '1000', amountLeft: '10000',
      tip: '0', activeFrom: '0', expiryDaa: GTC.toString(), refundTip: '3400000', tpPrice: '0', stopPrice: '230000000',
      slipBps: '300', bandDaa: '300', keeperTip: '2100000', minRestDaa: '50', armed: '0',
      trailStep: '0', trailGap: '0', trailWait: '0', parent: '0'.repeat(64), rptPrice: '0', rptUntil: '0',
    });
    // the minimum fill is worth 10 KAS at the stop (ceil(10 / 2.3 tokens) = 4348 base units); the trigger threshold defaults to it
    expect(s.minFill).toBe('4348');
    expect(BigInt(s.minFill)).toBe(env.kob.defaultMinFill(10n * TOK, 230_000_000n, SCALE));
    expect(s.minTouch).toBe('4348');
    expect(plan.states).toHaveLength(1);
    // the custody holds exactly the 10 tokens and belongs to the order's covenant id
    const c = recovered[0]!.custody!;
    expect(c.state).toMatchObject({ amount: '10000', owner: recovered[0]!.covenantId, owner_scheme: 4 });
    expect(recovered[0]!.value).toBe(CARRIER.toString());
    const d = plan.disclosure!;
    expect(d).toMatchObject({
      side: 'sell', tokenAmount: 10_000n, scale: 1_000n, minFill: 4_348n, minTouch: 4_348n, tokensEscrowed: 10_000n, tip: 0n, refundTip: 3_400_000n, keeperTip: 2_100_000n,
      // 230 KAS - 23 000 sompi per bps * 300 = 223.1 KAS: the worst the sell can get; it starts at the stop
      limitPrice: 223_100_000n, worstPrice: 223_100_000n, expectedPrice: 230_000_000n, allInPrice: 223_100_000n, allInTotal: 2_231_000_000n,
      kasLocked: 2n * CARRIER,
    });
    expect(d.carriers).toEqual([
      { kind: 'orderCarrier', amount: CARRIER, count: 1 },
      { kind: 'tokenCarrier', amount: CARRIER, count: 1 },
      { kind: 'tokenChangeCarrier', amount: CARRIER, count: 1, kept: true },
    ]);
    expect(d.expiry).toMatchObject({ kind: 'gtc', daa: GTC, deadlineUnixSeconds: null });
    expect(d.notes).toEqual(expect.arrayContaining(['stopTrigger', 'stopAuction', 'triggerExposure', 'keeperReserve', 'carrierReturned']));
    expect(plan.cond).toMatchObject({ type: 'stopMarket', keeper: { tip: 2_100_000n, expectedUpdates: 1, reserve: 2_100_000n, fundedFrom: 'carrier' } });
    expect(plan.cond!.legs).toMatchObject({ stop: 230_000_000n, stopWorst: 223_100_000n, slipBps: 300, bandDaa: 300n, limit: null, takeProfit: null, minTouch: 4_348n });
  });

  it('sell: a tip lowers the all-in price; every option is honoured', () => {
    const env = makeEnv();
    const { plan } = planOk(env, {
      type: 'stopMarket', side: 'sell', amount: 4n * TOK, stop: 230_000_000n, tip: 50_000n, slipBps: 500, bandDaa: 100n, keeperTip: 1_000_000n,
      minTouch: 2n * TOK, minRestDaa: 900n,
    });
    expect(askState(plan.states[0]!)).toMatchObject({ tip: '50000', slipBps: '500', bandDaa: '100', keeperTip: '1000000', minTouch: '2000', minRestDaa: '900' });
    const d = plan.disclosure!;
    expect(d.minTouch).toBe(2_000n);
    expect(d.worstPrice).toBe(230_000_000n - 23_000n * 500n);
    expect(d.allInPrice).toBe(d.worstPrice! - 50_000n);
    expect(d.allInTotal).toBe(4n * d.allInPrice!);
    expect(d.allInTotal).toBe(env.kob.condAskProceeds(plan.states[0]!, 4n * TOK, d.worstPrice!));
    expect(d.tip).toBe(50_000n);
  });

  it('buy: KobCondBid escrow at the stop ceiling plus delivery carriers and the arming tip', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, { type: 'stopMarket', side: 'buy', amount: 10n * TOK, stop: 260_000_000n });
    const s = bidState(plan.states[0]!);
    expect(s).toMatchObject({
      extensionCommitment: env.token.extensionCommitment, tpPrice: '0', stopPrice: '260000000', slipBps: '300', bandDaa: '300', keeperTip: '2100000',
      minTouch: '3847', minFill: '3847', armed: '0', amountLeft: '10000', deliveryCarrier: CARRIER.toString(), tip: '0',
    });
    // 10 tokens at the worst price 260 KAS + 26 000 * 300 = 267.8 KAS; three fills funded (ceil(10000 / 3847) = 3); the keeper's arm on top
    const ceiling = 260_000_000n + 26_000n * 300n;
    expect(BigInt(recovered[0]!.value)).toBe(10n * ceiling + 3n * CARRIER + 2_100_000n);
    // the escrow part is exactly kob-wasm condBidEscrow (the spend of the whole amount at the worst leg, rounded down, plus the carriers)
    expect(env.kob.condBidEscrow(plan.states[0]!, 3n)).toBe(10n * ceiling + 3n * CARRIER);
    expect(recovered[0]!.custody).toBeNull();
    const d = plan.disclosure!;
    expect(d).toMatchObject({ side: 'buy', tokensEscrowed: 0n, limitPrice: ceiling, worstPrice: ceiling, expectedPrice: 260_000_000n, allInPrice: ceiling });
    expect(d.carriers).toEqual([
      { kind: 'escrow', amount: 10n * ceiling, count: 1 },
      { kind: 'deliveryCarrier', amount: CARRIER, count: 3 },
      { kind: 'keeperReserve', amount: 2_100_000n, count: 1 },
    ]);
    expect(d.kasLocked).toBe(10n * ceiling + 3n * CARRIER + 2_100_000n);
  });

  it('buy: a tip raises the all-in ceiling and the escrow; maxFills sets the delivery carriers', () => {
    const env = makeEnv();
    const { plan } = planOk(env, { type: 'stopMarket', side: 'buy', amount: 5n * TOK, stop: 260_000_000n, tip: 10_000n, maxFills: 1n });
    const ceiling = 267_800_000n;
    expect(plan.disclosure!.allInPrice).toBe(ceiling + 10_000n);
    expect(plan.disclosure!.carriers[0]).toEqual({ kind: 'escrow', amount: 5n * (ceiling + 10_000n), count: 1 });
    expect(plan.disclosure!.carriers[1]).toEqual({ kind: 'deliveryCarrier', amount: CARRIER, count: 1 });
  });

  it('a buy no larger than its minimum fill funds one delivery carrier (never more fills than the minimum fill allows)', () => {
    const { plan } = planOk(makeEnv(), { type: 'stopMarket', side: 'buy', amount: 1n * TOK, stop: 260_000_000n });
    expect(plan.disclosure!.carriers.find((c) => c.kind === 'deliveryCarrier')).toMatchObject({ count: 1 });
    expect(bidState(plan.states[0]!).minFill).toBe('1000');
  });

  it('the minimum fill and the trigger threshold: defaults, explicit values, invalid values', () => {
    const env = makeEnv();
    const exp = planOk(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, minFill: 500n, minTouch: 2_500n });
    expect(askState(exp.plan.states[0]!)).toMatchObject({ minFill: '500', minTouch: '2500' });
    expect(exp.plan.disclosure).toMatchObject({ minFill: 500n, minTouch: 2_500n });
    // when it is not given the threshold is the larger of the minimum fill and a quarter of the amount
    expect(askState(planOk(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, minFill: 700n }).plan.states[0]!).minTouch).toBe('2500');
    expect(askState(planOk(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, minFill: 3_000n }).plan.states[0]!).minTouch).toBe('3000');
    // 100% of the amount (the strongest protection against stop hunting)
    expect(askState(planOk(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, minTouch: 10n * TOK }).plan.states[0]!).minTouch).toBe('10000');
    expect(errorCodes(planCond(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, minFill: 0n }))).toEqual(['MIN_FILL_INVALID']);
    expect(errorCodes(planCond(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, minFill: 10n * TOK + 1n }))).toEqual(['MIN_FILL_INVALID']);
    expect(errorCodes(planCond(env, { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, minTouch: 0n }))).toEqual(['COND_MIN_TOUCH_INVALID']);
    // a take-profit has no trigger: the disclosure shows none
    expect(planOk(env, { type: 'takeProfit', side: 'sell', amount: TOK, price: 300_000_000n }).plan.disclosure!.minTouch).toBeNull();
    // OCO: the default minimum fill is worth 10 KAS at the LOWER leg of a sell, the HIGHER leg of a buy
    const ocoSell = planOk(env, { type: 'oco', side: 'sell', amount: 10n * TOK, takeProfit: 300_000_000n, stop: 230_000_000n });
    expect(BigInt(askState(ocoSell.plan.states[0]!).minFill)).toBe(defMin(10n * TOK, 230_000_000n));
    const ocoBuy = planOk(env, { type: 'oco', side: 'buy', amount: 10n * TOK, takeProfit: 200_000_000n, stop: 260_000_000n });
    expect(BigInt(bidState(ocoBuy.plan.states[0]!).minFill)).toBe(defMin(10n * TOK, 260_000_000n));
  });

  it('an amount that is not a multiple of the scale: exact custody, totals rounded for the maker', () => {
    const env = makeEnv();
    const sell = planOk(env, { type: 'oco', side: 'sell', amount: 4_321n, takeProfit: 300_000_000n, stop: 230_000_000n, tip: 7n });
    expect(sell.recovered[0]!.custody!.state.amount).toBe('4321');
    // a seller receives at least ceil(4321 x (300 KAS - 7 sompi) / 1000)
    expect(sell.plan.disclosure!.allInTotal).toBe(ceilDiv(4_321n * (300_000_000n - 7n), SCALE));
    const buy = planOk(env, { type: 'takeProfit', side: 'buy', amount: 4_321n, price: 200_000_000n, tip: 7n });
    // a buyer pays at most floor(4321 x (200 KAS + 7 sompi) / 1000); the escrow is the same floor plus one carrier (one fill)
    expect(buy.plan.disclosure!.allInTotal).toBe((4_321n * 200_000_007n) / SCALE);
    expect(BigInt(buy.recovered[0]!.value)).toBe((4_321n * 200_000_007n) / SCALE + CARRIER);
  });
});

describe('stop-limit', () => {
  it('sell: the limit becomes the largest slipBps not beyond it; the band never reaches below the limit', () => {
    const { plan } = planOk(makeEnv(), { type: 'stopLimit', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, limit: 225_000_000n });
    const s = askState(plan.states[0]!);
    expect(s.slipBps).toBe('217');
    const d = plan.disclosure!;
    expect(d.worstPrice).toBe(230_000_000n - 23_000n * 217n);
    expect(d.worstPrice! >= 225_000_000n).toBe(true);
    // one more bps would cross the limit
    expect(230_000_000n - 23_000n * 218n < 225_000_000n).toBe(true);
    expect(plan.cond!.legs).toMatchObject({ limit: 225_000_000n, slipBps: 217 });
    expect(plan.disclosure!.notes).toContain('stopLimitMayNotFill');
  });

  it('buy: mirrored', () => {
    const { plan } = planOk(makeEnv(), { type: 'stopLimit', side: 'buy', amount: 10n * TOK, stop: 260_000_000n, limit: 268_000_000n });
    expect(bidState(plan.states[0]!).slipBps).toBe('307');
    expect(plan.disclosure!.worstPrice).toBe(260_000_000n + 26_000n * 307n);
    expect(plan.disclosure!.worstPrice! <= 268_000_000n).toBe(true);
  });

  it('a limit equal to the stop is a single price: slipBps 0, no auction', () => {
    const { plan } = planOk(makeEnv(), { type: 'stopLimit', side: 'sell', amount: 3n * TOK, stop: 230_000_000n, limit: 230_000_000n });
    expect(askState(plan.states[0]!)).toMatchObject({ slipBps: '0', bandDaa: '0' });
  });

  it('refuses a limit beyond the stop (sell above, buy below) and a missing limit price', () => {
    const env = makeEnv();
    const sell = planCond(env, { type: 'stopLimit', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, limit: 230_000_100n });
    expect(sell.ok).toBe(false);
    expect(errorCodes(sell)).toEqual(['COND_STOP_LIMIT_BEYOND_STOP']);
    expect(sell.issues[0]).toMatchObject({ field: 'limit', params: { side: 'sell', direction: 'above' } });
    const buy = planCond(env, { type: 'stopLimit', side: 'buy', amount: 1n * TOK, stop: 260_000_000n, limit: 259_999_900n });
    expect(errorCodes(buy)).toEqual(['COND_STOP_LIMIT_BEYOND_STOP']);
    const zero = planCond(env, { type: 'stopLimit', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, limit: 0n });
    expect(errorCodes(zero)).toEqual(['PRICE_NOT_POSITIVE']);
  });
});

describe('trailing stop', () => {
  const trail = { step: 1_000_000n, gap: 5_000_000n };

  it('sell: step, gap and wait land in the state; the keeper updates are funded from the carrier', () => {
    const { plan } = planOk(makeEnv(), { type: 'trailingStop', side: 'sell', amount: 10n * TOK, stop: 230_000_000n, trail });
    expect(askState(plan.states[0]!)).toMatchObject({ trailStep: '1000000', trailGap: '5000000', trailWait: '6000', stopPrice: '230000000', tpPrice: '0', armed: '0' });
    expect(plan.cond!.trail).toEqual({ step: 1_000_000n, gap: 5_000_000n, waitDaa: 6_000n, waitSeconds: 600n, maxUpdatesPerDay: 144n, expectedUpdates: 20 });
    // 20 trail updates plus the arm: 21 x 0.021 KAS, inside the 10 KAS carrier (nothing extra is locked)
    expect(plan.cond!.keeper).toEqual({ tip: 2_100_000n, expectedUpdates: 21, reserve: 44_100_000n, fundedFrom: 'carrier' });
    expect(plan.disclosure!.kasLocked).toBe(2n * CARRIER);
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['trailing', 'keeperReserve']));
  });

  it('buy: the keeper reserve is added to the escrow', () => {
    const { plan, recovered } = planOk(makeEnv(), {
      type: 'trailingStop', side: 'buy', amount: 5n * TOK, stop: 260_000_000n, trail: { ...trail, wait: 3_000n, expectedUpdates: 10 },
    });
    expect(bidState(plan.states[0]!)).toMatchObject({ trailStep: '1000000', trailGap: '5000000', trailWait: '3000' });
    const ceiling = 267_800_000n;
    // 5 tokens with a minimum fill of 3847 base units: at most two fills, two carriers
    expect(BigInt(recovered[0]!.value)).toBe(5n * ceiling + 2n * CARRIER + 11n * 2_100_000n);
    expect(plan.disclosure!.carriers).toContainEqual({ kind: 'keeperReserve', amount: 2_100_000n, count: 11 });
  });

  it('a take-profit caps the trail (trailing OCO)', () => {
    const { plan } = planOk(makeEnv(), { type: 'trailingStop', side: 'sell', amount: 2n * TOK, stop: 230_000_000n, trail, takeProfit: 300_000_000n });
    expect(askState(plan.states[0]!)).toMatchObject({ tpPrice: '300000000', stopPrice: '230000000', trailStep: '1000000' });
    expect(plan.disclosure!.limitPrice).toBe(300_000_000n);
    expect(plan.disclosure!.worstPrice).toBe(223_100_000n);
  });

  it('refuses a trail below the minimum interval or off the tick, and funding beyond the carrier', () => {
    const env = makeEnv();
    expect(errorCodes(planCond(env, { type: 'trailingStop', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, trail: { ...trail, wait: 100n } }))).toEqual(['COND_TRAIL_WAIT_TOO_SHORT']);
    expect(errorCodes(planCond(env, { type: 'trailingStop', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, trail: { step: 1_000_050n, gap: 0n } }))).toEqual(['PRICE_NOT_ON_TICK']);
    // 1000 updates x 0.021 KAS = 21 KAS cannot come out of a 10 KAS carrier
    const big = planCond(env, { type: 'trailingStop', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, trail: { ...trail, expectedUpdates: 1_000 } });
    expect(errorCodes(big)).toEqual(['COND_KEEPER_FUNDING_TOO_LARGE']);
    // ... but it fits a larger carrier
    const ok = planOk(makeEnv(), { type: 'trailingStop', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, trail: { ...trail, expectedUpdates: 1_000 }, carrier: 30n * KAS });
    expect(askState(ok.plan.states[0]!).trailStep).toBe('1000000');
  });
});

describe('take-profit', () => {
  it('sell: conditional limit leg only; no keeper, band or slippage', () => {
    const { plan } = planOk(makeEnv(), { type: 'takeProfit', side: 'sell', amount: 10n * TOK, price: 300_000_000n });
    expect(askState(plan.states[0]!)).toMatchObject({ tpPrice: '300000000', stopPrice: '0', slipBps: '0', bandDaa: '0', keeperTip: '0', trailStep: '0', armed: '0' });
    const d = plan.disclosure!;
    expect(d).toMatchObject({ limitPrice: 300_000_000n, allInPrice: 300_000_000n, expectedPrice: 300_000_000n, worstPrice: 300_000_000n, keeperTip: 0n });
    expect(d.notes).toContain('takeProfitLeg');
    expect(d.notes).not.toContain('triggerExposure');
    expect(plan.cond!.keeper).toBeNull();
  });

  it('buy: escrow is the limit all-in for the amount plus delivery carriers (no arming tip)', () => {
    const { plan, recovered } = planOk(makeEnv(), { type: 'takeProfit', side: 'buy', amount: 10n * TOK, price: 200_000_000n, tip: 20_000n });
    expect(bidState(plan.states[0]!)).toMatchObject({ tpPrice: '200000000', stopPrice: '0', keeperTip: '0', tip: '20000', minFill: '5000' });
    // 10 KAS at 2 KAS per token is a 5-token minimum fill: two fills
    expect(BigInt(recovered[0]!.value)).toBe(10n * 200_020_000n + 2n * CARRIER);
    expect(plan.disclosure!).toMatchObject({ limitPrice: 200_000_000n, allInPrice: 200_020_000n, allInTotal: 2_000_200_000n });
  });

  it('warns when the take-profit crosses the market right now (fills at the limit)', () => {
    const env = makeEnv(); // best bid 245 KAS, best ask 250 KAS
    const sell = planOk(env, { type: 'takeProfit', side: 'sell', amount: 1n * TOK, price: 244_000_000n });
    expect(codes(sell.plan)).toEqual(['COND_TP_CROSSES']);
    expect(sell.plan.issues[0]).toMatchObject({ severity: 'warning', params: { touch: 245_000_000n } });
    const buy = planOk(env, { type: 'takeProfit', side: 'buy', amount: 1n * TOK, price: 251_000_000n });
    expect(codes(buy.plan)).toEqual(['COND_TP_CROSSES']);
    // passive prices do not warn
    expect(planOk(env, { type: 'takeProfit', side: 'sell', amount: 1n * TOK, price: 300_000_000n }).plan.issues).toEqual([]);
  });
});

describe('OCO', () => {
  it('sell: ONE KobCondAsk carries both legs; the disclosure spans them', () => {
    const { plan, recovered } = planOk(makeEnv(), { type: 'oco', side: 'sell', amount: 10n * TOK, takeProfit: 300_000_000n, stop: 230_000_000n });
    expect(recovered).toHaveLength(1);
    expect(askState(plan.states[0]!)).toMatchObject({ tpPrice: '300000000', stopPrice: '230000000', slipBps: '300', bandDaa: '300', keeperTip: '2100000', armed: '0' });
    const d = plan.disclosure!;
    expect(d).toMatchObject({ limitPrice: 300_000_000n, worstPrice: 223_100_000n, expectedPrice: null, kasLocked: 2n * CARRIER });
    expect(d.notes).toEqual(expect.arrayContaining(['oco', 'partialFillsKeepLegs']));
  });

  it('buy: one KobCondBid, escrow at the stop ceiling (the higher leg)', () => {
    const { plan, recovered } = planOk(makeEnv(), { type: 'oco', side: 'buy', amount: 10n * TOK, takeProfit: 200_000_000n, stop: 260_000_000n });
    expect(bidState(plan.states[0]!)).toMatchObject({ tpPrice: '200000000', stopPrice: '260000000' });
    const ceiling = 267_800_000n;
    expect(BigInt(recovered[0]!.value)).toBe(10n * ceiling + 3n * CARRIER + 2_100_000n);
    expect(plan.disclosure).toMatchObject({ limitPrice: 200_000_000n, worstPrice: ceiling });
  });

  it('stop-limit OCO: an optional limit turns the stop leg into a stop-limit', () => {
    const { plan } = planOk(makeEnv(), { type: 'oco', side: 'sell', amount: 3n * TOK, takeProfit: 300_000_000n, stop: 230_000_000n, limit: 225_000_000n });
    expect(askState(plan.states[0]!).slipBps).toBe('217');
  });

  it('refuses legs in the wrong order', () => {
    const env = makeEnv();
    expect(errorCodes(planCond(env, { type: 'oco', side: 'sell', amount: 1n * TOK, takeProfit: 230_000_000n, stop: 240_000_000n }))).toEqual(['COND_TP_STOP_ORDER']);
    expect(errorCodes(planCond(env, { type: 'oco', side: 'buy', amount: 1n * TOK, takeProfit: 270_000_000n, stop: 260_000_000n }))).toEqual(['COND_TP_STOP_ORDER']);
  });
});

describe('expiry', () => {
  it('gtd by unix time converts with the clock rate and is refundable from that date', () => {
    const env = makeEnv();
    const at = CLOCK.unixSeconds + 3_600n;
    const { plan } = planOk(env, { type: 'takeProfit', side: 'sell', amount: 1n * TOK, price: 300_000_000n, expiry: { kind: 'gtdUnix', atUnixSeconds: at } });
    expect(askState(plan.states[0]!).expiryDaa).toBe((CLOCK.daa + 36_000n).toString());
    expect(plan.disclosure!.expiry).toEqual({ kind: 'gtd', daa: CLOCK.daa + 36_000n, approxUnixSeconds: at, deadlineUnixSeconds: null });
  });

  it('gtd by DAA', () => {
    const { plan } = planOk(makeEnv(), { type: 'stopMarket', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, expiry: { kind: 'gtdDaa', daa: CLOCK.daa + 1_000n } });
    expect(askState(plan.states[0]!).expiryDaa).toBe((CLOCK.daa + 1_000n).toString());
    expect(plan.disclosure!.expiry.kind).toBe('gtd');
  });

  it('day orders end at 00:00 UTC and publish the deadline in the placement record', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, { type: 'oco', side: 'sell', amount: 1n * TOK, takeProfit: 300_000_000n, stop: 230_000_000n, expiry: { kind: 'day' } });
    expect(recovered[0]!.deadline).toBe('1790726400');
    expect(plan.disclosure!.expiry).toMatchObject({ kind: 'day', deadlineUnixSeconds: 1_790_726_400n });
    expect(askState(plan.states[0]!).expiryDaa).toBe(env.kob.dayOrder(CLOCK.daa, CLOCK.unixSeconds).expiryDaa);
  });

  it('refuses past dates and dates beyond 90 days', () => {
    const env = makeEnv();
    const past = planCond(env, { type: 'stopMarket', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, expiry: { kind: 'gtdUnix', atUnixSeconds: CLOCK.unixSeconds - 5n } });
    expect(errorCodes(past)).toEqual(['EXPIRY_TOO_SOON']);
    const far = planCond(env, { type: 'stopMarket', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, expiry: { kind: 'gtdDaa', daa: CLOCK.daa + 77_760_001n } });
    expect(errorCodes(far)).toEqual(['EXPIRY_TOO_FAR']);
    expect(far.issues[0]!.field).toBe('expiry');
    // exactly 90 days is allowed
    expect(planCond(env, { type: 'stopMarket', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, expiry: { kind: 'gtdDaa', daa: CLOCK.daa + 77_760_000n } }).ok).toBe(true);
  });
});

describe('guards and funding', () => {
  const sellStop: CondIntent = { type: 'stopMarket', side: 'sell', amount: 10n * TOK, stop: 230_000_000n };

  it('refuses a stop that could trade against the user\'s own resting order (self-trade)', () => {
    // an own bid at 240 KAS is at or above the sell stop's worst price 223.1 KAS
    const own = [{ covenantId: 'ab'.repeat(32), side: 'buy' as const, price: 240_000_000n, tip: 0n, amountLeft: 3n * TOK, active: true }];
    const p = planCond(makeEnv({ ownOrders: own }), sellStop);
    expect(p.ok).toBe(false);
    expect(errorCodes(p)).toEqual(['SELF_TRADE']);
    // an own bid below the worst price, an own SELL, or an inactive order are fine
    expect(planCond(makeEnv({ ownOrders: [{ ...own[0]!, price: 220_000_000n }] }), sellStop).ok).toBe(true);
    expect(planCond(makeEnv({ ownOrders: [{ ...own[0]!, side: 'sell' }] }), sellStop).ok).toBe(true);
    expect(planCond(makeEnv({ ownOrders: [{ ...own[0]!, active: false }] }), sellStop).ok).toBe(true);
  });

  it('buy side: an own resting ask at or below the stop ceiling is a self-trade', () => {
    const own = [{ covenantId: 'cd'.repeat(32), side: 'sell' as const, price: 265_000_000n, tip: 0n, amountLeft: 3n * TOK, active: true }];
    const p = planCond(makeEnv({ ownOrders: own }), { type: 'stopMarket', side: 'buy', amount: 1n * TOK, stop: 260_000_000n });
    expect(errorCodes(p)).toEqual(['SELF_TRADE']);
  });

  it('warns (does not refuse) when the market already sits beyond the stop', () => {
    const env = makeEnv(); // best ask 250 KAS: a sell stop at 251 is already reached
    const p = planOk(env, { type: 'stopMarket', side: 'sell', amount: 1n * TOK, stop: 251_000_000n });
    expect(p.plan.issues).toMatchObject([{ code: 'COND_STOP_ALREADY_REACHED', severity: 'warning', field: 'stop' }]);
    // buy: best bid 245 KAS at or above a buy stop of 244
    const q = planOk(env, { type: 'stopMarket', side: 'buy', amount: 1n * TOK, stop: 244_000_000n });
    expect(codes(q.plan)).toEqual(['COND_STOP_ALREADY_REACHED']);
  });

  it('a sell tip that eats the lowest leg is refused', () => {
    const p = planCond(makeEnv(), { type: 'stopMarket', side: 'sell', amount: 1n * TOK, stop: 230_000_000n, tip: 223_100_000n });
    expect(errorCodes(p)).toEqual(['TIP_EXCEEDS_PRICE']);
  });

  it('reports missing tokens and missing KAS instead of throwing', () => {
    const noTokens = planCond(makeEnv({ tokenAmounts: [5n * TOK] }), sellStop);
    expect(noTokens.ok).toBe(false);
    expect(errorCodes(noTokens)).toEqual(['INSUFFICIENT_TOKENS']);
    expect(noTokens.states).toHaveLength(1);
    const noKas = planCond(makeEnv({ funding: [5n * KAS] }), sellStop);
    expect(errorCodes(noKas)).toEqual(['INSUFFICIENT_KAS']);
    const bid = planCond(makeEnv({ funding: [10n * KAS] }), { type: 'stopMarket', side: 'buy', amount: 10n * TOK, stop: 260_000_000n });
    expect(errorCodes(bid)).toEqual(['INSUFFICIENT_KAS']);
  });

  it('the token input\'s own KAS is recycled: order + custody + change carriers (30 KAS) need only 20 KAS of funding', () => {
    // the 100-token UTXO carries 10 KAS; selling 10 tokens leaves change, so three 10 KAS carriers are created
    const short = planCond(makeEnv({ funding: [15n * KAS] }), sellStop);
    expect(short.issues[0]).toMatchObject({ code: 'INSUFFICIENT_KAS', params: { needed: 20n * KAS, have: 15n * KAS } });
    const ok = planOk(makeEnv({ funding: [21n * KAS] }), sellStop);
    expect(ok.plan.built!.tx.inputs).toHaveLength(2);
  });

  it('spreads a sell over several token UTXOs and returns exact change', () => {
    const env = makeEnv({ tokenAmounts: [4n * TOK, 4n * TOK, 4n * TOK] });
    const { plan, recovered } = planOk(env, sellStop);
    expect(recovered[0]!.custody!.state.amount).toBe('10000');
    expect(plan.built!.tx.inputs.filter((i) => i.utxo.covenantId !== null)).toHaveLength(3);
    expect(plan.disclosure!.carriers).toContainEqual({ kind: 'tokenChangeCarrier', amount: CARRIER, count: 1, kept: true });
    // no change when the balance is exactly the order: no change carrier line
    const exact = planOk(makeEnv({ tokenAmounts: [10n * TOK] }), sellStop);
    expect(exact.plan.disclosure!.carriers.map((c) => c.kind)).toEqual(['orderCarrier', 'tokenCarrier']);
  });

  it('a custom carrier applies to the order and the custody', () => {
    const { plan, recovered } = planOk(makeEnv(), { ...sellStop, carrier: 20n * KAS });
    expect(recovered[0]!.value).toBe((20n * KAS).toString());
    expect(recovered[0]!.custody!.value).toBe((20n * KAS).toString());
    expect(plan.disclosure!.kasLocked).toBe(40n * KAS);
    expect(errorCodes(planCond(makeEnv(), { ...sellStop, carrier: 0n }))).toEqual(['COND_CARRIER_INVALID']);
  });

  it('honours the env carrier and the change key', () => {
    const env = { ...makeEnv({ carrier: 12n * KAS }), changeTo: OTHER_PK };
    const { plan, recovered } = planOk(env, sellStop);
    expect(recovered[0]!.value).toBe((12n * KAS).toString());
    expect(plan.request!.change).toBe(OTHER_PK);
  });

  it('a protocol refusal comes back as BUILD_REJECTED, not as an exception', () => {
    const env = makeEnv();
    const unknownProgram = { ...env, token: { ...env.token, templateHash: '00'.repeat(32) } };
    const p = planCond(unknownProgram, sellStop);
    expect(p.ok).toBe(false);
    expect(errorCodes(p)).toEqual(['BUILD_REJECTED']);
    expect(p.built).toBeNull();
    expect(p.states).toHaveLength(1);
  });

  it('works with the 8/8 token program (refund tip per program) and with several funding UTXOs', () => {
    const market = market8x8();
    const env = { ...makeEnv({ market, funding: [3n * KAS, 4n * KAS, 15n * KAS] }), tokenUtxos: [tokenUtxo(market, 100n * TOK)] };
    const { plan } = planOk(env, sellStop);
    expect(askState(plan.states[0]!)).toMatchObject({ refundTip: '4800000', keeperTip: '2100000', tokenTplHash: market.templateHash });
    expect(plan.built!.tx.inputs.length).toBeGreaterThan(2);
  });
});

describe('intent validation', () => {
  it('reports unknown types, sides and amounts without throwing', () => {
    const env = makeEnv();
    const bad = (i: unknown) => planCond(env, i as CondIntent);
    expect(errorCodes(bad({ type: 'nope', side: 'sell', amount: 1n * TOK }))).toEqual(['INTENT_UNKNOWN_TYPE']);
    expect(errorCodes(bad({ type: 'takeProfit', side: 'long', amount: 1n * TOK, price: 300_000_000n }))).toEqual(['SIDE_INVALID']);
    expect(errorCodes(bad({ type: 'takeProfit', side: 'sell', amount: 0n, price: 300_000_000n }))).toEqual(['AMOUNT_NOT_POSITIVE']);
    expect(errorCodes(bad({ type: 'takeProfit', side: 'sell', amount: 3, price: 300_000_000n }))).toEqual(['AMOUNT_NOT_POSITIVE']);
    expect(errorCodes(bad({ type: 'takeProfit', side: 'sell', amount: 1n << 63n, price: 300_000_000n }))).toEqual(['AMOUNT_TOO_LARGE']);
    expect(errorCodes(bad({ type: 'takeProfit', side: 'sell', amount: 1n * TOK, price: 300_000_000n, tip: -1n }))).toEqual(['TIP_NEGATIVE']);
    expect(errorCodes(bad({ type: 'takeProfit', side: 'sell', amount: 1n * TOK, price: 300_000_050n }))).toEqual(['PRICE_NOT_ON_TICK']);
    expect(errorCodes(bad({ type: 'stopMarket', side: 'buy', amount: 2n * TOK, stop: 260_000_000n, maxFills: 3n }))).toEqual(['MAX_FILLS_INVALID']);
  });

  it('plan.ok is false and nothing is built when there is an error; states are only listed once the order is fully specified', () => {
    const p = planCond(makeEnv(), { type: 'stopMarket', side: 'sell', amount: 0n, stop: 230_000_000n });
    expect(p).toMatchObject({ ok: false, built: null, request: null, disclosure: null, states: [] });
  });

  it('the kob-wasm build is deterministic for the same env (same request twice)', () => {
    const env = makeEnv();
    const a = planCond(env, { type: 'stopMarket', side: 'sell', amount: 2n * TOK, stop: 230_000_000n });
    const b = planCond(env, { type: 'stopMarket', side: 'sell', amount: 2n * TOK, stop: 230_000_000n });
    expect(a.built).toEqual(b.built);
  });
});

describe('timed activation (activeFrom) of plain conditionals', () => {
  const at = CLOCK.daa + 36_000n;
  const mk = (over: Record<string, unknown> = {}): CondIntent => ({ type: 'stopMarket', side: 'sell', amount: 4n * TOK, stop: 230_000_000n, ...over }) as CondIntent;

  it('the state carries activeFrom and the disclosure says when it activates (sell and buy, DAA and unix forms)', () => {
    const env = makeEnv();
    const s = planOk(env, mk({ activeFrom: { daa: at } }));
    expect(askState(s.plan.states[0]!).activeFrom).toBe(at.toString());
    expect(s.plan.disclosure!.activatesAt).toMatchObject({ daa: at });
    // GTC keeps counting from the placement (as for plain orders)
    expect(askState(s.plan.states[0]!).expiryDaa).toBe(GTC.toString());
    const b = planOk(env, { type: 'oco', side: 'buy', amount: 2n * TOK, stop: 260_000_000n, takeProfit: 220_000_000n, activeFrom: { unixSeconds: CLOCK.unixSeconds + 3_600n } });
    expect(BigInt(bidState(b.plan.states[0]!).activeFrom)).toBeGreaterThan(CLOCK.daa);
    expect(b.plan.disclosure!.activatesAt).not.toBeNull();
    // without it nothing changes
    const none = planOk(env, mk());
    expect(askState(none.plan.states[0]!).activeFrom).toBe('0');
    expect(none.plan.disclosure!.activatesAt).toBeNull();
  });

  it('a moment in the past is an info and the order is active at once; beyond 90 days or after the expiry is an error', () => {
    const env = makeEnv();
    const past = planCond(env, mk({ activeFrom: { daa: CLOCK.daa - 5n } }));
    expect(past.ok).toBe(true);
    expect(past.issues.find((i) => i.code === 'ACTIVE_IN_PAST')?.severity).toBe('info');
    expect(askState(past.states[0]!).activeFrom).toBe('0');
    expect(past.disclosure!.activatesAt).toBeNull();
    expect(errorCodes(planCond(env, mk({ activeFrom: { daa: GTC + 1n } })))).toContain('ACTIVE_TOO_FAR');
    expect(errorCodes(planCond(env, mk({ activeFrom: { daa: CLOCK.daa + 20_000n }, expiry: { kind: 'gtdDaa', daa: CLOCK.daa + 10_000n } })))).toContain('ACTIVE_AFTER_EXPIRY');
  });
});
