import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { checkConfig, loadConfig, publicConfig } from '../src/config.mjs';
import { Ledger, bandCheck, capRefusals } from '../src/guard.mjs';
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

test('price band: a buy far above or a sell far below the reference is refused', () => {
  const book = { last_fill: String(25_000_000n), best_bid: '24000000', best_ask: '26000000', scale: 100_000_000 };
  const at = (side, price, extra = {}) => bandCheck({ side, price, book, bandPct: 25, decimals: 8, ...extra });
  assert.equal(at('buy', 31_250_000n).ok, true, 'exactly at +25 % passes');
  assert.match(at('buy', 31_250_001n).reason, /^PRICE_BAND/);
  assert.match(at('sell', 18_749_999n).reason, /^PRICE_BAND/);
  assert.equal(at('sell', 18_750_000n).ok, true, 'exactly at -25 % passes');
  assert.equal(at('buy', 1n, { book: { best_bid: null, best_ask: null, last_fill: null, scale: 1e8 } }).ok, true);
  assert.equal(at('buy', 10n ** 12n, { bandPct: null }).ok, true);
  assert.match(at('buy', 1n, { decimals: null }).note, /decimals are unknown/);
});

test('price band: exact at any number of decimals, a half-sompi mid and a fractional band', () => {
  // 12 decimals: the book's scale stops at 10^9, so 25,000,000 sompi per 10^9 base units is 250 KAS
  // per whole token.
  const b12 = { last_fill: '25000000', scale: 1_000_000_000 };
  assert.equal(bandCheck({ side: 'buy', price: 312_50000000n, book: b12, bandPct: 25, decimals: 12 }).ok, true);
  assert.match(bandCheck({ side: 'buy', price: 312_50000001n, book: b12, bandPct: 25, decimals: 12 }).reason, /250 KAS per whole token/);
  assert.equal(bandCheck({ side: 'sell', price: 187_50000000n, book: b12, bandPct: 25, decimals: 12 }).ok, true);
  assert.equal(bandCheck({ side: 'sell', price: 187_49999999n, book: b12, bandPct: 25, decimals: 12 }).ok, false);
  // 18 decimals: 1 sompi per 10^9 base units is 10^9 sompi (10 KAS) per whole token.
  const b18 = { last_fill: '1', scale: 1_000_000_000 };
  assert.equal(bandCheck({ side: 'buy', price: 1_250_000_000n, book: b18, bandPct: 25, decimals: 18 }).ok, true);
  assert.equal(bandCheck({ side: 'buy', price: 1_250_000_001n, book: b18, bandPct: 25, decimals: 18 }).ok, false);
  // 0 decimals, a mid of 3 and 4 sompi (3.5), a 2.5 % band: the buy limit is 3.5875 sompi.
  const b0 = { last_fill: null, best_bid: '3', best_ask: '4', scale: 1 };
  assert.equal(bandCheck({ side: 'buy', price: 3n, book: b0, bandPct: '2.5', decimals: 0 }).ok, true);
  assert.equal(bandCheck({ side: 'buy', price: 4n, book: b0, bandPct: '2.5', decimals: 0 }).ok, false);
  assert.equal(bandCheck({ side: 'sell', price: 4n, book: b0, bandPct: 150, decimals: 0 }).ok, true, 'a band over 100 % never refuses a sell');
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
