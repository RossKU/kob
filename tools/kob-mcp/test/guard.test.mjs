import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { checkConfig, loadConfig, publicConfig } from '../src/config.mjs';
import { Ledger, bandCheck, bandReference, bookReference, capRefusals, tipRefusals } from '../src/guard.mjs';
import { kas, toSompi } from '../src/units.mjs';
import { PRIMARY } from './helpers.mjs';

const KAS = 100_000_000n;

test('units: KAS strings to sompi and back', () => {
  assert.equal(toSompi('1.5'), 150_000_000n);
  assert.equal(toSompi('0.25KAS'), 25_000_000n);
  assert.equal(toSompi(2), 200_000_000n);
  assert.equal(toSompi('123sompi'), 123n);
  assert.throws(() => toSompi('1.123456789'));
  assert.throws(() => toSompi('-1'));
  assert.equal(kas(150_000_000n), '1.5');
  assert.equal(kas(-5n), '-0.00000005');
});

test('ledger: reserves count until released; settle replaces the amount; days are separate', () => {
  const l = new Ledger(join(mkdtempSync(join(tmpdir(), 'kob-led-')), 'ledger.jsonl'));
  l.reserve('a', '2026-11-17', 10n * KAS);
  l.reserve('b', '2026-11-17', 20n * KAS);
  l.reserve('c', '2026-11-16', 99n * KAS);
  assert.equal(l.spentOn('2026-11-17'), 30n * KAS);
  l.settle('a', 7n * KAS, ['t1']);
  l.release('b', 'not sent');
  assert.equal(l.spentOn('2026-11-17'), 7n * KAS);
  assert.equal(l.spentOn('2026-11-16'), 99n * KAS);
  // A second reader of the same file (a restarted server) sees the same total.
  assert.equal(new Ledger(l.file).spentOn('2026-11-17'), 7n * KAS);
});

test('caps: per transaction, per day and fee', () => {
  const caps = { perTx: 100n * KAS, perDay: 150n * KAS, maxFee: KAS / 100n };
  assert.deepEqual(capRefusals({ spend: 50n * KAS, fee: 1000n }, caps, 0n), []);
  assert.match(capRefusals({ spend: 101n * KAS, fee: 1000n }, caps, 0n)[0], /^PER_TX_CAP/);
  assert.match(capRefusals({ spend: 60n * KAS, fee: 1000n }, caps, 100n * KAS)[0], /^DAILY_CAP/);
  assert.equal(capRefusals({ spend: 50n * KAS, fee: 1000n }, caps, 100n * KAS).length, 0, 'exactly at the cap passes');
  assert.match(capRefusals({ spend: KAS, fee: KAS / 10n }, caps, 0n)[0], /^MAX_FEE/);
});

const ref = (n, d = 1n, what = 'ref') => ({ num: BigInt(n), den: BigInt(d), what });

test('price band: a buy far above or a sell far below the reference is refused; no reference refuses (R2-T2)', () => {
  const at = (side, price, extra = {}) => bandCheck({ side, price, ref: ref(25_000_000), scale: 100_000_000, bandPct: 25, decimals: 8, ...extra });
  assert.equal(at('buy', 31_250_000n).ok, true, 'exactly at +25 % passes');
  assert.match(at('buy', 31_250_001n).reason, /^PRICE_BAND/);
  assert.match(at('sell', 18_749_999n).reason, /^PRICE_BAND/);
  assert.equal(at('sell', 18_750_000n).ok, true, 'exactly at -25 % passes');
  assert.match(at('buy', 1n, { ref: null }).reason, /^PRICE_BAND_UNCHECKED: no usable reference/);
  assert.equal(at('buy', 10n ** 12n, { bandPct: null }).ok, true);
  assert.match(at('buy', 1n, { decimals: null }).reason, /^PRICE_BAND_UNCHECKED: the token's decimals are unknown/);
});

test('price band: exact at any number of decimals, a half-sompi reference and a fractional band', () => {
  // 12 decimals: the book's scale stops at 10^9, so 25,000,000 sompi per 10^9 base units is 250 KAS
  // per whole token.
  const b12 = { ref: ref(25_000_000), scale: 1_000_000_000, bandPct: 25, decimals: 12 };
  assert.equal(bandCheck({ side: 'buy', price: 312_50000000n, ...b12 }).ok, true);
  assert.match(bandCheck({ side: 'buy', price: 312_50000001n, ...b12 }).reason, /250 KAS per whole token/);
  assert.equal(bandCheck({ side: 'sell', price: 187_50000000n, ...b12 }).ok, true);
  assert.equal(bandCheck({ side: 'sell', price: 187_49999999n, ...b12 }).ok, false);
  // 18 decimals: 1 sompi per 10^9 base units is 10^9 sompi (10 KAS) per whole token.
  const b18 = { ref: ref(1), scale: 1_000_000_000, bandPct: 25, decimals: 18 };
  assert.equal(bandCheck({ side: 'buy', price: 1_250_000_000n, ...b18 }).ok, true);
  assert.equal(bandCheck({ side: 'buy', price: 1_250_000_001n, ...b18 }).ok, false);
  // 0 decimals, a reference of 3.5 sompi, a 2.5 % band: the buy limit is 3.5875 sompi.
  const b0 = { ref: ref(7, 2), scale: 1, decimals: 0 };
  assert.equal(bandCheck({ side: 'buy', price: 3n, bandPct: '2.5', ...b0 }).ok, true);
  assert.equal(bandCheck({ side: 'buy', price: 4n, bandPct: '2.5', ...b0 }).ok, false);
  assert.equal(bandCheck({ side: 'sell', price: 4n, bandPct: 150, ...b0 }).ok, true, 'a band over 100 % never refuses a sell');
});

test('price band: a state price per its own scale is judged exactly (12 decimals)', () => {
  // 25,000,000 sompi per 10^9 base units is 250 KAS per whole token: +25 % is 312.5 KAS.
  const b12 = { ref: ref(25_000_000), scale: 1_000_000_000, bandPct: 25, decimals: 12 };
  assert.equal(bandCheck({ side: 'buy', price: 31_250_000n, priceScale: 1_000_000_000, ...b12 }).ok, true);
  assert.match(bandCheck({ side: 'buy', price: 31_250_001n, priceScale: 1_000_000_000, ...b12 }).reason, /a buy at 312\.50001 KAS/);
});

// The fields of a book row a taker can fill now: quoted at its limit, live, settled.
const live = (price) => ({ quote: String(price), status: 'open', settled: true, expired: false, deadline_passed: false });

test('book reference: depth on the resting side and on the fills, the own and large-minimum orders left out (R2-T2)', () => {
  const scale = 100_000_000;
  const depth = 1000n * 100_000_000n;
  const row = (price, amount, extra = {}) => ({ price: String(price), amount_left: String(amount), fillable_now: String(amount), min_fill: '100000000', scale, maker: 'aa'.repeat(32), ...live(price), ...extra });
  const fill = (price, amount) => ({ price: String(price), amount: String(amount) });
  const fills = [fill(25e6, 2000e8), fill(30e6, 4000e8)];
  // A one-token bid at 0.3 is passed over; 1,200 KAS of bids reach 1,000 at 0.24.
  const bids = [row(30e6, 1e8), row(24e6, 5000e8)];
  const buy = bookReference({ side: 'buy', rows: { bids }, fills, scale, depth });
  assert.deepEqual([buy.ref.num * BigInt(scale) / buy.ref.den, buy.ref.what], [24_000_000n * BigInt(scale), '1000 KAS of bids'], 'a buy takes the lower: the bids');
  // The fills' VWAP over the first 1,000 KAS: 2000 at 0.25 and 4000 at 0.3 is 0.2833..
  const sell = bookReference({ side: 'sell', rows: { asks: [row(20e6, 6000e8)] }, fills, scale, depth });
  assert.equal(sell.ref.num, BigInt(25e6 * 2000e8 + 30e6 * 4000e8));
  assert.equal(sell.ref.den, BigInt(6000e8));
  assert.match(bookReference({ side: 'buy', rows: { bids: [row(24e6, 3000e8)] }, fills, scale, depth }).why, /under 1000 KAS of bids/);
  assert.match(bookReference({ side: 'buy', rows: { bids }, fills: fills.slice(0, 1), scale, depth }).why, /under 1000 KAS of fills/);
  const own = [row(24e6, 5000e8, { maker: 'ef'.repeat(32) })];
  assert.equal(bookReference({ side: 'buy', rows: { bids: own }, fills, scale, depth, maker: 'EF'.repeat(32) }).ref, null, 'the wallet\'s own bids add no depth');
  const aon = [row(24e6, 5000e8, { min_fill: String(5000e8) })];
  assert.equal(bookReference({ side: 'buy', rows: { bids: aon }, fills, scale, depth }).ref, null, 'an all-or-nothing bid adds no depth');
  // The operator's price bounds; the book only tightens it.
  const op = ref(25e6, 1n, 'op');
  assert.equal(bandReference({ side: 'buy', op, book: ref(40e6) }), op);
  assert.equal(bandReference({ side: 'buy', op, book: ref(20e6) }).num, 20_000_000n);
  assert.equal(bandReference({ side: 'sell', op, book: ref(20e6) }), op);
  assert.equal(bandReference({ side: 'sell', op, book: ref(30e6) }).num, 30_000_000n);
  assert.equal(bandReference({ side: 'buy', op: null, book: null }), null);
});

test('book reference: a TWAP or DCA order counts only what a taker can fill now; a row without fillable_now counts nothing (R3T-1)', () => {
  const scale = 100_000_000;
  const depth = 1000n * 100_000_000n;
  const row = (price, amount, fillable, extra = {}) => ({ price: String(price), amount_left: String(amount), ...(fillable == null ? {} : { fillable_now: String(fillable) }), min_fill: '100000000', scale, maker: 'aa'.repeat(32), ...live(price), ...extra });
  const fills = [{ price: String(250e6), amount: String(5000e8) }];
  // 1,250 KAS left at 2.5 KAS a token, but one 10-token slice (25 KAS) fillable per interval.
  const r = bookReference({ side: 'buy', rows: { bids: [row(250e6, 500e8, 10e8)] }, fills, scale, depth });
  assert.equal(r.ref, null, 'one slice is not 1,000 KAS at risk');
  assert.match(r.why, /under 1000 KAS of bids fillable now/);
  // A slice not due now (fillable_now 0) adds nothing either.
  assert.equal(bookReference({ side: 'buy', rows: { bids: [row(250e6, 500e8, 0)] }, fills, scale, depth }).ref, null);
  // An older indexer that names no fillable_now: the book gives no reference, and says why.
  const old = bookReference({ side: 'buy', rows: { bids: [row(250e6, 500e8, null)] }, fills, scale, depth });
  assert.equal(old.ref, null);
  assert.match(old.why, /fillable_now/);
  // A plain order fillable in full sets it.
  assert.equal(bookReference({ side: 'buy', rows: { bids: [row(250e6, 500e8, 500e8)] }, fills, scale, depth }).ref.num, 250_000_000n);
});

test('book reference: printed fills never loosen it past the resting side (R3T-1)', () => {
  const scale = 100_000_000;
  const depth = 1000n * 100_000_000n;
  const row = (price, amount) => ({ price: String(price), amount_left: String(amount), fillable_now: String(amount), min_fill: '100000000', scale, maker: 'aa'.repeat(32), ...live(price) });
  // Wash fills at 10 times the bids: a buy is still judged against the bids.
  const high = [{ price: String(250e6), amount: String(5000e8) }];
  assert.equal(bookReference({ side: 'buy', rows: { bids: [row(25e6, 5000e8)] }, fills: high, scale, depth }).ref.num, 25_000_000n);
  // Wash fills at a 25th of the asks: a sell is still judged against the asks.
  const low = [{ price: String(1e6), amount: String(200000e8) }];
  assert.equal(bookReference({ side: 'sell', rows: { asks: [row(25e6, 5000e8)] }, fills: low, scale, depth }).ref.num, 25_000_000n);
});

test('book reference: an armed if-done entry counts at its quote, not its limit (R4T-1)', () => {
  const scale = 100_000_000;
  const depth = 1000n * 100_000_000n;
  // Fair 0.25 KAS a token. An entry bid: limit 2.5 (10x fair), quote 0.225 (its band auction at
  // the entry stop), 400 tokens: 1,000 KAS at the limit, 90 KAS at the quote.
  const row = (price, quote, amount, extra = {}) => ({ price: String(price), quote: String(quote), amount_left: String(amount), fillable_now: String(amount), min_fill: '100000000', scale, maker: 'aa'.repeat(32), status: 'open', settled: true, expired: false, deadline_passed: false, ...extra });
  const entry = row(250e6, 22.5e6, 400e8, { contract: 'KobIfdBid', auction: { kind: 'entry', current_price: String(22.5e6) } });
  const fills = [{ price: String(250e6), amount: String(400e8) }];
  assert.equal(bookReference({ side: 'buy', rows: { bids: [entry] }, fills, scale, depth }).ref, null, 'the entry is 90 KAS a taker can sell into now');
  // With enough of it, the reference is its quote, never its limit.
  const deep = row(250e6, 22.5e6, 5000e8, { contract: 'KobIfdBid' });
  assert.equal(bookReference({ side: 'buy', rows: { bids: [deep] }, fills, scale, depth }).ref.num, 22_500_000n);
  // An armed if-done ask: limit 0.025, quote 0.3: a sell is judged at the quote.
  const ask = row(2.5e6, 30e6, 5000e8, { contract: 'KobIfdAsk' });
  const low = [{ price: String(2.5e6), amount: String(100000e8) }];
  assert.equal(bookReference({ side: 'sell', rows: { asks: [ask] }, fills: low, scale, depth }).ref.num, 30_000_000n);
  // A rising bid whose quote is above its start: counted at the lower, its start.
  assert.equal(bookReference({ side: 'buy', rows: { bids: [row(24e6, 30e6, 5000e8)] }, fills, scale, depth }).ref.num, 24_000_000n);
});

test('book reference: a row a taker cannot fill now counts nothing (R4T-1)', () => {
  const scale = 100_000_000;
  const depth = 1000n * 100_000_000n;
  const row = (extra = {}) => ({ price: String(24e6), quote: String(24e6), amount_left: String(5000e8), fillable_now: String(5000e8), min_fill: '100000000', scale, maker: 'aa'.repeat(32), status: 'open', settled: true, expired: false, deadline_passed: false, ...extra });
  const fills = [{ price: String(25e6), amount: String(5000e8) }];
  const ref = (r, side = 'buy') => bookReference({ side, rows: side === 'buy' ? { bids: [r] } : { asks: [r] }, fills, scale, depth }).ref;
  assert.equal(ref(row()).num, 24_000_000n, 'the live row sets it');
  for (const [what, extra] of [
    ['no quote (an older indexer)', { quote: undefined }],
    ['a zero quote', { quote: '0' }],
    ['not settled', { settled: false }],
    ['no settled field', { settled: undefined }],
    ['expired', { expired: true }],
    ['deadline passed', { deadline_passed: true }],
    ['filled', { status: 'filled' }],
    ['cancelled', { status: 'cancelled' }],
    ['a minimum fill above what is fillable now', { fillable_now: String(10e8), amount_left: String(5000e8), min_fill: String(20e8) }],
    ['a minimum fill worth over 10 KAS at its limit', { price: String(250e6), min_fill: String(5e8) }],
    ['fillable now above the amount left', { fillable_now: String(5000e8), amount_left: String(1e8) }],
    ['an ask kind on the bid side', { contract: 'KobAsk' }],
    ['no scale (kobd sorts it as scale 1)', { scale: null }],
  ]) assert.equal(ref(row(extra)), null, what);
  // A bid counts at most its escrow (cur_value): 100 KAS of KAS behind 1,200 KAS of tokens.
  assert.equal(ref(row({ cur_value: String(100e8) })), null, 'a bid is worth at most its escrow');
  assert.equal(ref(row({ cur_value: String(1200e8) })).num, 24_000_000n);
});

test('tip caps: a share of the price and a KAS total over the order', () => {
  const caps = { maxTipPct: 1, maxTip: KAS };
  const ask = (tip, amountLeft) => ({ kind: 'KobAsk', state: { scale: '100000000', price: '25000000', tip: String(tip), amountLeft: String(amountLeft) } });
  assert.deepEqual(tipRefusals(ask(250000, 100e8), 0, caps), [], '1 % of 0.25 over 100 tokens is 0.25 KAS');
  assert.match(tipRefusals(ask(250001, 1e8), 0, caps)[0], /above 1% of the price/);
  assert.match(tipRefusals(ask(250000, 1000e8), 0, caps)[0], /come to 2\.5 KAS/);
  const bid = { kind: 'KobBid', state: { scale: '100000000', price: '25000000', tip: '250000' } };
  assert.match(tipRefusals(bid, 200n * KAS, caps)[0], /come to 1\.98/, 'a buy: escrow x tip / (price + tip)');
  assert.deepEqual(tipRefusals(ask(24_000_000, 1e8), 0, { maxTipPct: null, maxTip: null }), [], 'both caps off');
  assert.deepEqual(tipRefusals({ kind: 'KobCurveAsk', state: { tip: '5' } }, 0, caps), [], 'other kinds are not plain orders');
});

test('ledger: launches of the day, released ones not counted', () => {
  const l = new Ledger(join(mkdtempSync(join(tmpdir(), 'kob-led-')), 'ledger.jsonl'));
  l.reserve('a', '2026-11-17', KAS, { tool: 'kob_token_create' });
  l.reserve('b', '2026-11-17', KAS, { tool: 'kob_token_create' });
  l.reserve('c', '2026-11-17', KAS, { tool: 'kob_place_limit' });
  l.reserve('d', '2026-11-16', KAS, { tool: 'kob_token_create' });
  l.release('b', 'not sent');
  assert.equal(l.launchesOn('2026-11-17'), 1);
});

test('config: testnet by default, mainnet only with allowMainnet, the allow-list checked', () => {
  const base = { network: 'testnet-10', mode: 'confirm', keyFile: 'k.key', dataDir: 'd', limits: { perTxKas: 1, perDayKas: 2, maxFeeKas: 0.1, priceBandPct: 25 }, withdrawTo: [PRIMARY], cli: {} };
  const c = checkConfig(base, process.cwd(), {});
  assert.equal(c.network, 'testnet-10');
  assert.equal(c.caps.perDay, 2n * KAS);
  assert.throws(() => checkConfig({ ...base, network: 'mainnet', withdrawTo: [] }, '.', {}), /allowMainnet/);
  assert.doesNotThrow(() => checkConfig({ ...base, network: 'mainnet', allowMainnet: true, withdrawTo: [] }, '.', {}));
  assert.throws(() => checkConfig({ ...base, network: 'mainnet', allowMainnet: true }, '.', {}), /withdrawTo/, 'a testnet address on mainnet');
  assert.throws(() => checkConfig({ ...base, withdrawTo: ['kaspatest:nothex'] }, '.', {}), /withdrawTo/);
  assert.throws(() => checkConfig({ ...base, keyFile: null }, '.', {}), /KOB_KEY/);
  assert.doesNotThrow(() => checkConfig({ ...base, keyFile: null }, '.', { KOB_KEY: 'x' }));
  assert.equal(JSON.stringify(publicConfig(c)).includes('k.key'), false, 'the key path is not shown');
  assert.equal(publicConfig({ ...c, indexer: 'https://u:secret@idx.example/api?key=secret' }).indexer, 'https://idx.example/api', 'no indexer credentials (R3T-I3)');
});

test('config: KOB_MCP_* variables (the extension form) override the file; placeholders and empty values do not', () => {
  const env = { KOB_MCP_KEY_FILE: 'C:/keys/agent.key', KOB_MCP_PER_DAY_KAS: '70', KOB_MCP_WITHDRAW_TO: `${PRIMARY}, `, KOB_MCP_NODE: '${user_config.node}', KOB_MCP_MODE: '' };
  const c = loadConfig(null, env);
  assert.equal(c.network, 'testnet-10');
  assert.equal(c.mode, 'confirm');
  assert.equal(c.node, null);
  assert.equal(c.caps.perDay, 70n * KAS);
  assert.deepEqual(c.withdrawTo, [PRIMARY]);
  assert.throws(() => loadConfig(null, {}), /no config/);
});
