// Pair planner tests of the plain kinds (KobPair): every intent type of the KAS ticket on a pair A/B, both sides, every KCC-20 / KRON mix.
// Oracle (pair-testkit.ts): consensus validity, the recovered placement record, the custodies as outputs of their own tokens, the KAS and token
// accounting; every disclosed amount is checked against an independent BigInt model of the covenants' rounding (pair-fixtures.ts).
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import type { PairState } from '../types';
import type { OwnOrderRef, TokenMarket } from '../plan-types';
import { MAX_IDLE_DAA } from '../daa';
import { goldenRequest } from '../../testing/golden';
import { market3x3 } from '../../testing/fixtures';
import { CLOCK, KAS, MAKER_PK, ceilDivM, ceilQ, floorQ, kronA, kronB, level, makePairEnv, tokenA, tokenB } from './pair-fixtures';
import { K, codes, errorCodes, pairValid, plan } from './pair-testkit';

const pairState = (p: { states: { kind: string; state: unknown }[] }): PairState => {
  expect(p.states[0].kind).toBe('KobPair');
  return p.states[0].state as PairState;
};
const DC = 10n * KAS;
/** model of kob-protocol defaults::default_min_fill_pair at the fixtures' KAS reference of A (3 KAS per whole A) */
const defMin = (amount: bigint, scaleA: bigint, kasA: bigint | null = 3n * KAS): bigint => {
  if (kasA === null) return ceilDivM(amount, 4n);
  const want = ceilDivM(10n * KAS * scaleA, kasA);
  return want < 1n ? 1n : want > amount ? amount : want;
};
const minB = (x: bigint, y: bigint): bigint => (x < y ? x : y);
const famCode = (m: TokenMarket): string => (m.family === 'kron' ? '2' : '1');

const combos: [string, () => TokenMarket, () => TokenMarket][] = [
  ['KCC-20 / KCC-20', tokenA, tokenB],
  ['KRON / KCC-20', kronA, tokenB],
  ['KCC-20 / KRON', tokenA, kronB],
  ['KRON / KRON', kronA, kronB],
];

describe.each(combos)('KobPair limit orders on %s', (_name, mkA, mkB) => {
  const a = mkA();
  const b = mkB();
  const env = () => makePairEnv({ a, b });
  const amount = 10_007n; // 10.007 A: every quote below rounds
  const price = 1_499n; // B base units per whole A
  const tip = 123_457n; // sompi per whole A

  it('GTC sell: side ASK, S = A held exactly, T = B, default minFill, deliveries, the B received rounded UP, the KAS tip rounded DOWN', () => {
    const p = pairValid(env(), { type: 'limit', side: 'sell', amount, price, tip });
    const s = pairState(p);
    expect(s).toMatchObject({
      side: '1', sCovId: a.covenantId, tCovId: b.covenantId, sFamily: famCode(a), tFamily: famCode(b), sScale: String(a.scale), tScale: String(b.scale),
      price: String(price), tip: String(tip), tif: '0', amountLeft: String(amount), custody: String(amount), slope: '0', interval: '0', maxFill: '0',
      maker: MAKER_PK, deliveryCarrier: String(DC), expiryDaa: String(CLOCK.daa + MAX_IDLE_DAA), activeFrom: '0',
    });
    expect(s.tExt).toBe(b.family === 'kron' ? '00'.repeat(32) : b.extensionCommitment);
    const mf = defMin(amount, a.scale);
    expect(BigInt(s.minFill)).toBe(mf);
    // the pair tips of the two programs (kob-wasm pairTips = tipsFor of the state)
    expect(s.refundTip).toBe(K.tipsFor(p.states[0]).refundTip);
    expect(K.pairTips(a.program, b.program).refundTip).toBe(s.refundTip);
    const x = p.pair!;
    const fills = minB(ceilDivM(amount, mf), 3n);
    expect(x).toMatchObject({ kind: 'KobPair', side: 'sell', amount, escrowA: amount, escrowB: 0n, payMaxB: null, exitCarrier: null, deliveries: fills, deliveryCarrier: DC });
    expect(x.receiveMinB).toBe(ceilQ(amount, price, a.scale));
    expect(x.minFillB).toBe(ceilQ(mf, price, a.scale));
    expect(x.tipKasTotal).toBe(floorQ(amount, tip, a.scale));
    expect(x.orderValue).toBe(fills * DC + floorQ(amount, tip, a.scale));
    expect(x.trigger).toBeNull();
    const d = p.disclosure!;
    expect(d).toMatchObject({ side: 'sell', tokenAmount: amount, limitPrice: price, allInPrice: price, worstPrice: price, tip, expectedPrice: null });
    expect(d.allInTotal).toBe(ceilQ(amount, price, a.scale));
    expect(d.expiry.kind).toBe('gtc');
    expect(d.notes).toEqual(expect.arrayContaining(['gtc', 'carrierReturned']));
  });

  it('GTC buy: side BID, S = B escrow = floor(amount p / scale(A)) + 1 (exact floors are subadditive), the B paid rounded DOWN', () => {
    const p = pairValid(env(), { type: 'limit', side: 'buy', amount, price: 1_401n, tip });
    const s = pairState(p);
    expect(s).toMatchObject({ side: '2', sCovId: b.covenantId, tCovId: a.covenantId, sScale: String(b.scale), tScale: String(a.scale), amountLeft: String(amount) });
    expect(s.tExt).toBe(a.family === 'kron' ? '00'.repeat(32) : a.extensionCommitment);
    const mf = defMin(amount, a.scale);
    const possible = ceilDivM(amount, mf);
    expect(BigInt(s.custody)).toBe(floorQ(amount, 1_401n, a.scale) + 1n);
    expect(BigInt(s.custody)).toBe(K.pairBidEscrow(p.states[0], amount, possible));
    const x = p.pair!;
    expect(x).toMatchObject({ side: 'buy', escrowA: 0n, escrowB: BigInt(s.custody), receiveMinB: null });
    expect(x.payMaxB).toBe(floorQ(amount, 1_401n, a.scale));
    expect(x.minFillB).toBe(floorQ(mf, 1_401n, a.scale));
    expect(p.disclosure!.allInTotal).toBe(floorQ(amount, 1_401n, a.scale));
  });

  it('GTD and day orders, timed activation', () => {
    const gtd = pairValid(env(), { type: 'limit', side: 'sell', amount, price, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds + 86_400n } });
    expect(BigInt(pairState(gtd).expiryDaa)).toBe(CLOCK.daa + 864_000n);
    expect(gtd.disclosure!.expiry.kind).toBe('gtd');
    const day = pairValid(env(), { type: 'limit', side: 'buy', amount, price: 1_400n, lifetime: { kind: 'day' } });
    expect(day.disclosure!.expiry.kind).toBe('day');
    expect(day.request!.deadline).toBe('1790726400');
    const timed = pairValid(env(), { type: 'limit', side: 'sell', amount, price, activeFrom: { daa: CLOCK.daa + 5_000n } });
    expect(pairState(timed).activeFrom).toBe(String(CLOCK.daa + 5_000n));
    expect(timed.disclosure!.activatesAt?.daa).toBe(CLOCK.daa + 5_000n);
  });
});

describe('KobPair: the crossing policy against the pair book (B prices, the KAS tip never crosses)', () => {
  it('a sell through the best bid is an auction from the touch down to the limit (default)', () => {
    const p = pairValid(makePairEnv(), { type: 'limit', side: 'sell', amount: 4_000n, price: 1_400n, tip: 50_000_000n });
    const s = pairState(p);
    expect(s).toMatchObject({ price: '1450', priceEnd: '1400', decayStep: '1', activeFrom: String(CLOCK.daa + 30n) });
    expect(BigInt(s.slope)).toBe(ceilDivM(50n, 200n));
    expect(codes(p)).toContain('PAIR_MARKETABLE_AUCTION');
    expect(p.issues.find((i) => i.code === 'PAIR_MARKETABLE_AUCTION')!.params).toMatchObject({ touch: 1450n, ticker: 'BBB' });
    expect(p.pair!.receiveMinB).toBe(ceilQ(4_000n, 1_400n, 1_000n));
    expect(p.pair!.expectedB).toBe(ceilQ(4_000n, 1_450n, 1_000n));
    expect(p.pair!.notes).toContain('pairAuction');
    expect(p.disclosure!.notes).toEqual(expect.arrayContaining(['auction', 'marketable']));
  });

  it('a buy through the best ask rises from the touch up to the limit; the escrow is funded at the cap', () => {
    const p = pairValid(makePairEnv(), { type: 'limit', side: 'buy', amount: 4_000n, price: 1_560n });
    const s = pairState(p);
    expect(s).toMatchObject({ price: '1500', priceEnd: '1560' });
    expect(BigInt(s.custody)).toBe(floorQ(4_000n, 1_560n, 1_000n) + 1n);
    expect(p.pair!.payMaxB).toBe(floorQ(4_000n, 1_560n, 1_000n));
  });

  it('crossing: limit fills at the limit (warning), reject refuses, a timed limit is never an auction; a KAS tip does not cross', () => {
    const lim = pairValid(makePairEnv(), { type: 'limit', side: 'sell', amount: 4_000n, price: 1_400n, crossing: 'limit' });
    expect(pairState(lim).slope).toBe('0');
    expect(codes(lim)).toContain('PAIR_MARKETABLE_LIMIT_FILLS_AT_LIMIT');
    const rej = plan(makePairEnv(), { type: 'limit', side: 'sell', amount: 4_000n, price: 1_400n, crossing: 'reject' });
    expect(rej.ok).toBe(false);
    expect(errorCodes(rej)).toEqual(['PAIR_MARKETABLE_REJECTED']);
    const timed = pairValid(makePairEnv(), { type: 'limit', side: 'sell', amount: 4_000n, price: 1_400n, activeFrom: { daa: CLOCK.daa + 100n } });
    expect(pairState(timed).slope).toBe('0');
    // a sell at 1451 with a huge KAS tip: the bid is 1450, nothing crosses (a KAS-quoted ask's tip would)
    const tipped = pairValid(makePairEnv(), { type: 'limit', side: 'sell', amount: 4_000n, price: 1_451n, tip: 10n * KAS });
    expect(codes(tipped)).not.toContain('PAIR_MARKETABLE_AUCTION');
  });

  it('price bands warn in B (pair codes), with the quote ticker', () => {
    const p = pairValid(makePairEnv(), { type: 'limit', side: 'buy', amount: 4_000n, price: 700n });
    expect(codes(p)).toContain('PAIR_PRICE_FAR_FROM_MARKET');
    expect(p.issues.find((i) => i.code === 'PAIR_PRICE_FAR_FROM_MARKET')!.params).toMatchObject({ ticker: 'BBB' });
  });
});

describe.each(combos)('KobPair IOC / FOK / market / streaming / close on %s', (_name, mkA, mkB) => {
  const a = mkA();
  const b = mkB();
  const env = () => makePairEnv({ a, b });

  it('IOC and FOK: tif, minFill 1, one delivery, life 300 DAA, a bid escrow of floor + 1', () => {
    for (const [type, tif] of [['ioc', '1'], ['fok', '2']] as const) {
      const sell = pairValid(env(), { type, side: 'sell', amount: 3_000n, price: 1_450n });
      expect(pairState(sell)).toMatchObject({ tif, minFill: '1', expiryDaa: String(CLOCK.daa + 300n) });
      expect(sell.pair!.deliveries).toBe(1n);
      expect(sell.pair!.orderValue).toBe(DC);
      const buy = pairValid(env(), { type, side: 'buy', amount: 3_000n, price: 1_500n });
      expect(BigInt(pairState(buy).custody)).toBe(floorQ(3_000n, 1_500n, a.scale) + 1n);
      expect(buy.disclosure!.expiry.kind).toBe(type);
    }
  });

  it('FOK the pair book cannot fill is refused', () => {
    const p = plan(env(), { type: 'fok', side: 'buy', amount: 1_000_000n, price: 1_500n });
    expect(errorCodes(p)).toContain('FOK_INSUFFICIENT_DEPTH');
  });

  it('market sell: an IOC auction from the best bid down to the 3% bound, minFill 1, the B received at the bound rounded up', () => {
    const p = pairValid(env(), { type: 'market', side: 'sell', amount: 3_001n });
    const s = pairState(p);
    const end = 1_450n - (1_450n * 300n) / 10_000n;
    expect(s).toMatchObject({ tif: '1', minFill: '1', price: '1450', priceEnd: String(end), decayStep: '1', activeFrom: String(CLOCK.daa + 30n), expiryDaa: String(CLOCK.daa + 330n) });
    expect(BigInt(s.slope)).toBe(ceilDivM(1_450n - end, 200n));
    expect(p.pair!.receiveMinB).toBe(ceilQ(3_001n, end, a.scale));
    expect(p.pair!.expectedB).toBe(ceilQ(3_001n, 1_450n, a.scale));
    expect(p.disclosure).toMatchObject({ expectedPrice: 1_450n, worstPrice: end, limitPrice: end });
    expect(codes(p)).toContain('PAIR_MARKET_REFERENCE_SOURCE');
  });

  it('market buy rises from the best ask up to the bound; the escrow covers the bound', () => {
    const p = pairValid(env(), { type: 'market', side: 'buy', amount: 3_001n });
    const end = 1_500n + (1_500n * 300n) / 10_000n;
    expect(pairState(p)).toMatchObject({ price: '1500', priceEnd: String(end) });
    expect(BigInt(pairState(p).custody)).toBe(floorQ(3_001n, end, a.scale) + 1n);
    expect(p.pair!.payMaxB).toBe(floorQ(3_001n, end, a.scale));
  });

  it('market FOK (allOrNothing), streaming from the displayed price, close sells the whole A balance', () => {
    const fok = pairValid(env(), { type: 'market', side: 'sell', amount: 3_000n, allOrNothing: true });
    expect(pairState(fok).tif).toBe('2');
    const st = pairValid(env(), { type: 'streaming', side: 'buy', amount: 3_000n, displayedPrice: 1_480n, toleranceBps: 100n });
    expect(pairState(st)).toMatchObject({ price: '1480', priceEnd: String(1_480n + 14n) });
    const close = pairValid(env(), { type: 'close' });
    expect(pairState(close)).toMatchObject({ side: '1', amountLeft: String(100n * a.scale) });
    expect(close.disclosure!.notes).toContain('close');
  });

  it('market orders need liquidity on the opposite side of the pair book', () => {
    const p = plan(makePairEnv({ a, b, book: { asks: [], bids: [] } }), { type: 'market', side: 'buy', amount: 3_000n });
    expect(errorCodes(p)).toEqual(['NO_LIQUIDITY']);
  });
});

describe.each(combos)('KobPair TWAP / DCA / Dutch on %s', (_name, mkA, mkB) => {
  const a = mkA();
  const b = mkB();
  const env = () => makePairEnv({ a, b });

  it('TWAP sell: maxFill = slice, interval, one delivery carrier per slice; a per-slice auction decays to priceEnd', () => {
    const p = pairValid(env(), { type: 'twap', amount: 10_000n, sliceAmount: 2_500n, interval: { daa: 600n }, price: 1_500n });
    expect(pairState(p)).toMatchObject({ maxFill: '2500', interval: '600', side: '1', slope: '0' });
    expect(BigInt(pairState(p).minFill) <= 2_500n).toBe(true);
    expect(p.pair!.deliveries).toBe(4n);
    expect(p.pair!.orderValue).toBe(4n * DC);
    const au = pairValid(env(), { type: 'twap', amount: 10_000n, sliceAmount: 2_500n, interval: { daa: 600n }, price: 1_500n, priceEnd: 1_480n });
    expect(pairState(au)).toMatchObject({ priceEnd: '1480', slope: '1' });
    expect(au.pair!.receiveMinB).toBe(ceilQ(10_000n, 1_480n, a.scale));
  });

  it('DCA buy: one delivery per slice (or more when asked), the escrow at the price rounded down plus one unit', () => {
    const p = pairValid(env(), { type: 'dca', amount: 9_000n, sliceAmount: 3_000n, interval: { seconds: 3_600n }, price: 1_400n, maxFills: 5n });
    const s = pairState(p);
    expect(s).toMatchObject({ side: '2', maxFill: '3000', interval: '36000' });
    expect(p.pair!.deliveries).toBe(5n);
    expect(BigInt(s.custody)).toBe(floorQ(9_000n, 1_400n, a.scale) + 1n);
    expect(errorCodes(plan(env(), { type: 'dca', amount: 9_000n, sliceAmount: 3_000n, interval: { daa: 600n }, price: 1_400n, maxFills: 2n }))).toContain('MAX_FILLS_INVALID');
  });

  it('Dutch sell decays and a rising bid rises over the duration from activeFrom', () => {
    const d = pairValid(env(), { type: 'dutch', side: 'sell', amount: 5_000n, price: 1_700n, priceEnd: 1_500n, duration: { daa: 1_000n }, stepDaa: 10n });
    expect(pairState(d)).toMatchObject({ price: '1700', priceEnd: '1500', decayStep: '10', slope: String(ceilDivM(200n, 100n)), activeFrom: String(CLOCK.daa + 30n) });
    expect(d.pair!.receiveMinB).toBe(ceilQ(5_000n, 1_500n, a.scale));
    expect(d.pair!.expectedB).toBe(ceilQ(5_000n, 1_700n, a.scale));
    const r = pairValid(env(), { type: 'dutch', side: 'buy', amount: 5_000n, price: 1_300n, priceEnd: 1_420n, duration: { daa: 600n } });
    expect(pairState(r)).toMatchObject({ price: '1300', priceEnd: '1420', side: '2' });
    expect(r.pair!.payMaxB).toBe(floorQ(5_000n, 1_420n, a.scale));
    expect(errorCodes(plan(env(), { type: 'dutch', side: 'sell', amount: 5_000n, price: 1_500n, priceEnd: 1_600n, duration: { daa: 600n } }))).toEqual(['PRICE_END_INVALID']);
  });
});

describe('KobPair refusals and guards', () => {
  it('not enough A / not enough B / not enough KAS name the token', () => {
    const noA = plan(makePairEnv({ aAmounts: [1_000n] }), { type: 'limit', side: 'sell', amount: 5_000n, price: 1_500n });
    expect(errorCodes(noA)).toEqual(['PAIR_INSUFFICIENT_TOKENS']);
    expect(noA.issues[0].params).toMatchObject({ ticker: 'AAA', needed: 5_000n, have: 1_000n, shortfall: 4_000n });
    const noB = plan(makePairEnv({ bAmounts: [100n] }), { type: 'limit', side: 'buy', amount: 5_000n, price: 1_400n });
    expect(errorCodes(noB)).toEqual(['PAIR_INSUFFICIENT_TOKENS']);
    expect(noB.issues[0].params).toMatchObject({ ticker: 'BBB', have: 100n });
    const noKas = plan(makePairEnv({ funding: [KAS] }), { type: 'limit', side: 'sell', amount: 5_000n, price: 1_500n });
    expect(errorCodes(noKas)).toContain('INSUFFICIENT_KAS');
  });

  it('KRON caps: the A amount, a B escrow and the B delivery of one minimum fill', () => {
    const kk = { a: kronA(), b: kronB(), aAmounts: [2_000_000_000n], bAmounts: [4_000_000_000n] };
    expect(errorCodes(plan(makePairEnv(kk), { type: 'limit', side: 'sell', amount: 1_000_000_001n, price: 1n }))).toEqual(['AMOUNT_TOO_LARGE']);
    // a bid of 1e6 A (scale 1000: 1e9 base units... here 1_000_000 base units = 1000 KRA) at 2_000_000 KRB per KRA: escrow 2e9 KRB > 1e9
    const esc = plan(makePairEnv(kk), { type: 'limit', side: 'buy', amount: 1_000_000n, price: 2_000_000n });
    expect(errorCodes(esc)).toEqual(['PAIR_KRON_CUSTODY_TOO_LARGE']);
    expect(esc.issues.find((i) => i.code === 'PAIR_KRON_CUSTODY_TOO_LARGE')!.params).toMatchObject({ ticker: 'KRB' });
    // a sell whose minimum fill (the whole amount) would deliver 2e9 KRB in one output
    const del = plan(makePairEnv(kk), { type: 'limit', side: 'sell', amount: 1_000_000n, minFill: 1_000_000n, price: 2_000_000n });
    expect(errorCodes(del)).toEqual(['PAIR_KRON_DELIVERY_TOO_LARGE']);
  });

  it('bad prices and tips; the 2^62 bound of the B value; two equal tokens', () => {
    const env = makePairEnv();
    expect(errorCodes(plan(env, { type: 'limit', side: 'sell', amount: 5_000n, price: 0n }))).toEqual(['PRICE_NOT_POSITIVE']);
    expect(errorCodes(plan(env, { type: 'limit', side: 'sell', amount: 5_000n, price: 1n << 63n }))).toEqual(['PRICE_TOO_LARGE']);
    expect(errorCodes(plan(env, { type: 'limit', side: 'sell', amount: 5_000n, price: 1n << 61n }))).toEqual(['PAIR_NOTIONAL_TOO_LARGE']);
    expect(errorCodes(plan(env, { type: 'limit', side: 'sell', amount: 5_000n, price: 1_500n, tip: -1n }))).toEqual(['TIP_NEGATIVE']);
    expect(errorCodes(plan(env, { type: 'limit', side: 'sell', amount: 5_000n, price: 1_500n, tip: 1n << 62n }))).toEqual(['TIP_TOO_LARGE']);
    expect(errorCodes(plan(env, { type: 'limit', side: 'sell', amount: 0n, price: 1_500n }))).toEqual(['AMOUNT_NOT_POSITIVE']);
    const same = makePairEnv({ b: market3x3({ ticker: 'AAA' }) });
    expect(errorCodes(plan(same, { type: 'limit', side: 'sell', amount: 5_000n, price: 1_500n }))).toEqual(['PAIR_SAME_TOKEN']);
  });

  it('self-trade against an own live pair order of this pair (B prices, no tips)', () => {
    const own: OwnOrderRef[] = [{ covenantId: 'cc'.repeat(32), side: 'sell', price: 1_420n, tip: 0n, amountLeft: 1_000n, active: true }];
    const env = makePairEnv({ ownOrders: own });
    expect(errorCodes(plan(env, { type: 'limit', side: 'buy', amount: 5_000n, price: 1_420n, crossing: 'limit' }))).toContain('SELF_TRADE');
    expect(codes(plan(env, { type: 'limit', side: 'buy', amount: 5_000n, price: 1_419n }))).not.toContain('SELF_TRADE');
  });

  it('without A\'s KAS reference the default minimum fill is a quarter of the amount (info)', () => {
    const p = pairValid(makePairEnv({ kasPerWholeA: null }), { type: 'limit', side: 'sell', amount: 10_001n, price: 1_550n });
    expect(BigInt(pairState(p).minFill)).toBe(2_501n);
    expect(codes(p)).toContain('PAIR_KAS_REFERENCE_MISSING');
  });

  it('environment findings: guards unavailable (error until acknowledged), the pair book against the rate the KAS trades imply', () => {
    const env = { ...makePairEnv(), guardsUnavailable: ['book'] as const };
    expect(errorCodes(plan(env, { type: 'limit', side: 'sell', amount: 5_000n, price: 1_550n }))).toEqual(['GUARDS_UNAVAILABLE']);
    expect(plan({ ...env, guardsAcknowledged: true }, { type: 'limit', side: 'sell', amount: 5_000n, price: 1_550n }).ok).toBe(true);
    const div = plan(makePairEnv({ lastFillPrice: 3_000n }), { type: 'limit', side: 'sell', amount: 5_000n, price: 1_550n });
    expect(codes(div)).toContain('PAIR_MARKET_REFERENCE_DIVERGES');
    expect(div.issues.find((i) => i.code === 'PAIR_MARKET_REFERENCE_DIVERGES')!.params).toMatchObject({ reference: 1_475n, lastFill: 3_000n, ticker: 'BBB' });
  });

  it('maxFills of a resting order budgets the delivery carriers (both sides)', () => {
    const p = pairValid(makePairEnv(), { type: 'limit', side: 'sell', amount: 20_000n, price: 1_550n, maxFills: 5n });
    expect(p.pair!.deliveries).toBe(5n);
    expect(p.pair!.orderValue).toBe(5n * DC);
    expect(errorCodes(plan(makePairEnv(), { type: 'limit', side: 'sell', amount: 20_000n, price: 1_550n, maxFills: 0n }))).toEqual(['MAX_FILLS_INVALID']);
  });

  it('book levels of every source make the touch; an own plan never trades below its limit at the bound', () => {
    const book = { asks: [level(1_490n, 100n)], bids: [level(1_460n, 100n)] };
    const p = pairValid(makePairEnv({ book }), { type: 'market', side: 'sell', amount: 1_000n });
    expect(pairState(p).price).toBe('1460');
    expect(codes(p)).toContain('MARKET_DEPTH_INSUFFICIENT');
  });
});

describe('golden vectors: the planned KobPair is the protocol\'s', () => {
  it('pair.create.ask / pair.create.bid: the same state as the golden request (except its fixed expiry)', () => {
    const gb = market3x3({ covenant_id: '71'.repeat(32), ticker: 'G71' });
    const env = makePairEnv({ b: gb, carrier: 200_000_000n, book: { asks: [], bids: [] } });
    for (const [name, side] of [['pair.create.ask', 'sell'], ['pair.create.bid', 'buy']] as const) {
      const g = goldenRequest(name, MAKER_PK);
      const gs = (g as unknown as { order: { state: Record<string, string> } }).order.state;
      const p = pairValid(env, { type: 'limit', side, amount: 10_000n, price: 1_000n, minFill: 1_000n, maxFills: 9n });
      const s = pairState(p) as unknown as Record<string, string>;
      // the escrow is floor(amount p / scale(A)) + 1, like the golden bid
      const skip = new Set(['expiryDaa', 'decayStep', 'custody']);
      for (const k of Object.keys(gs)) if (!skip.has(k)) expect(s[k], `${name} ${k}`).toBe(gs[k]);
      expect(BigInt(s.custody)).toBe(side === 'sell' ? 10_000n : 10_001n);
      // the golden order value (9 fills x 2 KAS) is the planner's for 9 budgeted deliveries
      expect(p.pair!.orderValue).toBe(BigInt((g as { value: string }).value));
    }
  });
});

describe('fill rule consistency: what the plan discloses is what kob-wasm lets a filler take', () => {
  it('a full fill of a planned ask at its price pays the maker at least the disclosed B; a bid pays exactly the disclosed B', () => {
    const ask = pairValid(makePairEnv(), { type: 'limit', side: 'sell', amount: 7_777n, price: 1_533n });
    const fa = K.pairFill(ask.states[0], 7_777n, 1_533n, ask.pair!.orderValue);
    expect(fa.tOut).toBe(ask.pair!.receiveMinB);
    expect(fa.sOut).toBe(7_777n);
    const bid = pairValid(makePairEnv(), { type: 'limit', side: 'buy', amount: 7_777n, price: 1_333n });
    const fb = K.pairFill(bid.states[0], 7_777n, 1_333n, bid.pair!.orderValue);
    expect(fb.sOut).toBe(bid.pair!.payMaxB);
    expect(fb.outAmount).toBe(bid.pair!.escrowB - bid.pair!.payMaxB!);
  });
});
