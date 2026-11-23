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
import { cutMiddle, Kob } from '../src/kob.mjs';
import { paths } from '../src/store.mjs';

const TOKEN = '01'.repeat(32);
const MAKER = 'a1d9fd1347128011ad0036b077949d63274d5d3b8ac8351776c30c945ef2c2fb'; // an address vector of cli.test.mjs
const TPL = { KobAsk: 'a5'.repeat(32), KobBid: 'b5'.repeat(32) }; // fake-kob.mjs: the template hashes of this build
const stateOf = (o) => {
  const st = { maker: MAKER, tokenCovId: TOKEN, scale: '1000000', price: String(o.price), extensionCommitment: '00'.repeat(32) };
  return o.side === 1 ? { kind: 'KobAsk', state: { ...st, amountLeft: String(o.amount) } } : { kind: 'KobBid', state: st };
};

let kasUsd = 0.025; // what every venue trades at; the tests move it
let venuesDown = false;
let indexerDown = false;
let usdcUsd = 1.0; // Kraken USDC/USD; the tests move it out of the peg band (null: Kraken does not quote it)
let hiddenIds = new Set(); // orders a lying indexer leaves out of every answer (T-4)
let showOnly = null; // a Set: the indexer lists only these ids (every new order is never listed, R2-T1 case A)
let staleViews = new Map(); // id -> a view a lying indexer keeps serving (R2-T1 case C), listed even once the order ended
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
  if (url.includes('kraken') && url.includes('Ticker')) return { error: [], result: { USDTZUSD: { a: ['1.0'], b: ['1.0'] }, ...(usdcUsd == null ? {} : { USDCUSD: { a: [String(usdcUsd)], b: [String(usdcUsd)] } }) } };
  throw new Error(`no stub for ${url}`);
}

const hidden = (id) => hiddenIds.has(id) || (showOnly != null && !showOnly.has(id));

function stubFetch(chainFile) {
  globalThis.fetch = async (url) => {
    const u = String(url);
    const json = (body, status = 200) => ({ ok: status === 200, status, json: async () => body });
    if (u.startsWith('http://indexer.test')) {
      if (indexerDown) throw new Error('fetch failed');
      const c = JSON.parse(readFileSync(chainFile, 'utf8'));
      const view = (id, o, status) => staleViews.get(id) ?? ({ covenant_id: id, contract: o.side === 1 ? 'KobAsk' : 'KobBid', side: o.side, token: c.token, scale: 1_000_000,
        template_hash: TPL[o.side === 1 ? 'KobAsk' : 'KobBid'], state_known: true, state: stateOf(o),
        price: String(o.price), amount_left: String(o.side === 1 ? o.amount : Math.floor((o.value - 1e8) * 1e6 / o.price)), current: { txid: o.txid, index: 0, value: String(o.value) }, status,
        expired: o.expiresAt != null && Date.now() + indexerAheadMs >= o.expiresAt });
      if (u.includes('/v1/orders?')) {
        const items = Object.entries(c.orders).filter(([id]) => !hidden(id)).map(([id, o]) => view(id, o, 'open'));
        for (const [id, v] of staleViews) if (!c.orders[id]) items.push(v);
        return json({ items, next_cursor: null });
      }
      if (u.includes('/v1/fills')) return json({ items: [], next_cursor: null });
      const id = u.split('/v1/orders/')[1];
      if (hidden(id)) return json({ error: 'not_found' }, 404);
      if (c.orders[id]) return json(view(id, c.orders[id], 'open'));
      if (c.ended[id]) return json({ covenant_id: id, side: 1, status: c.ended[id], current: null });
      return json({ error: 'not_found' }, 404);
    }
    if (venuesDown) return json({}, 503);
    return json(venueResponse(u));
  };
}

function setup(safety = {}) {
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
    safety: { ...DEFAULTS.safety, resumeAfterSec: 0, placeGapSec: 0, ...safety },
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

test('STOP with the indexer down: the quotes are cancelled through the node from the bot\'s own records (sec-ops MM-4, R2-T1)', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  const { bot, chain, calls } = setup();
  await bot.cycle();
  await bot.cycle();
  await bot.cycle();
  const ids = Object.keys(chain().orders).sort();
  assert.equal(ids.length, 6);
  // Every order carries the state and outpoint its own placement produced.
  for (const id of ids) assert.equal(bot.state.orders[id].own.txid, chain().orders[id].txid);
  indexerDown = true;
  try {
    writeFileSync(bot.p.stop, 'stop\n');
    await assert.rejects(bot.cycle(), /fetch failed/);
    const blind = calls().filter((a) => a[0] === 'recover' && a.includes('--cancel'));
    assert.equal(blind.length, 1, 'one kob recover --cancel --yes');
    assert.equal(Object.keys(chain().orders).length, 0, 'cancelled on chain');
    assert.ok(ids.every((id) => bot.state.orders[id].pending?.kind === 'cancel'));
    assert.equal(bot.state.mode.mode, 'STOPPING');
    // The next failed cycle does not send the cancels again within orderVisibleTimeoutSec.
    await assert.rejects(bot.cycle(), /fetch failed/);
    assert.equal(calls().filter((a) => a[0] === 'recover' && a.includes('--cancel')).length, 1);
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
    assert.equal(calls().filter((a) => a[0] === 'cancel' || a[0] === 'recover').length, 0, 'no pull is due');
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

  // The indexer's 'expired' flag alone moves nothing: the expiry is the bot's own record (T-8).
  const early = new Bot(bot.cfg);
  early.log.quiet = true;
  early.kob = bot.kob;
  const n0 = calls().length;
  await early.cycle();
  assert.equal(calls().length, n0, 'no renewal on the indexer\'s word');

  // A restart once the bot's own record has lapsed renews each in place at once, with no cancel
  // and no new order.
  const st = JSON.parse(readFileSync(paths(dir).state, 'utf8'));
  for (const o of Object.values(st.orders)) o.expiresAt -= life + MIN;
  writeFileSync(paths(dir).state, JSON.stringify(st));
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
  assert.ok(actions.some((a) => a.type === 'amend' && a.renewOnly && /^renew: expiry due/.test(a.reason) && a.gtd));
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
  assert.ok(sent.every((a) => a[0] === 'recover' && a.includes('--cancel')), sent.map((a) => a[0]).join(','));
  assert.equal(Object.keys(chain().orders).length, 0);
  assert.ok(alerts().some((a) => a.reason === 'fee-cap'));
  await bot.cycle();
  assert.equal(bot.state.mode.mode, 'IDLE');
});

const alertsOf = (dir) => (existsSync(paths(dir).alerts) ? readFileSync(paths(dir).alerts, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);

test('a lying indexer that hides a quote: the node keeps it in the state file and it is cancelled from its view (T-4)', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  usdcUsd = 1.0;
  const { bot, chain, dir } = setup();
  await bot.cycle();
  await bot.cycle();
  await bot.cycle();
  const ids = Object.keys(chain().orders).sort();
  assert.equal(ids.length, 6);
  hiddenIds = new Set([ids[0]]);
  try {
    // The node still holds it: it stays in the state file and goes through kob cancel --view.
    await bot.cycle();
    assert.ok(bot.state.orders[ids[0]], 'not forgotten on the indexer\'s word');
    assert.equal(bot.state.orders[ids[0]].pending?.kind, 'cancel');
    assert.equal(chain().orders[ids[0]], undefined, 'cancelled on chain');
    assert.ok(alertsOf(dir).some((a) => a.reason === 'indexer-hides'));
    // Once the cancel is due to be seen, the node shows its UTXO spent: now it is forgotten.
    bot.state.orders[ids[0]].pending.since -= 200_000;
    await bot.cycle();
    assert.equal(bot.state.orders[ids[0]], undefined);
  } finally {
    hiddenIds = new Set();
  }
});

test('STOP does not end on the indexer\'s count while the node has not confirmed the quotes ended (T-4)', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  usdcUsd = 1.0;
  const { bot, chain } = setup();
  await bot.cycle();
  await bot.cycle();
  await bot.cycle();
  const ids = Object.keys(chain().orders);
  assert.equal(ids.length, 6);
  hiddenIds = new Set(ids); // the indexer lists nothing
  writeFileSync(bot.p.stop, 'stop\n');
  bot.kob.env.FAKE_KOB_NODE_DOWN = '1';
  try {
    assert.notEqual(await bot.cycle(), 'stopped');
    // The node does not answer: nothing is forgotten, the bot does not report stopped.
    for (const s of Object.values(bot.state.orders)) if (s.pending) s.pending.since -= 200_000;
    assert.notEqual(await bot.cycle(), 'stopped');
    assert.equal(Object.keys(bot.state.orders).length, 6);
    delete bot.kob.env.FAKE_KOB_NODE_DOWN;
    // The node shows every quote ended (once their cancels are due to be seen): forgotten, then stopped.
    for (const s of Object.values(bot.state.orders)) if (s.pending) s.pending.since -= 200_000;
    assert.equal(await bot.cycle(), 'stopped');
    assert.equal(Object.keys(bot.state.orders).length, 0);
  } finally {
    delete bot.kob.env.FAKE_KOB_NODE_DOWN;
    hiddenIds = new Set();
    rmSync(bot.p.stop, { force: true });
  }
});

test('the indexer down while fair drifts past half the spread: the quotes are pulled through the node (T-5)', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  usdcUsd = 1.0;
  const { bot, chain, calls } = setup();
  await bot.cycle();
  await bot.cycle();
  await bot.cycle();
  assert.equal(Object.keys(chain().orders).length, 6);
  indexerDown = true;
  try {
    // 0.3 %: within half the 1 % spread, and no jump: nothing is cancelled.
    kasUsd = 0.025 / 1.003;
    await assert.rejects(bot.cycle(), /fetch failed/);
    assert.equal(calls().filter((a) => a[0] === 'cancel' || a[0] === 'recover').length, 0);
    // 0.6 %: every quote would sit past fair; pulled from the saved views, the mode left as it is.
    kasUsd = 0.025 / 1.006;
    await assert.rejects(bot.cycle(), /fetch failed/);
    assert.equal(calls().filter((a) => a[0] === 'recover' && a.includes('--cancel')).length, 1);
    assert.equal(Object.keys(chain().orders).length, 0);
    assert.equal(bot.state.mode.mode, 'QUOTING');
  } finally {
    indexerDown = false;
    kasUsd = 0.025;
  }
});

test('an indexer view whose covenant_id is not 64 hex is ignored (sec-ops L21)', async () => {
  const saved = globalThis.fetch;
  globalThis.fetch = async () => ({ ok: true, status: 200, json: async () => ({ items: [
    { covenant_id: '--yes', token: TOKEN, side: 1, status: 'open', price: '1', current: {} },
    { covenant_id: 'cd'.repeat(32), token: TOKEN, side: 1, status: 'open', price: '1', current: {} },
  ], next_cursor: null }) });
  try {
    const { Indexer } = await import('../src/indexer.mjs');
    const live = await new Indexer('http://indexer.test').liveOrders(MAKER, TOKEN);
    assert.deepEqual(live.map((o) => o.id), ['cd'.repeat(32)]);
  } finally {
    globalThis.fetch = saved;
  }
});

test('no live USDC/USD and no fallback (the default): IDLE, nothing quoted, and it resumes by itself (L25)', { timeout: 60_000 }, async () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  usdcUsd = null;
  const { bot, chain } = setup();
  assert.equal(bot.cfg.token.usdFallback, null);
  try {
    await bot.cycle();
    await bot.cycle();
    assert.equal(`${bot.state.mode.mode}/${bot.state.mode.reason}`, 'IDLE/reference');
    assert.match(bot.state.last.reference.reason, /no USD rate for the token/);
    assert.equal(Object.keys(chain().orders).length, 0, 'no quote at an unchecked 1.0');
    usdcUsd = 1.0;
    for (let i = 0; i < 4; i++) await bot.cycle();
    assert.equal(bot.state.mode.mode, 'QUOTING');
    assert.ok(Object.keys(chain().orders).length > 0);
  } finally {
    usdcUsd = 1.0;
  }
});

// R2-T1: the bot tracks every order by the outpoint its own place or amend produced, never by the
// indexer's listing.
const reset = () => {
  kasUsd = 0.025;
  venuesDown = false;
  indexerDown = false;
  usdcUsd = 1.0;
  indexerAheadMs = 0;
  hiddenIds = new Set();
  showOnly = null;
  staleViews = new Map();
};
const places = (calls) => calls().filter((a) => a[0] === 'place');

test('a placed order the indexer never lists is kept, counted and not placed again (R2-T1 case A)', { timeout: 60_000 }, async () => {
  reset();
  showOnly = new Set(); // the indexer lists nothing, ever: no template can be learned either
  const { bot, chain, calls, dir } = setup({ orderVisibleTimeoutSec: 0 });
  try {
    for (let i = 0; i < 5; i++) await bot.cycle();
    assert.equal(Object.keys(chain().orders).length, 6, 'three bids and three asks on chain');
    assert.equal(places(calls).length, 6, 'each level placed once');
    assert.equal(Object.keys(bot.state.orders).length, 6, 'all kept in the state file');
    assert.ok(alertsOf(dir).some((a) => a.reason === 'node-unconfirmed'));
  } finally {
    reset();
  }
});

test('the capital cap holds while the indexer never lists the new orders (R2-T1 case A)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls } = setup({ orderVisibleTimeoutSec: 0 });
  try {
    for (let i = 0; i < 3; i++) await bot.cycle();
    assert.equal(Object.keys(chain().orders).length, 6);
    assert.ok(Object.values(bot.state.templates.kinds).every((k) => k.verified), 'templates proven on the node');
    showOnly = new Set(Object.keys(chain().orders)); // from now on no new order is ever listed
    // Pull and quote again: every new order is one the indexer never shows.
    venuesDown = true;
    await bot.cycle();
    await bot.cycle();
    venuesDown = false;
    const cap = Math.floor((1000 * 0.5) / 0.025) * 1e8; // maxBidPct of capitalUsd in sompi
    for (let i = 0; i < 8; i++) {
      await bot.cycle();
      const c = chain();
      const bids = Object.values(c.orders).filter((o) => o.side === 2);
      assert.ok(Object.keys(c.orders).length <= 6, `at most the ladder on chain (cycle ${i}: ${Object.keys(c.orders).length})`);
      assert.ok(bids.reduce((a, o) => a + o.value - 1e8, 0) <= cap, 'bid escrow within the cap');
      for (const id of Object.keys(c.orders)) assert.ok(bot.state.orders[id], `order ${id} on chain is tracked`);
    }
    assert.ok(places(calls).length > 6, 'levels are quoted again once the node shows the hidden ones cancelled');
  } finally {
    reset();
  }
});

test('an amended order the indexer hides is kept and cancelled through the node (R2-T1 case B)', { timeout: 60_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  try {
    for (let i = 0; i < 3; i++) await bot.cycle();
    const id = Object.keys(chain().orders).sort()[0];
    kasUsd = 0.025 / 1.004; // every quote is amended in place
    await bot.cycle();
    assert.equal(bot.state.orders[id].own.txid, chain().orders[id].txid, 'the amend\'s outpoint is the bot\'s record');
    hiddenIds = new Set([id]); // the indexer drops it (or calls it filled)
    await bot.cycle();
    assert.ok(bot.state.orders[id], 'not forgotten');
    assert.equal(bot.state.orders[id].pending?.kind, 'cancel');
    assert.equal(chain().orders[id], undefined, 'cancelled on chain from the bot\'s own record');
    assert.ok(alertsOf(dir).some((a) => a.reason === 'indexer-hides'));
    bot.state.orders[id].pending.since -= 200_000;
    await bot.cycle();
    assert.equal(bot.state.orders[id], undefined, 'gone once the node shows it spent');
  } finally {
    reset();
  }
});

test('STOP cancels an order whose indexer view is stale and ends (R2-T1 case C)', { timeout: 60_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    for (let i = 0; i < 3; i++) await bot.cycle();
    const id = Object.keys(chain().orders).sort()[0];
    const before = (await (await fetch('http://indexer.test/v1/orders?maker=x&status=active&limit=100')).json()).items.find((v) => v.covenant_id === id);
    kasUsd = 0.025 / 1.004;
    await bot.cycle(); // amended in place
    staleViews = new Map([[id, before]]); // the indexer keeps showing the pre-amend outpoint, forever
    writeFileSync(bot.p.stop, 'stop\n');
    let r;
    for (let i = 0; i < 6 && r !== 'stopped'; i++) {
      r = await bot.cycle();
      for (const s of Object.values(bot.state.orders)) if (s.pending) s.pending.since -= 200_000;
    }
    assert.equal(r, 'stopped');
    assert.equal(Object.keys(chain().orders).length, 0, 'nothing left on chain');
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

// R2-T3: every placement names --decimals; refused placements idle the bot with an alert, and it
// resumes by itself.
test('placements refused by the CLI raise an alert and IDLE, and resume by themselves (R2-T3)', { timeout: 60_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup();
  bot.kob.env.FAKE_KOB_REFUSE_PLACE = '1';
  try {
    await bot.cycle();
    await bot.cycle();
    assert.equal(`${bot.state.mode.mode}/${bot.state.mode.reason}`, 'IDLE/placement');
    assert.ok(alertsOf(dir).some((a) => a.reason === 'placement' && a.level === 'error'));
    await bot.cycle();
    assert.equal(bot.state.mode.mode, 'IDLE', 'never stops by itself');
    delete bot.kob.env.FAKE_KOB_REFUSE_PLACE;
    for (let i = 0; i < 3; i++) await bot.cycle();
    assert.equal(bot.state.mode.mode, 'QUOTING');
    assert.ok(Object.keys(chain().orders).length > 0);
    const placed = readFileSync(process.env.FAKE_KOB_LOG, 'utf8').trim().split('\n').map((l) => JSON.parse(l)).filter((a) => a[0] === 'place');
    assert.ok(placed.length && placed.every((a) => a[a.indexOf('--decimals') + 1] === '6'), 'every placement names --decimals');
    assert.ok(calls().length > 0);
  } finally {
    delete bot.kob.env.FAKE_KOB_REFUSE_PLACE;
    reset();
  }
});

// T-7: a placement or amend that would meet the bot's own order on the other side is skipped.
test('the bot never sends a quote that meets its own (T-7)', () => {
  reset();
  const { bot } = setup();
  bot.state.orders = { a: { side: 'sell', price: 4020000000 }, b: { side: 'buy', price: 3980000000 } };
  assert.equal(bot.crossesOwn({ kind: 'amend', id: 'b', side: 'buy', price: 4020000000 }), 4020000000);
  assert.equal(bot.crossesOwn({ kind: 'amend', id: 'b', side: 'buy', price: 4019999999 }), null);
  assert.equal(bot.crossesOwn({ kind: 'place', side: 'sell', price: 3980000000 }), 3980000000);
  assert.equal(bot.crossesOwn({ kind: 'cancel', id: 'a', side: 'sell' }), null);
});

test('a long stderr keeps its last line, where the CLI says what failed (T-13)', () => {
  const long = `${'x'.repeat(5000)} | error: submit: socket closed`;
  const cut = cutMiddle(long, 600);
  assert.equal(cut.length, 600);
  assert.ok(cut.endsWith('error: submit: socket closed'));
  assert.equal(cutMiddle('short', 600), 'short');
});
