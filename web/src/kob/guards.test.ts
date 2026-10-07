import { describe, expect, it } from 'vitest';
import type { BookView, Clock, OwnOrderRef } from './plan-types';
import {
  QUOTE_LIMIT, assessFok, checkAmount, checkCarrierRatio, checkFok, checkMinFill, checkNotional, checkPrice, checkPriceBand, checkSelfTrade, checkTimes, checkTip,
  crosses, crossingTouch, depthWithin, firmReferencePrice, referencePrice, touchPrice,
} from './guards';
import { CLOCK, bookOrder, level, market3x3, market8x8, marketKron } from '../testing/fixtures';
import { MAX_IDLE_DAA } from './daa';

const own = (side: 'sell' | 'buy', price: bigint, tip = 0n, over: Partial<OwnOrderRef> = {}): OwnOrderRef => ({
  covenantId: 'cd'.repeat(32), side, price, tip, amountLeft: 5n, active: true, ...over,
});
const codes = (i: { code: string }[]): string[] => i.map((x) => x.code);

describe('crossing', () => {
  it('crosses all-in: the buyer ceiling reaches the seller floor, equality crosses', () => {
    expect(crosses({ price: 100n, tip: 0n }, { price: 100n, tip: 0n })).toBe(true);
    expect(crosses({ price: 101n, tip: 0n }, { price: 100n, tip: 0n })).toBe(false);
    // a buy tip lifts the ceiling, a sell tip lowers the floor
    expect(crosses({ price: 101n, tip: 0n }, { price: 100n, tip: 1n })).toBe(true);
    expect(crosses({ price: 102n, tip: 2n }, { price: 100n, tip: 0n })).toBe(true);
    expect(crosses({ price: 103n, tip: 2n }, { price: 100n, tip: 0n })).toBe(false);
  });

  it('touch, reference and the marketable-limit probe read the book', () => {
    const b: BookView = { asks: [level(250n, 1n)], bids: [level(240n, 1n)] };
    expect(touchPrice(b, 'sell')).toBe(240n);
    expect(touchPrice(b, 'buy')).toBe(250n);
    expect(touchPrice({ asks: [], bids: [] }, 'sell')).toBeNull();
    expect(referencePrice(b)).toBe(245n);
    expect(referencePrice({ asks: [level(250n, 1n)], bids: [] })).toBe(250n);
    expect(referencePrice({ asks: [], bids: [] })).toBeNull();
    // the firm reference: an ask nobody has to take never sets it
    expect(firmReferencePrice({ asks: [level(250n, 1n)], bids: [level(240n, 1n)] })).toBe(245n);
    expect(firmReferencePrice({ asks: [level(10_000_000n, 1n)], bids: [level(240n, 1n)] })).toBe(240n);
    expect(firmReferencePrice({ asks: [level(10_000_000n, 1n)], bids: [] })).toBeNull();
    expect(firmReferencePrice({ asks: [], bids: [level(240n, 1n)] })).toBe(240n);
    // with the token's scale the bids must be worth one default minimum fill (10 KAS) together: a 1-base-unit bid at an absurd
    // price above the book does not set the reference, the depth below it does
    const S = 100_000_000n;
    const deep: BookView = { asks: [], bids: [level(1_000_000_000_000n, 1n), level(240_000_000n, 10n * S)] };
    expect(firmReferencePrice(deep)).toBe(1_000_000_000_000n);
    expect(firmReferencePrice(deep, S)).toBe(240_000_000n);
    expect(firmReferencePrice({ asks: [level(250_000_000n, S)], bids: deep.bids }, S)).toBe(245_000_000n);
    expect(firmReferencePrice({ asks: [], bids: [level(240_000_000n, S)] }, S)).toBeNull();
    expect(crossingTouch(b, 'sell', 240n, 0n)).toEqual({ crossing: true, touch: 240n });
    expect(crossingTouch(b, 'sell', 241n, 0n)).toEqual({ crossing: false, touch: 240n });
    expect(crossingTouch(b, 'buy', 250n, 0n).crossing).toBe(true);
    expect(crossingTouch(b, 'buy', 249n, 0n).crossing).toBe(false);
    // the book's own tip counts: a resting bid of 240 + tip 10 reaches a sell at 250
    const tipped: BookView = { asks: [], bids: [level(240n, 1n, 1, 10n)] };
    expect(crossingTouch(tipped, 'sell', 250n, 0n).crossing).toBe(true);
    expect(crossingTouch(tipped, 'sell', 251n, 0n).crossing).toBe(false);
  });

  it('depthWithin sums the levels reachable inside the bound', () => {
    const b: BookView = { asks: [level(250n, 5n), level(260n, 7n)], bids: [level(240n, 4n), level(230n, 9n)] };
    expect(depthWithin(b, 'buy', 250n, 0n)).toBe(5n);
    expect(depthWithin(b, 'buy', 260n, 0n)).toBe(12n);
    expect(depthWithin(b, 'sell', 240n, 0n)).toBe(4n);
    expect(depthWithin(b, 'sell', 230n, 0n)).toBe(13n);
    expect(depthWithin(b, 'sell', 231n, 1n)).toBe(13n); // the seller's tip lowers its floor
  });
});

describe('self-trade prevention (matcher.md 10.12)', () => {
  it('refuses a sell that would cross an own resting buy and vice versa', () => {
    expect(codes(checkSelfTrade([own('buy', 250n)], { side: 'sell', price: 250n, tip: 0n }))).toEqual(['SELF_TRADE']);
    expect(codes(checkSelfTrade([own('buy', 250n)], { side: 'sell', price: 251n, tip: 0n }))).toEqual([]);
    expect(codes(checkSelfTrade([own('sell', 250n)], { side: 'buy', price: 250n, tip: 0n }))).toEqual(['SELF_TRADE']);
    expect(codes(checkSelfTrade([own('sell', 250n)], { side: 'buy', price: 249n, tip: 0n }))).toEqual([]);
  });

  it('counts tips on both sides all-in', () => {
    // own buy 250 + tip 5 pays up to 255: a sell at 254 crosses, at 256 does not; the new sell tip lowers its floor
    expect(checkSelfTrade([own('buy', 250n, 5n)], { side: 'sell', price: 255n, tip: 0n })).toHaveLength(1);
    expect(checkSelfTrade([own('buy', 250n, 5n)], { side: 'sell', price: 256n, tip: 0n })).toHaveLength(0);
    expect(checkSelfTrade([own('buy', 250n, 5n)], { side: 'sell', price: 258n, tip: 3n })).toHaveLength(1);
    // own sell 250 - tip 5 receives at least 245: a buy at 245 crosses
    expect(checkSelfTrade([own('sell', 250n, 5n)], { side: 'buy', price: 245n, tip: 0n })).toHaveLength(1);
    expect(checkSelfTrade([own('sell', 250n, 5n)], { side: 'buy', price: 244n, tip: 0n })).toHaveLength(0);
    expect(checkSelfTrade([own('sell', 250n, 5n)], { side: 'buy', price: 242n, tip: 3n })).toHaveLength(1);
  });

  it('an auction / IOC is judged at its worst bound; inactive, same-side and empty orders are ignored', () => {
    // a market sell auctioning down to 240 (worst bound) crosses an own bid at 245
    expect(checkSelfTrade([own('buy', 245n)], { side: 'sell', price: 240n, tip: 0n })).toHaveLength(1);
    expect(checkSelfTrade([own('buy', 245n, 0n, { active: false })], { side: 'sell', price: 240n, tip: 0n })).toHaveLength(0);
    expect(checkSelfTrade([own('buy', 245n, 0n, { amountLeft: 0n })], { side: 'sell', price: 240n, tip: 0n })).toHaveLength(0);
    expect(checkSelfTrade([own('sell', 200n)], { side: 'sell', price: 100n, tip: 0n })).toHaveLength(0);
  });

  it('reports every crossing own order with its id and price', () => {
    const r = checkSelfTrade([own('buy', 250n), own('buy', 260n, 0n, { covenantId: 'ee'.repeat(32) })], { side: 'sell', price: 240n, tip: 0n });
    expect(r).toHaveLength(2);
    expect(r[1].params).toMatchObject({ ownCovenantId: 'ee'.repeat(32), ownPrice: 260n });
    expect(r[0].severity).toBe('error');
  });
});

describe('FOK pre-check (matcher.md 10.4, spec 9.9)', () => {
  const m3 = market3x3().slots; // 3 in / 3 out
  const m8 = market8x8().slots;
  const book = (bidLevels: [bigint, bigint, number][], askLevels: [bigint, bigint, number][] = []): BookView => ({
    bids: bidLevels.map(([p, l, o]) => level(p, l, o)),
    asks: askLevels.map(([p, l, o]) => level(p, l, o)),
  });

  it('accepts a FOK sell the book can fill with few enough bids', () => {
    const b = book([[245n, 5n, 1], [243n, 10n, 2]]);
    expect(checkFok({ side: 'sell', amount: 4n, limit: 240n, tip: 0n, slots: m3, book: b })).toEqual([]);
    expect(checkFok({ side: 'sell', amount: 15n, limit: 240n, tip: 0n, slots: m3, book: b })).toEqual([]); // 3 bids = 3 output slots
  });

  it('FOK sell: bids <= token OUTPUT slots (3/3 refuses 4 counterparties, 8/8 accepts)', () => {
    const b = book([[245n, 2n, 2], [244n, 2n, 2]]);
    const r = checkFok({ side: 'sell', amount: 4n, limit: 240n, tip: 0n, slots: m3, book: b });
    expect(codes(r)).toEqual(['FOK_TOO_MANY_COUNTERPARTIES']);
    expect(r[0].params).toMatchObject({ count: 4, max: 3, exact: 0 });
    expect(checkFok({ side: 'sell', amount: 4n, limit: 240n, tip: 0n, slots: m8, book: b })).toEqual([]);
  });

  it('FOK buy: asks <= token INPUT slots, and never more than 8 token inputs', () => {
    const asks = (n: number): BookView => ({ bids: [], asks: [level(250n, BigInt(n), n)] });
    expect(codes(checkFok({ side: 'buy', amount: 4n, limit: 260n, tip: 0n, slots: m3, book: asks(4) }))).toEqual(['FOK_TOO_MANY_COUNTERPARTIES']);
    expect(checkFok({ side: 'buy', amount: 4n, limit: 260n, tip: 0n, slots: m8, book: asks(4) })).toEqual([]);
    expect(checkFok({ side: 'buy', amount: 8n, limit: 260n, tip: 0n, slots: m8, book: asks(8) })).toEqual([]);
    expect(codes(checkFok({ side: 'buy', amount: 9n, limit: 260n, tip: 0n, slots: m8, book: asks(9) }))).toEqual(['FOK_TOO_MANY_COUNTERPARTIES']);
    // a 16-input program still cannot exceed the 8-input covenant limit
    const wide = { inputs: 16, outputs: 16 };
    const r = checkFok({ side: 'buy', amount: 9n, limit: 260n, tip: 0n, slots: wide, book: asks(9) });
    expect(codes(r)).toEqual(['FOK_TOO_MANY_TOKEN_INPUTS']);
    expect(assessFok({ side: 'buy', amount: 8n, limit: 260n, tip: 0n, slots: wide, book: asks(8) }).maxCounterparties).toBe(8);
  });

  it('rejects when not enough quantity crosses the limit', () => {
    const b = book([[245n, 5n, 1], [230n, 100n, 1]]);
    const r = checkFok({ side: 'sell', amount: 6n, limit: 240n, tip: 0n, slots: m8, book: b });
    expect(codes(r)).toEqual(['FOK_INSUFFICIENT_DEPTH']);
    expect(r[0].params).toMatchObject({ amount: 6n, available: 5n });
    // the limit itself decides which bids count; a tip on either side moves the boundary
    expect(checkFok({ side: 'sell', amount: 6n, limit: 230n, tip: 0n, slots: m8, book: b })).toEqual([]);
    expect(checkFok({ side: 'sell', amount: 6n, limit: 234n, tip: 4n, slots: m8, book: b })).toEqual([]);
    expect(checkFok({ side: 'sell', amount: 6n, limit: 235n, tip: 4n, slots: m8, book: b })).toHaveLength(1);
    expect(codes(checkFok({ side: 'buy', amount: 1n, limit: 260n, tip: 0n, slots: m8, book: { bids: [], asks: [] } }))).toEqual(['FOK_INSUFFICIENT_DEPTH']);
  });

  it('per-order entries make the check exact: the fewest crossing orders that hold the quantity', () => {
    // aggregated: one level in 6 orders is conservative (6 > 3 slots); exact entries show one big order suffices
    const aggregated: BookView = { asks: [], bids: [level(245n, 20n, 6)] };
    expect(codes(checkFok({ side: 'sell', amount: 10n, limit: 240n, tip: 0n, slots: m3, book: aggregated }))).toEqual(['FOK_TOO_MANY_COUNTERPARTIES']);
    const exact: BookView = {
      ...aggregated,
      bidOrders: [bookOrder(245n, 1n), bookOrder(245n, 1n), bookOrder(245n, 12n), bookOrder(245n, 2n), bookOrder(245n, 2n), bookOrder(245n, 2n)],
    };
    const a = assessFok({ side: 'sell', amount: 10n, limit: 240n, tip: 0n, slots: m3, book: exact });
    expect(a).toMatchObject({ exact: true, counterparties: 1, available: 20n, enough: true });
    expect(checkFok({ side: 'sell', amount: 10n, limit: 240n, tip: 0n, slots: m3, book: exact })).toEqual([]);
    // exact but genuinely fragmented: five orders of 2 need 5 counterparties for 10
    const frag: BookView = { asks: [], bids: [level(245n, 10n, 5)], bidOrders: Array.from({ length: 5 }, () => bookOrder(245n, 2n)) };
    const r = checkFok({ side: 'sell', amount: 10n, limit: 240n, tip: 0n, slots: m3, book: frag });
    expect(r[0].params).toMatchObject({ count: 5, exact: 1 });
  });

  it('exact entries also filter by price with their own tips', () => {
    const b: BookView = { asks: [], bids: [], bidOrders: [bookOrder(230n, 10n, 15n), bookOrder(200n, 10n)] };
    expect(assessFok({ side: 'sell', amount: 10n, limit: 240n, tip: 0n, slots: m8, book: b }).available).toBe(10n); // 230 + 15 reaches 240
  });

  it('an order whose minimum fill exceeds the whole FOK amount cannot take part', () => {
    const b: BookView = { asks: [], bids: [], bidOrders: [bookOrder(245n, 100n, 0n, 11n), bookOrder(245n, 4n, 0n, 1n)] };
    expect(assessFok({ side: 'sell', amount: 10n, limit: 240n, tip: 0n, slots: m8, book: b })).toMatchObject({ available: 4n, enough: false });
    expect(assessFok({ side: 'sell', amount: 11n, limit: 240n, tip: 0n, slots: m8, book: b })).toMatchObject({ available: 104n, enough: true, counterparties: 1 });
  });
});

describe('field checks', () => {
  const m = market3x3(); // tick 100, scale 1000 (3 decimals)

  it('amount: positive, within 64 bits, and a KRON custody within one token output', () => {
    expect(codes(checkAmount(0n, m))).toEqual(['AMOUNT_NOT_POSITIVE']);
    expect(codes(checkAmount(-3n, m))).toEqual(['AMOUNT_NOT_POSITIVE']);
    expect(checkAmount(1n, m)).toEqual([]);
    expect(checkAmount((1n << 63n) - 1n, m)).toEqual([]);
    expect(codes(checkAmount(1n << 63n, m))).toEqual(['AMOUNT_TOO_LARGE']);
    const k = marketKron();
    expect(checkAmount(1_000_000_000n, k)).toEqual([]);
    expect(codes(checkAmount(1_000_000_001n, k, 'amount'))).toEqual(['AMOUNT_TOO_LARGE']);
    // a bid holds no custody: only the 64-bit bound applies
    expect(checkAmount(1_000_000_001n, k, 'amount', false)).toEqual([]);
  });

  it('minimum fill: at least one base unit and at most the amount', () => {
    expect(checkMinFill(1n, 10n)).toEqual([]);
    expect(checkMinFill(10n, 10n)).toEqual([]);
    expect(codes(checkMinFill(0n, 10n))).toEqual(['MIN_FILL_INVALID']);
    expect(codes(checkMinFill(11n, 10n))).toEqual(['MIN_FILL_INVALID']);
    expect(checkMinFill(0n, 10n, 'exit.minFill')[0].field).toBe('exit.minFill');
  });

  it('notional: amount x rate / scale below 2^62 (the full fill rounded up)', () => {
    expect(QUOTE_LIMIT).toBe(1n << 62n);
    expect(checkNotional(1_000n, (1n << 62n) - 1n, 1_000n)).toEqual([]);
    expect(codes(checkNotional(1_000n, 1n << 62n, 1_000n))).toEqual(['NOTIONAL_TOO_LARGE']);
    // 1 base unit short of the bound, rounded up to it
    expect(codes(checkNotional(3n, ((1n << 62n) * 1_000n) / 3n + 1n, 1_000n))).toEqual(['NOTIONAL_TOO_LARGE']);
    expect(checkNotional(0n, 1n << 62n, 1n)).toEqual([]);
  });

  it('price: positive, 64-bit, on the tick (refused with the neighbours)', () => {
    expect(checkPrice(m, 250_000_000n)).toEqual([]);
    expect(codes(checkPrice(m, 0n))).toEqual(['PRICE_NOT_POSITIVE']);
    expect(codes(checkPrice(m, (1n << 63n)))).toEqual(['PRICE_TOO_LARGE']);
    const r = checkPrice(m, 250_000_050n, 'limit');
    expect(codes(r)).toEqual(['PRICE_NOT_ON_TICK']);
    expect(r[0].params).toMatchObject({ tick: 100n, below: 250_000_000n, above: 250_000_100n });
    expect(r[0].field).toBe('limit');
    expect(r[0].message).toMatch(/multiple of the tick/);
  });

  it('tip: not negative, and a sell must keep something', () => {
    expect(codes(checkTip('sell', -1n, 100n))).toEqual(['TIP_NEGATIVE']);
    expect(codes(checkTip('sell', 100n, 100n))).toEqual(['TIP_EXCEEDS_PRICE']);
    expect(checkTip('sell', 99n, 100n)).toEqual([]);
    expect(checkTip('buy', 1_000_000n, 100n)).toEqual([]);
  });

  it('times: 90-day horizon, expiry after activation, past activation is informational', () => {
    const c: Clock = CLOCK;
    const day90 = c.daa + MAX_IDLE_DAA;
    expect(checkTimes(c, { activeFrom: 0n, expiryDaa: day90 })).toEqual([]);
    expect(codes(checkTimes(c, { activeFrom: 0n, expiryDaa: day90 + 1n }))).toEqual(['EXPIRY_TOO_FAR']);
    expect(codes(checkTimes(c, { activeFrom: day90 + 1n, expiryDaa: day90 }))).toEqual(['ACTIVE_TOO_FAR', 'ACTIVE_AFTER_EXPIRY']);
    expect(codes(checkTimes(c, { activeFrom: 0n, expiryDaa: c.daa }))).toEqual(['EXPIRY_TOO_SOON']);
    expect(codes(checkTimes(c, { activeFrom: c.daa + 1000n, expiryDaa: c.daa + 1000n }))).toEqual(['ACTIVE_AFTER_EXPIRY']);
    const past = checkTimes(c, { activeFrom: 0n, expiryDaa: c.daa + 100n, requestedActiveFrom: c.daa - 5n });
    expect(past.map((i) => [i.code, i.severity])).toEqual([['ACTIVE_IN_PAST', 'info']]);
  });

  it('price band: warns far through the market and far away, silent near it or without a reference', () => {
    const ref = 1_000n;
    expect(checkPriceBand(ref, 'sell', 950n)).toEqual([]);
    expect(codes(checkPriceBand(ref, 'sell', 900n))).toEqual(['PRICE_AGGRESSIVE_VS_MARKET']); // exactly 10% through
    expect(codes(checkPriceBand(ref, 'buy', 1_100n))).toEqual(['PRICE_AGGRESSIVE_VS_MARKET']);
    expect(checkPriceBand(ref, 'buy', 1_099n)).toEqual([]);
    expect(codes(checkPriceBand(ref, 'sell', 1_500n))).toEqual(['PRICE_FAR_FROM_MARKET']);
    expect(checkPriceBand(ref, 'sell', 1_499n)).toEqual([]);
    expect(codes(checkPriceBand(ref, 'buy', 500n))).toEqual(['PRICE_FAR_FROM_MARKET']);
    expect(checkPriceBand(null, 'sell', 1n)).toEqual([]);
    expect(checkPriceBand(ref, 'sell', 900n)[0].severity).toBe('warning');
    expect(checkPriceBand(ref, 'sell', 900n)[0].params).toMatchObject({ percent: '10' });
  });

  it('carrier ratio warns when carriers exceed the order value', () => {
    expect(checkCarrierRatio(2_000_000_000n, 1_000_000_000n)).toHaveLength(1);
    expect(checkCarrierRatio(2_000_000_000n, 2_000_000_000n)).toEqual([]);
    expect(checkCarrierRatio(1n, null)).toEqual([]);
  });
});
