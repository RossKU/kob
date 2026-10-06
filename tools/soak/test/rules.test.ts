// Unit tests of the checker's pure rules: `npm test` (node --test, Node >= 22.18 strips the TypeScript types natively).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  checkFeeLine,
  maxFoldedExcess,
  checkFill,
  checkFokEvents,
  checkIfdAskExitFunding,
  checkKill,
  checkRearm,
  checkRepeatCycle,
  checkAmountBalance,
  checkRepeatAmount,
  checkTrigger,
  checkX402,
  diffOrder,
  diffSets,
  type EvidenceFact,
  healthIssues,
  killDaa,
  metricIssues,
  parseProm,
  partialTerms,
  Persist,
  quoteOf,
  replayLedger,
  stopFloor,
  stopCeiling,
  termsAtFill,
  worstBound,
} from '../src/checker/rules.ts';
import type { EventLike, OrderLike } from '../src/checker/types.ts';

const ID = 'aa'.repeat(32);
const TOKEN = 'bb'.repeat(32);
// protocol v3: base units and sompi per whole token. Scale 10^8 (8 decimals): 2 whole tokens are 200,000,000 base units, 20 KAS each.
const U = 100_000_000;
const ask = { scale: String(U), minFill: '1', price: '2000000000', tip: '0', slope: '0', priceEnd: '0', amountLeft: String(4 * U), tif: '0' };
const fill = (o: Partial<EventLike> = {}): EventLike => ({
  id: 1,
  covenant_id: ID,
  daa: 1000,
  txid: 'cc'.repeat(32),
  kind: 'fill',
  side: 1,
  amount: String(2 * U),
  price: '2000000000',
  payout: '4000000000',
  closes: false,
  detail: { verified: true, t: 995 },
  ...o,
});

// ------------------------------------------------------------------------------------------------ all-in

test('worst bounds per contract', () => {
  assert.equal(worstBound('KobAsk', ask).bound, 2_000_000_000n);
  assert.equal(worstBound('KobAsk', { ...ask, slope: '10', priceEnd: '1900000000' }).bound, 1_900_000_000n);
  assert.equal(worstBound('KobBid', { ...ask, slope: '10', priceEnd: '2100000000' }).bound, 2_100_000_000n);
  assert.equal(worstBound('KobBidKron', { ...ask }).dir, 'max');
  const cond = { tpPrice: '2500000000', stopPrice: '1800000000', slipBps: '300', trailStep: '0' };
  assert.equal(stopFloor(cond), 1_800_000_000n - 180_000n * 300n);
  assert.equal(worstBound('KobCondAsk', cond).bound, 1_800_000_000n - 180_000n * 300n);
  assert.equal(worstBound('KobCondBid', { tpPrice: '1500000000', stopPrice: '2200000000', slipBps: '300', trailStep: '0' }).bound, 2_200_000_000n + 220_000n * 300n);
  assert.equal(worstBound('KobCondAsk', { ...cond, trailStep: '1000' }).bound, null);
  assert.equal(worstBound('KobIfdBid', { price: '7' }).bound, 7n);
  assert.equal(worstBound('KobIfdAsk', { price: '7' }).dir, 'min');
});

test('stop band multiplies first (live TN10 KobCondBid 6d32b26e: filled at the exact ceiling 206035429084232)', () => {
  const live = { tpPrice: '0', stopPrice: '200034397169158', slipBps: '300', trailStep: '0' };
  assert.equal(stopCeiling(live), 206_035_429_084_232n);
  assert.equal(worstBound('KobCondBid', live).bound, 206_035_429_084_232n);
  assert.equal(stopFloor(live), 200_034_397_169_158n - 6_001_031_915_074n);
});

test('fill at the limit with the exact all-in payout passes', () => {
  const r = checkFill({ covenant_id: ID, contract: 'KobAsk' }, ask, fill());
  assert.deepEqual(r.violations, []);
  assert.ok(r.boundChecked && r.payoutChecked);
});

test('maker payout one sompi short is a violation', () => {
  const r = checkFill({ covenant_id: ID, contract: 'KobAsk' }, { ...ask, tip: '1000' }, fill({ payout: String(2n * (2_000_000_000n - 1000n) - 1n) }));
  assert.equal(r.violations.length, 1);
  assert.equal(r.violations[0].invariant, 'all-in');
  assert.equal(r.violations[0].detail.short, '1');
});

test('an ask amended in place is checked against the terms it filled under (TN10 9440babb: amended 2,443,640,000 -> 2,429,590,000, then filled)', () => {
  const placed = { ...ask, price: '2456750000' };
  const amend = (id: number, prevPrice: string): EventLike => ({ ...fill(), id, kind: 'amend', amount: null, price: null, payout: null, detail: { entry: 'cancel', previous: { activeFrom: 0, expiryDaa: 663483136, price: Number(prevPrice), tif: 0, tip: 0 } } });
  const evs = [amend(2, '2456750000'), amend(4, '2443640000')];
  const current = { ...ask, price: '2429590000' };
  const f1 = fill({ id: 3, price: '2443640000', payout: String(2n * 2_443_640_000n) });
  const f2 = fill({ id: 5, price: '2429590000', payout: String(2n * 2_429_590_000n) });
  // with the placement terms both fills look like sells below the limit
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobAsk' }, placed, f2).violations.length, 1);
  // the first fill happened between the two amends (terms = previous of amend 4), the second after the last one (current terms)
  assert.equal(termsAtFill(placed, current, evs, 3).price, '2443640000');
  assert.deepEqual(checkFill({ covenant_id: ID, contract: 'KobAsk' }, termsAtFill(placed, current, evs, 3), f1).violations, []);
  assert.deepEqual(checkFill({ covenant_id: ID, contract: 'KobAsk' }, termsAtFill(placed, current, evs, 5), f2).violations, []);
  // a fill below the amended price is still caught
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobAsk' }, termsAtFill(placed, current, evs, 5), fill({ id: 6, price: '2429589999', payout: '9999999999' })).violations.length, 1);
  // no amend: the memoized terms
  assert.equal(termsAtFill(placed, current, [], 5), placed);
});

test('sell below the worst bound / buy above it are violations', () => {
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobAsk' }, ask, fill({ price: '1999999999', payout: '9999999999' })).violations.length, 1);
  const bid = checkFill({ covenant_id: ID, contract: 'KobBid' }, ask, fill({ side: 2, price: '2000000001', payout: null }));
  assert.equal(bid.violations.length, 1);
  assert.equal(bid.payoutChecked, false);
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobBid' }, ask, fill({ side: 2, price: '1990000000', payout: null })).violations.length, 0);
});

test('decaying ask: event quote above priceEnd is fine, unverified fills use the worst bound', () => {
  const dutch = { ...ask, slope: '1000000', priceEnd: '1500000000' };
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobAsk' }, dutch, fill({ price: '1600000000', payout: '3200000000' })).violations.length, 0);
  // unverified: the event price may be the start price; the payout is held to the worst bound only
  const r = checkFill({ covenant_id: ID, contract: 'KobAsk' }, dutch, fill({ price: '2000000000', payout: '3000000000', detail: { verified: false } }));
  assert.equal(r.violations.length, 0);
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobAsk' }, dutch, fill({ price: null, payout: '2999999999', detail: {} })).violations.length, 1);
});

test('merged take-profit of a booked exit: payout is the profit over rptPrice', () => {
  const exit = { ...ask, tpPrice: '2100000000', stopPrice: '0', slipBps: '0', trailStep: '0', rptPrice: '2000000000' };
  const ev = fill({ price: '2100000000', payout: String(2n * 100_000_000n), detail: { verified: true, t: 1, merged_into: 'dd'.repeat(32) } });
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobCondAsk' }, exit, ev).violations.length, 0);
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobCondAsk' }, exit, { ...ev, payout: String(2n * 100_000_000n - 1n) }).violations.length, 1);
});

test('sell-first entry fill funds its exit with the proceeds', () => {
  const s = { scale: String(U), price: '2000000000', tip: '0' };
  const exit = { covenant_id: 'ee'.repeat(32), contract: 'KobCondBid', side: 2, status: 'open', last_daa: 1, current: { txid: fill().txid, index: 1, value: '3999999999' } } as OrderLike;
  assert.ok(checkIfdAskExitFunding({ covenant_id: ID }, s, fill(), exit));
  assert.equal(checkIfdAskExitFunding({ covenant_id: ID }, s, fill(), { ...exit, current: { ...exit.current!, value: '5000000000' } }), null);
  assert.equal(checkIfdAskExitFunding({ covenant_id: ID }, s, fill(), { ...exit, current: { ...exit.current!, txid: 'ff'.repeat(32) } }), null);
});

test('terms of a terminated order: the bid cap from budget_rate, asks keep only the payout check', () => {
  const bid = partialTerms({ price: '2000000000', tip: '0', scale: U, budget_rate: '2000000000', tif: 0 })!;
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, fill({ side: 2, price: '2000000001', payout: null })).violations.length, 1);
  const rising = partialTerms({ price: '2000000000', tip: '0', scale: U, budget_rate: '2100000000' })!;
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobBid' }, rising, fill({ side: 2, price: '2050000000', payout: null })).violations.length, 0);
  const a = partialTerms({ price: '2000000000', tip: '0', scale: U })!;
  const r = checkFill({ covenant_id: ID, contract: 'KobAsk' }, a, fill({ price: '1500000000', payout: '3000000000' }));
  assert.deepEqual(r.violations, []);
  assert.equal(r.boundChecked, false);
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobAsk' }, a, fill({ price: '1500000000', payout: '2999999999' })).violations.length, 1);
  assert.equal(partialTerms({ price: null, scale: 1 }), null);
  // a tip is part of the budget rate: priceMax = budget_rate - tip
  assert.equal(partialTerms({ price: '2000000000', tip: '1000', scale: U, budget_rate: '2000001000' })!.slope, '0');
  assert.equal(partialTerms({ price: '2000000000', tip: '1000', scale: U, budget_rate: '2100001000' })!.priceEnd, '2100000000');
});

// ------------------------------------------------------------------------------------------------ IOC / FOK

const ioc = (o: Partial<OrderLike> = {}): OrderLike => ({ covenant_id: ID, contract: 'KobAsk', side: 1, status: 'open', tif: 1, last_daa: 10_000, current_daa: 10_000, active_from: 0, expiry_daa: 99_999_999, kill_daa: null, ...o });

test('kill time: max(UTXO DAA, activeFrom) + 600, capped by the expiry', () => {
  assert.equal(killDaa(ioc()), 10_600n);
  assert.equal(killDaa(ioc({ active_from: 20_000 })), 20_600n);
  assert.equal(killDaa(ioc({ expiry_daa: 10_100 })), 10_100n);
  assert.equal(killDaa(ioc({ kill_daa: 12_345 })), 12_345n);
});

test('an IOC alive past kill + grace is reported, within grace it is not', () => {
  assert.equal(checkKill(ioc(), 11_800n).length, 0);
  assert.equal(checkKill(ioc(), 11_801n).length, 1);
  assert.equal(checkKill(ioc({ tif: 0 }), 99_000n).length, 0);
  assert.equal(checkKill(ioc({ status: 'killed' }), 99_000n).length, 0);
  assert.equal(checkKill(ioc({ tif: 2, status: 'partial' }), 10_100n).length, 1);
});

test('IOC kill deadline runs on the indexer cursor and only while the indexer follows (a lagging indexer is not blamed)', () => {
  // kill 10,600; the cursor is 1,231 past it (as in the 07:17 incident) but the indexer only started following at 11,800
  assert.equal(checkKill(ioc(), 11_831n, 1200n, null).length, 0, 'lagging: nothing is overdue');
  assert.equal(checkKill(ioc(), 11_831n, 1200n, 11_800n).length, 0, 'grace runs from followingSince, not from the kill time');
  assert.equal(checkKill(ioc(), 13_000n, 1200n, 11_800n).length, 0);
  const v = checkKill(ioc(), 13_001n, 1200n, 11_800n);
  assert.equal(v.length, 1);
  assert.equal(v[0].detail.cursorDaa, '13001');
  assert.equal(v[0].detail.overdueSinceFollowingDaa, '1201');
  // following since before the kill: the plain rule
  assert.equal(checkKill(ioc(), 11_800n, 1200n, 9_000n).length, 0);
  assert.equal(checkKill(ioc(), 11_801n, 1200n, 9_000n).length, 1);
  // a FOK left partial is wrong whatever the indexer state
  assert.equal(checkKill(ioc({ tif: 2, status: 'partial' }), 10_100n, 1200n, null).length, 1);
});

test('an IOC / FOK created after its own expiry is overdue only from its creation', () => {
  // expiry 10,100, created (accepted) at 12,000: kill time 10,100 passed before the order existed (a creation that sat in the mempool)
  const late = ioc({ last_daa: 12_000, current_daa: 12_000, expiry_daa: 10_100 });
  assert.equal(killDaa(late), 10_100n);
  assert.equal(checkKill(late, 13_200n).length, 0);
  const v = checkKill(late, 13_201n);
  assert.equal(v.length, 1);
  assert.equal(v[0].detail.bornDaa, '12000');
  assert.equal(v[0].detail.overdueSinceFollowingDaa, '1201');
});

test('FOK events: a non-closing fill or a remainder kill in the fill transaction', () => {
  const o = { covenant_id: ID, contract: 'KobAsk', tif: 2 };
  assert.equal(checkFokEvents(o, [fill({ closes: true })]).length, 0);
  assert.equal(checkFokEvents(o, [fill({ closes: false })]).length, 1);
  assert.equal(checkFokEvents(o, [fill({ closes: false }), fill({ id: 2, kind: 'kill', amount: String(2 * U), closes: true })]).length, 2);
  assert.equal(checkFokEvents({ ...o, tif: 1 }, [fill({ closes: false })]).length, 0);
});

// ------------------------------------------------------------------------------------------------ triggers (touch)

const condAsk = { tokenCovId: TOKEN, scale: String(U), stopPrice: '1800000000', tpPrice: '0', trailStep: '0', trailGap: '0', minTouch: '1000', minRestDaa: '600', activeFrom: '0' };
/** a resting ask filled in the trigger transaction (100,000 base units, resting since DAA 1000) */
const evd = (o: Partial<EvidenceFact> = {}): EvidenceFact => ({ order: 'dd'.repeat(32), contract: 'KobAsk', side: 1, tokenCovId: TOKEN, scale: String(U), price: '1790000000', amount: '100000', slope: '0', exposedSince: '1000', ...o });
const arm = fill({ kind: 'arm', daa: 2000, amount: null, price: null, payout: null });
const trail = fill({ kind: 'trail', daa: 2000, amount: null, price: null, payout: null });
const CA = { covenant_id: ID, contract: 'KobCondAsk' };

test('arm backed by a qualifying resting ask fill passes', () => {
  const r = checkTrigger(CA, condAsk, arm, [evd()]);
  assert.ok(r.ok, r.reasons.join(','));
  // the Kron twin is evidence too, and exactly minRestDaa of exposure is enough
  assert.ok(checkTrigger(CA, condAsk, arm, [evd({ contract: 'KobAskKron', exposedSince: '1400' })]).ok);
  assert.ok(checkTrigger({ covenant_id: ID, contract: 'KobCondAskKron' }, condAsk, arm, [evd()]).ok);
});

test('arm with no fill in the transaction is an error', () => {
  const r = checkTrigger(CA, condAsk, arm, []);
  assert.equal(r.ok, false);
  assert.equal(r.violations.length, 1);
  assert.equal(r.violations[0].severity, 'error');
  assert.equal(r.violations[0].invariant, 'trigger');
});

test('arm on evidence of the wrong side is an error', () => {
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ contract: 'KobBid', side: 2 })]).violations[0].severity, 'error');
  // a buy stop needs a filled resting BID, not an ask
  const bidStop = { ...condAsk, stopPrice: '2200000000' };
  assert.equal(checkTrigger({ covenant_id: ID, contract: 'KobCondBid' }, bidStop, arm, [evd({ price: '2300000000' })]).ok, false);
  // a filled pair / conditional / if-done leg is never evidence of a KAS stop
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ contract: 'KobCondAsk' })]).ok, false);
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ contract: 'KobPair' })]).ok, false);
});

test('arm with the evidence price beyond the stop is an error (both sides, the boundary passes)', () => {
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ price: '1800000001' })]).violations[0].severity, 'error');
  assert.ok(checkTrigger(CA, condAsk, arm, [evd({ price: '1800000000' })]).ok);
  const bidStop = { ...condAsk, stopPrice: '2200000000' };
  const bidEv = (price: string) => evd({ contract: 'KobBid', side: 2, price });
  assert.ok(checkTrigger({ covenant_id: ID, contract: 'KobCondBid' }, bidStop, arm, [bidEv('2200000000')]).ok);
  assert.equal(checkTrigger({ covenant_id: ID, contract: 'KobCondBid' }, bidStop, arm, [bidEv('2199999999')]).ok, false);
  // stop entries read entryStop
  assert.ok(checkTrigger({ covenant_id: ID, contract: 'KobIfdBid' }, { ...bidStop, entryStop: '2100000000' }, arm, [bidEv('2100000000')]).ok);
  assert.equal(checkTrigger({ covenant_id: ID, contract: 'KobIfdAsk' }, { ...condAsk, entryStop: '1700000000' }, arm, [evd({ price: '1700000001' })]).ok, false);
});

test('arm with evidence amount below minTouch is an error', () => {
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ amount: '999' })]).violations[0].severity, 'error');
  assert.ok(checkTrigger(CA, condAsk, arm, [evd({ amount: '1000' })]).ok, 'exactly minTouch base units is enough');
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ amount: 0 })]).ok, false);
});

test('arm on decaying evidence, another token or another scale is an error', () => {
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ slope: '5' })]).violations[0].severity, 'error');
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ tokenCovId: 'ee'.repeat(32) })]).ok, false);
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ scale: '1' })]).ok, false);
});

test('arm on evidence exposed for less than minRestDaa, or before activeFrom, is an error', () => {
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ exposedSince: '1401' })]).violations[0].severity, 'error');
  assert.equal(checkTrigger(CA, { ...condAsk, activeFrom: '2001' }, arm, [evd()]).ok, false);
  assert.ok(checkTrigger(CA, { ...condAsk, activeFrom: '2000' }, arm, [evd()]).ok);
});

test('arm whose evidence exposure cannot be verified is a warning, unless another fill proves it', () => {
  const r = checkTrigger(CA, condAsk, arm, [evd({ exposedSince: null })]);
  assert.equal(r.ok, false);
  assert.equal(r.violations.length, 1);
  assert.equal(r.violations[0].severity, 'warn');
  assert.match(String(r.violations[0].detail.problem), /cannot be verified/);
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ slope: null })]).violations[0].severity, 'warn');
  // an unknown exposure never hides a failed rule: the price is beyond the stop
  assert.equal(checkTrigger(CA, condAsk, arm, [evd({ exposedSince: null, price: '1800000001' })]).violations[0].severity, 'error');
  assert.ok(checkTrigger(CA, condAsk, arm, [evd({ exposedSince: null }), evd()]).ok);
});

test('one bad and one good fill in the transaction: the good one backs the arm', () => {
  assert.ok(checkTrigger(CA, condAsk, arm, [evd({ contract: 'KobBid', side: 2 }), evd()]).ok);
});

test('trail backed by a resting bid fill passes; wrong side or a stop the evidence does not justify is an error', () => {
  const trailing = { ...condAsk, trailStep: '10000000', trailGap: '50000000', stopPrice: '1850000000' };
  const bidEv = (price: string) => evd({ contract: 'KobBid', side: 2, price });
  const latest = { latestStopChange: true };
  assert.ok(checkTrigger(CA, trailing, trail, [bidEv('1900000000')], latest).ok);
  assert.ok(checkTrigger(CA, trailing, trail, [bidEv('1909999999')], latest).ok, 'k = floor((rp - gap - old)/step): the new stop is the highest step below rp - gap');
  assert.equal(checkTrigger(CA, trailing, trail, [bidEv('1899999999')], latest).ok, false, 'new stop above rp - gap');
  assert.equal(checkTrigger(CA, trailing, trail, [bidEv('1910000000')], latest).ok, false, 'the evidence justified one more step');
  assert.ok(checkTrigger(CA, trailing, trail, [bidEv('1910000000')]).ok, 'an older trail: side only');
  assert.equal(checkTrigger(CA, trailing, trail, [evd({ price: '1900000000' })]).ok, false, 'a sell stop trails on a bid, not an ask');
  assert.equal(checkTrigger(CA, condAsk, trail, [bidEv('1900000000')]).ok, false, 'no trailStep');
  assert.equal(checkTrigger(CA, trailing, trail, []).violations[0].severity, 'error');
  assert.equal(checkTrigger(CA, trailing, trail, [{ ...bidEv('1900000000'), amount: '10' }], latest).ok, false, 'below minTouch');
});

test('buy stop trails down on a resting ask fill', () => {
  const trailing = { ...condAsk, stopPrice: '2150000000', trailStep: '10000000', trailGap: '50000000' };
  const CB = { covenant_id: ID, contract: 'KobCondBid' };
  const latest = { latestStopChange: true };
  assert.ok(checkTrigger(CB, trailing, trail, [evd({ price: '2100000000' })], latest).ok);
  assert.equal(checkTrigger(CB, trailing, trail, [evd({ price: '2100000001' })], latest).ok, false);
  assert.equal(checkTrigger(CB, trailing, trail, [evd({ contract: 'KobBid', side: 2, price: '2100000000' })], latest).ok, false);
});

// ------------------------------------------------------------------------------------------------ repeat

// amounts in base units (a multiple of U = one whole token of scale 10^8 unless the test is about the last base unit)
const entry = (amount: string): OrderLike => ({ covenant_id: ID, contract: 'KobIfdBid', side: 2, status: 'open', last_daa: 1, state: { kind: 'KobIfdBid', state: { amountLeft: amount, scale: String(U), price: '2000000000', tip: '0', rptAmount: String(7 * U) } } });
const exitOf = (amount: string, status = 'open', parent = ID): OrderLike => ({ covenant_id: 'e' + amount + status, contract: 'KobCondAsk', side: 1, status, last_daa: 1, state: { kind: 'KobCondAsk', state: { amountLeft: amount, parent, scale: String(U), tpPrice: '2100000000', tip: '0', rptPrice: '2000000000' } } });

test('entry amount + booked exit amounts <= N (base units, down to the last unit)', () => {
  assert.equal(checkRepeatAmount(entry(String(U)), 3 * U, [exitOf(String(2 * U))]), null);
  assert.ok(checkRepeatAmount(entry(String(2 * U)), 3 * U, [exitOf(String(2 * U))]));
  // one base unit over N is a violation, N exactly is not (N may be the decimal string of the view)
  assert.equal(checkRepeatAmount(entry(String(2 * U)), String(3 * U), [exitOf(String(U))]), null);
  assert.ok(checkRepeatAmount(entry(String(2 * U)), String(3 * U), [exitOf(String(U + 1))]));
  assert.equal(checkRepeatAmount(entry(String(2 * U)), 3 * U, [exitOf(String(2 * U), 'filled')]), null);
  assert.equal(checkRepeatAmount(entry(String(2 * U)), 3 * U, [exitOf(String(2 * U), 'open', 'ff'.repeat(32))]), null);
  assert.equal(checkRepeatAmount(entry(String(2 * U)), null, [exitOf(String(2 * U))]), null);
});

test('rearm pairs with a merged exit fill of the same amount', () => {
  const exit = 'e1';
  const rearm = fill({ kind: 'rearm', amount: String(2 * U), detail: { exit } });
  const good = fill({ covenant_id: exit, amount: String(2 * U), detail: { merged_into: ID } });
  assert.equal(checkRearm(ID, rearm, [good]), null);
  assert.ok(checkRearm(ID, rearm, []));
  assert.ok(checkRearm(ID, rearm, [{ ...good, amount: String(2 * U - 1) }]));
  assert.ok(checkRearm(ID, rearm, [{ ...good, detail: {} }]));
  assert.ok(checkRearm(ID, fill({ kind: 'rearm', detail: {} }), null));
});

test('repeat cycle: rptPrice is the entry budget rate and the maker keeps the spread', () => {
  const x = exitOf(String(2 * U));
  const ok = fill({ covenant_id: x.covenant_id, price: '2100000000', payout: String(2n * 100_000_000n) });
  assert.deepEqual(checkRepeatCycle(entry('0'), x, ok), []);
  assert.equal(checkRepeatCycle(entry('0'), x, { ...ok, payout: String(2n * 100_000_000n - 1n) }).length, 1);
  assert.equal(checkRepeatCycle(entry('0'), x, { ...ok, price: '1900000000' }).length, 1);
  const wrongBudget = { ...x, state: { kind: 'KobCondAsk', state: { ...x.state!.state, rptPrice: '1999999999' } } };
  assert.ok(checkRepeatCycle(entry('0'), wrongBudget, ok).some((v) => v.subject.endsWith(':rptPrice')));
});

// ------------------------------------------------------------------------------------------------ fees

test('fee == 100 x max(compute, transientNormalized) exactly in relay mode', () => {
  const l = { txid: 't1', fee: '2500000', minFee: '2500000', feeRate: '100', feeMode: 'relay', compute: 20000, transientNormalized: 25000, storage: 90000 };
  assert.equal(checkFeeLine(l), null);
  assert.ok(checkFeeLine({ ...l, fee: '2500001' }));
  assert.ok(checkFeeLine({ ...l, minFee: '2000000' }));
  assert.ok(checkFeeLine({ ...l, feeRate: '101', fee: String(101 * 25000), minFee: '2500000' }));
  assert.equal(checkFeeLine({ ...l, feeMode: 'priority' }), 'unchecked');
});

// ------------------------------------------------------------------------------------------------ x402

const led = (e: Record<string, unknown>) => JSON.stringify({ entry: e });

test('x402: each payment settled exactly once', () => {
  const now = 10_000_000;
  const ledger = [
    led({ txid: 'p1', state: 'pending', requestHash: 'r1', consumed: [{ txid: 'u', index: 0 }] }),
    led({ txid: 'p1', state: 'accepted', requestHash: 'r1', kind: 'native', consumed: [{ txid: 'u', index: 0 }], updatedMs: now }),
    led({ txid: 'p2', state: 'failed', requestHash: 'r2' }),
    '{"entry":{"txid":"torn',
  ].join('\n');
  const ok = checkX402([{ ts: now, path: '/native', txid: 'p1' }], ledger, now);
  assert.deepEqual(ok.violations, []);
  assert.equal(ok.stats.settled, 1);
  assert.equal(ok.stats.ledgerFailed, 1);
  assert.equal(replayLedger(ledger).bad, 1);
  // missing after the grace period, failed after a 200, recorded twice
  const r = checkX402(
    [
      { ts: 0, path: '/token', txid: 'p9' },
      { ts: now, path: '/swap', txid: 'p2' },
      { ts: now, path: '/native', txid: 'p1' },
      { ts: now, path: '/native', txid: 'p1' },
    ],
    ledger,
    now,
  );
  assert.deepEqual(r.violations.map((v) => v.subject).sort(), ['p1:dup', 'p2:failed', 'p9']);
  // a pending payment inside the grace period is not a violation
  assert.deepEqual(checkX402([{ ts: now - 1000, path: '/token', txid: 'p7' }], '', now).violations, []);
});

test('x402: one outpoint consumed by two accepted settlements, one txid bound to two requests', () => {
  const ledger = [
    led({ txid: 'a', state: 'accepted', requestHash: 'r1', consumed: [{ txid: 'u', index: 1 }] }),
    led({ txid: 'b', state: 'accepted', requestHash: 'r2', consumed: [{ txid: 'u', index: 1 }] }),
    led({ txid: 'c', state: 'pending', requestHash: 'r3' }),
    led({ txid: 'c', state: 'accepted', requestHash: 'r4' }),
  ].join('\n');
  const subjects = checkX402([], ledger, 0).violations.map((v) => v.subject).sort();
  assert.deepEqual(subjects, ['c:requests', 'u:1:double']);
});

// ------------------------------------------------------------------------------------------------ agreement, health, persistence

test('indexer agreement diffs', () => {
  const a = { covenant_id: ID, contract: 'KobAsk', side: 1, status: 'open', filled_amount: '0', amount_left: '300000000', last_daa: 5, current: { txid: 't', index: 0, value: '1' }, state_known: true } as OrderLike;
  assert.deepEqual(diffOrder(a, { ...a }), []);
  assert.deepEqual(diffOrder(a, null), ['missing in B']);
  assert.equal(diffOrder(a, { ...a, status: 'partial', amount_left: '200000000' }).length, 2);
  assert.deepEqual(diffSets(['1', '2'], ['2', '3']), { onlyA: ['1'], onlyB: ['3'] });
});

test('health and metrics issues', () => {
  const now = 1_000_000;
  assert.deepEqual(healthIssues({ state: 'following', cursor_daa: 1, node_daa: 2, lag_daa: 5, last_progress_unix_ms: now - 1000 }, now), []);
  assert.equal(healthIssues({ state: 'gap', cursor_daa: 1, node_daa: 2, lag_daa: 5000, last_progress_unix_ms: now - 120_000 }, now).length, 3);
  assert.deepEqual(healthIssues(null, now), ['unreachable']);
  const m = parseProm('# HELP x\nkob_matcher_node_synced 1\nkob_matcher_book_stale 0\nkob_matcher_last_step_seconds 990\nfoo{a="b"} 2.5\n');
  assert.equal(m['foo{a="b"}'], 2.5);
  assert.deepEqual(metricIssues(m, 1000), []);
  assert.deepEqual(metricIssues({ ...m, kob_matcher_node_synced: 0, kob_matcher_book_stale: 1 }, 1000).length, 2);
  assert.deepEqual(metricIssues(m, 1100), ['last step 110 s ago']);
});

test('persistence: fires once after N rounds of the same condition; a changed signature restarts', () => {
  const p = new Persist();
  assert.equal(p.observe('k', true, 0, 2, 0, 'a').fire, false);
  assert.equal(p.observe('k', true, 10, 2, 0, 'b').fire, false);
  assert.equal(p.observe('k', true, 20, 2, 0, 'b').fire, true);
  assert.equal(p.observe('k', true, 30, 2, 0, 'b').fire, false);
  assert.equal(p.observe('k', false, 40).fire, false);
  assert.equal(p.observe('k', true, 50, 1, 20).fire, false);
  assert.equal(p.observe('k', true, 70, 1, 20).fire, true);
});

test('a buy costs at most its all-in limit (escrow drawn less the delivery output, rising-bid surplus included)', () => {
  const bid = { ...ask, tip: '1000', deliveryCarrier: '1000000000' };
  const allIn = 2n * (2_000_000_000n + 1000n);
  // a rising bid consumes used(n) = ceil(n * (pMax + tip) / scale) at the budget rate and returns the surplus on the delivery output
  const surplus = 2n * 4_000_000n;
  const ev = (cost: bigint, soldOut = false) =>
    fill({
      side: 2,
      payout: null,
      detail: {
        verified: true,
        t: 995,
        escrow_before: '50000000000',
        ...(soldOut ? {} : { escrow_after: String(50_000_000_000n - cost - 1_000_000_000n - surplus) }),
        delivery_value: String(1_000_000_000n + surplus + (soldOut ? 50_000_000_000n - cost - 1_000_000_000n - surplus : 0n)),
      },
    });
  const ok = checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, ev(allIn));
  assert.equal(ok.violations.length, 0);
  assert.equal(ok.payoutChecked, true);
  const over = checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, ev(allIn + 1n));
  assert.equal(over.violations.length, 1);
  assert.match(String(over.violations[0].detail.problem), /cost more/);
  // sold out: no continuation, everything but the cost goes to the delivery output
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, ev(allIn, true)).violations.length, 0);
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, ev(allIn + 1n, true)).violations.length, 1);
  // an event from before the indexer reported the delivery output: not checked
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, fill({ side: 2, payout: null, detail: { verified: true, t: 995, escrow_before: '1' } })).payoutChecked, false);
});

// fee rule with a folded change: kob-protocol tx.rs Draft::seal keeps a change only if the transaction with it stays within the block limits;
// a change alone below ~C/500001 = 1_999_996 sompi (storage mass C/c) is folded into the fee (the fee is the floor + the change + the change output's fee)
const feeBase = { txid: 'aa', feeRate: '100', feeMode: 'relay' } as const;
// compute-bound amend (the mm bot's): floor 471900; the change output adds 412 compute = 41200 sompi, so the largest fold is 1_999_996 + 41_200
const mmLine = { ...feeBase, minFee: '471900', compute: 4719, transientNormalized: 4698, storage: 0, changeOutput: null };
// transient-bound amend (t1/t4/mm): floor 1929800; the change output adds 104 normalized transient = 10400 sompi
const trLine = { ...feeBase, minFee: '1929800', compute: 12789, transientNormalized: 19298, storage: 0, changeOutput: null };

test('fee: exactly the floor is fine', () => {
  assert.equal(checkFeeLine({ ...mmLine, fee: '471900' }), null);
  assert.equal(checkFeeLine({ ...trLine, fee: '1929800' }), null);
  assert.equal(checkFeeLine({ ...trLine, fee: '1929800', changeOutput: 1 }), null);
});

test('fee: the fold bound is floor(C / 500001) + the change output fee, derived from the builder rule', () => {
  assert.equal(maxFoldedExcess(mmLine), 1_999_996n + 41_200n);
  assert.equal(maxFoldedExcess(trLine), 1_999_996n + 10_400n);
  // a transaction that already carries storage mass leaves less room for the change: c <= C / (500001 - storage)
  assert.equal(maxFoldedExcess({ ...trLine, storage: 400_000 }), 1_000_000_000_000n / 100_001n + 10_400n);
  // the no-change transaction itself over the storage limit: no bound
  assert.equal(maxFoldedExcess({ ...trLine, storage: 500_001 }), null);
});

test('fee: a folded excess up to the bound is accepted (info-level result), one above it is an error', () => {
  // the live incidents: mm amend fee 2508700 vs 471900 (excess 2036800), t2 amend 2756500 vs 1929800 (excess 826700)
  const live = checkFeeLine({ ...mmLine, fee: '2508700' });
  assert.ok(live && typeof live === 'object' && 'kind' in live);
  assert.deepEqual(live, { kind: 'folded', txid: 'aa', bot: undefined, what: undefined, fee: '2508700', expected: '471900', excess: '2036800', maxFold: '2041196' });
  const t2 = checkFeeLine({ ...trLine, fee: '2756500' });
  assert.ok(t2 && typeof t2 === 'object' && 'kind' in t2 && t2.excess === '826700');
  // the smallest excess (1 sompi: a change too small to even pay for its own output), and exactly the bound
  assert.ok(checkFeeLine({ ...mmLine, fee: String(471900n + 1n) }) && 'kind' in (checkFeeLine({ ...mmLine, fee: String(471900n + 1n) }) as object));
  const edge = checkFeeLine({ ...mmLine, fee: String(471900n + 2_041_196n) });
  assert.ok(edge && typeof edge === 'object' && 'kind' in edge);
  // one sompi above the bound is an overpayment
  const over = checkFeeLine({ ...mmLine, fee: String(471900n + 2_041_197n) });
  assert.ok(over && typeof over === 'object' && 'invariant' in over);
  assert.equal(over.invariant, 'fee');
  assert.equal(over.severity, 'error');
  assert.equal((over.detail as { excess: string }).excess, '2041197');
  // a large overpayment
  const big = checkFeeLine({ ...trLine, fee: String(1929800n + 5_000_000n) });
  assert.ok(big && typeof big === 'object' && 'invariant' in big);
});

test('fee: below the floor is always an error, with or without a change', () => {
  for (const changeOutput of [null, 0, 2]) {
    const r = checkFeeLine({ ...trLine, fee: '1929799', changeOutput });
    assert.ok(r && typeof r === 'object' && 'invariant' in r, `changeOutput ${changeOutput}`);
    assert.equal(r.severity, 'error');
  }
  assert.ok('invariant' in (checkFeeLine({ ...mmLine, fee: '0' }) as object));
});

test('fee: with a change output present or unknown, any excess is an error', () => {
  for (const excess of [1n, 826_700n, 2_036_800n]) {
    const withChange = checkFeeLine({ ...trLine, fee: String(1929800n + excess), changeOutput: 2 });
    assert.ok(withChange && typeof withChange === 'object' && 'invariant' in withChange);
    assert.equal(withChange.severity, 'error');
    // an old line without the changeOutput field cannot be told from an overpayment
    const { changeOutput: _c, ...old } = trLine;
    const legacy = checkFeeLine({ ...old, fee: String(1929800n + excess) });
    assert.ok(legacy && typeof legacy === 'object' && 'invariant' in legacy);
  }
});

test('fee: a folded fee still needs the builder minFee to equal the floor and feeRate 100', () => {
  const r1 = checkFeeLine({ ...mmLine, fee: '2508700', minFee: '471800' });
  assert.ok(r1 && typeof r1 === 'object' && 'invariant' in r1);
  const r2 = checkFeeLine({ ...mmLine, fee: '2508700', feeRate: '101' });
  assert.ok(r2 && typeof r2 === 'object' && 'invariant' in r2);
  // modes other than relay are not checked
  assert.equal(checkFeeLine({ ...mmLine, fee: '2508700', feeMode: 'priority' }), 'unchecked');
});

test('fee: every folded transaction of the live soak (12 lines) is explained', () => {
  const lines: [string, number, number, string][] = [
    // [fee, compute, transientNormalized, floor]
    ['3859900', 12789, 19298, '1929800'],
    ['2669400', 12797, 19314, '1931400'],
    ['2283400', 4719, 4698, '471900'],
    ['3285100', 12789, 19298, '1929800'],
    ['2377000', 4719, 4698, '471900'],
    ['3757500', 12789, 19298, '1929800'],
    ['3779400', 12789, 19298, '1929800'],
    ['2508700', 4719, 4698, '471900'],
    ['3333100', 12789, 19298, '1929800'],
    ['2388200', 4719, 4698, '471900'],
    ['2277200', 4719, 4698, '471900'],
    ['2162500', 4719, 4698, '471900'],
  ];
  for (const [fee, compute, transientNormalized, minFee] of lines) {
    const r = checkFeeLine({ ...feeBase, fee, minFee, compute, transientNormalized, storage: 0, changeOutput: null });
    assert.ok(r && typeof r === 'object' && 'kind' in r, `${fee}`);
  }
});

test('evidence of a terminated order: partial terms carry the token and activation; the exposure floor still catches a short rest', () => {
  const t = partialTerms({ price: '2314350000', tip: '0', scale: U, budget_rate: '2314350000', tif: 1, token: TOKEN, active_from: 0 })!;
  assert.equal(t.tokenCovId, TOKEN);
  assert.equal(t.activeFrom, '0');
  const bidStop = { ...condAsk, stopPrice: '2310090000', minRestDaa: '50', minTouch: '1' };
  const bid = (o: Partial<EvidenceFact> = {}) => evd({ contract: 'KobBid', side: 2, price: '2314350000', amount: '1', tokenCovId: t.tokenCovId, slope: null, exposedSince: null, ...o });
  const armAt = fill({ kind: 'arm', daa: 584755879, amount: null, price: null, payout: null });
  // the 2026-10-01 case: an IOC bid resting 69 DAA, filled next to the arm, seen by the checker only after it closed -> unverifiable, a warning
  const r = checkTrigger({ covenant_id: ID, contract: 'KobCondBid' }, bidStop, armAt, [evd({ price: '2312040000', exposedSince: '584755325' }), bid({ exposedFloor: 584755810 })]);
  assert.equal(r.violations[0].severity, 'warn');
  // rested less than minRestDaa even from the earliest possible start: an error
  const short = checkTrigger({ covenant_id: ID, contract: 'KobCondBid' }, bidStop, armAt, [bid({ exposedFloor: 584755850 })]);
  assert.equal(short.violations[0].severity, 'error');
  assert.match(short.reasons.join(';'), /at the earliest/);
});

test('a stop filled at its trigger is checked like an arm: evidence side and price; a trailed stop read late is only a warning', () => {
  const trig = fill({ kind: 'fill', daa: 2000, amount: String(U), price: '1800000000', payout: null });
  assert.ok(checkTrigger(CA, condAsk, trig, [evd()]).ok);
  assert.equal(checkTrigger(CA, condAsk, trig, [evd({ price: '1800000001' })]).violations[0].severity, 'error');
  assert.equal(checkTrigger(CA, condAsk, trig, [evd({ contract: 'KobBid', side: 2 })]).violations[0].severity, 'error');
  assert.equal(checkTrigger(CA, condAsk, trig, [evd({ price: '1800000001' })], { stopUnknown: true }).violations[0].severity, 'warn');
  assert.equal(checkTrigger(CA, condAsk, trig, []).violations[0].severity, 'error');
});

// ------------------------------------------------------------------------------------------------ fees: the dynamic fee policy (fee == recorded rate x mass, rate within [floor, maxRate])

const dyn = (rate: number, over: Record<string, unknown> = {}) => ({
  txid: 'd1', feeRate: String(rate), feeMode: 'relay', compute: 20000, transientNormalized: 25000, storage: 90000,
  fee: String(rate * 25000), minFee: String(rate * 25000), ...over,
}) as Parameters<typeof checkFeeLine>[0];

test('fee: a dynamic rate is not an incident when fee == rate x mass, for every rate in [100, maxRate]', () => {
  for (const rate of [100, 101, 140, 194, 301, 999, 1000]) assert.equal(checkFeeLine(dyn(rate)), null, `rate ${rate}`);
  // the mass is the larger of compute and the normalized transient mass, as before
  assert.equal(checkFeeLine(dyn(150, { compute: 30000, transientNormalized: 25000, fee: String(150 * 30000), minFee: String(150 * 30000) })), null);
});

test('fee: a rate outside [floor, maxRate] is an incident even when fee == rate x mass', () => {
  for (const rate of [99, 50, 1001, 5000]) {
    const r = checkFeeLine(dyn(rate));
    assert.ok(r && typeof r === 'object' && 'invariant' in r, `rate ${rate}`);
    assert.equal(r.severity, 'error');
    assert.match(String((r.detail as { problems: string[] }).problems[0]), /outside \[100, 1000\]/);
  }
  // the bounds are the configured ones
  const tight = { floor: 100n, maxRate: 300n };
  assert.equal(checkFeeLine(dyn(300), tight), null);
  const over = checkFeeLine(dyn(301), tight);
  assert.ok(over && typeof over === 'object' && 'invariant' in over);
  // a maxRate below the floor means the floor
  assert.equal(checkFeeLine(dyn(100), { floor: 100n, maxRate: 50n }), null);
  assert.ok(checkFeeLine(dyn(101), { floor: 100n, maxRate: 50n }));
  // a garbage rate is an incident, not a crash
  assert.ok(checkFeeLine(dyn(150, { feeRate: 'abc' })));
});

test('fee: a fee that is not its recorded rate x mass is an incident, above or below', () => {
  for (const delta of [-1, 1, 25000, -25000]) {
    const r = checkFeeLine(dyn(150, { fee: String(150 * 25000 + delta) }));
    assert.ok(r && typeof r === 'object' && 'invariant' in r, `delta ${delta}`);
    assert.equal(r.invariant, 'fee');
  }
  // the fee of a DIFFERENT rate than the one recorded (rate 150 recorded, 200 paid)
  assert.ok(checkFeeLine(dyn(150, { fee: String(200 * 25000), minFee: String(150 * 25000) })));
  // the builder's own target must agree with the recorded rate
  const r = checkFeeLine(dyn(150, { minFee: String(100 * 25000) }));
  assert.ok(r && typeof r === 'object' && 'invariant' in r);
  assert.match(String((r.detail as { problems: string[] }).problems.join(' ')), /builder minFee/);
  assert.equal(checkFeeLine(dyn(150, { feeMode: 'priority' })), 'unchecked');
});

test('fee: the change fold bound scales with the recorded rate', () => {
  const mm = { ...dyn(150), compute: 4719, transientNormalized: 4698, storage: 0, minFee: String(150 * 4719), changeOutput: null };
  // the change output adds 412 compute mass: 150 sompi per gram of it
  assert.equal(maxFoldedExcess(mm, 150n), 1_999_996n + 150n * 412n);
  assert.equal(maxFoldedExcess(mm), 1_999_996n + 100n * 412n, 'the default rate is the floor');
  const expected = 150n * 4719n;
  const folded = checkFeeLine({ ...mm, fee: String(expected + 2_000_000n) });
  assert.ok(folded && typeof folded === 'object' && 'kind' in folded);
  assert.equal(folded.expected, String(expected));
  assert.equal(folded.maxFold, String(1_999_996n + 61_800n));
  const edge = checkFeeLine({ ...mm, fee: String(expected + 1_999_996n + 61_800n) });
  assert.ok(edge && typeof edge === 'object' && 'kind' in edge);
  const over = checkFeeLine({ ...mm, fee: String(expected + 1_999_996n + 61_800n + 1n) });
  assert.ok(over && typeof over === 'object' && 'invariant' in over);
  // an excess with a change output present is still an incident at any rate
  const withChange = checkFeeLine({ ...mm, changeOutput: 1, fee: String(expected + 1n) });
  assert.ok(withChange && typeof withChange === 'object' && 'invariant' in withChange);
  // a folded fee at a rate outside the bounds is not explained by the fold
  const out = checkFeeLine({ ...mm, feeRate: '1500', minFee: String(1500 * 4719), fee: String(1500 * 4719 + 10) });
  assert.ok(out && typeof out === 'object' && 'invariant' in out);
});

// ------------------------------------------------------------------------------------------------ amounts, prices and rounding (protocol v3)

test('quoteOf rounds as the covenants do: down for what a maker pays, up for what a maker must receive', () => {
  assert.equal(quoteOf(3n, 2500n, 1000n, 'down'), 7n);
  assert.equal(quoteOf(3n, 2500n, 1000n, 'up'), 8n);
  assert.equal(quoteOf(4n, 2500n, 1000n, 'down'), 10n);
  assert.equal(quoteOf(4n, 2500n, 1000n, 'up'), 10n, 'an exact value rounds to itself');
  assert.equal(quoteOf(0n, 2500n, 1000n, 'up'), 0n);
  assert.equal(quoteOf(1n, 1n, 1_000_000_000n, 'up'), 1n);
  assert.equal(quoteOf(1n, 1n, 1_000_000_000n, 'down'), 0n);
  // beyond 64 bits: exact (BigInt), no wrap
  assert.equal(quoteOf(2n ** 62n, 2n ** 62n, 1_000_000_000n, 'down'), (2n ** 124n) / 1_000_000_000n);
  assert.equal(quoteOf(-1n, 1n, 1n, 'up'), null);
  assert.equal(quoteOf(1n, 1n, 0n, 'up'), null);
});

test('a seller must receive the rounded-UP value of the filled amount: one sompi below is a violation', () => {
  // scale 1000, price 2500 per whole token, tip 100: 3 base units are worth 3 * 2400 / 1000 = 7.2 -> 8 sompi
  const s = { scale: '1000', minFill: '1', price: '2500', tip: '100', slope: '0', priceEnd: '0', amountLeft: '10', tif: '0' };
  const ev = (payout: string) => fill({ amount: '3', price: '2500', payout, detail: { verified: true, t: 1 } });
  assert.deepEqual(checkFill({ covenant_id: ID, contract: 'KobAsk' }, s, ev('8')).violations, []);
  const r = checkFill({ covenant_id: ID, contract: 'KobAsk' }, s, ev('7'));
  assert.equal(r.violations.length, 1, 'a floor-based rule would accept 7');
  assert.equal(r.violations[0].detail.required, '8');
  assert.equal(r.violations[0].detail.short, '1');
  // no tip: 3 * 2500 / 1000 = 7.5 -> 8
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobAsk' }, { ...s, tip: '0' }, ev('7')).violations.length, 1);
  assert.deepEqual(checkFill({ covenant_id: ID, contract: 'KobAsk' }, { ...s, tip: '0' }, ev('8')).violations, []);
});

test('a buyer pays at most the rounded-DOWN value: one sompi above is a violation', () => {
  // scale 1000, price 2500, tip 100: 3 base units cost at most floor(3 * 2600 / 1000) = 7 sompi
  const bid = { scale: '1000', minFill: '1', price: '2500', tip: '100', slope: '0', priceEnd: '0', tif: '0' };
  const ev = (cost: bigint) => fill({ side: 2, amount: '3', price: '2500', payout: null, detail: { verified: true, t: 1, escrow_before: '1000', escrow_after: String(1000n - cost - 50n), delivery_value: '50' } });
  assert.deepEqual(checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, ev(7n)).violations, []);
  const over = checkFill({ covenant_id: ID, contract: 'KobBid' }, bid, ev(8n));
  assert.equal(over.violations.length, 1);
  assert.equal(over.violations[0].detail.cap, '7');
});

test('a merged take-profit pays the maker the rounded-up proceeds minus the rounded-up merge return', () => {
  // scale 1000, 3 base units: proceeds ceil(3 * 2100 / 1000) = 7, merge return ceil(3 * 2000 / 1000) = 6: the maker keeps at least 1
  const exit = { scale: '1000', minFill: '1', tip: '0', tpPrice: '2100', stopPrice: '0', slipBps: '0', trailStep: '0', rptPrice: '2000', amountLeft: '3', tif: '0' };
  const ev = (payout: string) => fill({ amount: '3', price: '2100', payout, detail: { verified: true, t: 1, merged_into: 'dd'.repeat(32) } });
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobCondAsk' }, exit, ev('1')).violations.length, 0);
  assert.equal(checkFill({ covenant_id: ID, contract: 'KobCondAsk' }, exit, ev('0')).violations.length, 1);
});

test('repeat cycle at a small scale: profit = ceil(m * (tp - tip) / scale) - ceil(m * rptPrice / scale)', () => {
  const e = { covenant_id: ID, contract: 'KobIfdBid', side: 2, status: 'open', last_daa: 1, state: { kind: 'KobIfdBid', state: { amountLeft: '0', scale: '1000', price: '1900', tip: '100', rptAmount: '9' } } } as OrderLike;
  const x = { covenant_id: 'e1', contract: 'KobCondAsk', side: 1, status: 'open', last_daa: 1, state: { kind: 'KobCondAsk', state: { amountLeft: '3', parent: ID, scale: '1000', tpPrice: '2100', tip: '0', rptPrice: '2000' } } } as OrderLike;
  const f = (payout: string) => fill({ covenant_id: 'e1', amount: '3', price: '2100', payout });
  assert.deepEqual(checkRepeatCycle(e, x, f('1')), []);
  assert.equal(checkRepeatCycle(e, x, f('0')).length, 1);
  // an exit booked with another budget rate than price + tip of the entry
  const bad = { ...x, state: { kind: 'KobCondAsk', state: { ...x.state!.state, rptPrice: '1900' } } } as OrderLike;
  assert.ok(checkRepeatCycle(e, bad, f('9')).some((v) => v.subject.endsWith(':rptPrice')));
});

test('sell-first repeat: the exit carries rptPrice = price - tip and rptPre = prefund of the entry', () => {
  const e = { covenant_id: ID, contract: 'KobIfdAsk', side: 1, status: 'open', last_daa: 1, state: { kind: 'KobIfdAsk', state: { amountLeft: '0', scale: '1000', price: '2100', tip: '100', prefund: '50' } } } as OrderLike;
  const mk = (rptPrice: string, rptPre: string) => ({ covenant_id: 'e1', contract: 'KobCondBid', side: 2, status: 'open', last_daa: 1, state: { kind: 'KobCondBid', state: { amountLeft: '3', parent: ID, scale: '1000', rptPrice, rptPre } } }) as OrderLike;
  const f = fill({ covenant_id: 'e1', amount: '3', price: '2000', payout: null });
  assert.deepEqual(checkRepeatCycle(e, mk('2000', '50'), f), []);
  assert.equal(checkRepeatCycle(e, mk('2100', '50'), f).length, 1);
  assert.equal(checkRepeatCycle(e, mk('2000', '49'), f).length, 1);
});

test('a state of an older layout (no scale) is not checkable: no payout verdict, no crash', () => {
  const old = { price: '2000000000', tipLot: '0', lotUnits: '1', unit: '100000000', slope: '0', priceEnd: '0' };
  const r = checkFill({ covenant_id: ID, contract: 'KobAsk' }, old, fill());
  assert.deepEqual(r.violations, []);
  assert.equal(r.payoutChecked, false);
});

test('amount accounting: filled + left == initial for a live plain ask or KobPair, to the last base unit', () => {
  const o = (initial: string, filled: string, left: string | null, contract = 'KobAsk', status = 'partial'): OrderLike => ({ covenant_id: ID, contract, side: 1, status, last_daa: 1, initial_amount: initial, filled_amount: filled, amount_left: left });
  assert.equal(checkAmountBalance(o('300000000', '100000000', '200000000')), null);
  assert.equal(checkAmountBalance(o('300000000', '100000000', '200000000', 'KobPair')), null);
  assert.equal(checkAmountBalance(o('300000000', '100000000', '200000000', 'KobAskKron')), null);
  const bad = checkAmountBalance(o('300000000', '100000000', '199999999'));
  assert.equal(bad?.severity, 'error');
  assert.equal(bad?.detail.delta, '-1');
  // a closed order, a bid, a repeat entry and a view without the numbers are not checked
  assert.equal(checkAmountBalance(o('300000000', '300000000', null, 'KobAsk', 'filled')), null);
  assert.equal(checkAmountBalance(o('300000000', '1', '1', 'KobIfdAsk')), null);
  assert.equal(checkAmountBalance(o('300000000', '1', '1', 'KobBid')), null);
  assert.equal(checkAmountBalance({ ...o('1', '1', '1'), initial_amount: null }), null);
});
