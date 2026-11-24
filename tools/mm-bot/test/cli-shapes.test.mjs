// The kob CLI documents mm-bot parses (R8T-2), each pinned against a sample transcribed from the
// Rust source of crates/kob-cli (the json! literal that prints it, named at each sample): the
// stand-in CLI must print the same fields, and the bot's readers must read the sample itself.
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { Bot } from '../src/bot.mjs';
import { DEFAULTS } from '../src/config.mjs';
import { Kob } from '../src/kob.mjs';
import { balancesDoc, mergeDoc, orderState, placePlan, recoverCancel, recoverEntry, recoverReport, replacePlan, signDoc, VERSION_LINE } from './cli-docs.mjs';

const H = (b) => b.repeat(32);
const dir = mkdtempSync(join(tmpdir(), 'mm-bot-shapes-'));
const STATE = { kind: 'KobAsk', state: { maker: H('a1'), tokenCovId: H('01'), tokenTplHash: H('7e'), tplPrefixLen: '41', tplSuffixLen: '1200', scale: '1000000',
  minFill: '1', price: '4000000000', tip: '0', tif: '0', activeFrom: '0', expiryDaa: '99999', refundTip: '0', interval: '0', maxFill: '0', slope: '0', priceEnd: '0',
  decayStep: '1', amountLeft: '150000000', extensionCommitment: H('00') } };

// crates/kob-cli/src/trade/flow.rs:85-93 (the --json document of a signing command), :152-165
// (its `result` once submitted); `transaction` is show.rs:219-236; place's `plan` place.rs:1041-1044.
const PLACE = {
  action: 'place limit sell', network: 'testnet-10',
  plan: { states: [STATE], request: { order: STATE, value: '100000000', tokens: [], tokenCarrier: '100000000', funding: [], change: null, lockTime: '0', deadline: null, records: [], fee: {} } },
  issues: [{ code: 'X', severity: 'info', message: 'm' }],
  transaction: { txid: H('aa'), inputs: ['in'], outputs: ['out'], fee: '427000', net: { kas: '-1', tokens: {} }, signatures: 1 },
  new_covenant_ids: [H('cd')],
  result: { status: 'submitted', txid: H('aa'), fee: '427000', covenant_ids: [H('cd')], file: null, engine_validated: true },
};
// crates/kob-cli/src/trade/actions.rs:746: a plain ask's or bid's replace plan, in place or a
// cancel-replace: `replacement`, and no `states`.
const REPLACE = { ...PLACE, action: `cancel-replace ${H('cd')}`, plan: { order: H('cd'), inPlace: false, current: STATE, replacement: { ...STATE, state: { ...STATE.state, price: '4100000000' } }, value: '99500000' }, new_covenant_ids: [H('ce')],
  result: { ...PLACE.result, covenant_ids: [H('ce')] } };
// crates/kob-cli/src/recover.rs:849-884 (an entry), :888-903 (the report), :982-991 (a cancel).
const RECOVER = {
  network: 'testnet-10', node: 'ws://127.0.0.1:18210',
  orders: [
    { covenant_id: H('cd'), kind: 'KobAsk', template: 'KobAsk (pinned)', template_hash: H('a5'), maker: H('a1'), status: 'live', note: null, source: 'own-orders.json', candidates: 1,
      outpoint: `${H('aa')}:0`, value: '100000000', utxo_daa: '1000', state: 'ab', amount_left: '150000000', scale: '1000000', min_fill: '1', found_by: 'known state', custody: { status: 'none' },
      cancel: { status: 'submitted', txid: H('bb'), fee: '500000', funding: 'order', file: null, engine_validated: true } },
    { covenant_id: H('ce'), kind: 'KobBid', template: 'KobBid (pinned)', template_hash: H('b5'), maker: H('a1'), status: 'not_found', note: "spent: no unspent output at the order's script (1 candidate)", source: 'own-orders.json', candidates: 1 },
  ],
  rejected: [],
  summary: { live: 1, not_found: 1, unsupported: 0, other_maker: 0, rejected: 0 },
};
// crates/kob-cli/src/trade/views.rs:776-795.
const BALANCES = { maker: H('a1'), kas: { free: '100000000000', locked: '0', total: '100000000000' },
  tokens: [{ token: H('01'), ticker: 'T', declared_ticker: null, decimals: 6, free: '5000000', free_utxos: 2, free_verified: true, in_orders: '0', bought: '0', strays: '0',
    kas_escrow: '0', kas_carriers: '0', kas_locked: '0', orders: 0, text: { name: 'T', free: '5', in_orders: '0', bought: '0', strays: '0', kas_locked: '0' } }],
  skipped: [] };
// crates/kob-cli/src/trade/merge.rs:347-356 (the summary) with the links of :427-430, :440.
const MERGE = { action: 'merge T', network: 'testnet-10', token: H('01'), utxos_before: 3, utxos_after: 1, transactions: 1, merged_amount: '5', fee: '500000',
  links: [{ link: 1, status: 'submitted', txid: H('aa'), fee: '500000' }] };

/** Every key path of a document (arrays by their first element). */
function paths(v, at = '') {
  if (Array.isArray(v)) return v.length ? paths(v[0], `${at}[]`) : [`${at}[]`];
  if (v && typeof v === 'object') return Object.keys(v).flatMap((k) => [`${at}.${k}`, ...paths(v[k], `${at}.${k}`)]);
  return [];
}
const same = (fake, real, what) => assert.deepEqual([...new Set(paths(fake))].sort(), [...new Set(paths(real))].sort(), `${what}: the fake prints other fields than the CLI`);

test('the stand-in CLI prints the real CLI\'s fields, document for document (R8T-2)', () => {
  const st = orderState({ side: 1, price: 4e9, amount: 150e6, expiryDaa: '99999' });
  same(st, STATE, 'an ask state (kob-protocol state.rs:655-688)');
  same(signDoc({ action: 'a', plan: placePlan(st, 1e8), txid: H('aa'), fee: 1, ids: [H('cd')], submitted: true, issues: [{ code: 'X', severity: 'info', message: 'm' }] }), PLACE, 'kob place');
  same(signDoc({ action: 'a', plan: replacePlan(H('cd'), false, st, st, 1), txid: H('aa'), fee: 1, ids: [H('ce')], submitted: true, issues: [{ code: 'X', severity: 'info', message: 'm' }] }), REPLACE, 'kob replace');
  const live = recoverEntry({ id: H('cd'), kind: 'KobAsk', tpl: H('a5'), status: 'live', live: { txid: H('aa'), value: 1, amountLeft: 1 }, cancel: recoverCancel(H('bb'), 1) });
  same(recoverReport([live, recoverEntry({ id: H('ce'), kind: 'KobBid', tpl: H('b5'), status: 'not_found', note: 'x' })]), RECOVER, 'kob recover');
  same(balancesDoc({ maker: H('a1'), kasFree: 1, token: H('01'), free: 1, freeUtxos: 1 }), BALANCES, 'kob balances');
  same(mergeDoc({ token: H('01'), before: 3, txids: [H('aa')], fee: 1 }), MERGE, 'kob merge');
  // crates/kob-cli/src/main.rs:311 (its test): `kob <version> (development build, no network, commit unknown)`.
  assert.match(VERSION_LINE, /^kob \S+ \((development build, no network|\S+ build), commit \S+\)$/);
});

/** A Kob whose CLI prints `doc` (a JSON document, or a raw string) and exits `code`. */
function cli(doc, code = 0) {
  const file = join(dir, `doc-${Math.random()}.json`);
  writeFileSync(file, typeof doc === 'string' ? doc : JSON.stringify(doc, null, 2));
  process.env.FAKE_KOB_LOG = join(dir, 'calls.log');
  const cfg = { ...DEFAULTS, network: 'testnet-10', node: 'ws://127.0.0.1:18210', indexer: null, kobCli: process.execPath, keyFile: join(dir, 'bot.key'), dataDir: dir };
  const kob = new Kob(cfg, { prefixArgs: [fileURLToPath(new URL('./fake-kob.mjs', import.meta.url))] });
  Object.assign(kob.env, { FAKE_KOB_SAMPLE: file, FAKE_KOB_SAMPLE_CODE: String(code) });
  return { kob, cfg };
}

test('the bot reads each sample of the real CLI (R8T-2)', async () => {
  // kob place: the txid, the fee and the new covenant id; the state from plan.states.
  const p = await cli(PLACE).kob.place(H('01'), 'sell', 150e6, 4e9);
  assert.deepEqual([p.ok, p.txid, p.fee, p.newIds], [true, H('aa'), 427000, [H('cd')]]);
  // kob replace (a cancel-replace): the new record's state is plan.replacement.
  const { kob, cfg } = cli(REPLACE);
  const r = await kob.amend(H('cd'), 4.1e9);
  assert.deepEqual([r.ok, r.newIds], [true, [H('ce')]]);
  const bot = new Bot({ ...cfg, token: { ...DEFAULTS.token, covenantId: H('01'), decimals: 6 } });
  bot.log.quiet = true;
  const rec = bot.placed({ side: 'sell', level: 0, price: 4.1e9, amount: 150e6 }, r.doc, r.txid, Date.now(), null);
  assert.equal(rec.own?.state?.state?.price, '4100000000', 'the replacement state, not none');
  assert.equal(rec.value, 99500000);
  // kob recover: live, its outpoint, its amount left and its cancel; a spent bid's note.
  const rc = await cli(RECOVER).kob.recover(join(dir, 'x.json'), { cancel: true });
  assert.equal(rc.ok, true);
  assert.equal(rc.orders[0].cancel.txid, H('bb'));
  assert.equal(rc.orders[0].amount_left, '150000000');
  assert.match(rc.orders[1].note, /spent: no unspent output at the order's script/);
  // kob balances and kob merge.
  const b = await cli(BALANCES).kob.balances(H('01'));
  assert.deepEqual([b.maker, b.kasFree, b.token.free, b.token.freeUtxos, b.token.decimals], [H('a1'), 100000000000, 5000000, 2, 6]);
  const m = await cli(MERGE).kob.merge(H('01'));
  assert.deepEqual([m.ok, m.txids, m.fee], [true, [H('aa')], 500000]);
});

// R8T-3: an answer that does not read is never a pass, and never "nothing sent".
test('an unreadable dry run is no pass; an unreadable --yes with exit 0 may have been sent (R8T-3)', async () => {
  const bad = '{"action": "place limit sell", "x": "\\u{202e}"}';
  const r = await cli(bad).kob.place(H('01'), 'sell', 150e6, 4e9);
  assert.equal(r.ok, false);
  assert.match(r.error, /plan unreadable/);
  assert.equal(r.sentFee, undefined, 'nothing was signed');
  // No preflight: the --yes itself answers unreadably.
  const { kob } = cli(bad);
  kob.cfg = { ...kob.cfg, cli: { ...kob.cfg.cli, preflight: false } };
  const y = await kob.place(H('01'), 'sell', 150e6, 4e9);
  assert.equal(y.ok, false);
  assert.ok(y.sentFee > 0, 'counted as possibly sent');
});
