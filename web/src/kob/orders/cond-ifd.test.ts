// IFD, IFO / bracket (stop entries included) and repeat IFD / IFO, both sides. Every plan is consensus-checked (cond-testkit.planOk).
// Amounts are base units of the 3-decimal test token (TOK = 1000 base units = one token = the scale), prices sompi per whole token.
import { describe, expect, it } from 'vitest';
import type { CondIntent, ExitSpec } from '../intent-cond';
import { CLOCK, KAS, TOK, makeEnv } from '../../testing/fixtures';
import { ceilDiv } from '../units';
import { planCond } from './cond';
import { MERGE_COST_BUY_FIRST, MERGE_COST_SELL_FIRST, mergeTipRate } from './cond-common';
import { askState, bidState, codes, committedExit, errorCodes, ifdAsk, ifdBid, planOk } from './cond-testkit';

const C = 1_000_000_000n; // carrier
const GTC_EXIT = 1n << 62n;
const GTC = CLOCK.daa + 77_760_000n;
const ZERO = '0'.repeat(64);
const SCALE = 1_000n;
const KT = 2_100_000n; // keeper tip of the reference program (kob-wasm keeperTips)
const RT = '3500000'; // refund tip of the reference program

const buyFirst = (over: Partial<Extract<CondIntent, { type: 'ifd' }>> = {}): CondIntent => ({
  type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n }, ...over,
});
const sellFirst = (over: Partial<Extract<CondIntent, { type: 'ifd' }>> = {}): CondIntent => ({
  type: 'ifd', side: 'sell', amount: 10n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n }, ...over,
});

describe('IFD buy-first (KobIfdBid -> KobCondAsk exit)', () => {
  it('states, funding and disclosure of a take-profit exit', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, buyFirst());
    const e = ifdBid(plan.states[0]!);
    expect(e).toMatchObject({
      maker: env.maker, scale: '1000', amountLeft: '10000', price: '240000000', tip: '0', activeFrom: '0', expiryDaa: GTC.toString(), refundTip: RT,
      deliveryCarrier: C.toString(), exitCarrier: C.toString(), minFill: '2500', entryStop: '0', bandDaa: '0', keeperTip: '0', armed: '0', rptAmount: '0',
      minTouch: '0', extensionCommitment: env.token.extensionCommitment,
    });
    // minFill default ceil(10000 / 4) = 2500 -> at most 4 fills, each needs a delivery and an exit carrier: exactly kob-wasm ifdBidEscrow
    expect(recovered[0]!.value).toBe((10n * 240_000_000n + 4n * 2n * C).toString());
    expect(env.kob.ifdBidEscrow(plan.states[0]!)).toBe(10n * 240_000_000n + 4n * 2n * C);
    // the committed exit round-trips through kob-wasm (IfdBidState::exit()) and is the last listed state
    expect(plan.states).toHaveLength(2);
    expect(committedExit(env, plan.states[0]!)).toEqual(plan.states[1]);
    const x = askState(plan.states[1]!);
    expect(x).toMatchObject({
      amountLeft: '10000', tpPrice: '300000000', stopPrice: '0', tip: '0', expiryDaa: GTC_EXIT.toString(), refundTip: RT, keeperTip: '0',
      slipBps: '0', bandDaa: '0', armed: '0', parent: ZERO, rptPrice: '0', rptUntil: '0', maker: env.maker, scale: '1000',
    });
    // the exit's minimum fill: the amount worth 10 KAS at its leg (3334 at 3 KAS), at most the entry's minimum fill (one exit per entry fill)
    expect(x.minFill).toBe('2500');
    const d = plan.disclosure!;
    expect(d).toMatchObject({
      side: 'buy', tokenAmount: 10_000n, scale: 1_000n, minFill: 2_500n, minTouch: null, tokensEscrowed: 0n, limitPrice: 240_000_000n,
      allInPrice: 240_000_000n, allInTotal: 2_400_000_000n, worstPrice: 240_000_000n, expectedPrice: null, tip: 0n, kasLocked: 10n * 240_000_000n + 8n * C,
    });
    expect(d.carriers).toEqual([
      { kind: 'escrow', amount: 2_400_000_000n, count: 1 },
      { kind: 'deliveryCarrier', amount: C, count: 4 },
      { kind: 'exitCarrier', amount: C, count: 4 },
    ]);
    expect(d.notes).toEqual(expect.arrayContaining(['ifd', 'position', 'buyFirst', 'minFill', 'exitGtc']));
    expect(plan.cond!.entry).toEqual({ price: 240_000_000n, stop: null, bandDaa: 0n, minFill: 2_500n, maxFills: 4n, minTouch: 0n, minRestDaa: 50n });
    expect(plan.cond!.exit).toMatchObject({ side: 'sell', takeProfitAllIn: 300_000_000n, stopWorstAllIn: null, expiry: 'gtc', prefund: null, tip: 0n, minFill: 2_500n });
    expect(plan.cond!.repeat).toBeNull();
  });

  it('a stop-loss exit: the sell stop leg with the wallet defaults; its trigger threshold is a quarter of the entry amount', () => {
    const env = makeEnv();
    const { plan } = planOk(env, buyFirst({ exit: { stop: 200_000_000n } }));
    expect(askState(plan.states[1]!)).toMatchObject({
      tpPrice: '0', stopPrice: '200000000', slipBps: '300', bandDaa: '300', keeperTip: KT.toString(), minTouch: '2500', minFill: '2500', minRestDaa: '50',
    });
    expect(plan.cond!.exit).toMatchObject({ stopWorstAllIn: 200_000_000n - 20_000n * 300n, takeProfitAllIn: null });
    expect(plan.cond!.exit!.legs.minTouch).toBe(2_500n);
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['exitStop', 'triggerExposure']));
  });

  it('the exit minimum fill and trigger threshold: defaults and explicit values', () => {
    const env = makeEnv();
    // a large order: the 10-KAS amount at the 2 KAS stop (5 tokens) is below the entry minimum fill (25 tokens); the
    // exit's trigger threshold is a quarter of the entry's amount (25 tokens), not its own minimum fill: one print of 5
    // tokens does not arm the stop-loss of a 100-token position
    const big = planOk(env, buyFirst({ amount: 100n * TOK, exit: { stop: 200_000_000n } }));
    expect(askState(big.plan.states[1]!)).toMatchObject({ minFill: '5000', minTouch: '25000' });
    expect(BigInt(askState(big.plan.states[1]!).minTouch)).toBe(env.kob.defaultMinTouch(5_000n, 100n * TOK));
    expect(BigInt(askState(big.plan.states[1]!).minFill)).toBe(env.kob.defaultMinFill(100n * TOK, 200_000_000n, SCALE));
    const exp = planOk(env, buyFirst({ exit: { stop: 200_000_000n, minFill: 700n, minTouch: 10n * TOK } }));
    expect(askState(exp.plan.states[1]!)).toMatchObject({ minFill: '700', minTouch: '10000' });
    expect(exp.plan.cond!.exit).toMatchObject({ minFill: 700n });
    expect(errorCodes(planCond(env, buyFirst({ exit: { takeProfit: 300_000_000n, minFill: 0n } })))).toEqual(['MIN_FILL_INVALID']);
  });

  it('a stop-limit exit and a trailing stop exit', () => {
    const env = makeEnv();
    const sl = planOk(env, buyFirst({ exit: { stop: 200_000_000n, stopLimit: 190_000_000n } }));
    expect(askState(sl.plan.states[1]!).slipBps).toBe('500'); // floor(10_000_000 / 20_000)
    expect(sl.plan.disclosure!.notes).toContain('stopLimitMayNotFill');
    const tr = planOk(env, buyFirst({ exit: { stop: 200_000_000n, trail: { step: 1_000_000n, gap: 4_000_000n, wait: 3_000n, expectedUpdates: 5 } } }));
    expect(askState(tr.plan.states[1]!)).toMatchObject({ trailStep: '1000000', trailGap: '4000000', trailWait: '3000' });
    expect(tr.plan.disclosure!.notes).toContain('trailing');
    expect(tr.plan.cond!.exit!.keeper).toMatchObject({ expectedUpdates: 6, fundedFrom: 'carrier' });
  });

  it('refuses an exit whose trailing keeper funding exceeds its carrier', () => {
    const p = planCond(makeEnv(), buyFirst({ exit: { stop: 200_000_000n, trail: { step: 1_000_000n, gap: 0n, expectedUpdates: 500 } } }));
    expect(errorCodes(p)).toEqual(['COND_KEEPER_FUNDING_TOO_LARGE']);
  });

  it('IFO / bracket: the exit is an OCO (take-profit + stop) and the plan is one position', () => {
    const env = makeEnv();
    const { plan } = planOk(env, { type: 'ifo', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n, stop: 200_000_000n } });
    expect(askState(plan.states[1]!)).toMatchObject({ tpPrice: '300000000', stopPrice: '200000000', slipBps: '300', amountLeft: '10000' });
    expect(committedExit(env, plan.states[0]!)).toEqual(plan.states[1]);
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['ifo', 'position']));
    expect(plan.states.map((s) => s.kind)).toEqual(['KobIfdBid', 'KobCondAsk']);
  });

  it('a tip on the entry and on the exit, minFill override', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, buyFirst({ tip: 30_000n, minFill: 5n * TOK, exit: { takeProfit: 300_000_000n, tip: 40_000n } }));
    expect(ifdBid(plan.states[0]!)).toMatchObject({ tip: '30000', minFill: '5000' });
    expect(askState(plan.states[1]!).tip).toBe('40000');
    // 10 tokens at 240 KAS + 0.0003 tip, two fills
    expect(recovered[0]!.value).toBe((10n * 240_030_000n + 2n * 2n * C).toString());
    expect(plan.disclosure).toMatchObject({ allInPrice: 240_030_000n, allInTotal: 2_400_300_000n, tip: 30_000n });
    expect(plan.cond!.exit!.takeProfitAllIn).toBe(300_000_000n - 40_000n);
  });

  it('minFill defaults to ceil(amount / 4) for any size and bounds the funded fills', () => {
    for (const [amount, minFill, fills] of [[1n, 1n, 1n], [3n, 1n, 3n], [4n, 1n, 4n], [5n, 2n, 3n], [8n, 2n, 4n], [10_000n, 2_500n, 4n], [17_001n, 4_251n, 4n]] as const) {
      const { plan } = planOk(makeEnv(), buyFirst({ amount }));
      expect(ifdBid(plan.states[0]!).minFill, `amount ${amount}`).toBe(minFill.toString());
      expect(BigInt(ifdBid(plan.states[0]!).minFill)).toBe(makeEnv().kob.defaultMinFillIfd(amount));
      expect(plan.cond!.entry!.maxFills).toBe(fills);
    }
    // minFill = amount: a single fill
    const one = planOk(makeEnv(), buyFirst({ minFill: 10n * TOK }));
    expect(one.plan.cond!.entry!.maxFills).toBe(1n);
    expect(BigInt(one.recovered[0]!.value)).toBe(10n * 240_000_000n + 2n * C);
  });

  it('an amount that is not a multiple of the scale: the escrow and the totals follow the quote rule', () => {
    const env = makeEnv();
    const bf = planOk(env, { type: 'ifo', side: 'buy', amount: 4_321n, tip: 7n, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n, stop: 200_000_000n } });
    // spend of the whole amount at the limit (rounded down) + ceil(4321 / 1081) = 4 x (delivery + exit) carriers
    expect(BigInt(bf.recovered[0]!.value)).toBe((4_321n * 240_000_007n) / SCALE + 4n * 2n * C);
    expect(BigInt(bf.recovered[0]!.value)).toBe(env.kob.ifdBidEscrow(bf.plan.states[0]!));
    expect(bf.plan.disclosure!.allInTotal).toBe((4_321n * 240_000_007n) / SCALE);
    const sf = planOk(env, { type: 'ifo', side: 'sell', amount: 4_321n, tip: 7n, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n, stop: 280_000_000n } });
    expect(sf.recovered[0]!.custody!.state.amount).toBe('4321');
    // prefund rate: the stop ceiling 288.4 KAS less the proceeds 260 KAS - 7 sompi; ceil(4321 x 28_400_007 / 1000) + 3 sompi of rounding (4 fills)
    expect(ifdAsk(sf.plan.states[0]!).prefund).toBe('28400007');
    expect(BigInt(sf.recovered[0]!.value)).toBe(C + ceilDiv(4_321n * 28_400_007n, SCALE) + 3n + 4n * C);
    expect(BigInt(sf.recovered[0]!.value)).toBe(env.kob.ifdAskEscrow(sf.plan.states[0]!, C));
    // a seller receives at least ceil(4321 x (260 KAS - 7 sompi) / 1000)
    expect(sf.plan.disclosure!.allInTotal).toBe(ceilDiv(4_321n * 259_999_993n, SCALE));
  });

  it('stop entry: armed by a touch, auction from the stop to the limit; the arming tip is funded on top', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, buyFirst({ entry: { price: 260_000_000n, stop: 255_000_000n } }));
    // the entry's trigger threshold defaults to its minimum fill (2500)
    expect(ifdBid(plan.states[0]!)).toMatchObject({ entryStop: '255000000', bandDaa: '300', keeperTip: KT.toString(), minTouch: '2500', minRestDaa: '50', armed: '0' });
    expect(recovered[0]!.value).toBe((10n * 260_000_000n + 8n * C + KT).toString());
    expect(plan.disclosure).toMatchObject({ limitPrice: 260_000_000n, expectedPrice: 255_000_000n, worstPrice: 260_000_000n, keeperTip: KT, minTouch: 2_500n });
    expect(plan.disclosure!.carriers).toContainEqual({ kind: 'keeperReserve', amount: KT, count: 1 });
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['stopEntry', 'stopEntryAuction', 'triggerExposure']));
    expect(plan.cond!.entry).toMatchObject({ stop: 255_000_000n, bandDaa: 300n, minTouch: 2_500n });
    // a stop equal to the limit is one price: no auction
    const flat = planOk(env, buyFirst({ entry: { price: 260_000_000n, stop: 260_000_000n } }));
    expect(ifdBid(flat.plan.states[0]!)).toMatchObject({ entryStop: '260000000', bandDaa: '0' });
  });

  it('stop entry knobs: band, keeper tip, trigger threshold and exposure', () => {
    const { plan } = planOk(makeEnv(), buyFirst({ entry: { price: 260_000_000n, stop: 255_000_000n, bandDaa: 100n, keeperTip: 1_000_000n, minTouch: 3n * TOK, minRestDaa: 900n } }));
    expect(ifdBid(plan.states[0]!)).toMatchObject({ bandDaa: '100', keeperTip: '1000000', minTouch: '3000', minRestDaa: '900' });
  });

  it('the exit lives until cancelled by default; a dated exit is committed as given', () => {
    const at = CLOCK.daa + 500_000n;
    const { plan } = planOk(makeEnv(), buyFirst({ exit: { takeProfit: 300_000_000n, expiry: { kind: 'gtdDaa', daa: at } } }));
    expect(askState(plan.states[1]!).expiryDaa).toBe(at.toString());
    expect(plan.cond!.exit!.expiry).toBe('gtd');
    expect(plan.disclosure!.notes).toContain('exitGtd');
  });

  it('the entry can be a day order (deadline in the placement record); an exit cannot', () => {
    const env = makeEnv();
    const { recovered, plan } = planOk(env, buyFirst({ expiry: { kind: 'day' } }));
    expect(recovered[0]!.deadline).toBe('1790726400');
    expect(plan.disclosure!.expiry.kind).toBe('day');
    const bad = planCond(env, buyFirst({ exit: { takeProfit: 300_000_000n, expiry: { kind: 'day' } as never } }));
    expect(errorCodes(bad)).toEqual(['COND_EXIT_EXPIRY_DAY']);
  });
});

describe('IFD sell-first (KobIfdAsk -> KobCondBid exit)', () => {
  it('states, custody and funding of a take-profit (buy-back) exit', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, sellFirst());
    const e = ifdAsk(plan.states[0]!);
    expect(e).toMatchObject({
      amountLeft: '10000', price: '260000000', tip: '0', prefund: '0', exitCarrier: C.toString(), minFill: '2500', entryStop: '0', bandDaa: '0',
      keeperTip: '0', armed: '0', rptAmount: '0', refundTip: RT,
    });
    // custody holds exactly the 10 tokens; the order UTXO carries the entry carrier + 4 exit carriers + 3 sompi of prefund rounding (kob-wasm ifdAskEscrow)
    expect(recovered[0]!.custody!.state).toMatchObject({ amount: '10000', owner: recovered[0]!.covenantId, owner_scheme: 4 });
    expect(recovered[0]!.value).toBe((C + 4n * C + 3n).toString());
    expect(env.kob.ifdAskEscrow(plan.states[0]!, C)).toBe(C + 4n * C + 3n);
    expect(committedExit(env, plan.states[0]!)).toEqual(plan.states[1]);
    const x = bidState(plan.states[1]!);
    expect(x).toMatchObject({
      amountLeft: '10000', tpPrice: '240000000', stopPrice: '0', deliveryCarrier: C.toString(), expiryDaa: GTC_EXIT.toString(), parent: ZERO, rptPrice: '0',
      rptPre: '0', rptUntil: '0', extensionCommitment: env.token.extensionCommitment,
    });
    const d = plan.disclosure!;
    expect(d).toMatchObject({ side: 'sell', tokensEscrowed: 10_000n, limitPrice: 260_000_000n, allInPrice: 260_000_000n, allInTotal: 2_600_000_000n, kasLocked: 6n * C + 3n });
    expect(d.carriers).toEqual([
      { kind: 'orderCarrier', amount: C, count: 1 },
      { kind: 'prefund', amount: 3n, count: 1 },
      { kind: 'exitCarrier', amount: C, count: 4 },
      { kind: 'tokenCarrier', amount: C, count: 1 },
      { kind: 'tokenChangeCarrier', amount: C, count: 1, kept: true },
    ]);
    expect(d.notes).toEqual(expect.arrayContaining(['sellFirst']));
  });

  it("prefund: the default is the smallest rate covering the exit stop leg's worst buy-back", () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, { type: 'ifo', side: 'sell', amount: 10n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n, stop: 280_000_000n } });
    // the stop ceiling is 280 KAS + 28 000 * 300 = 288.4 KAS; the entry brings 260 KAS per token: 28.4 KAS per token must be pre-funded
    const prefund = 288_400_000n - 260_000_000n;
    expect(ifdAsk(plan.states[0]!).prefund).toBe(prefund.toString());
    // ceil(10000 x 28.4 KAS / 1000) + 3 sompi of rounding (each of the 4 fills rounds its prefund up)
    expect(BigInt(recovered[0]!.value)).toBe(C + 10n * prefund + 3n + 4n * C);
    expect(plan.disclosure!.carriers).toContainEqual({ kind: 'prefund', amount: 10n * prefund + 3n, count: 1 });
    expect(plan.cond!.exit).toMatchObject({ prefund, side: 'buy', stopWorstAllIn: 288_400_000n });
    expect(bidState(plan.states[1]!)).toMatchObject({ tpPrice: '240000000', stopPrice: '280000000', slipBps: '300' });
    expect(committedExit(env, plan.states[0]!)).toEqual(plan.states[1]);
  });

  it('a larger prefund is honoured; a shorter one is refused with the rate needed', () => {
    const env = makeEnv();
    const legs: ExitSpec = { takeProfit: 240_000_000n, stop: 280_000_000n };
    const more = planOk(env, { type: 'ifo', side: 'sell', amount: 2n * TOK, entry: { price: 260_000_000n }, exit: legs, prefund: 40_000_000n });
    expect(ifdAsk(more.plan.states[0]!).prefund).toBe('40000000');
    const less = planCond(env, { type: 'ifo', side: 'sell', amount: 2n * TOK, entry: { price: 260_000_000n }, exit: legs, prefund: 28_399_999n });
    expect(less.ok).toBe(false);
    expect(errorCodes(less)).toEqual(['COND_PREFUND_SHORT']);
    expect(less.issues[0]!.params).toMatchObject({ needed: 28_400_000n, given: 28_399_999n });
    const neg = planCond(env, { type: 'ifo', side: 'sell', amount: 2n * TOK, entry: { price: 260_000_000n }, exit: legs, prefund: -1n });
    expect(errorCodes(neg)).toEqual(['COND_PREFUND_INVALID']);
    // a tip on the entry lowers its proceeds: the prefund grows by that amount
    const tipped = planOk(env, { type: 'ifo', side: 'sell', amount: 2n * TOK, entry: { price: 260_000_000n }, exit: legs, tip: 10_000n });
    expect(ifdAsk(tipped.plan.states[0]!).prefund).toBe((28_400_000n + 10_000n).toString());
  });

  it('a sell-stop entry: the arming keeper is paid from the entry carrier, so nothing extra is locked', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, sellFirst({ entry: { price: 240_000_000n, stop: 244_000_000n }, exit: { takeProfit: 200_000_000n } }));
    expect(ifdAsk(plan.states[0]!)).toMatchObject({ entryStop: '244000000', bandDaa: '300', keeperTip: KT.toString(), minTouch: '2500', prefund: '0' });
    expect(recovered[0]!.value).toBe((C + 4n * C + 3n).toString());
    expect(plan.disclosure).toMatchObject({ expectedPrice: 244_000_000n, limitPrice: 240_000_000n, worstPrice: 240_000_000n });
    expect(plan.cond!.keeper).toMatchObject({ tip: KT, fundedFrom: 'carrier', expectedUpdates: 1 });
  });

  it('trailing exits and stop-limit exits work sell-first too (buy stop leg)', () => {
    const env = makeEnv();
    const { plan } = planOk(env, {
      type: 'ifo', side: 'sell', amount: 4n * TOK, entry: { price: 260_000_000n },
      exit: { takeProfit: 240_000_000n, stop: 280_000_000n, stopLimit: 285_000_000n, trail: { step: 1_000_000n, gap: 3_000_000n } },
    });
    expect(bidState(plan.states[1]!)).toMatchObject({ trailStep: '1000000', trailGap: '3000000', slipBps: '178' });
    // limit 285 KAS: floor(5_000_000 / 28_000) = 178 bps
    expect(plan.cond!.exit!.legs.limit).toBe(285_000_000n);
    expect(ifdAsk(plan.states[0]!).prefund).toBe((280_000_000n + 28_000n * 178n - 260_000_000n).toString());
  });
});

describe('IFD validation', () => {
  const env = makeEnv();
  const bad = (i: CondIntent) => planCond(env, i);

  it('checks the exit legs each type demands', () => {
    expect(errorCodes(bad(buyFirst({ exit: {} })))).toEqual(['COND_EXIT_NEEDS_ONE_LEG']);
    expect(errorCodes(bad(buyFirst({ exit: { takeProfit: 300_000_000n, stop: 200_000_000n } })))).toEqual(['COND_EXIT_NEEDS_ONE_LEG']);
    expect(errorCodes(bad({ type: 'ifo', side: 'buy', amount: TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n } }))).toEqual(['COND_EXIT_NEEDS_BOTH']);
    expect(errorCodes(bad({ type: 'ifo', side: 'buy', amount: TOK, entry: { price: 240_000_000n }, exit: { stop: 200_000_000n } }))).toEqual(['COND_EXIT_NEEDS_BOTH']);
    expect(errorCodes(bad({ type: 'repeatIfd', side: 'buy', amount: TOK, entry: { price: 240_000_000n }, exit: { stop: 200_000_000n } }))).toEqual(['COND_EXIT_NEEDS_TP_ONLY']);
    expect(errorCodes(bad({ type: 'repeatIfd', side: 'buy', amount: TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n, stop: 200_000_000n } }))).toEqual(['COND_EXIT_NEEDS_TP_ONLY']);
    expect(errorCodes(bad(buyFirst({ exit: { takeProfit: 300_000_000n, stopLimit: 190_000_000n } })))).toEqual(['COND_EXIT_LIMIT_NEEDS_STOP']);
  });

  it('exit legs must be in order (IFO: take-profit above the stop when selling, below when buying back)', () => {
    expect(errorCodes(bad({ type: 'ifo', side: 'buy', amount: TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 200_000_000n, stop: 210_000_000n } }))).toEqual(['COND_TP_STOP_ORDER']);
    expect(errorCodes(bad({ type: 'ifo', side: 'sell', amount: TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 280_000_000n, stop: 270_000_000n } }))).toEqual(['COND_TP_STOP_ORDER']);
  });

  it('entry stop beyond the limit: a buy stop above its limit, a sell stop below it', () => {
    const b = bad(buyFirst({ entry: { price: 260_000_000n, stop: 260_000_100n } }));
    expect(errorCodes(b)).toEqual(['COND_ENTRY_STOP_BEYOND_LIMIT']);
    expect(b.issues[0]).toMatchObject({ field: 'entry.stop', params: { side: 'buy', direction: 'below' } });
    const s = bad(sellFirst({ entry: { price: 260_000_000n, stop: 259_999_900n } }));
    expect(errorCodes(s)).toEqual(['COND_ENTRY_STOP_BEYOND_LIMIT']);
    expect(s.issues[0]).toMatchObject({ params: { side: 'sell', direction: 'above' } });
  });

  it('minFill outside 1..amount', () => {
    for (const minFill of [0n, -1n, 10n * TOK + 1n]) {
      const p = bad(buyFirst({ minFill }));
      expect(errorCodes(p), String(minFill)).toEqual(['COND_MIN_FILL_INVALID']);
      expect(p.issues[0]!.params).toMatchObject({ amount: 10n * TOK });
    }
  });

  it('prices on the tick; tips within the entry price', () => {
    expect(bad(buyFirst({ entry: { price: 240_000_050n } })).issues[0]).toMatchObject({ code: 'PRICE_NOT_ON_TICK', field: 'entry.price' });
    expect(bad(buyFirst({ exit: { takeProfit: 300_000_050n } })).issues[0]).toMatchObject({ code: 'PRICE_NOT_ON_TICK', field: 'exit.takeProfit' });
    expect(errorCodes(bad(sellFirst({ tip: 260_000_000n })))).toEqual(['TIP_EXCEEDS_PRICE']);
    expect(errorCodes(bad(buyFirst({ exit: { takeProfit: 300_000_000n, tip: -1n } })))).toEqual(['TIP_NEGATIVE']);
  });

  it('a plain IFD may exit at a loss but says so (warning); the plan is still built', () => {
    const { plan } = planOk(env, buyFirst({ exit: { takeProfit: 230_000_000n } }));
    expect(plan.issues).toMatchObject([{ code: 'COND_TP_NOT_PROFITABLE', severity: 'warning', params: { profitPerToken: -10_000_000n } }]);
    const sell = planOk(env, sellFirst({ exit: { takeProfit: 270_000_000n } }));
    expect(codes(sell.plan)).toEqual(['COND_TP_NOT_PROFITABLE']);
  });

  it('self-trade: the entry is an error, the exit only a warning', () => {
    const own = { covenantId: 'ab'.repeat(32), price: 235_000_000n, tip: 0n, amountLeft: 2n * TOK, active: true };
    const entry = planCond(makeEnv({ ownOrders: [{ ...own, side: 'sell' }] }), buyFirst());
    expect(errorCodes(entry)).toEqual(['SELF_TRADE']);
    // an own bid at 305 KAS could match the exit's take-profit at 300 KAS later: warn only
    const exit = planOk(makeEnv({ ownOrders: [{ ...own, side: 'buy', price: 305_000_000n }] }), buyFirst());
    expect(exit.plan.issues).toMatchObject([{ code: 'SELF_TRADE', severity: 'warning', field: 'exit' }]);
  });

  it('warns when the entry crosses the market at placement (limit entries only)', () => {
    // best ask 250 KAS: a buy entry at 251 fills right away at its limit
    const crossing = planOk(env, buyFirst({ entry: { price: 251_000_000n }, exit: { takeProfit: 300_000_000n } }));
    expect(codes(crossing.plan)).toEqual(['COND_ENTRY_CROSSES']);
    const sell = planOk(env, sellFirst({ entry: { price: 244_000_000n }, exit: { takeProfit: 200_000_000n } }));
    expect(codes(sell.plan)).toEqual(['COND_ENTRY_CROSSES']);
    // a stop entry is not "crossing": it waits for its trigger
    expect(codes(planOk(env, buyFirst({ entry: { price: 260_000_000n, stop: 251_000_000n } })).plan)).toEqual([]);
  });

  it('validates the stop entry parameters', () => {
    const entry = { price: 260_000_000n, stop: 255_000_000n };
    const cases: [Partial<typeof entry> & Record<string, bigint>, string][] = [
      [{ bandDaa: -1n }, 'COND_BAND_INVALID'],
      [{ keeperTip: -1n }, 'COND_KEEPER_TIP_INVALID'],
      [{ minTouch: 0n }, 'COND_MIN_TOUCH_INVALID'],
      [{ minRestDaa: -1n }, 'COND_MIN_REST_INVALID'],
    ];
    for (const [extra, code] of cases) expect(errorCodes(bad(buyFirst({ entry: { ...entry, ...extra } }))), code).toEqual([code]);
    // both prices are checked at once
    expect(bad(buyFirst({ entry: { price: 260_000_050n, stop: 255_000_050n } })).issues.map((i) => i.field)).toEqual(['entry.price', 'entry.stop']);
  });

  it('reports missing tokens (sell-first) and missing KAS (both) as issues', () => {
    expect(errorCodes(planCond(makeEnv({ tokenAmounts: [3n * TOK] }), sellFirst()))).toEqual(['INSUFFICIENT_TOKENS']);
    expect(errorCodes(planCond(makeEnv({ funding: [5n * KAS] }), buyFirst()))).toEqual(['INSUFFICIENT_KAS']);
    expect(errorCodes(planCond(makeEnv({ funding: [3n * KAS] }), sellFirst()))).toEqual(['INSUFFICIENT_KAS']);
  });

  it('refuses an entry expiry beyond 90 days and a stop-entry keeper reserve that does not fit half of the carrier', () => {
    expect(errorCodes(bad(buyFirst({ expiry: { kind: 'gtdDaa', daa: CLOCK.daa + 77_760_001n } })))).toEqual(['EXPIRY_TOO_FAR']);
    const entry = { price: 240_000_000n, stop: 244_000_000n };
    const exit = { takeProfit: 200_000_000n };
    // sell-first: the arming tip is paid from the carrier; 3 KAS does not fit half of a 5 KAS carrier, 2 KAS does
    expect(errorCodes(bad(sellFirst({ entry: { ...entry, keeperTip: 3n * KAS }, exit, carrier: 5n * KAS })))).toEqual(['COND_KEEPER_FUNDING_TOO_LARGE']);
    expect(planCond(env, sellFirst({ entry: { ...entry, keeperTip: 2n * KAS }, exit, carrier: 5n * KAS })).ok).toBe(true);
  });
});

describe('repeat IFD / IFO', () => {
  const rep = (over: Record<string, unknown> = {}): CondIntent => ({
    type: 'repeatIfd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n }, repeat: { count: 2n }, ...over,
  } as CondIntent);
  const repSell = (over: Record<string, unknown> = {}): CondIntent => ({
    type: 'repeatIfd', side: 'sell', amount: 10n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n }, repeat: { count: 2n }, ...over,
  } as CondIntent);
  // the merge costs as tip rates over the smallest take-profit fill: the exit minimum fill 2500 (= the entry's)
  const MT_BUY = ceilDiv(MERGE_COST_BUY_FIRST * SCALE, 2_500n); // 400_000 sompi per token
  const MT_SELL = ceilDiv(MERGE_COST_SELL_FIRST * SCALE, 2_500n); // 600_000 sompi per token

  it('the merge tip rate pays the merge cost even on the smallest take-profit fill', () => {
    expect(MT_BUY).toBe(400_000n);
    expect(MT_SELL).toBe(600_000n);
    expect(mergeTipRate(MERGE_COST_BUY_FIRST, SCALE, 2_500n)).toBe(MT_BUY);
    for (const [cost, scale, minFill] of [[1_000_000n, 1_000n, 7n], [1_500_000n, 100_000_000n, 123_456n], [1n, 1n, 1n]] as const) {
      const rate = mergeTipRate(cost, scale, minFill);
      // floor(minFill x rate / scale) >= cost, and one sompi less does not
      expect((minFill * rate) / scale).toBeGreaterThanOrEqual(cost);
      expect((minFill * (rate - 1n)) / scale).toBeLessThan(cost);
    }
  });

  it('buy-first: rptAmount = 1 + K * N, merge cost in the exit tip, one more exit carrier in the escrow', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, rep());
    expect(ifdBid(plan.states[0]!).rptAmount).toBe('20001'); // 1 + 2 * 10000
    // the exit's tip pays the merge; the committed exit is plain
    const x = askState(plan.states[1]!);
    expect(x).toMatchObject({ tip: MT_BUY.toString(), parent: ZERO, rptPrice: '0', rptUntil: '0', amountLeft: '10000' });
    expect(committedExit(env, plan.states[0]!)).toEqual(plan.states[1]);
    // budget + ceil(10000 / 2500) x (delivery + exit) + one more exit carrier: kob-wasm ifdBidEscrow
    expect(recovered[0]!.value).toBe((10n * 240_000_000n + 4n * 2n * C + C).toString());
    expect(BigInt(recovered[0]!.value)).toBe(env.kob.ifdBidEscrow(plan.states[0]!));
    expect(plan.disclosure!.carriers).toEqual([
      { kind: 'escrow', amount: 2_400_000_000n, count: 1 },
      { kind: 'deliveryCarrier', amount: C, count: 4 },
      { kind: 'exitCarrier', amount: C, count: 5 },
    ]);
    expect(plan.cond!.repeat).toEqual({ count: 2n, rptAmount: 20_001n, cycleAmount: 10_000n, mergeTip: MT_BUY, profitPerToken: 300_000_000n - MT_BUY - 240_000_000n, untilDaa: GTC });
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['repeat', 'repeatCounted', 'repeatReBuys', 'repeatStopLossEnds', 'mergeTip', 'cancelPosition']));
    expect(plan.disclosure!.notes).not.toContain('repeatUnlimited');
    // the placement record verifies through recoverOrders (planOk) and carries the repeat amount
    expect(ifdBid(recovered[0]!.order).rptAmount).toBe('20001');
  });

  it('sell-first: the exit tip carries the (larger) sell-first merge cost; entry value has no extra carrier', () => {
    const env = makeEnv();
    const { plan, recovered } = planOk(env, repSell());
    expect(ifdAsk(plan.states[0]!).rptAmount).toBe('20001');
    expect(bidState(plan.states[1]!).tip).toBe(MT_SELL.toString());
    expect(committedExit(env, plan.states[0]!)).toEqual(plan.states[1]);
    // entry carrier (which the repeating entry keeps when it sells out) + 4 exit carriers; prefund 0 here (3 sompi of rounding)
    expect(recovered[0]!.value).toBe((C + 4n * C + 3n).toString());
    expect(recovered[0]!.custody!.state.amount).toBe('10000');
    expect(plan.cond!.repeat).toMatchObject({ rptAmount: 20_001n, mergeTip: MT_SELL, profitPerToken: 260_000_000n - (240_000_000n + MT_SELL) });
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['repeat', 'repeatReSells', 'prefund']));
  });

  it('the exit tip adds to the merge cost; the profit is counted after tips', () => {
    const { plan } = planOk(makeEnv(), rep({ exit: { takeProfit: 300_000_000n, tip: 25_000n }, tip: 5_000n }));
    expect(askState(plan.states[1]!).tip).toBe((MT_BUY + 25_000n).toString());
    expect(plan.cond!.repeat!.profitPerToken).toBe((300_000_000n - MT_BUY - 25_000n) - (240_000_000n + 5_000n));
  });

  it('unlimited (the default): K large enough that only the 90-day bound ends it', () => {
    const env = makeEnv();
    const { plan } = planOk(env, rep({ repeat: undefined }));
    expect(ifdBid(plan.states[0]!).rptAmount).toBe((1n + 10_000_000n * 10_000n).toString());
    expect(BigInt(ifdBid(plan.states[0]!).rptAmount)).toBeLessThan(1n << 62n);
    expect(plan.cond!.repeat).toMatchObject({ count: null, rptAmount: 100_000_000_001n });
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['repeatUnlimited']));
    // the entry expires within 90 days of placement (default GTC = placement + 90 days)
    expect(BigInt(ifdBid(plan.states[0]!).expiryDaa)).toBeLessThanOrEqual(CLOCK.daa + 77_760_000n);
    expect(plan.cond!.repeat!.untilDaa).toBe(CLOCK.daa + 77_760_000n);
  });

  it('rptAmount arithmetic for several sizes and counts, both sides', () => {
    for (const [amount, k] of [[1_000n, 1n], [3_000n, 5n], [7_000n, 2n], [10_000n, 100n], [1_234n, 3n]] as const) {
      const bf = planOk(makeEnv(), rep({ amount, repeat: { count: k } }));
      expect(ifdBid(bf.plan.states[0]!).rptAmount).toBe((1n + k * amount).toString());
      const sf = planOk(makeEnv(), repSell({ amount, repeat: { count: k } }));
      expect(ifdAsk(sf.plan.states[0]!).rptAmount).toBe((1n + k * amount).toString());
    }
  });

  it('a dated repeat: the expiry bounds the repeat', () => {
    const at = CLOCK.daa + 1_000_000n;
    const { plan } = planOk(makeEnv(), rep({ expiry: { kind: 'gtdDaa', daa: at } }));
    expect(ifdBid(plan.states[0]!).expiryDaa).toBe(at.toString());
    expect(plan.cond!.repeat!.untilDaa).toBe(at);
    expect(errorCodes(planCond(makeEnv(), rep({ expiry: { kind: 'gtdDaa', daa: CLOCK.daa + 77_760_001n } })))).toEqual(['EXPIRY_TOO_FAR']);
    expect(errorCodes(planCond(makeEnv(), rep({ expiry: { kind: 'gtdUnix', atUnixSeconds: CLOCK.unixSeconds + 8_000_000n } })))).toEqual(['EXPIRY_TOO_FAR']);
  });

  it('repeat IFO: a stop-loss exit next to the take-profit (a stop-loss ends the repeat for that amount)', () => {
    const env = makeEnv();
    const bf = planOk(env, {
      type: 'repeatIfo', side: 'buy', amount: 5n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n, stop: 200_000_000n }, repeat: { count: 3n },
    });
    expect(ifdBid(bf.plan.states[0]!).rptAmount).toBe('15001');
    // entry minimum fill ceil(5000 / 4) = 1250 is also the exit's: merge tip ceil(0.01 KAS x 1000 / 1250)
    const mt = ceilDiv(MERGE_COST_BUY_FIRST * SCALE, 1_250n);
    expect(askState(bf.plan.states[1]!)).toMatchObject({ tpPrice: '300000000', stopPrice: '200000000', tip: mt.toString(), minFill: '1250' });
    expect(committedExit(env, bf.plan.states[0]!)).toEqual(bf.plan.states[1]);
    const sf = planOk(env, {
      type: 'repeatIfo', side: 'sell', amount: 5n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n, stop: 280_000_000n }, repeat: { count: 3n },
    });
    // prefund covers the stop leg's worst buy-back including the merge tip
    const mts = ceilDiv(MERGE_COST_SELL_FIRST * SCALE, 1_250n);
    expect(ifdAsk(sf.plan.states[0]!).prefund).toBe((288_400_000n + mts - 260_000_000n).toString());
    expect(bidState(sf.plan.states[1]!)).toMatchObject({ stopPrice: '280000000', tip: mts.toString() });
  });

  it('requires the take-profit to beat the entry all-in (merge tip included), on both sides; break-even is not a profit', () => {
    const env = makeEnv();
    const profit = (p: ReturnType<typeof planCond>): unknown => p.issues.find((i) => i.code === 'COND_TP_NOT_PROFITABLE')?.params;
    // buy-first: entry 240 KAS all-in; the exit receives take-profit minus the 0.004 KAS merge tip
    expect(errorCodes(planCond(env, rep({ exit: { takeProfit: 240_400_000n } })))).toEqual(['COND_TP_NOT_PROFITABLE']);
    expect(profit(planCond(env, rep({ exit: { takeProfit: 240_200_000n } })))).toMatchObject({ profitPerToken: -200_000n });
    expect(planCond(env, rep({ exit: { takeProfit: 240_400_100n } })).ok).toBe(true);
    // a tip on the entry raises the bar
    const tipped = planCond(env, rep({ exit: { takeProfit: 241_300_000n }, tip: 950_000n }));
    expect(errorCodes(tipped)).toEqual(['COND_TP_NOT_PROFITABLE']);
    expect(profit(tipped)).toMatchObject({ profitPerToken: 241_300_000n - MT_BUY - (240_000_000n + 950_000n) });
    // sell-first: entry brings 260 KAS; the exit pays take-profit plus the 0.006 KAS merge tip
    expect(errorCodes(planCond(env, repSell({ exit: { takeProfit: 259_400_000n } })))).toEqual(['COND_TP_NOT_PROFITABLE']);
    expect(errorCodes(planCond(env, repSell({ exit: { takeProfit: 260_000_000n } })))).toEqual(['COND_TP_NOT_PROFITABLE']);
    expect(planCond(env, repSell({ exit: { takeProfit: 259_399_900n } })).ok).toBe(true);
    // a plain IFD only warns about the same numbers
    const plain = planOk(env, buyFirst({ exit: { takeProfit: 240_000_000n } }));
    expect(codes(plain.plan)).toEqual(['COND_TP_NOT_PROFITABLE']);
  });

  it('refuses a repeat count below one', () => {
    for (const count of [0n, -3n]) expect(errorCodes(planCond(makeEnv(), rep({ repeat: { count } }))), String(count)).toEqual(['COND_REPEAT_COUNT_INVALID']);
  });

  it('stop entries re-arm unarmed: the arming keeper is funded per expected cycle (at most ten)', () => {
    const env = makeEnv();
    const one = planOk(env, rep({ entry: { price: 260_000_000n, stop: 255_000_000n }, repeat: { count: 2n } }));
    // K = 2 -> 3 cycles -> 3 arming tips, taken from the escrow on top of the budget
    expect(one.plan.disclosure!.carriers).toContainEqual({ kind: 'keeperReserve', amount: KT, count: 3 });
    expect(one.plan.cond!.keeper).toMatchObject({ expectedUpdates: 3, reserve: 3n * KT, fundedFrom: 'escrow' });
    const unl = planOk(env, rep({ entry: { price: 260_000_000n, stop: 255_000_000n }, repeat: undefined }));
    expect(unl.plan.disclosure!.carriers).toContainEqual({ kind: 'keeperReserve', amount: KT, count: 10 });
  });

  it('value equals the entry escrow for random amounts, minimum fills and repeat counts (both sides), and always builds', () => {
    let seed = 987654321n;
    const next = (m: bigint): bigint => {
      seed = (seed * 6364136223846793005n + 1442695040888963407n) % (1n << 63n);
      return (seed >> 20n) % m;
    };
    const env = makeEnv({ funding: [100_000n * KAS], tokenAmounts: [500n * TOK] });
    for (let i = 0; i < 24; i++) {
      const amount = 1n + next(30_000n);
      const minFill = 1n + next(amount);
      const k = 1n + next(6n);
      const repeat = next(2n) === 1n;
      const fills = ceilDiv(amount, minFill);
      const price = 240_000_000n + 100n * next(1000n);
      const bf = planOk(env, {
        type: repeat ? 'repeatIfd' : 'ifd', side: 'buy', amount, minFill, entry: { price }, exit: { takeProfit: 400_000_000n }, ...(repeat ? { repeat: { count: k } } : {}),
      } as CondIntent);
      // floor(amount x price / scale) + a delivery and an exit carrier per possible fill (+ the repeating entry's own exit carrier)
      expect(BigInt(bf.recovered[0]!.value), `buy amount=${amount} minFill=${minFill} repeat=${repeat}`).toBe((amount * price) / SCALE + fills * 2n * C + (repeat ? C : 0n));
      if (repeat) expect(ifdBid(bf.plan.states[0]!).rptAmount).toBe((1n + k * amount).toString());
      const sf = planOk(env, {
        type: repeat ? 'repeatIfd' : 'ifd', side: 'sell', amount, minFill, entry: { price: 500_000_000n + price }, exit: { takeProfit: 240_000_000n }, ...(repeat ? { repeat: { count: k } } : {}),
      } as CondIntent);
      // the carrier, a sompi of prefund rounding per extra fill (prefund 0 here), an exit carrier per possible fill
      expect(BigInt(sf.recovered[0]!.value), `sell amount=${amount} minFill=${minFill}`).toBe(C + (fills - 1n) + fills * C);
      expect(sf.recovered[0]!.custody!.state.amount).toBe(amount.toString());
    }
  });
});

describe('IFD timed entry and exit lifetime', () => {
  const at = CLOCK.daa + 36_000n;

  it('activeFrom times the ENTRY only: the entry state carries it, the committed exit stays 0 (buy-first and sell-first)', () => {
    const env = makeEnv();
    const bf = planOk(env, buyFirst({ activeFrom: { daa: at } }));
    expect(ifdBid(bf.plan.states[0]!).activeFrom).toBe(at.toString());
    expect(askState(bf.plan.states[1]!).activeFrom).toBe('0');
    expect(committedExit(env, bf.plan.states[0]!)).toEqual(bf.plan.states[1]);
    expect(bf.plan.disclosure!.activatesAt).toMatchObject({ daa: at });
    const sf = planOk(env, sellFirst({ activeFrom: { daa: at } }));
    expect(ifdAsk(sf.plan.states[0]!).activeFrom).toBe(at.toString());
    expect(bidState(sf.plan.states[1]!).activeFrom).toBe('0');
    const rp = planOk(env, { type: 'repeatIfd', side: 'buy', amount: 4n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n }, activeFrom: { daa: at } });
    expect(ifdBid(rp.plan.states[0]!).activeFrom).toBe(at.toString());
  });

  it('past activation is an info, too-far and after-expiry are errors', () => {
    const env = makeEnv();
    const past = planCond(env, buyFirst({ activeFrom: { daa: CLOCK.daa - 1n } }));
    expect(past.ok).toBe(true);
    expect(past.issues.find((i) => i.code === 'ACTIVE_IN_PAST')?.severity).toBe('info');
    expect(ifdBid(past.states[0]!).activeFrom).toBe('0');
    expect(errorCodes(planCond(env, buyFirst({ activeFrom: { daa: GTC + 1n } })))).toContain('ACTIVE_TOO_FAR');
    expect(errorCodes(planCond(env, buyFirst({ activeFrom: { daa: at }, expiry: { kind: 'gtdDaa', daa: at - 1n } })))).toContain('ACTIVE_AFTER_EXPIRY');
  });

  it('a trailing single-stop IFD exit (no take-profit) plans with the exit lifetime of a date', () => {
    const exp = CLOCK.daa + 5_000_000n;
    const { plan } = planOk(makeEnv(), buyFirst({ exit: { stop: 200_000_000n, trail: { step: 1_000_000n, gap: 2_000_000n }, expiry: { kind: 'gtdDaa', daa: exp } } }));
    expect(askState(plan.states[1]!)).toMatchObject({ trailStep: '1000000', trailGap: '2000000', expiryDaa: exp.toString() });
    expect(plan.cond!.exit).toMatchObject({ expiry: 'gtd' });
    expect(plan.disclosure!.notes).toContain('exitGtd');
  });
});
