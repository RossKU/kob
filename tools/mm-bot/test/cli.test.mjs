// The CLI wrapper (against a stand-in CLI), the config checks and the address encoding.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { addressOfXOnly } from '../src/address.mjs';
import { DEFAULTS, loadConfig, validate } from '../src/config.mjs';
import { Kob, parseJsonStream } from '../src/kob.mjs';

const TOKEN = '01'.repeat(32);
const dir = mkdtempSync(join(tmpdir(), 'mm-bot-test-'));

function fake(mode, { dryRun = false } = {}) {
  const log = join(dir, `calls-${mode}-${dryRun}-${Math.random()}.log`);
  writeFileSync(log, '');
  process.env.FAKE_KOB_LOG = log;
  process.env.FAKE_KOB_MODE = mode;
  const cfg = { ...DEFAULTS, network: 'testnet-10', node: 'ws://127.0.0.1:18210', indexer: null, kobCli: process.execPath, keyFile: join(dir, 'bot.key'), dataDir: dir };
  const kob = new Kob(cfg, { dryRun, prefixArgs: [fileURLToPath(new URL('./fake-kob.mjs', import.meta.url))] });
  const calls = () => readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l));
  return { kob, calls };
}

test('parseJsonStream reads one or several documents (kob cancel A B prints one per id)', () => {
  assert.deepEqual(parseJsonStream('{"a":1}\n{"b":"}{"}\n'), [{ a: 1 }, { b: '}{' }]);
  assert.deepEqual(parseJsonStream(''), []);
  assert.deepEqual(parseJsonStream('{\n "x": [1, {"y": "\\"q"}]\n}'), [{ x: [1, { y: '"q' }] }]);
});

test('a signing command is planned with --dry-run, then signed with --yes; the key is only a path', async () => {
  const { kob, calls } = fake('ok');
  const r = await kob.place(TOKEN, 'sell', 2500000, 3787500000);
  assert.equal(r.ok, true);
  assert.equal(r.txid, 'ab'.repeat(32));
  assert.equal(r.fee, 427000);
  assert.deepEqual(r.newIds, ['cd'.repeat(32)]);
  const c = calls();
  assert.equal(c.length, 2);
  assert.ok(c[0].includes('--dry-run') && !c[0].includes('--yes'));
  assert.ok(c[1].includes('--yes') && !c[1].includes('--dry-run'));
  for (const a of c) {
    assert.deepEqual(a.slice(0, 9), ['place', 'limit', '--token', TOKEN, '--side', 'sell', '--amount', '2500000base', '--price']);
    assert.equal(a[9], '3787500000sompi', 'integer units: nothing is rounded by the CLI');
    assert.ok(a.includes('--json') && a.includes('--network') && a.includes('testnet-10'));
    assert.equal(a[a.indexOf('--key-file') + 1], join(dir, 'bot.key'));
  }
});

test('a quote is placed and amended with --gtd (its expiry), the same on the plan and the signing run', async () => {
  const { kob, calls } = fake('ok');
  assert.equal((await kob.place(TOKEN, 'buy', 2500000, 3787500000, '2026-11-20T13:30:00Z')).ok, true);
  assert.equal((await kob.amend('cd'.repeat(32), 3790000000, '2026-11-20T13:31:00Z')).ok, true);
  const c = calls();
  assert.equal(c.length, 4);
  for (const a of c.slice(0, 2)) assert.equal(a[a.indexOf('--gtd') + 1], '2026-11-20T13:30:00Z');
  for (const a of c.slice(2)) {
    assert.deepEqual(a.slice(0, 6), ['replace', 'cd'.repeat(32), '--price', '3790000000sompi', '--gtd', '2026-11-20T13:31:00Z']);
  }
  // Without one, nothing is added (a 90-day GTC).
  const { kob: k2, calls: c2 } = fake('ok');
  await k2.amend('cd'.repeat(32), 3790000000);
  assert.ok(c2().every((a) => !a.includes('--gtd')));
});

test('a plan with an error issue is never signed', async () => {
  const { kob, calls } = fake('plan-error');
  const r = await kob.amend('cd'.repeat(32), 3800000000);
  assert.equal(r.ok, false);
  assert.match(r.error, /TOKEN_UTXOS_FRAGMENTED/);
  assert.equal(calls().length, 1);
});

test('the exit code is read first: a failed submission or a usage error is a failure', async () => {
  let { kob } = fake('submit-fail');
  let r = await kob.cancel('cd'.repeat(32));
  assert.equal(r.ok, false);
  assert.match(r.error, /exit 1: error: submission failed/);
  ({ kob } = fake('usage'));
  r = await kob.cancel('cd'.repeat(32));
  assert.equal(r.ok, false);
  assert.match(r.error, /exit 2/);
});

test('--dry-run never runs --yes', async () => {
  const { kob, calls } = fake('ok', { dryRun: true });
  const r = await kob.place(TOKEN, 'buy', 1000000, 3700000000);
  assert.equal(r.ok, true);
  assert.equal(r.dryRun, true);
  assert.equal(r.fee, 427000);
  assert.ok(calls().every((a) => !a.includes('--yes')));
});

test('balances: integer units and the maker key', async () => {
  const { kob } = fake('ok');
  const b = await kob.balances(TOKEN);
  const row = { free: 5000000, inOrders: 0, freeUtxos: 2, decimals: 6, freeVerified: true };
  assert.deepEqual(b, { maker: 'ef'.repeat(32), kasFree: 100000000000, kasTotal: 100000000000, token: row, tokens: { [TOKEN]: row } });
});

test('config: the defaults plus a token pass; bad values are listed together', () => {
  const ok = { ...DEFAULTS, token: { ...DEFAULTS.token, covenantId: TOKEN, decimals: 6 } };
  assert.deepEqual(validate(ok), []);
  const bad = validate({ ...ok, network: 'testnet', token: { ...ok.token, covenantId: 'USDX' }, quotes: { ...ok.quotes, repriceThreshold: 0.02, maxBidPct: 70 } });
  assert.ok(bad.some((p) => /network/.test(p)));
  assert.ok(bad.some((p) => /pin the token by its covenant id/.test(p)));
  assert.ok(bad.some((p) => /repriceThreshold must be below/.test(p)));
  assert.ok(validate({ ...ok, reference: { ...ok.reference, pairAgreement: 0.006 } }).some((p) => /pairAgreement must be below half/.test(p)));
  assert.ok(validate({ ...ok, reference: { ...ok.reference, pairAgreement: null } }).some((p) => /pairAgreement/.test(p)), 'off only with minVenues 3');
  assert.deepEqual(validate({ ...ok, reference: { ...ok.reference, pairAgreement: null, minVenues: 3 } }), []);
  // E-L5: one USDT/USD source may sit up to usdtAgreement from 1, so it too stays below half the spread.
  assert.ok(validate({ ...ok, reference: { ...ok.reference, usdtAgreement: 0.006 } }).some((p) => /usdtAgreement must be below half/.test(p)));
  assert.ok(bad.some((p) => /must not exceed 100/.test(p)));
  const p = join(dir, 'cfg.json');
  writeFileSync(p, JSON.stringify({ network: 'testnet-10', keyFile: 'bot.key', dataDir: 'run', token: { covenantId: TOKEN, decimals: 6 } }));
  const cfg = loadConfig(p);
  assert.equal(cfg.keyFile, join(dir, 'bot.key'), 'relative paths are relative to the config file');
  assert.equal(cfg.dataDir, join(dir, 'run'));
  assert.equal(cfg.quotes.spreadPct, 1.0);
  assert.equal(cfg.token.usdSource, 'kraken:USDCUSD');
});

test('the Kaspa address of an x-only key (vectors from the Kaspa SDK)', () => {
  const key = 'a1d9fd1347128011ad0036b077949d63274d5d3b8ac8351776c30c945ef2c2fb';
  assert.equal(addressOfXOnly(key, 'mainnet'), 'kaspa:qzsanlgngufgqyddqqmtqau5n43jwn2a8w9vsdghwmpse9z77tp0k6yzkhpl6');
  assert.equal(addressOfXOnly(key, 'testnet-10'), 'kaspatest:qzsanlgngufgqyddqqmtqau5n43jwn2a8w9vsdghwmpse9z77tp0kmzydclw7');
  assert.throws(() => addressOfXOnly('xyz', 'mainnet'));
});
