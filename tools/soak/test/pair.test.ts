// Unit tests of the pair-order logic of the soak (protocol v3 KobPair / KobCondPair / KobIfdPair: token A for token B): invariant 11 (a pair
// fill never sets a price and honours the order's guarantee in B; pair arms name evidence of one of the two modes), invariant 12 (supply), the
// pair arithmetic of the pair bots and the multi-market report (`npm test`).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { checkAmountBalance, checkFill, checkPairEvidence, checkPairFill, checkSupply, isPairContract } from '../src/checker/rules.ts';
import { buildReport, type ReportData } from '../src/checker/report-model.ts';
import { fairPairPrice, pairRateOf, pairReference, ratText, withBps } from '../src/bots/pair-math.ts';
import { randUnits, usdToUnits } from '../src/util.ts';
import type { EventLike } from '../src/checker/types.ts';

const TUSD = '7cfe8aa0'.padEnd(64, '0');
const TETH = 'e7e70000'.padEnd(64, '1');
const ORDER = { covenant_id: 'cc'.repeat(32), contract: 'KobPair' };
const U = 100_000_000n; // one whole TETH (A, scale 10^8)

/** A pair fill event as the indexer serves it: `price` null, `detail.pair` (amount_b = tOut of an ask / sOut of a bid). */
const fill = (side: 'ask' | 'bid', n: bigint, amountB: bigint, price: bigint, over: Record<string, unknown> = {}, evPrice: string | null = null): EventLike => ({
  id: 1, covenant_id: ORDER.covenant_id, daa: 100, txid: 'ff'.repeat(32), kind: 'fill', side: side === 'ask' ? 1 : 2, amount: n.toString(), price: evPrice, payout: null, closes: false,
  detail: { input: 0, pair: { side, base: TETH, quote: TUSD, a_scale: Number(U), amount_a: n.toString(), amount_b: amountB.toString(), price: price.toString(), tip_kas: '0', counterparty: 'route', price_source: 'none', ...over } },
});

test('pair contracts', () => {
  for (const c of ['KobPair', 'KobCondPair', 'KobIfdPair']) assert.ok(isPairContract(c));
  for (const c of ['KobAsk', 'KobBidKron', 'KobCross', 'KobCondAsk']) assert.ok(!isPairContract(c));
});

test('pair fill: an ask receives at least ceil(n x q / scale(A)) of B; a bid pays at most floor(...); the surplus is reported', () => {
  // 2.5 TETH at 268_929 TUSD base units per whole TETH: exactly 672_322.5 -> an ask needs 672_323, a bid pays at most 672_322
  const n = 250_000_000n;
  const q = 268_929n;
  assert.deepEqual(checkPairFill(ORDER, fill('ask', n, 672_323n, q)).violations, []);
  const more = checkPairFill(ORDER, fill('ask', n, 672_400n, q));
  assert.deepEqual(more.violations, []);
  assert.equal(more.surplus, 77n);
  const short = checkPairFill(ORDER, fill('ask', n, 672_322n, q)).violations;
  assert.equal(short.length, 1);
  assert.equal(short[0]!.invariant, 'pair');
  assert.match(String(short[0]!.detail.problem), /received less/);
  assert.deepEqual(checkPairFill({ ...ORDER, contract: 'KobIfdPair' }, fill('bid', n, 672_322n, q)).violations, []);
  const over = checkPairFill({ ...ORDER, contract: 'KobCondPair' }, fill('bid', n, 672_323n, q)).violations;
  assert.match(String(over[0]?.detail.problem), /paid more/);
  assert.ok(checkPairFill(ORDER, fill('ask', n, 672_323n, q)).checked);
});

test('pair fill: a price on the event or another price source is an error (pair fills never set a price); KAS kinds and other events are ignored', () => {
  const priced = checkPairFill(ORDER, fill('ask', U, 268_929n, 268_929n, {}, '268929')).violations;
  assert.match(String(priced[0]?.detail.problem), /carries a price/);
  const sourced = checkPairFill(ORDER, fill('ask', U, 268_929n, 268_929n, { price_source: 'kas_books' })).violations;
  assert.match(String(sourced[0]?.detail.problem), /names a price source/);
  const noDetail = checkPairFill(ORDER, { ...fill('ask', U, 1n, 1n), detail: { input: 0 } });
  assert.equal(noDetail.violations[0]?.severity, 'warn');
  assert.deepEqual(checkPairFill({ ...ORDER, contract: 'KobAsk' }, fill('ask', U, 0n, 1n)).violations, []);
  assert.deepEqual(checkPairFill(ORDER, { ...fill('ask', U, 0n, 1n), kind: 'cancel' }).violations, []);
  // the KAS all-in rule leaves pair fills to invariant 11
  assert.deepEqual(checkFill(ORDER, { price: '1', scale: '1' }, fill('ask', U, 0n, 1n)).violations, []);
});

test('pair evidence: an arm / trail names two KAS-book fills (mode 0) or one resting pair order (mode 1)', () => {
  const ev = (kind: string, evidence: unknown): EventLike => ({ id: 2, covenant_id: ORDER.covenant_id, daa: 100, txid: 'ee'.repeat(32), kind, side: 1, amount: null, price: null, payout: null, closes: false, detail: { evidence } });
  const cond = { covenant_id: ORDER.covenant_id, contract: 'KobCondPair' };
  assert.equal(checkPairEvidence(cond, ev('arm', { mode: 0, inputs: [1, 2], orders: ['a', 'b'], a: '5000000', b: '100000000' }), ['KobAsk', 'KobBidKron']), null);
  assert.equal(checkPairEvidence(cond, ev('trail', { mode: 1, inputs: [1], orders: ['p'], price: '50500' }), ['KobPair']), null);
  assert.match(String(checkPairEvidence(cond, ev('arm', { mode: 1, inputs: [1], price: '50500' }), ['KobAsk'])?.detail.problem), /not a resting KobPair/);
  assert.match(String(checkPairEvidence(cond, ev('arm', { mode: 0, inputs: [1, 2], a: '5', b: '1' }), ['KobPair', 'KobAsk'])?.detail.problem), /not two plain/);
  assert.match(String(checkPairEvidence(cond, ev('arm', { mode: 0, inputs: [1], a: '5', b: '1' }))?.detail.problem), /two KAS-book fills/);
  assert.match(String(checkPairEvidence(cond, ev('arm', { mode: 2, inputs: [1] }))?.detail.problem), /unknown mode/);
  assert.match(String(checkPairEvidence(cond, ev('arm', undefined))?.detail.problem), /without evidence/);
  // a fill without evidence (no arm in it) is not an arm; KAS kinds are invariant 5's
  assert.equal(checkPairEvidence(cond, ev('fill', undefined)), null);
  assert.equal(checkPairEvidence({ ...cond, contract: 'KobCondAsk' }, ev('arm', undefined)), null);
});

test('amount accounting: a live KobPair keeps filled + left == initial', () => {
  const o = (init: string, filled: string, left: string, contract = 'KobPair') => ({ covenant_id: 'aa'.repeat(32), contract, status: 'partial', initial_amount: init, filled_amount: filled, amount_left: left }) as never;
  assert.equal(checkAmountBalance(o('300', '100', '200')), null);
  assert.ok(checkAmountBalance(o('300', '100', '199')));
  assert.equal(checkAmountBalance(o('300', '100', '199', 'KobCondPair')), null, 'a conditional is not accounted');
});

test('supply: live == supply + baseline passes; any other offset is minted or burnt', () => {
  const supply = 10_000_000n * 10n ** 8n;
  assert.equal(checkSupply(TETH, 'TETH', supply, supply, 0n), null);
  assert.match(String(checkSupply(TETH, 'TETH', supply, supply + 5n, 0n)?.detail.problem), /minted/);
  assert.match(String(checkSupply(TETH, 'TETH', supply, supply - 5n, 0n)?.detail.problem), /burnt/);
  assert.equal(checkSupply(TUSD, 'TUSD', supply, supply - 700n, -700n), null);
  assert.ok(checkSupply(TUSD, 'TUSD', supply, supply - 699n, -700n));
});

test('pair rate: B base units per whole A from the two KAS references; the pair book reference when close to it', () => {
  // 1 TETH = 61_800 KAS, 1 TUSD = 22.98 KAS: 2689.2950... TUSD = 268_929_503_916 TUSD base units (scale 10^8) per whole TETH
  const fair = pairRateOf(6_180_000_000_000n, 2_298_000_000n, U)!;
  assert.equal(fair, (6_180_000_000_000n * U) / 2_298_000_000n);
  assert.equal(pairRateOf(1n, 0n, U), null);
  // the book's touch per base unit of A: ask 2700 / bid 2680 TUSD per TETH -> midpoint 2690 TUSD per whole TETH (in TUSD base units)
  const lvl = (whole: bigint) => ({ price_num: whole.toString(), price_den: U.toString() });
  const view = { asks: [lvl(270_000_000_000n)], bids: [lvl(268_000_000_000n)] };
  assert.equal(pairReference(view, U, fair, 300), 269_000_000_000n);
  assert.equal(pairReference({ asks: [lvl(400_000_000_000n)], bids: [] }, U, fair, 300), null, 'more than 3 % away: unused');
  assert.equal(pairReference(null, U, fair, 300), null);
  // the older fair price as a rational, and its bps shifts
  const p = fairPairPrice({ perToken: 6_180_000_000_000n }, { perToken: 2_298_000_000n });
  const v = Number(p.num) / Number(p.den);
  assert.ok(Math.abs(v - 61_800 / 22.98) < 1e-6);
  const up = withBps(p, 100);
  assert.ok(Math.abs(Number(up.num) / Number(up.den) / v - 1.01) < 1e-9);
  assert.equal(ratText({ num: 2n, den: 3n }, 4, 'down'), '0.6666');
  assert.equal(ratText({ num: 2n, den: 3n }, 4, 'up'), '0.6667');
});

test('report: one market line per soak token, multi-book, pair and supply lines', () => {
  const d: ReportData = {
    now: Date.UTC(2026, 9, 1, 2, 0),
    soakStartMs: Date.UTC(2026, 9, 1, 0, 0),
    token: { ticker: 'TUSD', covenantId: TUSD, decimals: 8 },
    markets: [
      { ticker: 'TUSD', covenantId: TUSD, decimals: 8, fills: { count: 10, sell: 5, buy: 5, trades: 5, volumeTokens: '500000000', volumeKas: '11000000000' }, openAsks: 6, openBids: 7, supply: { delta: '-700', baseline: '-700' } },
      { ticker: 'TETH', covenantId: TETH, decimals: 8, fills: { count: 4, sell: 2, buy: 2, trades: 2, volumeTokens: '50000', volumeKas: '3090000000' }, openAsks: 6, openBids: 6, supply: { delta: '0', baseline: '0' } },
    ],
    multi: { txMultiBook: 3, txPair: 2, pairFills: 3, txMultiPair: 1 },
    fills: { count: 14, sell: 7, buy: 7, trades: 6, volumeTokens: '500000000', volumeKas: '11000000000', asOf: null },
    openAsks: 6,
    openBids: 7,
    orders: [{ contract: 'KobPair', tif: 0, status: 'filled', side: 1 }],
    ordersComplete: true,
    bots: null,
    executors: [],
    x402: { paidByPath: {}, errors: {}, ledger: null },
    incidents: [],
    checks: { pairFills: 3, pairChecked: 3, 'pairCounterparty:route': 1, 'pairCounterparty:netting': 2, pairTriggers: 2, pairTriggerMode0: 1, pairTriggerMode1: 1, supplyChecks: 4, supplyOk: 4 },
    checkerTs: null,
    resources: { current: null, peak: {}, peakTotal: 0 },
    supervisor: [],
  };
  const { text, json } = buildReport(d);
  assert.match(text, /market   TUSD  fills 10 .*volume 5\.00 TUSD \/ 110\.00 KAS/);
  assert.match(text, /multi    txs filling 2\+ books 3  with a pair fill 2 \(2\+ pair fills 1\)  pair fills 3/);
  assert.match(text, /pair 3\/3 \(route 1, netting 2, inventory 0; triggers 2: mode 0 1, mode 1 1\)  supply 4\/4/);
  assert.match(text, /supply   TUSD delta -700 \(baseline -700: pre-run holdings no tracker knows\)  TETH delta 0/);
  assert.equal((json.multi as { txPair: number }).txPair, 2);
});

test('sizes: a USD amount in base units of a token (TUSD 1:1, an asset at its reference), and random draws in a range', () => {
  assert.equal(usdToUnits(4, 1, 8), 400_000_000n);
  assert.equal(usdToUnits(4, 2500, 8), 160_000n);
  assert.equal(usdToUnits(1, 100_000, 8), 1000n);
  assert.equal(usdToUnits(1e-12, 1, 8), 1n, 'at least one base unit');
  for (let k = 0; k < 200; k++) {
    const v = randUnits(50_000_000n, 400_000_000n);
    assert.ok(v >= 50_000_000n && v <= 400_000_000n);
  }
  assert.equal(randUnits(7n, 7n), 7n);
});
