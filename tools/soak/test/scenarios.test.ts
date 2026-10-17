// The trader scenarios of the 2026-10-13 gap review: the pure action builders (bots/scenario-math.ts), the x402 retry turns and the merchant's
// faults (bots/x402-retry.ts), the external tokens' registry entries (KRON), and the checker's scenario invariants (checker/scenario-rules.ts).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { datedPairLimit, externalReference, fixedKasRef, ladderLevels, ladderSetting, LADDER_MAX_LEVELS, LADDER_MIN_LEVELS, mergeTotal, oldestFirst, pairAmendPrice, rungSize } from '../src/bots/scenario-math.ts';
import { ConflictRace, DEFAULT_X402_WEIGHTS, FaultInjector, RETRY_BASE, x402Weights } from '../src/bots/x402-retry.ts';
import { DEFAULT_PAIR_WEIGHTS, DEFAULT_WEIGHTS } from '../src/bots/weights.ts';
import { externalRegistryToken, soakTemplates } from '../src/registry-templates.ts';
import {
  amendedAfter, checkCancelAll, checkLadder, checkLadderCancel, checkMerge, checkMergeRecord, checkPlannedReplacement, checkReplacement, checkX402Retries, repricedRungs,
  SCENARIO_GRACE_MS,
} from '../src/checker/scenario-rules.ts';
import type { CancelAllRecord, LadderCancelRecord, LadderRecord, MergeRecord, ReplaceRecord } from '../src/scenarios.ts';
import type { EventLike, OrderLike } from '../src/checker/types.ts';

/** a deterministic stream of draws in [0, 1) */
const seq = (...xs: number[]) => {
  let i = 0;
  return () => xs[i++ % xs.length]!;
};

// ------------------------------------------------------------------------------------------------ action builders

test('ladder levels stay within the ticket bounds (2..20), short ladders most of the time', () => {
  for (let i = 0; i < 2000; i++) {
    const n = ladderLevels();
    assert.ok(n >= LADDER_MIN_LEVELS && n <= LADDER_MAX_LEVELS, String(n));
  }
  assert.equal(ladderLevels(seq(0.1, 0)), 2);
  assert.equal(ladderLevels(seq(0.1, 0.999)), 6);
  assert.equal(ladderLevels(seq(0.9, 0)), 7);
  assert.equal(ladderLevels(seq(0.9, 0.999)), 20);
});

test('a ladder setting is a percent step or an end price behind the start (a buy ends below, a sell above)', () => {
  const pct = ladderSetting('buy', 1_000_000n, 5, seq(0.2, 0.5));
  assert.equal(pct.mode, 'pct');
  assert.ok(pct.mode === 'pct' && pct.bps >= 3n && pct.bps <= 40n);
  const endBuy = ladderSetting('buy', 1_000_000n, 5, seq(0.7, 0));
  assert.deepEqual(endBuy, { levels: 5, mode: 'end', end: 1_000_000n - 3_000n });
  const endSell = ladderSetting('sell', 1_000_000n, 5, seq(0.7, 0.9999999));
  assert.ok(endSell.mode === 'end' && endSell.end > 1_000_000n && endSell.end <= 1_030_000n);
});

test('every rung of a ladder has one size: the budget split evenly, never below the minimum', () => {
  assert.equal(rungSize(1_000n, 4, 10n), 250n);
  assert.equal(rungSize(1_000n, 20, 100n), 100n);
  assert.equal(rungSize(7n, 2, 1n), 3n);
});

test('the renewal candidate is the oldest order', () => {
  assert.equal(oldestFirst([]), null);
  const a = { id: 'a', genesis: { daa: 30 } };
  const b = { id: 'b', genesis: { daa: 10 } };
  assert.equal(oldestFirst([a, b, { id: 'c', genesis: { daa: 20 } }]), b);
});

test('dated pair limits: GTD 30..120 min, a day order, a timed activation 2..6 min ahead with an expiry after it', () => {
  const now = 1_800_000_000n;
  const gtd = datedPairLimit('gtd', 'sell', 5n, 77n, now, seq(0));
  assert.deepEqual(gtd, { type: 'limit', side: 'sell', amount: 5n, price: 77n, lifetime: { kind: 'gtd', at: now + 1800n } });
  assert.equal((datedPairLimit('gtd', 'buy', 5n, 77n, now, seq(0.9999999)).lifetime as { at: bigint }).at, now + 7200n);
  assert.deepEqual(datedPairLimit('day', 'buy', 5n, 77n, now).lifetime, { kind: 'day' });
  const timed = datedPairLimit('timed', 'buy', 5n, 77n, now, seq(0.5, 0.5));
  assert.equal(timed.activeFrom!.unixSeconds, now + 240n);
  assert.equal((timed.lifetime as { at: bigint }).at, now + 240n + 3600n);
});

test('a pair amend moves the limit away from the market by 5..25 bps, at least one unit', () => {
  assert.equal(pairAmendPrice('sell', 1_000_000n, seq(0)), 1_000_500n);
  assert.equal(pairAmendPrice('buy', 1_000_000n, seq(0.9999999)), 997_500n);
  assert.equal(pairAmendPrice('sell', 10n, seq(0)), 11n);
  assert.equal(pairAmendPrice('buy', 1n, seq(0)), 1n);
});

test('an external token is priced at its book midpoint when near its configured reference, else at the reference', () => {
  assert.equal(externalReference({ bid: null, ask: null }, 1000n, 2000), 1000n);
  assert.equal(externalReference({ bid: 1050n, ask: 1150n }, 1000n, 2000), 1100n);
  assert.equal(externalReference({ bid: 1500n, ask: 1700n }, 1000n, 2000), 1000n);
  assert.equal(externalReference({ bid: 900n, ask: null }, 1000n, 2000), 900n);
  const fx = fixedKasRef({ kasUsd: 0.05, ts: 1 }, 5_000_000n)!;
  assert.deepEqual([fx.kasUsd, fx.perToken, fx.ts], [0.05, 5_000_000n, 1]);
  assert.ok(Math.abs(fx.usd - 0.0025) < 1e-12);
  assert.equal(fixedKasRef(null, 5_000_000n), null);
  assert.equal(fixedKasRef({ kasUsd: 0.05, ts: 1 }, 0n), null);
});

test('the trader weights carry the new scenarios (KAS book and pair)', () => {
  for (const k of ['ladder', 'ladderCancel', 'renew', 'mergeTokens']) assert.ok((DEFAULT_WEIGHTS[k] ?? 0) > 0, k);
  for (const k of ['gtd', 'day', 'timed', 'amend', 'cancelAll']) assert.ok((DEFAULT_PAIR_WEIGHTS[k] ?? 0) > 0, k);
});

test('mergeTotal sums the inputs', () => {
  assert.equal(mergeTotal([{ amount: '1' }, { amount: '20' }, { amount: '300' }]), 321n);
});

// ------------------------------------------------------------------------------------------------ x402 retry turns and faults

test('x402 turn weights: defaults, overrides, invoice only when enabled, 0 switches a kind off', () => {
  const w = x402Weights({ invoices: true });
  assert.deepEqual(Object.keys(w).sort(), Object.keys(DEFAULT_X402_WEIGHTS).sort());
  assert.ok(!('invoice' in x402Weights({ invoices: false })));
  const o = x402Weights({ invoices: false, weights: { '/retry/lost': 5, '/swap': 0, '/bogus': 3 } });
  assert.equal(o['/retry/lost'], 5);
  assert.ok(!('/swap' in o) && !('/bogus' in o));
  for (const p of Object.keys(RETRY_BASE)) assert.ok(['/native', '/token', '/swap'].includes(RETRY_BASE[p]!));
});

test('the merchant faults the FIRST paid request of a payment on a fault path only', () => {
  const f = new FaultInjector();
  assert.equal(f.decide('/retry/drop', null), 'none', 'the unpaid request (402) goes through');
  assert.equal(f.decide('/retry/drop', 'sig-1'), 'drop');
  assert.equal(f.decide('/retry/drop', 'sig-1'), 'none', 'the re-send of the same artifact goes through');
  assert.equal(f.decide('/retry/lost', 'sig-2'), 'lose');
  assert.equal(f.decide('/retry/lost', 'sig-2'), 'none');
  assert.equal(f.decide('/native', 'sig-3'), 'none');
  assert.equal(f.decide('/retry/conflict', 'sig-4'), 'none', 'the conflict path is driven by the quote, not the merchant');
  const small = new FaultInjector(undefined, 2);
  small.decide('/retry/drop', 'a');
  small.decide('/retry/drop', 'b');
  small.decide('/retry/drop', 'c');
  assert.equal(small.decide('/retry/drop', 'a'), 'drop', 'the oldest signature is forgotten past the bound');
});

test('the conflict race is armed for one quote only', () => {
  const r = new ConflictRace();
  assert.equal(r.take(), false);
  r.arm();
  assert.equal(r.take(), true);
  assert.equal(r.take(), false, 'the rebuild quotes without racing');
  r.arm();
  r.disarm();
  assert.equal(r.take(), false);
});

// ------------------------------------------------------------------------------------------------ external tokens (KRON)

test('an external KRON token is listed on its KRON program, without extension commitment', () => {
  const kron = { ticker: 'TKRON', name: 'TN10 KRON test', covenantId: 'ab'.repeat(32), program: 'KronToken2433', decimals: 8, trade: { bookShare: 0.05, refKas: '0.1' } } as const;
  const e = externalRegistryToken(kron);
  assert.deepEqual([e.family, e.template_id, e.extension_commitment, e.extension_class, e.max_token_inputs, e.max_token_outputs], ['kron', 'kron-2433', null, 'none', 4, 5]);
  assert.deepEqual(soakTemplates([kron]).map((t) => (t as { id: string }).id), ['kcc20-ref-3x3', 'kron-2433']);
  assert.deepEqual(soakTemplates([{ ...kron, program: 'KronToken2732' }]).map((t) => (t as { id: string }).id), ['kcc20-ref-3x3', 'kron-2732']);
  assert.throws(() => externalRegistryToken({ ...kron, extensionCommitment: '11'.repeat(32) }), /no extension commitment/);
});

// ------------------------------------------------------------------------------------------------ checker: replace (renew, pair amend)

const OLD = 'aa'.repeat(32);
const NEW = 'bb'.repeat(32);
const TX = 'cc'.repeat(32);
const ask = { maker: 'm', tokenCovId: 't', scale: '100000000', minFill: '10', price: '500', tip: '2', tif: '0', activeFrom: '0', expiryDaa: '1000', amountLeft: '300' };
const renewRec = (over: Partial<ReplaceRecord> = {}): ReplaceRecord => ({
  kind: 'renew', ts: 0, bot: 't1', token: 't', old: { id: OLD, spends: `${'dd'.repeat(32)}:0` }, oldExpiryDaa: '1000', id: OLD, inPlace: true, txid: TX,
  before: ask, expect: { ...ask, expiryDaa: '9000' }, ...over,
});
const order = (over: Partial<OrderLike> = {}): OrderLike => ({ covenant_id: OLD, contract: 'KobAsk', side: 1, status: 'open', last_daa: 1, genesis: { txid: 'ee'.repeat(32), daa: 1 }, ...over });
const ev = (id: number, kind: string, txid: string, cov = OLD): EventLike => ({ id, covenant_id: cov, daa: id, txid, kind, side: null, amount: null, price: null, payout: null, closes: false });

test('a renewal plan keeps every term and only extends the expiry', () => {
  assert.equal(checkPlannedReplacement(renewRec()), null);
  const moved = checkPlannedReplacement(renewRec({ expect: { ...ask, expiryDaa: '9000', price: '501' } }));
  assert.equal(moved?.invariant, 'replace');
  assert.deepEqual((moved?.detail as { changed: string[] }).changed, ['price']);
  assert.match(String(checkPlannedReplacement(renewRec({ expect: { ...ask, expiryDaa: '1000' } }))?.detail.problem), /does not extend/);
});

test('a renewal of a partly filled order fits a minimum fill above what is left down to what is left: accepted (TN10 2026-10-16)', () => {
  // TN10 1dba094b...: a conditional order with minFill 442 and 2 base units left renewed with minFill 2 (the same fills: only the whole rest)
  const left = { ...ask, minFill: '442', amountLeft: '2' };
  assert.equal(checkPlannedReplacement(renewRec({ before: left, expect: { ...left, minFill: '2', expiryDaa: '9000' } })), null);
  // down to anything but what is left, or a minimum fill at or below what is left that changes, is still a changed term
  for (const [b, e] of [[left, '1'], [left, '3'], [{ ...ask, minFill: '10' }, '9'], [{ ...ask, minFill: '300' }, '299']] as const) {
    const v = checkPlannedReplacement(renewRec({ before: b, expect: { ...b, minFill: e, expiryDaa: '9000' } }));
    assert.deepEqual((v?.detail as { changed: string[] } | undefined)?.changed, ['minFill'], `${b.minFill} -> ${e}`);
  }
  // a plain bid (no amountLeft: its quantity is its escrow) keeps its minimum fill
  const { amountLeft: _, ...bid } = ask;
  const v = checkPlannedReplacement(renewRec({ before: bid, expect: { ...bid, minFill: '1', expiryDaa: '9000' } }));
  assert.deepEqual((v?.detail as { changed: string[] } | undefined)?.changed, ['minFill']);
});

test('a pair amend plan keeps pair, side, size, tip and minimum fill', () => {
  const pair = { maker: 'm', side: '1', sCovId: 'a', tCovId: 'b', sScale: '100', tScale: '100', tip: '3', tif: '0', minFill: '5', amountLeft: '50', price: '1000', custody: '50', expiryDaa: '10' };
  const rec = renewRec({ kind: 'pairAmend', inPlace: false, id: NEW, before: pair, expect: { ...pair, price: '1010', expiryDaa: '12' } });
  assert.equal(checkPlannedReplacement(rec), null);
  const v = checkPlannedReplacement({ ...rec, expect: { ...rec.expect, tCovId: 'x', amountLeft: '49' } });
  assert.deepEqual((v?.detail as { changed: string[] }).changed, ['tCovId', 'amountLeft']);
});

test('a pair amend whose new price crosses the market is planned as a crossing auction: accepted (TN10 2026-10-14 B-10)', () => {
  const pair = { maker: 'm', side: '2', sCovId: 'a', tCovId: 'b', sScale: '100', tScale: '100', tip: '3', tif: '0', minFill: '5', amountLeft: '50', price: '1000', slope: '0', priceEnd: '0', activeFrom: '0', interval: '0', maxFill: '0', expiryDaa: '10' };
  const auction = { ...pair, price: '990', slope: '85571259', priceEnd: '1010', activeFrom: '595000000', expiryDaa: '12' };
  const rec = renewRec({ kind: 'pairAmend', inPlace: false, id: NEW, before: pair, expect: auction });
  assert.equal(checkPlannedReplacement(rec), null);
  // a slope without an end price is no crossing auction; an order with a schedule must keep it; an auction that changes another kept term is still caught
  assert.deepEqual((checkPlannedReplacement({ ...rec, expect: { ...auction, priceEnd: '0' } })?.detail as { changed: string[] }).changed, ['slope']);
  assert.deepEqual((checkPlannedReplacement({ ...rec, expect: { ...auction, interval: '600' } })?.detail as { changed: string[] }).changed, ['interval']);
  assert.deepEqual((checkPlannedReplacement({ ...rec, expect: { ...auction, tip: '4' } })?.detail as { changed: string[] }).changed, ['tip']);
  // an order that was already an auction keeps its slope
  const dutch = { ...pair, slope: '7', priceEnd: '900' };
  assert.deepEqual((checkPlannedReplacement({ ...rec, before: dutch, expect: { ...dutch, slope: '8' } })?.detail as { changed: string[] }).changed, ['slope']);
});

test('the renewal on chain: planned terms (fills aside), superseded by a later amend, not landed, pending', () => {
  const rec = renewRec();
  const st = (s: Record<string, string>) => ({ kind: 'KobAsk', state: s });
  const landed = [ev(1, 'genesis', 'ee'.repeat(32)), ev(5, 'amend', TX)];
  assert.deepEqual(checkReplacement(rec, order({ state: st({ ...rec.expect, amountLeft: '100' }) }), landed, 1000), { done: true, ok: true, violations: [] });
  const off = checkReplacement(rec, order({ state: st({ ...rec.expect, expiryDaa: '8000' }) }), landed, 1000);
  assert.ok(off.done && off.violations[0]?.invariant === 'replace');
  assert.deepEqual(checkReplacement(rec, order({ state: st({ ...rec.expect, price: '600' }) }), [...landed, ev(9, 'amend', 'ff'.repeat(32))], 1000), { done: true, ok: true, violations: [], note: 'superseded' });
  assert.deepEqual(checkReplacement(rec, order(), [ev(1, 'genesis', 'ee'.repeat(32))], 1000), { done: false });
  assert.deepEqual(checkReplacement(rec, order(), [ev(1, 'genesis', 'ee'.repeat(32))], SCENARIO_GRACE_MS + 1), { done: true, ok: false, violations: [], note: 'not landed' });
  assert.deepEqual(checkReplacement(rec, null, null, 5), { done: false });
  // a closed replacement (no state): compared on the view's price / tip / expiry
  assert.ok(checkReplacement(rec, order({ status: 'filled', price: '500', tip: '2', expiry_daa: 9000 }), landed, 1000).done);
  const closedOff = checkReplacement(rec, order({ status: 'filled', price: '500', tip: '2', expiry_daa: 1000 }), landed, 1000);
  assert.ok(closedOff.done && closedOff.violations.length === 1);
});

test('a replacement by a new order is recognised by its genesis transaction', () => {
  const rec = renewRec({ inPlace: false, id: NEW });
  const v = order({ covenant_id: NEW, genesis: { txid: TX, daa: 3 }, state: { kind: 'KobAsk', state: rec.expect } });
  assert.deepEqual(checkReplacement(rec, v, [], 1000), { done: true, ok: true, violations: [] });
});

// ------------------------------------------------------------------------------------------------ checker: ladder

const rung = (i: number) => ({ id: String(i).repeat(64).slice(0, 64), price: String(1000 - i * 10), txid: `${String(i).repeat(64).slice(0, 63)}f` });
const ladderRec = (n: number, side: 'buy' | 'sell' = 'buy'): LadderRecord => ({ kind: 'ladder', ts: 0, bot: 't1', token: 'tok', side, size: '700', levels: n, rungs: Array.from({ length: n }, (_, i) => rung(i + 1)) });
const rungView = (r: { id: string; price: string; txid: string }, over: Partial<OrderLike> = {}): OrderLike =>
  order({ covenant_id: r.id, contract: 'KobBid', side: 2, token: 'tok', price: r.price, genesis: { txid: r.txid, daa: 1 }, ...over });

test('ladder rungs are independent orders of their own transactions, one side, their own prices', () => {
  const rec = ladderRec(4);
  assert.deepEqual(checkLadder(rec, rec.rungs.map((r) => rungView(r)), 1000), { done: true, ok: true, violations: [] });
  assert.deepEqual(checkLadder(rec, [rungView(rec.rungs[0]!), null, null, null], 1000), { done: false });
  const shared = { ...rec, rungs: [rec.rungs[0]!, { ...rec.rungs[1]!, txid: rec.rungs[0]!.txid }] };
  assert.match(String(checkLadder(shared, [null, null], 0).done && (checkLadder(shared, [null, null], 0) as { violations: { detail: { problem: string } }[] }).violations[0]!.detail.problem), /share/);
  const bad = checkLadder(rec, rec.rungs.map((r, i) => rungView(r, i === 2 ? { parent: 'p', price: '1' } : {})), 1000);
  assert.ok(bad.done && bad.violations[0]?.invariant === 'ladder');
  assert.deepEqual((bad as { violations: { detail: { rungs: { level: number; problems: string[] }[] } }[] }).violations[0]!.detail.rungs[0]!.level, 3);
});

test('a rung amended after its placement holds the amend\'s price: not a ladder violation (TN10 2026-10-16)', () => {
  const rec = ladderRec(4);
  const views = rec.rungs.map((r, i) => rungView(r, i === 2 ? { price: '977' } : {}));
  const moved = rec.rungs[2]!;
  assert.deepEqual(repricedRungs(rec, views), [moved.id]);
  assert.deepEqual(repricedRungs(rec, [views[0]!, null, null, null]), []);
  // the rung's events: its creation in its own transaction, then an in-place amend (the trader's Amend of that order)
  const created = [ev(1, 'create', moved.txid, moved.id)];
  assert.equal(amendedAfter(created, moved.txid), false);
  assert.equal(amendedAfter([...created, ev(2, 'amend', 'ab'.repeat(32), moved.id)], moved.txid), true);
  assert.deepEqual(checkLadder(rec, views, 1000, new Set([moved.id])), { done: true, ok: true, violations: [], note: 'amended' });
  // not amended: the price is the ladder's problem; an amended rung is still held to every other term
  const v = checkLadder(rec, views, 1000);
  assert.deepEqual((v as { violations: { detail: { rungs: { problems: string[] }[] } }[] }).violations[0]!.detail.rungs[0]!.problems, [`price 977 != ${moved.price}`]);
  const parented = checkLadder(rec, rec.rungs.map((r, i) => rungView(r, i === 2 ? { price: '977', parent: 'p' } : {})), 1000, new Set([moved.id]));
  assert.deepEqual((parented as { violations: { detail: { rungs: { problems: string[] }[] } }[] }).violations[0]!.detail.rungs[0]!.problems, ['has a parent']);
  // an auctioning rung is held to its end price, from its proven state
  const auction = rungView(moved, { price: '900', state: { kind: 'KobBid', state: { price: '900', slope: '5', priceEnd: moved.price } } });
  assert.deepEqual(repricedRungs(rec, [views[0]!, views[1]!, auction, views[3]!]), []);
});

test('ladder prices step away from the market; sell rungs share the ladder size', () => {
  const up = ladderRec(3, 'sell');
  const v = checkLadder(up, up.rungs.map((r) => rungView(r, { contract: 'KobAsk', side: 1 })), 0);
  assert.ok(v.done && /step away/.test(String(v.violations[0]?.detail.problem)));
  const sell: LadderRecord = { ...up, rungs: up.rungs.map((r, i) => ({ ...r, price: String(1000 + i * 10) })) };
  const views = sell.rungs.map((r) => rungView(r, { contract: 'KobAsk', side: 1, initial_amount: '700' }));
  assert.deepEqual(checkLadder(sell, views, 0), { done: true, ok: true, violations: [] });
  views[1] = rungView(sell.rungs[1]!, { contract: 'KobAsk', side: 1, initial_amount: '699' });
  assert.ok((checkLadder(sell, views, 0) as { violations: unknown[] }).violations.length === 1);
});

test('cancelling one rung touches no other', () => {
  const rec: LadderCancelRecord = { kind: 'ladderCancel', ts: 0, bot: 't1', token: 'tok', rungs: ['r1', 'r2', 'r3'], cancelled: { id: 'r2', spends: 'x:0' }, txid: TX };
  const evs = (m: Record<string, EventLike[] | null>) => new Map(Object.entries(m));
  const ok = evs({ r1: [], r2: [ev(4, 'cancel', TX, 'r2')], r3: [ev(5, 'fill', 'ab'.repeat(32), 'r3')] });
  assert.deepEqual(checkLadderCancel(rec, order({ covenant_id: 'r2', status: 'cancelled' }), ok, 1000), { done: true, ok: true, violations: [] });
  const touched = evs({ r1: [ev(6, 'fill', TX, 'r1')], r2: [ev(4, 'cancel', TX, 'r2')], r3: [] });
  const v = checkLadderCancel(rec, order({ covenant_id: 'r2', status: 'cancelled' }), touched, 1000);
  assert.ok(v.done && v.violations[0]?.invariant === 'ladder' && (v.violations[0].detail.touched as string[])[0] === 'r1');
  // not indexed yet, still live at the spent outpoint: wait, then a warning
  const live = order({ covenant_id: 'r2', current: { txid: 'x', index: 0, value: '1' } });
  const none = evs({ r1: [], r2: [], r3: [] });
  assert.deepEqual(checkLadderCancel(rec, live, none, 1000), { done: false });
  const late = checkLadderCancel(rec, live, none, SCENARIO_GRACE_MS + 1);
  assert.ok(late.done && late.violations[0]?.severity === 'warn');
  // a fill won the race (the rung moved on): fine
  assert.deepEqual(checkLadderCancel(rec, order({ covenant_id: 'r2', status: 'partial', current: { txid: 'y', index: 0, value: '1' } }), none, 1000), { done: true, ok: true, violations: [], note: 'raced' });
});

test('a cancel-all ends every order (or loses the race to a fill)', () => {
  const rec: CancelAllRecord = { kind: 'cancelAll', ts: 0, bot: 't1', scope: 'pair:TETH/TUSD', orders: [{ id: 'o1', spends: 'a:0' }, { id: 'o2', spends: 'b:1' }], txids: [TX] };
  const vs = (a: Partial<OrderLike>, b: Partial<OrderLike>) => new Map<string, OrderLike | null>([['o1', order({ covenant_id: 'o1', ...a })], ['o2', order({ covenant_id: 'o2', ...b })]]);
  assert.deepEqual(checkCancelAll(rec, vs({ status: 'cancelled' }, { status: 'cancelled' }), 1000), { done: true, ok: true, violations: [] });
  assert.deepEqual(checkCancelAll(rec, vs({ status: 'cancelled' }, { status: 'partial', current: { txid: 'z', index: 0, value: '1' } }), 1000), { done: true, ok: true, violations: [], note: 'raced' });
  const stuck = vs({ status: 'cancelled' }, { status: 'open', current: { txid: 'b', index: 1, value: '1' } });
  assert.deepEqual(checkCancelAll(rec, stuck, 1000), { done: false });
  const v = checkCancelAll(rec, stuck, SCENARIO_GRACE_MS + 1);
  assert.ok(v.done && v.violations[0]?.invariant === 'cancel-all' && (v.violations[0].detail.live as string[])[0] === 'o2');
});

// ------------------------------------------------------------------------------------------------ checker: merge

const mergeRec = (over: Partial<MergeRecord> = {}): MergeRecord => ({
  kind: 'merge', ts: 0, bot: 't1', token: 'tok', ticker: 'TUSD', maker: 'MK',
  inputs: [{ txid: 'a', index: 0, amount: '5' }, { txid: 'b', index: 1, amount: '7' }, { txid: 'c', index: 0, amount: '9' }, { txid: 'd', index: 2, amount: '1' }, { txid: 'e', index: 0, amount: '3' }],
  total: '25', links: ['l1', 'l2'], final: { txid: 'l2', index: 0 }, ...over,
});

test('a merge plan merges distinct inputs into one output of their sum (3 -> 1, then 1 + 2 -> 1)', () => {
  assert.equal(checkMergeRecord(mergeRec()), null);
  assert.ok(checkMergeRecord(mergeRec({ total: '24' })));
  assert.ok(checkMergeRecord(mergeRec({ inputs: [{ txid: 'a', index: 0, amount: '5' }, { txid: 'a', index: 0, amount: '5' }], total: '10' })));
  assert.ok(checkMergeRecord(mergeRec({ final: { txid: 'l1', index: 0 } })));
});

test('the merged output holds the inputs\' total for the wallet', () => {
  assert.deepEqual(checkMerge(mergeRec(), { amount: '25', owner: 'mk' }, 0), { done: true, ok: true, violations: [] });
  const v = checkMerge(mergeRec(), { amount: '24', owner: 'MK' }, 0);
  assert.ok(v.done && v.violations[0]?.invariant === 'merge');
  assert.ok((checkMerge(mergeRec(), { amount: '25', owner: 'other' }, 0) as { violations: unknown[] }).violations.length === 1);
  assert.deepEqual(checkMerge(mergeRec(), null, 0), { done: false });
  assert.deepEqual(checkMerge(mergeRec(), null, SCENARIO_GRACE_MS + 1), { done: true, ok: false, violations: [], note: 'output not found' });
});

// ------------------------------------------------------------------------------------------------ checker: x402 retries

const ledger = (entries: { txid: string; state: string }[]) => entries.map((e) => JSON.stringify({ entry: e })).join('\n');

test('x402 retries: a superseded attempt is never accepted; a lost answer is served replayed', () => {
  const pays = [
    { ts: 0, path: '/retry/conflict', txid: 'p1', scenario: 'conflict', attempts: 2, resends: 0, superseded: ['s1'] },
    { ts: 0, path: '/retry/lost', txid: 'p2', scenario: 'lost', attempts: 1, resends: 1, replayed: true },
    { ts: 0, path: '/retry/drop', txid: 'p3', scenario: 'drop', attempts: 1, resends: 0 },
    { ts: 0, path: '/native', txid: 'p4' },
  ];
  const ok = checkX402Retries(pays, ledger([{ txid: 'p1', state: 'accepted' }, { txid: 's1', state: 'failed' }, { txid: 'p2', state: 'accepted' }]));
  assert.deepEqual(ok, { violations: [], stats: { retryPayments: 3, resent: 1, rebuilt: 1, notExercised: 1 } });
  const twice = checkX402Retries(pays, ledger([{ txid: 'p1', state: 'accepted' }, { txid: 's1', state: 'accepted' }]));
  assert.equal(twice.violations.length, 1);
  assert.match(String(twice.violations[0]!.detail.problem), /both accepted/);
  const fresh = checkX402Retries([{ ...pays[1]!, replayed: false }], '');
  assert.match(String(fresh.violations[0]!.detail.problem), /not replayed/);
});
