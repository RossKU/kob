// Pair planner tests of the if-done kinds (KobIfdPair with KobCondPair exits): IFD / IFO / bracket, stop entries, repeat IFD / IFO, buy-first
// and sell-first, every KCC-20 / KRON mix. Same oracle as pair-simple.test.ts; the committed exit, the B escrow / prefund, the exit carrier and
// the entry's KAS are checked against an independent BigInt model and against kob-wasm (`ifdPairExit`, `ifdPairExitFor`, `ifdPairAmounts`).
import { describe, expect, it } from 'vitest';
import type { CondPairState, IfdPairState } from '../types';
import type { TokenMarket } from '../plan-types';
import { I64_MAX } from '../units';
import { KAS, ceilDivM, ceilQ, floorQ, kronA, kronB, makePairEnv, tokenA, tokenB } from './pair-fixtures';
import { K, codes, errorCodes, pairValid, plan } from './pair-testkit';
import { PAIR_MERGE_COST_BUY_FIRST, PAIR_MERGE_COST_SELL_FIRST } from './pair-ifd';

const DC = 10n * KAS;
const entryOf = (p: { states: { kind: string; state: unknown }[] }): IfdPairState => {
  expect(p.states[0].kind).toBe('KobIfdPair');
  return p.states[0].state as IfdPairState;
};
const exitOf = (p: { states: { kind: string; state: unknown }[] }): CondPairState => {
  expect(p.states[1].kind).toBe('KobCondPair');
  return p.states[1].state as CondPairState;
};
const defMin = (amount: bigint, scaleA: bigint): bigint => {
  const want = ceilDivM(10n * KAS * scaleA, 3n * KAS);
  return want > amount ? amount : want;
};
const minB = (x: bigint, y: bigint): bigint => (x < y ? x : y);
const maxB = (x: bigint, y: bigint): bigint => (x > y ? x : y);
const band = (stop: bigint, bps = 300n): bigint => (stop * bps) / 10_000n;

const combos: [string, () => TokenMarket, () => TokenMarket][] = [
  ['KCC-20 / KCC-20', tokenA, tokenB],
  ['KRON / KCC-20', kronA, tokenB],
  ['KCC-20 / KRON', tokenA, kronB],
  ['KRON / KRON', kronA, kronB],
];

describe.each(combos)('KobIfdPair on %s', (_name, mkA, mkB) => {
  const a = mkA();
  const b = mkB();
  const env = () => makePairEnv({ a, b });
  const amount = 8_003n;
  const minFill = ceilDivM(amount, 4n);
  const fills = ceilDivM(amount, minFill);
  const exitMin = minB(defMin(amount, a.scale), minFill);
  const exitFills = minB(2n, ceilDivM(amount, exitMin));
  const keeperTip = BigInt(K.pairTips(a.program, b.program).keeperTip);

  it('IFD buy-first: the B escrow is the spend of the whole amount at the limit (rounded down); every fill creates an ASK exit holding its A', () => {
    const tip = 50_001n;
    const p = pairValid(env(), { type: 'ifd', side: 'buy', amount, tip, entry: { price: 1_401n }, exit: { takeProfit: 1_601n } });
    const e = entryOf(p);
    expect(e).toMatchObject({
      side: '2', aCovId: a.covenantId, bCovId: b.covenantId, aScale: String(a.scale), bScale: String(b.scale), price: '1401', prefund: '0', tip: String(tip),
      minFill: String(minFill), entryStop: '0', bandDaa: '0', minTouch: '0', keeperTip: '0', armed: '0', rptAmount: '0', amountLeft: String(amount), deliveryCarrier: String(DC),
    });
    expect(BigInt(e.custody)).toBe(floorQ(amount, 1_401n, a.scale));
    expect(e.exitState.length).toBe(2 * 432);
    // the committed exit (kob-wasm ifdPairExit) is a KobCondPair ASK of the same pair with the take-profit
    const committed = K.ifdPairExit(p.states[0]).state as CondPairState;
    expect(committed).toMatchObject({ side: '1', sCovId: a.covenantId, tCovId: b.covenantId, tpPrice: '1601', stopPrice: '0', minFill: String(exitMin), amountLeft: '0', custody: '0', tip: '0' });
    expect(e.exitState).toBe(K.ifdPairCommitExit(K.ifdPairExit(p.states[0])));
    // the exit of a fill of the whole amount: custody = the A bought
    expect(exitOf(p)).toMatchObject({ amountLeft: String(amount), custody: String(amount), side: '1' });
    expect(p.states[1]).toEqual(K.ifdPairExitFor(p.states[0], amount, amount));
    const exitCarrier = DC * exitFills;
    expect(BigInt(e.exitCarrier)).toBe(exitCarrier);
    const x = p.pair!;
    expect(x).toMatchObject({ kind: 'KobIfdPair', side: 'buy', escrowA: 0n, escrowB: BigInt(e.custody), deliveries: fills, exitCarrier, receiveMinB: null });
    expect(x.payMaxB).toBe(floorQ(amount, 1_401n, a.scale));
    expect(x.minFillB).toBe(floorQ(minFill, 1_401n, a.scale));
    expect(x.tipKasTotal).toBe(floorQ(amount, tip, a.scale));
    expect(x.orderValue).toBe(fills * (DC + exitCarrier) + floorQ(amount, tip, a.scale));
    expect(x.orderValue).toBe(K.pairKasValue(p.states[0], 0n));
    expect(x.exit).toEqual({ kind: 'takeProfit', takeProfit: 1_601n, stop: null, custodyPerFill: 'baseAmount' });
    expect(x.trigger).toBeNull();
    expect(p.cond?.entry).toMatchObject({ price: 1_401n, stop: null, minFill, maxFills: fills });
    expect(p.cond?.exit).toMatchObject({ side: 'sell', takeProfitAllIn: 1_601n, exitCarrier, prefund: null, minFill: exitMin });
    expect(p.disclosure!.notes).toEqual(expect.arrayContaining(['ifd', 'position', 'buyFirst', 'minFill', 'exitGtc']));
  });

  it('IFO sell-first: A custody and a B prefund covering the exit\'s worst buy-back; every fill creates a BID exit holding proceeds plus prefund', () => {
    const p = pairValid(env(), { type: 'ifo', side: 'sell', amount, entry: { price: 1_599n }, exit: { takeProfit: 1_399n, stop: 1_700n } });
    const e = entryOf(p);
    const worst = maxB(1_399n, 1_700n + band(1_700n));
    const prefund = worst - 1_599n;
    expect(e).toMatchObject({ side: '1', price: '1599', prefund: String(prefund), amountLeft: String(amount) });
    expect(BigInt(e.custody)).toBe(ceilQ(amount, prefund, a.scale) + (fills - 1n));
    expect(BigInt(e.custody)).toBe(K.ifdPairBCustodyNeeded(p.states[0]));
    const committed = K.ifdPairExit(p.states[0]).state as CondPairState;
    expect(committed).toMatchObject({ side: '2', sCovId: b.covenantId, tCovId: a.covenantId, tpPrice: '1399', stopPrice: '1700', slipBps: '300', keeperTip: String(keeperTip) });
    // the exit's trigger threshold scales with the entry's amount (at most what one exit holds), not its own minimum fill
    expect(BigInt(committed.minTouch)).toBe(K.defaultMinTouch(BigInt(committed.minFill), amount));
    const proceeds = ceilQ(amount, 1_599n, a.scale);
    const pre = ceilQ(amount, prefund, a.scale);
    expect(exitOf(p)).toMatchObject({ amountLeft: String(amount), custody: String(proceeds + pre) });
    // the exit carrier funds its fills and its keeper (one arm)
    const exitCarrier = DC * exitFills + keeperTip;
    expect(BigInt(e.exitCarrier)).toBe(exitCarrier);
    const x = p.pair!;
    expect(x).toMatchObject({ side: 'sell', escrowA: amount, escrowB: BigInt(e.custody), payMaxB: null });
    expect(x.receiveMinB).toBe(proceeds);
    expect(x.exit).toEqual({ kind: 'oco', takeProfit: 1_399n, stop: 1_700n, custodyPerFill: 'proceedsPlusPrefund' });
    expect(p.cond?.exit?.prefund).toBe(prefund);
    expect(p.disclosure!.notes).toEqual(expect.arrayContaining(['ifo', 'sellFirst', 'prefund', 'exitStop']));
    // a fill of one minimum fill: what kob-wasm says the entry receives and moves
    const one = K.ifdPairAmounts(p.states[0], minFill, 1_599n);
    expect(one.proceeds).toBe(ceilQ(minFill, 1_599n, a.scale));
    expect(one.pre).toBe(ceilQ(minFill, prefund, a.scale));
    expect(x.minFillB).toBe(one.proceeds);
  });

  it('stop entry (buy-first): the trigger, the auction band, the keeper reserve of one arm on the entry UTXO', () => {
    const p = pairValid(env(), { type: 'ifd', side: 'buy', amount, entry: { price: 1_600n, stop: 1_550n }, exit: { stop: 1_450n } });
    const e = entryOf(p);
    expect(e).toMatchObject({ entryStop: '1550', bandDaa: '300', minTouch: String(minFill), minRestDaa: '50', keeperTip: String(keeperTip) });
    expect(p.pair!.trigger).toMatchObject({ kind: 'KobIfdPair', stop: '1550', direction: 'risesTo' });
    expect(p.disclosure!.carriers.find((c) => c.kind === 'keeperReserve')).toMatchObject({ amount: keeperTip, count: 1 });
    expect(p.pair!.orderValue).toBe(K.pairKasValue(p.states[0], 0n)! + keeperTip);
    expect(p.pair!.expectedB).toBe(floorQ(amount, 1_550n, a.scale));
    expect(p.pair!.exit?.kind).toBe('stop');
    expect(K.pairArms(p.states[0], { mode: 'pair', price: '1550' })).toBe(true);
    expect(K.pairArms(p.states[0], { mode: 'pair', price: '1549' })).toBe(false);
  });

  it('repeat IFD (buy-first, counted) and repeat IFO (sell-first, unlimited): rptAmount, the merge tip, one more exit carrier', () => {
    const r = pairValid(env(), { type: 'repeatIfd', side: 'buy', amount, entry: { price: 1_400n }, exit: { takeProfit: 1_500n, tip: 1_000n }, repeat: { count: 5n } });
    const e = entryOf(r);
    expect(e.rptAmount).toBe(String(1n + 5n * amount));
    const mergeTip = ceilDivM(PAIR_MERGE_COST_BUY_FIRST * a.scale, exitMin);
    const exitTip = 1_000n + mergeTip;
    expect((K.ifdPairExit(r.states[0]).state as CondPairState).tip).toBe(String(exitTip));
    const exitCarrier = DC * exitFills + floorQ(amount, exitTip, a.scale);
    expect(BigInt(e.exitCarrier)).toBe(exitCarrier);
    expect(r.pair!.orderValue).toBe(fills * (DC + exitCarrier) + exitCarrier);
    expect(r.pair!.repeat).toEqual({ count: 5n, levels: 1n, rptAmount: 1n + 5n * amount, unlimited: false });
    expect(r.cond?.repeat).toMatchObject({ count: 5n, cycleAmount: amount, mergeTip, profitPerToken: 100n });
    expect(r.disclosure!.notes).toEqual(expect.arrayContaining(['repeat', 'repeatCounted', 'repeatReBuys', 'mergeTip', 'cancelPosition']));

    const u = pairValid(env(), { type: 'repeatIfo', side: 'sell', amount, entry: { price: 1_600n }, exit: { takeProfit: 1_500n, stop: 1_650n } });
    const k = 10_000_000n < (I64_MAX - 1n) / amount ? 10_000_000n : (I64_MAX - 1n) / amount;
    expect(entryOf(u).rptAmount).toBe(String(1n + k * amount));
    expect(u.pair!.repeat).toEqual({ count: k, levels: 1n, rptAmount: 1n + k * amount, unlimited: true });
    expect(u.cond?.repeat?.mergeTip).toBe(ceilDivM(PAIR_MERGE_COST_SELL_FIRST * a.scale, exitMin));
    expect(u.disclosure!.notes).toEqual(expect.arrayContaining(['repeatUnlimited', 'repeatReSells', 'repeatStopLossEnds']));
  });
});

describe('KobIfdPair economics and refusals', () => {
  it('the take-profit must beat the entry: a plain IFD warns, a repeat refuses', () => {
    const w = pairValid(makePairEnv(), { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_390n } });
    expect(w.issues.find((i) => i.code === 'PAIR_TP_NOT_PROFITABLE')).toMatchObject({ severity: 'warning', params: { profitPerToken: -10n, ticker: 'BBB' } });
    const r = plan(makePairEnv(), { type: 'repeatIfd', side: 'sell', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_400n } });
    expect(errorCodes(r)).toEqual(['PAIR_TP_NOT_PROFITABLE']);
  });

  it('prefund: a larger one is kept, a short one or a negative one is refused', () => {
    const big = pairValid(makePairEnv(), { type: 'ifd', side: 'sell', amount: 4_000n, prefund: 500n, entry: { price: 1_500n }, exit: { takeProfit: 1_300n } });
    expect(entryOf(big).prefund).toBe('500');
    expect(BigInt(entryOf(big).custody)).toBe(ceilQ(4_000n, 500n, 1_000n) + 3n);
    const short = plan(makePairEnv(), { type: 'ifd', side: 'sell', amount: 4_000n, prefund: 10n, entry: { price: 1_500n }, exit: { stop: 1_600n } });
    expect(errorCodes(short)).toEqual(['PAIR_PREFUND_SHORT']);
    expect(short.issues[0].params).toMatchObject({ needed: 1_600n + band(1_600n) - 1_500n, given: 10n, ticker: 'BBB' });
    expect(errorCodes(plan(makePairEnv(), { type: 'ifd', side: 'sell', amount: 4_000n, prefund: -1n, entry: { price: 1_500n }, exit: { takeProfit: 1_300n } }))).toEqual(['COND_PREFUND_INVALID']);
    // without a prefund need (a take-profit below the entry) the B custody is only the per-fill slack
    const none = pairValid(makePairEnv(), { type: 'ifd', side: 'sell', amount: 4_000n, entry: { price: 1_500n }, exit: { takeProfit: 1_300n } });
    expect(entryOf(none)).toMatchObject({ prefund: '0', custody: '3' });
  });

  it('exit shapes and entry stops are refused as on the KAS ticket', () => {
    const env = makePairEnv();
    expect(errorCodes(plan(env, { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n, stop: 1_300n } }))).toEqual(['COND_EXIT_NEEDS_ONE_LEG']);
    expect(errorCodes(plan(env, { type: 'ifo', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n } }))).toEqual(['COND_EXIT_NEEDS_BOTH']);
    expect(errorCodes(plan(env, { type: 'repeatIfd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { stop: 1_300n } }))).toEqual(['COND_EXIT_NEEDS_TP_ONLY']);
    expect(errorCodes(plan(env, { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_400n, stop: 1_450n }, exit: { takeProfit: 1_500n } }))).toEqual(['COND_ENTRY_STOP_BEYOND_LIMIT']);
    expect(errorCodes(plan(env, { type: 'ifd', side: 'buy', amount: 4_000n, minFill: 5_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n } }))).toEqual(['COND_MIN_FILL_INVALID']);
    expect(errorCodes(plan(env, { type: 'repeatIfd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n }, repeat: { count: 0n } }))).toEqual(['COND_REPEAT_COUNT_INVALID']);
    expect(errorCodes(plan(env, { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n, expiry: { kind: 'day' } } }))).toEqual(['COND_EXIT_EXPIRY_DAY']);
  });

  it('not enough B (buy-first escrow, sell-first prefund) or A (sell-first custody)', () => {
    const b1 = plan(makePairEnv({ bAmounts: [100n] }), { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n } });
    expect(errorCodes(b1)).toEqual(['PAIR_INSUFFICIENT_TOKENS']);
    expect(b1.issues[0].params).toMatchObject({ ticker: 'BBB', needed: floorQ(4_000n, 1_400n, 1_000n) });
    const b2 = plan(makePairEnv({ bAmounts: [100n] }), { type: 'ifd', side: 'sell', amount: 4_000n, entry: { price: 1_500n }, exit: { stop: 1_600n } });
    expect(errorCodes(b2)).toEqual(['PAIR_INSUFFICIENT_TOKENS']);
    expect(b2.issues[0].params).toMatchObject({ ticker: 'BBB' });
    const a1 = plan(makePairEnv({ aAmounts: [1_000n] }), { type: 'ifd', side: 'sell', amount: 4_000n, entry: { price: 1_500n }, exit: { takeProfit: 1_300n } });
    expect(errorCodes(a1)).toEqual(['PAIR_INSUFFICIENT_TOKENS']);
    expect(a1.issues[0].params).toMatchObject({ ticker: 'AAA' });
  });

  it('KRON caps: a buy-first KRON escrow, a sell-first exit custody of KRON B', () => {
    const kk = { a: kronA(), b: kronB(), aAmounts: [2_000_000_000n], bAmounts: [4_000_000_000n] };
    expect(errorCodes(plan(makePairEnv(kk), { type: 'ifd', side: 'buy', amount: 1_000_000n, entry: { price: 2_000_000n }, exit: { takeProfit: 2_100_000n } }))).toEqual(['PAIR_KRON_CUSTODY_TOO_LARGE']);
    const sf = plan(makePairEnv(kk), { type: 'ifd', side: 'sell', amount: 4_000_000n, minFill: 4_000_000n, entry: { price: 300_000n }, exit: { takeProfit: 200_000n } });
    expect(errorCodes(sf)).toEqual(['PAIR_KRON_DELIVERY_TOO_LARGE']);
  });

  it('self-trade: the entry is checked now, the exit only warns', () => {
    const env = makePairEnv({ ownOrders: [{ covenantId: 'ce'.repeat(32), side: 'sell', price: 1_390n, tip: 0n, amountLeft: 10n, active: true }] });
    expect(errorCodes(plan(env, { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n } }))).toEqual(['SELF_TRADE']);
    const env2 = makePairEnv({ ownOrders: [{ covenantId: 'cf'.repeat(32), side: 'buy', price: 1_600n, tip: 0n, amountLeft: 10n, active: true }] });
    const p = pairValid(env2, { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_400n }, exit: { takeProfit: 1_500n } });
    expect(p.issues.find((i) => i.code === 'SELF_TRADE')?.severity).toBe('warning');
  });

  it('an entry at or through the pair book warns that it fills at once', () => {
    expect(codes(pairValid(makePairEnv(), { type: 'ifd', side: 'buy', amount: 4_000n, entry: { price: 1_500n }, exit: { takeProfit: 1_600n } }))).toContain('COND_ENTRY_CROSSES');
  });
});
