// Whole cycles against a stand-in CLI with a small ledger (fake-kob.mjs "chain") and a stubbed fetch; no network.
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { Bot } from '../src/bot.mjs';
import { DEFAULTS } from '../src/config.mjs';
import { Kob } from '../src/kob.mjs';
import { paths } from '../src/store.mjs';

const TOKEN = '01'.repeat(32);
const MAKER = 'a1d9fd1347128011ad0036b077949d63274d5d3b8ac8351776c30c945ef2c2fb'; // an address vector of cli.test.mjs

let kasUsd = 0.025; // what every venue trades at; the tests move it
let venuesDown = false;

/** Raw kline responses of each venue at `kasUsd`, the last 8 minutes, the newest still in progress. */
function venueResponse(url) {
  const now = Date.now();
  const minute = Math.floor(now / 60_000) * 60_000;
  const ts = Array.from({ length: 8 }, (_, i) => minute - (7 - i) * 60_000);
  const p = kasUsd;
  const base = 10_000;
  if (url.includes('bybit')) return { retCode: 0, retMsg: 'OK', result: { list: ts.slice().reverse().map((t) => [String(t), '0', '0', '0', String(p), String(base), String(base * p)]) } };
  if (url.includes('kucoin')) return { code: '200000', data: ts.slice().reverse().map((t) => [String(t / 1000), '0', String(p), '0', '0', String(base), String(base * p)]) };
  if (url.includes('gateio')) return ts.map((t) => [String(t / 1000), String(base * p), String(p), '0', '0', '0', String(base), 'true']);
  if (url.includes('mexc')) return ts.map((t) => [t, '0', '0', '0', String(p), String(base), t + 60_000, String(base * p)]);
  if (url.includes('kraken') && url.includes('OHLC')) return { error: [], result: { KASUSD: ts.map((t) => [t / 1000, '0', '0', '0', String(p), String(p), String(base), 5]), last: 0 } };
  if (url.includes('kraken') && url.includes('Ticker')) return { error: [], result: { USDTZUSD: { a: ['1.0'], b: ['1.0'] }, USDCUSD: { a: ['1.0'], b: ['1.0'] } } };
  throw new Error(`no stub for ${url}`);
}

function stubFetch(chainFile) {
  globalThis.fetch = async (url) => {
    const u = String(url);
    const json = (body, status = 200) => ({ ok: status === 200, status, json: async () => body });
    if (u.startsWith('http://indexer.test')) {
      const c = JSON.parse(readFileSync(chainFile, 'utf8'));
      const view = (id, o, status) => ({ covenant_id: id, contract: o.side === 1 ? 'KobAsk' : 'KobBid', side: o.side, token: c.token, scale: 1_000_000,
        price: String(o.price), amount_left: String(o.side === 1 ? o.amount : Math.floor((o.value - 1e8) * 1e6 / o.price)), current: { txid: o.txid, index: 0, value: String(o.value) }, status });
      if (u.includes('/v1/orders?')) return json({ items: Object.entries(c.orders).map(([id, o]) => view(id, o, 'open')), next_cursor: null });
      if (u.includes('/v1/fills')) return json({ items: [], next_cursor: null });
      const id = u.split('/v1/orders/')[1];
      if (c.orders[id]) return json(view(id, c.orders[id], 'open'));
      if (c.ended[id]) return json({ covenant_id: id, side: 1, status: c.ended[id], current: null });
      return json({ error: 'not_found' }, 404);
    }
    if (venuesDown) return json({}, 503);
    return json(venueResponse(u));
  };
}

function setup() {
  const dir = mkdtempSync(join(tmpdir(), 'mm-bot-cycle-'));
  const chainFile = join(dir, 'chain.json');
  // 30,000 KAS and 600 tokens in 2 UTXOs (a merge is due before the first ask)
  writeFileSync(chainFile, JSON.stringify({ maker: MAKER, token: TOKEN, kasFree: 30_000e8, tokenFree: 600e6, tokenUtxos: 2, orders: {}, ended: {}, n: 0 }));
  process.env.FAKE_KOB_LOG = join(dir, 'calls.log');
  process.env.FAKE_KOB_MODE = 'chain';
  process.env.FAKE_KOB_CHAIN = chainFile;
  writeFileSync(process.env.FAKE_KOB_LOG, '');
  stubFetch(chainFile);
  const cfg = {
    ...DEFAULTS, network: 'testnet-10', indexer: 'http://indexer.test', kobCli: process.execPath, keyFile: join(dir, 'bot.key'), dataDir: dir,
    token: { ...DEFAULTS.token, covenantId: TOKEN, decimals: 6, label: 'TUSDC6' },
    reference: { ...DEFAULTS.reference, crossCheck: { ...DEFAULTS.reference.crossCheck, enabled: false } },
    safety: { ...DEFAULTS.safety, resumeAfterSec: 0, placeGapSec: 0 },
  };
  const bot = new Bot(cfg);
  bot.log.quiet = true;
  bot.kob = new Kob(cfg, { prefixArgs: [fileURLToPath(new URL('./fake-kob.mjs', import.meta.url))] });
  const chain = () => JSON.parse(readFileSync(chainFile, 'utf8'));
  const calls = () => readFileSync(process.env.FAKE_KOB_LOG, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)).filter((a) => a.includes('--yes'));
  return { bot, chain, calls, dir };
}

test('a full life: idle, merge, quote both sides, hold, amend on a move, pull on stale venues, resume, stop', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  const { bot, chain, calls, dir } = setup();

  // cycle 1: the bot starts IDLE/cooldown; with resumeAfterSec 0 it quotes at once: the bids are placed, and the token's
  // 2 free UTXOs are merged before any ask
  assert.equal(bot.state.mode.mode, 'IDLE');
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'QUOTING');
  let c = chain();
  assert.equal(Object.values(c.orders).filter((o) => o.side === 2).length, 3, 'three bids');
  assert.equal(c.tokenUtxos, 1, 'merged');
  assert.ok(calls().some((a) => a[0] === 'merge'));

  // cycle 2: the asks
  await bot.cycle();
  c = chain();
  const asks = Object.values(c.orders).filter((o) => o.side === 1);
  assert.equal(asks.length, 3);
  // fair = 1 / 0.025 = 40 KAS; a 1 % spread: level 0 ask at 40 x 1.005, bid at 40 x 0.995; 15 % of 1000 USD = 150 tokens a level
  assert.deepEqual(asks.map((o) => o.price).sort(), [4020000000, 4040000000, 4060000000]);
  assert.ok(asks.every((o) => o.amount === 150e6));
  const bidPrices = Object.values(c.orders).filter((o) => o.side === 2).map((o) => o.price).sort();
  assert.deepEqual(bidPrices, [3940000000, 3960000000, 3980000000]);
  const ids = Object.keys(c.orders).sort();

  // nothing moved: nothing is sent
  const before = calls().length;
  await bot.cycle();
  assert.equal(calls().length, before, 'no timer-driven amends');

  // KAS/USD down 0.2 % (fair up 0.2 %): still under the 0.25 % threshold
  kasUsd = 0.025 / 1.002;
  await bot.cycle();
  assert.equal(calls().length, before);

  // down 0.4 %: every order is amended in place; the ids stay
  kasUsd = 0.025 / 1.004;
  await bot.cycle();
  const amends = calls().slice(before);
  assert.equal(amends.length, 6);
  assert.ok(amends.every((a) => a[0] === 'replace'));
  assert.deepEqual(Object.keys(chain().orders).sort(), ids, 'amended in place: same covenant ids');
  // a cycle later the amends are seen and nothing more is sent
  await bot.cycle();
  assert.equal(calls().length, before + 6);

  // the venues go dark: IDLE/reference and every quote pulled
  venuesDown = true;
  await bot.cycle();
  assert.equal(`${bot.state.mode.mode}/${bot.state.mode.reason}`, 'IDLE/reference');
  assert.equal(Object.keys(chain().orders).length, 0);
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'IDLE', 'never stops by itself');

  // the venues come back: cool-down, then quoting again
  venuesDown = false;
  await bot.cycle();
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'QUOTING');
  await bot.cycle();
  await bot.cycle();
  assert.ok(Object.keys(chain().orders).length >= 5);

  // the operator's STOP: everything is cancelled, then the cycle reports "stopped"
  writeFileSync(paths(dir).stop, 'stop');
  let r = await bot.cycle();
  assert.equal(bot.state.mode.mode, 'STOPPING');
  assert.equal(Object.keys(chain().orders).length, 0);
  r = await bot.cycle();
  assert.equal(r, 'stopped');
  // the next start, once the STOP file is gone, begins in IDLE/cooldown, not STOPPING
  rmSync(paths(dir).stop);
  const again = new Bot(bot.cfg);
  assert.equal(`${again.state.mode.mode}/${again.state.mode.reason}`, 'IDLE/cooldown');

  // the logs: a fee ledger, an actions log and a status snapshot
  assert.ok(bot.state.fees.txs > 0);
  assert.equal(bot.state.fees.sompi, bot.state.fees.txs * 500000);
  const actions = readFileSync(paths(dir).actions, 'utf8').trim().split('\n').map((l) => JSON.parse(l));
  assert.ok(actions.some((a) => a.type === 'mode' && a.to === 'IDLE' && a.reason === 'reference'));
  assert.ok(actions.some((a) => a.type === 'amend' && a.status === 'submitted'));
  assert.ok(existsSync(paths(dir).state));
});

test('one side without inventory idles alone, with a deposit message; the other side quotes', { timeout: 30_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  const { bot, chain } = setup();
  const c = chain();
  c.tokenFree = 0;
  c.tokenUtxos = 0;
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify(c));
  await bot.cycle();
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'QUOTING');
  assert.equal(Object.values(chain().orders).filter((o) => o.side === 2).length, 3);
  assert.equal(Object.values(chain().orders).filter((o) => o.side === 1).length, 0);
  assert.deepEqual(bot.state.last.sides.asks, { quoting: false, reason: 'no inventory' });
  const asks = bot.state.last.funds.find((f) => f.key === 'asks');
  assert.equal(asks.level, 'idle');
  assert.match(asks.message, /^Insufficient TUSDC6 \(0101.*\): asks need 45% = 450 USD = 450 TUSDC6, 0 TUSDC6 \(0%\) held; deposit 450 TUSDC6 to kaspatest:qzsanlgngufgqyddqqmtqau5n43jwn2a8w9vsdghwmpse9z77tp0kmzydclw7 to resume asks\.$/);
});
