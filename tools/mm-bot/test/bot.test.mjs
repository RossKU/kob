// Whole cycles against a stand-in CLI with a small ledger (fake-kob.mjs "chain") and a stubbed
// fetch, with no network.
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
let indexerDown = false;
let usdcUsd = 1.0; // Kraken USDC/USD; the tests move it out of the peg band
let indexerAheadMs = 0; // the indexer judges expiry this far in the future (time passing while the bot is dead)

/**
 * Raw kline responses of each venue at `kasUsd`, the last 8 minutes, the newest still in progress.
 */
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
  if (url.includes('kraken') && url.includes('Ticker')) return { error: [], result: { USDTZUSD: { a: ['1.0'], b: ['1.0'] }, USDCUSD: { a: [String(usdcUsd)], b: [String(usdcUsd)] } } };
  throw new Error(`no stub for ${url}`);
}

function stubFetch(chainFile) {
  globalThis.fetch = async (url) => {
    const u = String(url);
    const json = (body, status = 200) => ({ ok: status === 200, status, json: async () => body });
    if (u.startsWith('http://indexer.test')) {
      if (indexerDown) throw new Error('fetch failed');
      const c = JSON.parse(readFileSync(chainFile, 'utf8'));
      const view = (id, o, status) => ({ covenant_id: id, contract: o.side === 1 ? 'KobAsk' : 'KobBid', side: o.side, token: c.token, scale: 1_000_000,
        price: String(o.price), amount_left: String(o.side === 1 ? o.amount : Math.floor((o.value - 1e8) * 1e6 / o.price)), current: { txid: o.txid, index: 0, value: String(o.value) }, status,
        expired: o.expiresAt != null && Date.now() + indexerAheadMs >= o.expiresAt });
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
  // 30,000 KAS and 600 tokens in 2 UTXOs (a merge is due before the first ask).
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

  // Cycle 1: the bot starts IDLE/cooldown. With resumeAfterSec 0 it quotes at once: the bids are
  // placed, and the token's 2 free UTXOs are merged before any ask.
  assert.equal(bot.state.mode.mode, 'IDLE');
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'QUOTING');
  let c = chain();
  assert.equal(Object.values(c.orders).filter((o) => o.side === 2).length, 3, 'three bids');
  assert.equal(c.tokenUtxos, 1, 'merged');
  assert.ok(calls().some((a) => a[0] === 'merge'));

  // Cycle 2: the asks.
  await bot.cycle();
  c = chain();
  const asks = Object.values(c.orders).filter((o) => o.side === 1);
  assert.equal(asks.length, 3);
  // fair = 1 / 0.025 = 40 KAS. With a 1 % spread, the level 0 ask is at 40 x 1.005 and the bid at
  // 40 x 0.995. 15 % of 1000 USD = 150 tokens a level.
  assert.deepEqual(asks.map((o) => o.price).sort(), [4020000000, 4040000000, 4060000000]);
  assert.ok(asks.every((o) => o.amount === 150e6));
  const bidPrices = Object.values(c.orders).filter((o) => o.side === 2).map((o) => o.price).sort();
  assert.deepEqual(bidPrices, [3940000000, 3960000000, 3980000000]);
  const ids = Object.keys(c.orders).sort();

  // Nothing moved: nothing is sent.
  const before = calls().length;
  await bot.cycle();
  assert.equal(calls().length, before, 'no timer-driven amends');

  // KAS/USD down 0.2 % (fair up 0.2 %): still under the 0.25 % threshold.
  kasUsd = 0.025 / 1.002;
  await bot.cycle();
  assert.equal(calls().length, before);

  // Down 0.4 %: every order is amended in place and the ids stay.
  kasUsd = 0.025 / 1.004;
  await bot.cycle();
  const amends = calls().slice(before);
  assert.equal(amends.length, 6);
  assert.ok(amends.every((a) => a[0] === 'replace'));
  assert.deepEqual(Object.keys(chain().orders).sort(), ids, 'amended in place: same covenant ids');
  // A cycle later the amends are seen and nothing more is sent.
  await bot.cycle();
  assert.equal(calls().length, before + 6);

  // The venues go dark: IDLE/reference and every quote pulled.
  venuesDown = true;
  await bot.cycle();
  assert.equal(`${bot.state.mode.mode}/${bot.state.mode.reason}`, 'IDLE/reference');
  assert.equal(Object.keys(chain().orders).length, 0);
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'IDLE', 'never stops by itself');

  // The venues come back: cool-down, then quoting again.
  venuesDown = false;
  await bot.cycle();
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'QUOTING');
  await bot.cycle();
  await bot.cycle();
  assert.ok(Object.keys(chain().orders).length >= 5);

  // The operator's STOP: everything is cancelled, then the cycle reports "stopped".
  writeFileSync(paths(dir).stop, 'stop');
  let r = await bot.cycle();
  assert.equal(bot.state.mode.mode, 'STOPPING');
  assert.equal(Object.keys(chain().orders).length, 0);
  r = await bot.cycle();
  assert.equal(r, 'stopped');
  // The next start, once the STOP file is gone, begins in IDLE/cooldown, not STOPPING.
  rmSync(paths(dir).stop);
  const again = new Bot(bot.cfg);
  assert.equal(`${again.state.mode.mode}/${again.state.mode.reason}`, 'IDLE/cooldown');

  // The logs: a fee ledger, an actions log and a status snapshot.
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

test('STOP with the indexer down: the quotes are cancelled from the saved views through the node (sec-ops MM-4)', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  const { bot, chain, calls } = setup();
  await bot.cycle();
  await bot.cycle();
  await bot.cycle(); // the views of all six quotes are saved
  const ids = Object.keys(chain().orders).sort();
  assert.equal(ids.length, 6);
  const saved = JSON.parse(readFileSync(paths(bot.cfg.dataDir).views, 'utf8')).items.map((v) => v.covenant_id).sort();
  assert.deepEqual(saved, ids);
  indexerDown = true;
  try {
    writeFileSync(bot.p.stop, 'stop\n');
    await assert.rejects(bot.cycle(), /fetch failed/);
    const blind = calls().filter((a) => a[0] === 'cancel' && a.includes('--view'));
    assert.equal(blind.length, 1, 'one kob cancel --view --yes');
    assert.ok(ids.every((id) => bot.state.orders[id].pending?.kind === 'cancel'));
    assert.equal(bot.state.mode.mode, 'STOPPING');
    // The next failed cycle does not send the cancels again within orderVisibleTimeoutSec.
    await assert.rejects(bot.cycle(), /fetch failed/);
    assert.equal(calls().filter((a) => a[0] === 'cancel' && a.includes('--view')).length, 1);
  } finally {
    indexerDown = false;
    rmSync(bot.p.stop, { force: true });
  }
});

test('the indexer down while quoting normally: nothing is cancelled blind', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  const { bot, calls } = setup();
  await bot.cycle();
  await bot.cycle();
  indexerDown = true;
  try {
    await assert.rejects(bot.cycle(), /fetch failed/);
    assert.equal(calls().filter((a) => a[0] === 'cancel').length, 0, 'no pull is due');
  } finally {
    indexerDown = false;
  }
});

// Review finding E-L4: every quote carries a short expiry and is renewed in place, so the quotes of
// a dead bot lapse by themselves.
const MIN = 60_000;
const gtdOf = (a) => (a.includes('--gtd') ? Date.parse(a[a.indexOf('--gtd') + 1]) : null);
const book = (c) => {
  const bids = Object.values(c.orders).filter((o) => o.side === 2).map((o) => o.price);
  const asks = Object.values(c.orders).filter((o) => o.side === 1).map((o) => o.price);
  return { bids, asks, bestBid: Math.max(...bids), bestAsk: Math.min(...asks) };
};

test('quotes expire lifeMin after their last action, are renewed in place after renewAfterMin, and lapse when the bot dies', { timeout: 90_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  usdcUsd = 1.0;
  indexerAheadMs = 0;
  const { bot, chain, calls, dir } = setup();
  const life = bot.cfg.quotes.lifeMin * MIN;
  const t0 = Date.now();
  await bot.cycle();
  await bot.cycle();
  let c = chain();
  const ids = Object.keys(c.orders).sort();
  assert.equal(ids.length, 6);
  // Every placement carries --gtd lifeMin ahead (to the second), and the state file knows it.
  const places = calls().filter((a) => a[0] === 'place');
  assert.equal(places.length, 6);
  for (const a of places) {
    const g = gtdOf(a);
    assert.ok(g > t0 + life - 2_000 && g <= Date.now() + life, `--gtd ${a[a.indexOf('--gtd') + 1]}`);
  }
  for (const id of ids) assert.equal(bot.state.orders[id].expiresAt, c.orders[id].expiresAt);

  // Nothing moves and renewAfterMin has not passed: nothing is sent.
  const before = calls().length;
  await bot.cycle();
  assert.equal(calls().length, before);

  // renewAfterMin (45 min) later: the six quotes are amended in place with a fresh --gtd, at the same
  // prices, and the ledger books them as renewals.
  for (const id of ids) bot.state.orders[id].expiresAt -= bot.cfg.quotes.renewAfterMin * MIN;
  const prices = Object.fromEntries(ids.map((id) => [id, c.orders[id].price]));
  const t1 = Date.now();
  await bot.cycle();
  const renews = calls().slice(before);
  assert.equal(renews.length, 6);
  assert.ok(renews.every((a) => a[0] === 'replace' && gtdOf(a) >= t1 + life - 1_000));
  c = chain();
  assert.deepEqual(Object.keys(c.orders).sort(), ids, 'in place: the same covenant ids');
  for (const id of ids) assert.equal(c.orders[id].price, prices[id]);
  assert.equal(bot.state.fees.byKind.renew.n, 6);
  assert.equal(bot.state.fees.byKind.renew.sompi, 6 * 500000);
  // The founder's rules hold through the renewal: a 1 % total gap, no self-trade, 150 tokens a level.
  const b = book(c);
  assert.ok(b.bestBid < b.bestAsk);
  assert.equal((b.bestAsk - b.bestBid) / 40e8, 0.01);
  assert.ok(Object.values(c.orders).filter((o) => o.side === 1).every((o) => o.amount === 150e6));
  // Seen by the indexer, and not due again.
  await bot.cycle();
  assert.equal(calls().length, before + 6);

  // The bot dies here. Every quote lapses at most lifeMin after the bot's last action: once that
  // has passed, the indexer flags every quote expired (conforming matchers no longer fill them).
  const lastAction = Date.now();
  c = chain();
  assert.ok(Object.values(c.orders).every((o) => o.expiresAt != null && o.expiresAt <= lastAction + life));
  indexerAheadMs = life + MIN;
  const viewed = JSON.parse(JSON.stringify(c.orders));
  assert.ok(Object.values(viewed).every((o) => Date.now() + indexerAheadMs >= o.expiresAt), 'all lapsed');

  // A restart finds them expired and renews each in place at once, with no cancel and no new order.
  const again = new Bot(bot.cfg);
  again.log.quiet = true;
  again.kob = bot.kob;
  const n = calls().length;
  await again.cycle();
  indexerAheadMs = 0;
  const after = calls().slice(n);
  assert.equal(after.length, 6);
  assert.ok(after.every((a) => a[0] === 'replace' && a.includes('--gtd')));
  assert.deepEqual(Object.keys(chain().orders).sort(), ids);
  assert.ok(Object.values(chain().orders).every((o) => o.expiresAt > Date.now() + life - 5_000));
  const actions = readFileSync(paths(dir).actions, 'utf8').trim().split('\n').map((l) => JSON.parse(l));
  assert.ok(actions.some((a) => a.type === 'amend' && a.renewOnly && a.reason === 'renew: past its expiry' && a.gtd));
});

test('renewals keep the founder rules: the peg band idles and resumes, the fee cap idles, alerts go to alerts.jsonl, the bot never stops', { timeout: 90_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  usdcUsd = 1.0;
  indexerAheadMs = 0;
  const { bot, chain, calls, dir } = setup();
  await bot.cycle();
  await bot.cycle();
  assert.equal(Object.keys(chain().orders).length, 6);
  const alerts = () => (existsSync(paths(dir).alerts) ? readFileSync(paths(dir).alerts, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);

  // USDC/USD leaves the 0.5 % band: IDLE/peg, every quote cancelled (no renewal), an alert line.
  usdcUsd = 0.99;
  await bot.cycle();
  assert.equal(`${bot.state.mode.mode}/${bot.state.mode.reason}`, 'IDLE/peg');
  assert.equal(Object.keys(chain().orders).length, 0);
  assert.ok(alerts().some((a) => a.reason === 'peg' && a.state === 'IDLE'));
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'IDLE', 'never stops by itself');

  // Back in the band: it resumes on its own, and the new quotes carry --gtd again.
  usdcUsd = 1.0;
  const n = calls().length;
  for (let i = 0; i < 4; i++) await bot.cycle();
  assert.equal(bot.state.mode.mode, 'QUOTING');
  const placed = calls().slice(n).filter((a) => a[0] === 'place');
  assert.ok(placed.length >= 5 && placed.every((a) => a.includes('--gtd')));
  const b = book(chain());
  assert.ok(b.bestBid < b.bestAsk, 'no self-trade');

  // A renewal is due but the day's fees reached the cap less the cancel reserve: no renewal is
  // sent, the bot idles on the fee cap and pulls its quotes (cancels always go), and does not stop.
  const ids = Object.keys(bot.state.orders);
  for (const id of ids) bot.state.orders[id].expiresAt -= bot.cfg.quotes.renewAfterMin * MIN;
  const day = new Date().toISOString().slice(0, 10);
  const cap = bot.cfg.safety.maxFeesKasPerDay - bot.cfg.safety.cancelReserveKas;
  bot.state.fees = { day, sompi: cap * 1e8, txs: 1, byKind: { amend: { n: 1, sompi: cap * 1e8 } } };
  const m = calls().length;
  await bot.cycle();
  assert.equal(`${bot.state.mode.mode}/${bot.state.mode.reason}`, 'IDLE/fee-cap');
  const sent = calls().slice(m);
  assert.ok(sent.every((a) => a[0] === 'cancel'), sent.map((a) => a[0]).join(','));
  assert.equal(Object.keys(chain().orders).length, 0);
  assert.ok(alerts().some((a) => a.reason === 'fee-cap'));
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'IDLE');
});
