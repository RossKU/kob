// Whole cycles against a stand-in CLI with a small ledger (fake-kob.mjs "chain") and a stubbed
// fetch, with no network.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { Bot, PROVEN_KEEP_AFTER_EXPIRY_MS, PROVEN_LOOKUPS, PROVEN_MAX, PROVEN_PASS_BATCH } from '../src/bot.mjs';
import { DEFAULTS } from '../src/config.mjs';
import { cutMiddle, Kob } from '../src/kob.mjs';
import { paths } from '../src/store.mjs';
import { VERSION_LINE } from './cli-docs.mjs';

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
let listHidden = new Set(); // orders it leaves out of the maker's list only (its per-order view still answers)
let showOnly = null; // a Set: the indexer lists only these ids (every new order is never listed, R2-T1 case A)
let staleViews = new Map(); // id -> a view a lying indexer keeps serving (R2-T1 case C), listed even once the order ended
let fillsFeed = []; // the indexer's fill events, newest first
let indexerAheadMs = 0;
let listCut = false; // the maker's list is all newer orders of others, with more pages left (R4T-3)
let orderUrls = []; // the list reads the bot made // the indexer judges expiry this far in the future (time passing while the bot is dead)

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
        orderUrls.push(u);
        if (listCut) return json({ items: [], next_cursor: 'older' });
        const items = Object.entries(c.orders).filter(([id]) => !hidden(id) && !listHidden.has(id)).map(([id, o]) => view(id, o, 'open'));
        for (const [id, v] of staleViews) if (!c.orders[id]) items.push(v);
        return json({ items, next_cursor: null });
      }
      if (u.includes('/v1/fills')) {
        const q = new URL(u).searchParams;
        const before = q.get('before') == null ? Infinity : Number(q.get('before'));
        const limit = Number(q.get('limit') ?? 50);
        const rest = fillsFeed.filter((f) => f.id < before);
        const items = rest.slice(0, limit);
        return json({ items, next_cursor: rest.length > limit ? String(items.at(-1).id) : null });
      }
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

function setup(safety = {}, quotes = {}) {
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
    quotes: { ...DEFAULTS.quotes, ...quotes },
  };
  const bot = new Bot(cfg);
  bot.log.quiet = true;
  bot.kob = new Kob(cfg, { prefixArgs: [fileURLToPath(new URL('./fake-kob.mjs', import.meta.url))] });
  const chain = () => JSON.parse(readFileSync(chainFile, 'utf8'));
  // A test's node read also shows what the chain paid the bot back (a cancel's carrier, R6T-2).
  let outs = null;
  Object.defineProperty(bot, 'nodeOutputs', { configurable: true, get: () => outs, set: (f) => { outs = f && (async (...a) => [...(chain().outputs ?? []), ...(await f(...a))]); } });
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
  listHidden = new Set();
  showOnly = null;
  staleViews = new Map();
  fillsFeed = [];
  listCut = false;
  orderUrls = [];
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
    // The alert names the CLI's refusal, so a refusal the bot cannot fix (R3T-5) is not silent.
    assert.ok(alertsOf(dir).some((a) => a.reason === 'placement' && /decimals come from the indexer alone/.test(a.message)));
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

// R3T-2: an order anyone made under the bot's key is not the bot's.
test('an uncancellable order someone else made under the bot\'s key does not freeze quoting (R3T-2)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    for (let i = 0; i < 3; i++) await bot.cycle();
    const mine = Object.keys(chain().orders);
    assert.equal(mine.length, 6);
    // A custody-less ask at 1 sompi with a huge amount, under the bot's key.
    const fake = 'fe'.repeat(32);
    const c = chain();
    c.orders[fake] = { side: 1, price: 1, amount: 1e15, value: 1e8, txid: '77'.repeat(32), expiresAt: null, foreign: true };
    writeFileSync(file, JSON.stringify(c));
    await bot.cycle();
    assert.equal(bot.state.orders[fake], undefined, 'not taken for the bot\'s own');
    // Fair moves: every bid is amended, none skipped as a self-trade against the fake ask.
    const before = calls().length;
    kasUsd = 0.025 / 1.004;
    await bot.cycle();
    const amended = calls().slice(before).filter((a) => a[0] === 'replace').map((a) => a[1]);
    for (const id of mine) assert.ok(amended.includes(id), `order ${id} amended`);
    // An ask the bot cancelled is placed again: the fake's amount fills no cap.
    const c2 = chain();
    const ask = mine.find((id) => c2.orders[id].side === 1);
    delete c2.orders[ask];
    c2.ended[ask] = 'cancelled';
    bot.state.orders[ask].cancelSentAt = Date.now();
    writeFileSync(file, JSON.stringify(c2));
    const placedBefore = places(calls).length;
    for (let i = 0; i < 2; i++) await bot.cycle();
    assert.ok(places(calls).length > placedBefore, 'the empty ask level is placed again');
    // Its cancel is tried a bounded number of times, and one alert names it.
    for (let i = 0; i < 6; i++) {
      for (const f of Object.values(bot.state.foreign ?? {})) if (f.lastTry) f.lastTry -= 600_000;
      await bot.cycle();
    }
    const tries = readFileSync(process.env.FAKE_KOB_LOG, 'utf8').trim().split('\n').map((l) => JSON.parse(l)).filter((a) => a[0] === 'cancel' && a[1] === fake);
    assert.equal(bot.state.foreign[fake].tries, 3, 'tried three times');
    assert.ok(tries.length >= 3 && tries.length <= 6, `bounded cancel calls (${tries.length}: a dry run and a send each at most)`);
    assert.equal(alertsOf(dir).filter((a) => a.reason === 'foreign-order').length, 1, 'one alert');
    // The bot's own orders still pass the self-trade check.
    assert.equal(bot.crossesOwn({ kind: 'place', side: 'buy', price: 1e12 }) != null, true);
  } finally {
    reset();
  }
});

test('a retired token\'s order whose cancel keeps failing is given up after three tries, with one alert (R3T-2)', async () => {
  reset();
  const { bot, dir } = setup({ orderVisibleTimeoutSec: 0 });
  const OLD = '0f'.repeat(32);
  const fake = { id: 'fe'.repeat(32), side: 'sell', price: 1, amountLeft: 1e15 };
  bot.cfg.retiredTokens = [OLD];
  bot.maker = MAKER;
  bot.idx.liveOrders = async () => [fake];
  let n = 0;
  bot.kob.cancel = async () => { n++; return { ok: false, error: 'exit 1: error: custody not found on the node: not cancelled' }; };
  const bal = { tokens: { [OLD]: { free: 0, freeUtxos: 0, decimals: 6 } } };
  for (let i = 0; i < 6; i++) await bot.retire(bal, false);
  assert.equal(n, 3, 'three tries, then none');
  assert.equal(bot.state.retired[OLD].done, true, 'the clean-up is not held by it');
  assert.equal(alertsOf(dir).filter((a) => a.reason === 'retired-cancel-failed').length, 1);
  reset();
});

// R4T-3: the list is read for the quoted token only, and a list cut at its page cap is no proof
// that the indexer hides an order.
test('a maker list cut at its page cap does not cancel the quotes as hidden (R4T-3)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup();
  try {
    for (let i = 0; i < 3; i++) await bot.cycle();
    assert.equal(Object.keys(chain().orders).length, 6);
    assert.ok(orderUrls.every((u) => u.includes(`&token=${TOKEN}`)), 'the list asks for the token');
    listCut = true;
    orderUrls = [];
    const before = calls().length;
    await bot.cycle();
    assert.equal(orderUrls.length, 10, 'at most 10 pages of 100');
    assert.equal(Object.keys(chain().orders).length, 6, 'no quote cancelled');
    assert.ok(!calls().slice(before).some((a) => a[0] === 'cancel' || (a[0] === 'recover' && a.includes('--cancel'))));
    assert.equal(alertsOf(dir).filter((a) => a.reason === 'indexer-hides').length, 0);
  } finally {
    reset();
  }
});

test('a retired token\'s cancels are bounded per cycle, and its failure counts cover only listed orders (R4T-4)', async () => {
  reset();
  const { bot } = setup({ orderVisibleTimeoutSec: 0 });
  const OLD = '0f'.repeat(32);
  const many = Array.from({ length: 50 }, (_, i) => ({ id: (i + 16).toString(16).padStart(64, 'e'), side: 'sell', price: 1, amountLeft: 1e15 }));
  bot.cfg.retiredTokens = [OLD];
  bot.maker = MAKER;
  let listed = many;
  bot.idx.liveOrders = async () => listed;
  let n = 0;
  bot.kob.cancel = async () => { n++; return { ok: false, error: 'exit 1: error: custody not found on the node: not cancelled' }; };
  const bal = { tokens: { [OLD]: { free: 0, freeUtxos: 0, decimals: 6 } } };
  await bot.retire(bal, false);
  assert.equal(n, 2, 'two cancels a cycle, as for the quoted token');
  for (let i = 0; i < 80; i++) await bot.retire(bal, false);
  assert.equal(n, 150, 'each order tried three times in all');
  assert.equal(bot.state.retired[OLD].done, true);
  // The orders are gone from the list: their failure counts go too.
  listed = [];
  await bot.retire(bal, false);
  assert.deepEqual(bot.state.retired[OLD].failed, {});
  // A slow CLI: no new cancel starts once the cycle's retired budget is spent.
  listed = many.slice(0, 5);
  bot.state.retired[OLD] = { done: false, cancels: {}, failed: {} };
  n = 0;
  const realNow = Date.now;
  let t = realNow();
  Date.now = () => t;
  bot.kob.cancel = async () => { n++; t += 60_000; return { ok: false, error: 'timeout' }; };
  try {
    await bot.retire(bal, false);
  } finally {
    Date.now = realNow;
  }
  assert.equal(n, 1, 'one call past the budget, then none this cycle');
  reset();
});

// R3T-3: an ask the node cannot find is kept until there is proof it ended.
test('a partly filled ask hidden by the indexer is kept, counted and not placed again; the indexer\'s per-order view finds it (R3T-3)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const id = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
    // Someone fills 7 base units, an amount k x minFill does not reach; the indexer hides it.
    c.orders[id].amount -= 7;
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([id]);
    const placed = places(calls).length;
    for (let i = 0; i < 3; i++) await bot.cycle();
    assert.ok(bot.state.orders[id], 'not forgotten');
    assert.equal(bot.state.orders[id].unconfirmed, true);
    assert.equal(places(calls).length, placed, 'its level is not placed again');
    assert.equal(alertsOf(dir).filter((a) => a.reason === 'node-unconfirmed').length, 1, 'one alert');
    // Its record goes to kob recover with every hint the bot has; the indexer's per-order view
    // names the amount, the node confirms it, and the order is cancelled through the node.
    hiddenIds = new Set();
    listHidden = new Set([id]);
    await bot.cycle();
    assert.equal(chain().orders[id], undefined, 'found and cancelled on chain');
    assert.equal(bot.state.orders[id]?.unconfirmed, false);
  } finally {
    reset();
  }
});

test('an ask the node cannot find is not forgotten on that alone; a bid not found is spent and ends (R3T-3)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const ask = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
    const bid = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 2);
    // Both gone from the node (filled), and the indexer lists neither.
    delete c.orders[ask];
    delete c.orders[bid];
    c.ended[ask] = 'filled';
    c.ended[bid] = 'filled';
    writeFileSync(file, JSON.stringify(c));
    for (let i = 0; i < 2; i++) await bot.cycle();
    assert.equal(bot.state.orders[bid], undefined, 'a bid\'s script never changes: not found is spent');
    assert.ok(bot.state.orders[ask], 'an ask may sit at an amount the search did not reach: kept');
    assert.equal(bot.state.orders[ask].unconfirmed, true);
    // STOP pulls it too: a cancel through the node is tried, and STOP waits for it.
    writeFileSync(bot.p.stop, 'stop\n');
    assert.notEqual(await bot.cycle(), 'stopped');
    const recent = readFileSync(process.env.FAKE_KOB_LOG, 'utf8').trim().split('\n').map((l) => JSON.parse(l)).slice(-20);
    // kob recover --cancel from its record finds nothing, then kob cancel <id> (checked on the node).
    assert.ok(recent.some((a) => a[0] === 'recover' && a.includes('--cancel')));
    assert.ok(recent.some((a) => a[0] === 'cancel' && a[1] === ask), 'its cancel is tried');
    assert.ok(bot.state.orders[ask], 'and STOP waits for it');
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

// R3T-3 follow-up: a fully filled ask is proven by a payment the node shows, never by the
// indexer's word alone (and kept as proven, R5T-1); an unproven one gives its level back at its
// own expiry.
async function filledAsk(bot, chain) {
  for (let i = 0; i < 3; i++) await bot.cycle();
  const c = chain();
  const id = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
  const o = c.orders[id];
  // A taker buys it all in one transaction; the order leaves the node and the indexer's list.
  delete c.orders[id];
  c.ended[id] = 'filled';
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify(c));
  hiddenIds = new Set([id]);
  const txid = '9a'.repeat(32);
  fillsFeed = [{ id: 1, covenant_id: id, txid, amount: String(o.amount), price: String(o.price) }];
  // What the maker receives: ceil(amount x (price - tip) / scale), the carriers on top.
  const proceeds = Math.ceil((o.amount * o.price) / 1e6);
  return { id, o, txid, proceeds };
}

test('a fully filled ask whose payment the node shows is proven in the same cycle and gives its level back (R3T-3, R5T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds + 2e8 }];
    const placed = places(calls).length;
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven, 'proven on the payment, and kept');
    assert.equal(alertsOf(dir).filter((a) => a.reason === 'node-unconfirmed').length, 0, 'no alert');
    await bot.cycle();
    assert.ok(places(calls).length > placed, 'its level is placed again');
  } finally {
    reset();
  }
});

test('fills the indexer reports with no payment on the node do not end an ask (R3T-3)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    // No output, then one that pays less than the proceeds, then the bot's own transaction.
    await bot.cycle();
    assert.ok(bot.state.orders[id]);
    bot.nodeOutputs = async () => [{ txid, index: 0, amount: proceeds - 1 }];
    await bot.cycle();
    assert.ok(bot.state.orders[id], 'a payment short of the proceeds is no proof');
    const own = bot.state.ownTx.at(-1);
    fillsFeed = [{ id: 2, covenant_id: id, txid: own, amount: fillsFeed[0].amount, price: fillsFeed[0].price }, ...fillsFeed];
    bot.nodeOutputs = async () => [{ txid: own, index: 0, amount: 1e15 }];
    await bot.cycle();
    assert.ok(bot.state.orders[id], 'the bot\'s own transaction is no payment');
    assert.equal(bot.state.orders[id].unconfirmed, true);
  } finally {
    reset();
  }
});

test('a payment the bot saw on the node and later spent still proves the fill (R3T-3)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    const feed = fillsFeed;
    fillsFeed = []; // the indexer lags: no fill yet
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    await bot.cycle();
    assert.ok(bot.state.orders[id], 'no fill reported yet: kept');
    bot.nodeOutputs = async () => []; // the bot spent it since
    fillsFeed = feed;
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven, 'the recorded payment counts');
  } finally {
    reset();
  }
});

// R4T-2: a payment proves a fill only once, and only when the node first showed it after the
// ask's last placement or amend.
test('a payment the node showed before the ask was placed proves nothing (R4T-2)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    // An operator top-up, on the node from the start.
    const topUp = '7c'.repeat(32);
    bot.nodeOutputs = async () => [{ txid: topUp, index: 0, amount: 1e15 }];
    const { id } = await filledAsk(bot, chain);
    fillsFeed = [{ ...fillsFeed[0], txid: topUp }];
    for (let i = 0; i < 2; i++) await bot.cycle();
    assert.ok(bot.state.orders[id], 'kept: the payment predates the ask');
    assert.equal(bot.state.orders[id].unconfirmed, true);
  } finally {
    reset();
  }
});

test('a payment record proves one fill once (R4T-2)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid } = await filledAsk(bot, chain);
    // One payment output big enough for every ask.
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: 1e15 }];
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven, 'the first ask is proven by it');
    // A second ask leaves the node; the indexer names the same payment for its whole amount.
    const c = chain();
    const b = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1 && bot.state.orders[k]);
    const amount = bot.state.orders[b].amount;
    delete c.orders[b];
    c.ended[b] = 'filled';
    writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify(c));
    hiddenIds = new Set([id, b]);
    fillsFeed = [{ id: 2, covenant_id: b, txid, amount: String(amount), price: String(bot.state.orders[b].price) }, ...fillsFeed];
    for (let i = 0; i < 2; i++) await bot.cycle();
    assert.ok(bot.state.orders[b], 'kept: the payment already proved a fill');
    assert.equal(bot.state.orders[b].unconfirmed, true);
    // Saved with the state file: a restart does not forget it.
    const again = new Bot(bot.cfg);
    assert.ok(Object.values(again.state.payments).some((p) => p.usedBy === id));
  } finally {
    reset();
  }
});

// R4T-5: more than one page of fills between two reads is read back to the last one seen.
test('a fill behind 150 newer fills of the token is still recorded and proves the ask (R4T-5)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    const noise = Array.from({ length: 150 }, (_, i) => ({ id: 151 - i, covenant_id: 'ee'.repeat(32), txid: '5e'.repeat(32), amount: '1', price: '1' }));
    fillsFeed = [...noise, ...fillsFeed];
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven, 'its fill was read and proven');
    assert.equal(bot.state.lastFillId, 151);
  } finally {
    reset();
  }
});

// R4T-5: the operator drops an unresolved ask by id, after the node was asked, never while it is live.
test('an unresolved ask is dropped by the operator only when the node does not show it live (R4T-5)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    const { id } = await filledAsk(bot, chain);
    await bot.cycle();
    assert.equal(bot.state.orders[id].unconfirmed, true);
    // A second ask partly filled by an odd amount and hidden: live, at an amount the bot does not know.
    const c = chain();
    const b = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1 && k !== id);
    c.orders[b].amount -= 7;
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([id, b]);
    await bot.cycle();
    assert.deepEqual(bot.unresolvedOrders().map((o) => o.id).sort(), [id, b].sort());
    // mm-bot unresolved writes the records it lists for kob recover, whatever the last cycle looked up (R5T-I3).
    const cfgFile = join(dir, 'config.json');
    writeFileSync(cfgFile, JSON.stringify({ network: 'testnet-10', keyFile: 'bot.key', dataDir: dir, indexer: 'http://indexer.test', token: { covenantId: TOKEN, decimals: 6 } }));
    const u = spawnSync(process.execPath, [fileURLToPath(new URL('../bin/mm-bot.mjs', import.meta.url)), 'unresolved', '--config', cfgFile], { encoding: 'utf8', timeout: 30_000 });
    assert.equal(u.status, 0, u.stderr);
    assert.ok(u.stdout.includes(paths(dir).unresolved), u.stdout);
    assert.deepEqual(JSON.parse(readFileSync(paths(dir).unresolved, 'utf8')).items.map((i) => i.covenant_id).sort(), [id, b].sort());
    // Told its amount, the node finds it live: refused, and kept.
    const live = await bot.operatorDrop(b, { amountLeft: chain().orders[b].amount });
    assert.equal(live.ok, false);
    assert.equal(live.node.status, 'live');
    assert.ok(bot.state.orders[b]);
    // A tracked order that is not unresolved is refused too.
    const normal = Object.keys(bot.state.orders).find((k) => !bot.state.orders[k].unconfirmed && !bot.state.orders[k].parked);
    assert.equal((await bot.operatorDrop(normal)).ok, false);
    // The filled one: not found on the node, but the indexer reported a fill of it since its
    // placement, so it may sit at an amount the search did not reach: refused without --force (R5T-3).
    const doubt = await bot.operatorDrop(id);
    assert.equal(doubt.ok, false);
    assert.match(doubt.message, /the indexer reported 1 fill\(s\) of it since its last placement or amend/);
    assert.match(doubt.message, /--force/);
    assert.match(doubt.message, /still live stays fillable, and the bot no longer cancels it/);
    assert.ok(bot.state.orders[id], 'kept');
    // Under a template not proven on the node, not finding it proves nothing: refused too.
    const kinds = bot.state.templates.kinds;
    bot.state.templates.kinds = { KobAsk: { hash: kinds.KobAsk.hash, verified: false } };
    const unproven = await bot.operatorDrop(id, { amountLeft: 5 });
    assert.equal(unproven.ok, false);
    assert.match(unproven.message, /template not yet proven on the node/);
    bot.state.templates.kinds = kinds;
    // Forced: dropped, saved and logged, and its record kept for kob recover.
    const gone = await bot.operatorDrop(id, { force: true });
    assert.equal(gone.ok, true, gone.message);
    assert.equal(gone.node.status, 'gone');
    assert.match(gone.message, /still live stays fillable/);
    assert.ok(gone.message.includes(`kob recover --from ${paths(dir).dropped}`), gone.message);
    assert.equal(bot.state.orders[id], undefined);
    assert.equal(new Bot(bot.cfg).state.orders[id], undefined, 'saved');
    assert.ok(readFileSync(paths(dir).actions, 'utf8').includes('"operator-drop"'));
    const kept = JSON.parse(readFileSync(paths(dir).dropped, 'utf8')).items;
    assert.equal(kept.length, 1);
    assert.equal(kept[0].covenant_id, id);
    assert.equal(kept[0].state.kind, 'KobAsk', 'the record kob recover reads');
    assert.equal(kept[0].dropped.forced, true);
    assert.match(kept[0].dropped.why, /forced past/);
    assert.ok(kept[0].dropped.at && kept[0].dropped.amount > 0 && kept[0].dropped.fills.length === 1);
  } finally {
    reset();
  }
});

test('an unproven ask past its own expiry is parked: still counted, and STOP does not wait for it (R3T-3)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, o } = await filledAsk(bot, chain);
    await bot.cycle();
    assert.equal(bot.state.orders[id].unconfirmed, true);
    const placed = places(calls).length;
    await bot.cycle();
    assert.equal(places(calls).length, placed, 'before its expiry the level stays blocked');
    bot.state.orders[id].expiresAt = Date.now() - 1000;
    await bot.cycle();
    await bot.cycle();
    assert.ok(bot.state.orders[id], 'still kept');
    assert.equal(bot.state.orders[id].parked, true);
    assert.ok(alertsOf(dir).some((a) => a.reason === 'unresolved-expired'));
    // Its tokens still count toward the ask cap.
    const on = bot.ordersNow().filter((x) => x.side === 'sell').reduce((a, x) => a + x.amountLeft, 0);
    assert.ok(on >= o.amount);
    writeFileSync(bot.p.stop, 'stop\n');
    let r;
    for (let i = 0; i < 4 && r !== 'stopped'; i++) {
      r = await bot.cycle();
      for (const s of Object.values(bot.state.orders)) if (s.pending) s.pending.since -= 200_000;
    }
    assert.equal(r, 'stopped');
    assert.ok(bot.state.orders[id], 'kept for the operator');
  } finally {
    rmSync(bot.p.stop, { force: true });
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

// R5T-1: a payment proof is not bound to the ask, so it never makes the bot forget one. A paid
// ask is kept as proven: its level and cap share are free, it is still looked up and cancelled.
const provenIds = (bot) => Object.keys(bot.state.orders).filter((k) => bot.state.orders[k].proven);

test('a_payout_of_another_ask_never_proves_a_hidden_ask (R5T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const [a, b] = Object.keys(c.orders).sort().filter((k) => c.orders[k].side === 1);
    const A = c.orders[a];
    const placedA = bot.state.orders[a].amount;
    // Ask A is bought in part, 1,500,007 base units (payout P1), then 3 more (a fill the indexer
    // leaves out); ask B is bought in full (payout P2). Both payouts land after A's placement.
    // The indexer hides both asks and names P2 as the payment of the rest of A.
    const n1 = 1_500_007;
    A.amount -= n1 + 3;
    delete c.orders[b];
    c.ended[b] = 'filled';
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([a, b]);
    const p1 = '91'.repeat(32);
    const p2 = '92'.repeat(32);
    bot.nodeOutputs = async () => [{ txid: p1, index: 1, amount: Math.ceil((n1 * A.price) / 1e6) }, { txid: p2, index: 1, amount: 1e15 }];
    fillsFeed = [{ id: 2, covenant_id: a, txid: p2, amount: String(placedA - n1), price: String(A.price) }, { id: 1, covenant_id: a, txid: p1, amount: String(n1), price: String(A.price) }];
    await bot.cycle();
    // The forged proof frees A's level, but A is not forgotten.
    assert.ok(bot.state.orders[a], 'A is still tracked');
    assert.ok(bot.state.orders[a].proven, 'kept as proven');
    assert.ok(chain().orders[a], 'and still live on chain');
    assert.ok(bot.state.orders[b], 'B is kept too (its payout is used)');
    // The bot stops; the indexer's per-order view of A names its amount again: the rotation
    // lookup finds A live and cancels it through the node, in STOP as in any mode.
    writeFileSync(bot.p.stop, 'stop\n');
    hiddenIds = new Set([b]);
    listHidden = new Set([a]);
    await bot.cycle();
    assert.equal(chain().orders[a], undefined, 'found and cancelled on chain');
    assert.equal(chain().ended[a], 'cancelled');
    assert.ok(alertsOf(dir).some((x) => x.reason === 'proven-live'));
    for (const s of Object.values(bot.state.orders)) if (s.pending) s.pending.since -= 200_000;
    await bot.cycle();
    assert.equal(bot.state.orders[a], undefined, 'it ends once the node shows the cancel');
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

test('the amount left after each fill the indexer reported is a lookup hint: a hidden, partly filled ask is found (R5T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const a = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
    const A = c.orders[a];
    const placed = bot.state.orders[a].amount;
    // A true fill of an odd amount, then a fill the indexer makes up for the rest.
    const n1 = 1_500_007;
    A.amount -= n1;
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([a]);
    fillsFeed = [{ id: 2, covenant_id: a, txid: '92'.repeat(32), amount: String(placed - n1), price: String(A.price) }, { id: 1, covenant_id: a, txid: '91'.repeat(32), amount: String(n1), price: String(A.price) }];
    await bot.cycle();
    assert.equal(chain().ended[a], 'cancelled', 'found at the amount after the true fill, and cancelled');
    assert.ok(!bot.state.orders[a]?.proven);
  } finally {
    reset();
  }
});

test('a proven ask frees its level and cap share, and ends 2 h after its expiry, not before (R5T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    const placed = places(calls).length;
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven);
    assert.equal(bot.ordersNow().find((x) => x.id === id).proven, true, 'outside the ask cap');
    assert.equal(bot.unresolvedOrders().find((x) => x.id === id).proven, true, 'listed by mm-bot unresolved');
    await bot.cycle();
    assert.ok(places(calls).length > placed, 'its level is placed again');
    const h = PROVEN_KEEP_AFTER_EXPIRY_MS;
    assert.equal(h, 2 * 3_600_000);
    bot.state.orders[id].expiresAt = Date.now() - h + 60_000;
    await bot.cycle();
    assert.ok(bot.state.orders[id], 'kept until 2 h past its expiry');
    bot.state.orders[id].expiresAt = Date.now() - h - 1;
    await bot.cycle();
    assert.equal(bot.state.orders[id], undefined, 'ended 2 h past its expiry');
    assert.ok(readFileSync(paths(bot.cfg.dataDir).actions, 'utf8').includes('"proven, past its expiry"'));
  } finally {
    reset();
  }
});

/** `n` copies of proven record `id` under new ids, past their expiry, looked up longest ago. */
function cloneProven(bot, id, n) {
  const ids = [];
  for (let i = 0; i < n; i++) {
    const k = (0xf0000 + i).toString(16).padStart(64, 'e');
    bot.state.orders[k] = JSON.parse(JSON.stringify({ ...bot.state.orders[id], expiresAt: Date.now() - 1000, proven: { ...bot.state.orders[id].proven, lookedAt: i } }));
    ids.push(k);
  }
  return ids;
}

test('proven asks are looked up a few a cycle, in rotation (R5T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven);
    const all = [id, ...cloneProven(bot, id, 11)];
    const looked = [];
    const check = bot.nodeCheck.bind(bot);
    bot.nodeCheck = async (ids, h) => { looked.push(ids.filter((k) => all.includes(k))); return check(ids, h); };
    for (let i = 0; i < 3; i++) await bot.cycle();
    for (const l of looked) assert.ok(l.length <= PROVEN_LOOKUPS, `${l.length} proven lookups in one cycle`);
    assert.deepEqual([...new Set(looked.flat())].sort(), [...all].sort(), 'each looked up within ceil(12 / 5) cycles');
    assert.equal(provenIds(bot).length, 12, 'none dropped');
  } finally {
    reset();
  }
});

test('a full proven list frees no level on a new proof, and warns (R5T-1)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven);
    cloneProven(bot, id, PROVEN_MAX - 1);
    assert.equal(provenIds(bot).length, PROVEN_MAX);
    // A second ask filled in full and paid: no room left.
    const c = chain();
    const b = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1 && bot.state.orders[k] && !bot.state.orders[k].proven);
    const o = c.orders[b];
    delete c.orders[b];
    c.ended[b] = 'filled';
    writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify(c));
    hiddenIds = new Set([id, b]);
    const t2 = '9b'.repeat(32);
    fillsFeed = [{ id: 2, covenant_id: b, txid: t2, amount: String(bot.state.orders[b].amount), price: String(o.price) }, ...fillsFeed];
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }, { txid: t2, index: 1, amount: 1e15 }];
    await bot.cycle();
    await bot.cycle();
    assert.ok(!bot.state.orders[b].proven, 'not proven: the list is full');
    assert.equal(bot.state.orders[b].unconfirmed, true, 'its level stays blocked');
    assert.equal(alertsOf(dir).filter((x) => x.reason === 'proven-full').length, 1, 'one warning');
    assert.ok(!Object.values(bot.state.payments).some((p) => p.usedBy === b), 'its payment is not used');
  } finally {
    reset();
  }
});

test('an output of the bot\'s own transaction never counts as a payment, after its txid rolled out (R5T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id } = await filledAsk(bot, chain);
    // The change of one of the bot's own transactions, shown unspent on the node after the ask's
    // placement; its txid later rolls out of ownTx.
    const own = bot.state.ownTx.at(-1);
    bot.state.orders[id].ownAt = Date.now() - 60_000;
    bot.nodeOutputs = async () => [{ txid: own, index: 1, amount: 1e15 }];
    fillsFeed = []; // the indexer has named no fill yet
    await bot.cycle();
    assert.equal(bot.state.payments[`${own}:1`]?.own, true, 'recorded as own');
    bot.state.ownTx = [];
    fillsFeed = [{ id: 1, covenant_id: id, txid: own, amount: String(bot.state.orders[id].amount), price: String(bot.state.orders[id].price) }];
    for (let i = 0; i < 2; i++) await bot.cycle();
    assert.ok(bot.state.orders[id], 'kept');
    assert.ok(!bot.state.orders[id].proven, 'the own change output proves nothing');
    assert.equal(bot.state.orders[id].unconfirmed, true);
  } finally {
    reset();
  }
});

test('a state file of the previous format loads and nothing in it is dropped (R5T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid } = await filledAsk(bot, chain);
    await bot.cycle();
    assert.equal(bot.state.orders[id].unconfirmed, true);
    // The shape a661bb8c0 wrote: no proven or dropped records, fills without their time, a
    // payment record six days old that the node no longer shows.
    const old = JSON.parse(readFileSync(paths(bot.cfg.dataDir).state, 'utf8'));
    for (const s of Object.values(old.orders)) {
      delete s.proven;
      for (const f of s.fills ?? []) delete f.at;
    }
    const k = `${'7d'.repeat(32)}:0`;
    old.payments = { [k]: { txid: '7d'.repeat(32), sompi: 5e8, at: Date.now() - 6 * 86_400_000 } };
    delete old.dropped;
    writeFileSync(paths(bot.cfg.dataDir).state, JSON.stringify(old));
    const again = new Bot(bot.cfg);
    again.log.quiet = true;
    again.kob = bot.kob;
    again.nodeOutputs = async () => [];
    await again.cycle();
    for (const o of Object.keys(old.orders)) assert.ok(again.state.orders[o] || again.state.recent[o], `${o} kept, or ended on the node's word`);
    assert.ok(again.state.orders[id]?.unconfirmed, 'the unresolved ask is kept');
    assert.ok(again.state.payments[k], 'the old payment record is kept');
    assert.ok(again.hintsOf(id).length > 0);
    // Its payment shows up now: it is proven, not dropped.
    again.nodeOutputs = async () => [{ txid, index: 1, amount: 1e15 }];
    await again.cycle();
    assert.ok(again.state.orders[id]?.proven);
  } finally {
    reset();
  }
});

// R5T-1 follow-up: when a STOP (or an IDLE pull) begins, every proven ask gets one node lookup
// before the pull counts as finished, PROVEN_PASS_BATCH a cycle.
async function stopUntilDone(bot, max) {
  writeFileSync(bot.p.stop, 'stop\n');
  for (let i = 1; i <= max; i++) {
    const r = await bot.cycle();
    if (r === 'stopped') return i;
    for (const s of Object.values(bot.state.orders)) if (s.pending) s.pending.since -= 200_000;
  }
  return null;
}

test('a STOP looks up every proven ask: one live on the node is cancelled before the stop is reported done (R5T-1)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    const { id, o, txid, proceeds } = await filledAsk(bot, chain);
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven);
    // Ten other proven records the rotation reaches first, so that only the pass reaches this one.
    cloneProven(bot, id, 10);
    bot.state.orders[id].proven.lookedAt = Date.now();
    // The ask is live on the node after all (the indexer named a fill wrongly) and stays hidden.
    const c = chain();
    c.orders[id] = o;
    delete c.ended[id];
    writeFileSync(file, JSON.stringify(c));
    writeFileSync(bot.p.stop, 'stop\n');
    assert.notEqual(await bot.cycle(), 'stopped', 'the first STOP cycle does not end it');
    assert.equal(chain().ended[id], 'cancelled', 'the pass found it and cancelled it through the node');
    assert.ok(alertsOf(dir).some((x) => x.reason === 'proven-live'));
    const n = await stopUntilDone(bot, 6);
    assert.ok(n != null, 'the stop ends');
    assert.equal(bot.state.orders[id], undefined, 'it ended on the node\'s word before the stop was done');
    const log = readFileSync(paths(dir).log, 'utf8');
    assert.match(log, /stopped: no quote left that the node finds.*proven asks: 11 of 11 looked up on the node, 1 found live and cancelled, 10 not found \(listed in .*proven-not-found\.json\)/);
    assert.equal(JSON.parse(readFileSync(paths(dir).provenNotFound, 'utf8')).items.length, 10);
    // Never through the indexer's view.
    const cancels = readFileSync(process.env.FAKE_KOB_LOG, 'utf8').trim().split('\n').map((l) => JSON.parse(l)).filter((a) => a[0] === 'cancel' && a[1] === id);
    assert.deepEqual(cancels, []);
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

test('a STOP with 60 proven asks that are gone ends within the bounded number of cycles (R5T-1)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    await bot.cycle();
    cloneProven(bot, id, 59);
    const looked = [];
    const many = bot.cancelMany.bind(bot);
    bot.cancelMany = async (ids) => { looked.push(ids.filter((k) => bot.state.orders[k]?.proven).length); return many(ids); };
    const n = await stopUntilDone(bot, Math.ceil(60 / PROVEN_PASS_BATCH) + 2);
    assert.ok(n != null && n >= Math.ceil(60 / PROVEN_PASS_BATCH), `stopped after ${n} cycles`);
    assert.ok(Math.max(...looked) <= PROVEN_PASS_BATCH, 'at most one batch a call');
    assert.equal(Object.keys(bot.state.orders).filter((k) => bot.state.orders[k].proven).length, 60, 'none forgotten');
    assert.match(readFileSync(paths(dir).log, 'utf8'), /proven asks: 60 of 60 looked up on the node, 0 found live and cancelled, 60 not found/);
    assert.equal(JSON.parse(readFileSync(paths(dir).provenNotFound, 'utf8')).items.length, 60);
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

// R6T-2: an order ends on its cancel only once the node shows the cancel's own transaction (an
// output of it to the bot's address), never because the cancel was submitted.
const age = (bot) => { for (const s of Object.values(bot.state.orders)) if (s.pending) s.pending.since -= 200_000; };

test('a_cancel_that_never_confirmed_does_not_end_a_hidden_ask (R6T-2)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const a = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
    const left = c.orders[a].amount - 1_500_007;
    // The indexer hides live ask A; the bot finds it on the node and cancels it. The cancel is
    // submitted, but a fill of an odd amount lands first: A is live at amount - 1,500,007, and
    // no output of the cancel ever shows. The indexer reports no fill.
    hiddenIds = new Set([a]);
    bot.kob.env.FAKE_KOB_CANCEL_LOST = '1500007';
    await bot.cycle();
    assert.ok(bot.state.orders[a].cancelSentAt, 'the cancel went out');
    assert.equal(chain().orders[a].amount, left, 'and a fill landed instead');
    delete bot.kob.env.FAKE_KOB_CANCEL_LOST;
    for (let i = 0; i < 2; i++) {
      age(bot);
      await bot.cycle();
    }
    assert.ok(bot.state.orders[a], 'A is still tracked');
    assert.equal(bot.state.orders[a].unconfirmed, true, 'unresolved, looked up each cycle');
    assert.ok(chain().orders[a], 'and live on chain');
    assert.ok(!bot.state.recent[a], 'not ended');
  } finally {
    reset();
  }
});

test('a cancel that confirms on the node ends the ask (R6T-2)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const a = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
    hiddenIds = new Set([a]);
    await bot.cycle();
    const tx = bot.state.orders[a].cancelTxids.at(-1);
    assert.equal(chain().ended[a], 'cancelled');
    age(bot);
    await bot.cycle();
    assert.equal(bot.state.orders[a], undefined, 'ended');
    assert.ok(bot.state.recent[a]);
    assert.ok(Object.values(bot.state.payments).some((p) => p.txid === tx), 'on the output of its cancel');
    assert.match(readFileSync(paths(bot.cfg.dataDir).log, 'utf8'), /ended: cancelled \(its cancel shows on the node\)/);
  } finally {
    reset();
  }
});

test('a cancel-replaced order is kept until the node shows the replace; one a fill displaced stays tracked (R6T-2)', { timeout: 120_000 }, async () => {
  for (const how of ['ok', 'lost']) {
    reset();
    const { bot, chain } = setup();
    try {
      bot.nodeOutputs = async () => [];
      for (let i = 0; i < 3; i++) await bot.cycle();
      const before = new Set(Object.keys(chain().orders));
      const asks = [...before].filter((k) => chain().orders[k].side === 1);
      // A move amends every quote; the CLI does it as a cancel-replace (strays on the orders).
      bot.kob.env.FAKE_KOB_REPLACE_NEW = how;
      kasUsd = 0.025 / 1.004;
      await bot.cycle();
      delete bot.kob.env.FAKE_KOB_REPLACE_NEW;
      for (const a of asks) assert.ok(bot.state.orders[a]?.replacedBy, `${a} kept, as replaced`);
      for (let i = 0; i < 2; i++) {
        age(bot);
        await bot.cycle();
      }
      if (how === 'ok') {
        for (const a of asks) assert.equal(bot.state.orders[a], undefined, 'ended once the node holds its replacement');
      } else {
        for (const a of asks) {
          assert.ok(chain().orders[a], 'live on chain, filled in part');
          assert.ok(bot.state.orders[a], 'still tracked');
          assert.equal(bot.state.orders[a].replacedBy, null);
        }
        const ghosts = Object.keys(bot.state.orders).filter((k) => !before.has(k) && !chain().orders[k]);
        assert.deepEqual(ghosts, [], 'the replacements that never went out end');
      }
    } finally {
      delete bot.kob.env.FAKE_KOB_REPLACE_NEW;
      reset();
    }
  }
});

// R6T-1: every partial fill of an ask pays the bot an output the node shows, so the amount left
// after it can be found from the payment alone, whatever the indexer reports.
test('a_lie_about_the_first_fill_leaves_a_proven_ask_unfound (R6T-1): the payment names the true amount, and the ask is cancelled before the stop is done', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const [a, b] = Object.keys(c.orders).sort().filter((k) => c.orders[k].side === 1);
    const A = c.orders[a];
    const placedA = bot.state.orders[a].amount;
    // A is bought in part, n1 base units (payout P1, exactly allIn); B in full (payout P2).
    const n1 = 1_500_007;
    A.amount -= n1;
    delete c.orders[b];
    c.ended[b] = 'filled';
    writeFileSync(file, JSON.stringify(c));
    // The indexer hides both, for good, and lies about A's first fill: n1 - 5, the rest paid by P2.
    hiddenIds = new Set([a, b]);
    const p1 = '91'.repeat(32);
    const p2 = '92'.repeat(32);
    bot.nodeOutputs = async () => [{ txid: p1, index: 1, amount: Math.ceil((n1 * A.price) / 1e6) }, { txid: p2, index: 1, amount: 1e15 }];
    fillsFeed = [{ id: 2, covenant_id: a, txid: p2, amount: String(placedA - n1 + 5), price: String(A.price) }, { id: 1, covenant_id: a, txid: p1, amount: String(n1 - 5), price: String(A.price) }];
    // B (no fill reported) stays unresolved; past its expiry the stop does not wait for it.
    bot.state.orders[b].expiresAt = Date.now() - 1000;
    const n = await stopUntilDone(bot, 8);
    assert.ok(n != null, 'the stop ends');
    assert.equal(chain().ended[a], 'cancelled', 'A was found at the amount P1 names and cancelled through the node');
    assert.equal(bot.state.orders[a], undefined, 'and ended on its confirmed cancel');
    assert.equal(bot.state.payments[`${p1}:1`].usedBy, a, 'P1 is marked as its fill');
    assert.match(readFileSync(paths(dir).log, 'utf8'), /from the payment\(s\) 9191.*not from the indexer/);
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

test('a proven ask is found by a payment the node shows after the proof (R6T-1)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, dir } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const [a, b] = Object.keys(c.orders).sort().filter((k) => c.orders[k].side === 1);
    const A = c.orders[a];
    const placedA = bot.state.orders[a].amount;
    const n1 = 1_500_007;
    A.amount -= n1;
    delete c.orders[b];
    c.ended[b] = 'filled';
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([a, b]);
    const p1 = '91'.repeat(32);
    const p2 = '92'.repeat(32);
    // The indexer names B's payout as the fill of all of A; A's own payout is not shown yet.
    bot.nodeOutputs = async () => [{ txid: p2, index: 1, amount: 1e15 }];
    fillsFeed = [{ id: 1, covenant_id: a, txid: p2, amount: String(placedA), price: String(A.price) }];
    await bot.cycle();
    assert.ok(bot.state.orders[a].proven, 'proven on the forged fill');
    bot.nodeOutputs = async () => [{ txid: p2, index: 1, amount: 1e15 }, { txid: p1, index: 1, amount: Math.ceil((n1 * A.price) / 1e6) }];
    await bot.cycle();
    assert.equal(chain().ended[a], 'cancelled', 'the rotation finds it at the amount P1 names');
    assert.ok(alertsOf(dir).some((x) => x.reason === 'proven-live'));
  } finally {
    reset();
  }
});

test('two odd fills the indexer never reports: the sum of their payments finds the ask (R6T-1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const a = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
    const A = c.orders[a];
    const n1 = 1_500_007;
    const n2 = 2_300_011;
    A.amount -= n1 + n2;
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([a]);
    const pay = (n) => Math.ceil((n * A.price) / 1e6);
    bot.nodeOutputs = async () => [{ txid: '93'.repeat(32), index: 1, amount: pay(n1) }, { txid: '94'.repeat(32), index: 1, amount: pay(n2) }];
    await bot.cycle();
    assert.equal(chain().ended[a], 'cancelled', 'found at amount - n1 - n2 and cancelled');
    assert.ok(!bot.state.orders[a]?.unconfirmed);
  } finally {
    reset();
  }
});

test('a payment that could pay for more than 200 fill amounts is skipped as a hint, with a log line (R6T-1)', () => {
  reset();
  const { bot, dir } = setup();
  bot.nodeOutputs = async () => [];
  const id = 'f1'.repeat(32);
  // 1,000 sompi per whole token of 10^6 base units: a base unit is worth 0.001 sompi.
  bot.state.orders[id] = { side: 'sell', level: 0, price: 1000, amount: 50_000_000, ownAmount: 50_000_000, amountLeft: 50_000_000, ownAt: 1000,
    own: { state: { kind: 'KobAsk', state: { price: '1000', scale: '1000000', amountLeft: '50000000' } } } };
  bot.state.payments = { [`${'95'.repeat(32)}:0`]: { txid: '95'.repeat(32), sompi: 7, at: 2000 } };
  assert.deepEqual(bot.payHints(id), [], 'n in (6,000, 7,000]: 1,000 amounts, skipped');
  assert.match(readFileSync(paths(dir).log, 'utf8'), /1 payment\(s\) to the bot not used as lookup hints: each could pay for more than 200 different fill amounts/);
  // At 4,000 sompi a base unit, the same kind of payment names one amount.
  bot.state.orders[id].own.state.state.price = '4000000000';
  bot.state.payments = { [`${'95'.repeat(32)}:0`]: { txid: '95'.repeat(32), sompi: 4000 * 1_500_007, at: 2000 } };
  assert.deepEqual(bot.payHints(id), [50_000_000 - 1_500_007]);
});

// R6T-3: without --force, mm-bot drop takes an ask only on the node's word that it ended, or past
// the end rule (2 h past its expiry).
test('drop refuses, without --force, an ask the node cannot find when no fill was reported (R6T-3)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const a = Object.keys(c.orders).sort().find((k) => c.orders[k].side === 1);
    // A fill of an odd amount the indexer never reports; it hides the ask too.
    c.orders[a].amount -= 7;
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([a]);
    await bot.cycle();
    assert.equal(bot.state.orders[a].unconfirmed, true);
    const r = await bot.operatorDrop(a);
    assert.equal(r.ok, false, r.message);
    assert.equal(r.node.status, 'gone');
    assert.match(r.message, /the indexer reported no fill of it and no cancel the bot sent shows on the node.*the most suspicious case.*--force/);
    assert.ok(bot.state.orders[a], 'kept');
    // Past the end rule (2 h after its expiry) it drops without --force.
    bot.state.orders[a].expiresAt = Date.now() - PROVEN_KEEP_AFTER_EXPIRY_MS - 1000;
    const late = await bot.operatorDrop(a);
    assert.equal(late.ok, true, late.message);
    assert.equal(bot.state.orders[a], undefined);
  } finally {
    reset();
  }
});

// R6T-6: in a STOP or IDLE pull the pull's own cancels go out before the pass over the proven asks.
test('a STOP sends the pull\'s cancels before the pass over the proven asks (R6T-6)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  try {
    bot.nodeOutputs = async () => [];
    const { id, txid, proceeds } = await filledAsk(bot, chain);
    bot.nodeOutputs = async () => [{ txid, index: 1, amount: proceeds }];
    await bot.cycle();
    assert.ok(bot.state.orders[id].proven);
    cloneProven(bot, id, 5);
    const seq = [];
    const exec = bot.execute.bind(bot);
    bot.execute = async (actions) => { if (actions.some((x) => x.kind === 'cancel')) seq.push('pull'); return exec(actions); };
    const step = bot.provenPassStep.bind(bot);
    bot.provenPassStep = async (...a) => { seq.push('pass'); return step(...a); };
    writeFileSync(bot.p.stop, 'stop\n');
    await bot.cycle();
    assert.deepEqual(seq.slice(0, 2), ['pull', 'pass']);
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

// Whole-fill asks (quotes.askPartialFills false): every ask's minimum fill is its amount, so it
// is filled in full or not at all, and the node alone says whether it is live.
const WHOLE = { askPartialFills: false };
const askIds = (bot) => Object.keys(bot.state.orders).filter((k) => bot.state.orders[k].side === 'sell');

test('whole fills: every ask is placed with its minimum fill at its amount, and an amend keeps it there', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain, calls } = setup({}, WHOLE);
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const sells = calls().filter((a) => a[0] === 'place' && a[a.indexOf('--side') + 1] === 'sell');
    assert.equal(sells.length, 3);
    for (const a of sells) assert.equal(a[a.indexOf('--min-fill') + 1], a[a.indexOf('--amount') + 1], 'minimum fill = amount');
    assert.ok(calls().filter((a) => a[0] === 'place' && a[a.indexOf('--side') + 1] === 'buy').every((a) => !a.includes('--min-fill')), 'bids unchanged');
    for (const id of askIds(bot)) assert.ok(bot.isWhole(bot.state.orders[id]));
    // A move amends every quote in place: the replacement keeps minFill = amount left.
    const before = calls().length;
    kasUsd = 0.025 / 1.004;
    await bot.cycle();
    assert.ok(calls().slice(before).some((a) => a[0] === 'replace'));
    for (const id of askIds(bot)) {
      const st = bot.state.orders[id].own.state.state;
      assert.equal(st.minFill, st.amountLeft);
      assert.equal(Number(st.price), chain().orders[id].price, 'the amended state');
      assert.ok(bot.isWhole(bot.state.orders[id]));
    }
    assert.deepEqual(bot.hintsOf(askIds(bot)[0]), [], 'one candidate: its own state');
  } finally {
    reset();
  }
});

test('whole fills: the R6T-1 sketch cannot hide an ask; a hidden live one is cancelled, a filled one frees its level at once', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup({}, WHOLE);
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const c = chain();
    const [a, b] = Object.keys(c.orders).sort().filter((k) => c.orders[k].side === 1);
    const A = c.orders[a];
    const placedA = A.amount;
    // A cannot be bought in part. B is bought in full; the indexer hides both, reports a made-up
    // first fill of A and names B's payout for the rest of it.
    delete c.orders[b];
    c.ended[b] = 'filled';
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([a, b]);
    const p2 = '92'.repeat(32);
    bot.nodeOutputs = async () => [{ txid: p2, index: 1, amount: 1e15 }];
    fillsFeed = [{ id: 2, covenant_id: a, txid: p2, amount: String(placedA - 1_500_002), price: String(A.price) }, { id: 1, covenant_id: a, txid: '91'.repeat(32), amount: '1500002', price: String(A.price) }];
    const placed = places(calls).length;
    await bot.cycle();
    assert.equal(chain().ended[a], 'cancelled', 'A is found at its amount and cancelled by the normal cycle');
    assert.equal(bot.state.orders[b], undefined, 'B ended in the same cycle, with no payment proof');
    assert.ok(bot.state.recent[b]);
    assert.ok(!Object.values(bot.state.payments).some((p) => p.usedBy), 'no payment was used');
    assert.ok(!Object.values(bot.state.orders).some((s) => s.proven || s.unconfirmed || s.parked), 'nothing proven, unresolved or parked');
    assert.equal(alertsOf(dir).filter((x) => x.reason === 'node-unconfirmed').length, 0);
    assert.match(readFileSync(paths(dir).log, 'utf8'), /ended: not on the node at its own state: a whole-fill ask cannot move/);
    await bot.cycle();
    assert.ok(places(calls).length > placed, 'the freed levels are placed again');
    const recovers = readFileSync(process.env.FAKE_KOB_LOG, 'utf8').trim().split('\n').map((l) => JSON.parse(l)).filter((x) => x[0] === 'recover');
    assert.ok(recovers.every((x) => !x.includes('--amount-left')), 'no hints: one candidate per ask');
  } finally {
    reset();
  }
});

test('whole fills: a hidden live ask is cancelled by STOP before it is done', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain } = setup({}, WHOLE);
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const asks = Object.keys(chain().orders).filter((k) => chain().orders[k].side === 1);
    hiddenIds = new Set(asks);
    fillsFeed = asks.map((k, i) => ({ id: i + 1, covenant_id: k, txid: '9c'.repeat(32), amount: '7', price: '1' }));
    const n = await stopUntilDone(bot, 6);
    assert.ok(n != null, 'the stop ends');
    for (const k of asks) assert.equal(chain().ended[k], 'cancelled');
    assert.equal(Object.keys(chain().orders).length, 0, 'nothing left on chain');
  } finally {
    rmSync(bot.p.stop, { force: true });
    reset();
  }
});

test('whole fills: an amend of unknown outcome or a new CLI build keeps the ask under the node-word rules', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup({}, WHOLE);
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const [a, b] = askIds(bot).sort();
    const s = bot.state.orders[a];
    assert.ok(bot.wholeGone(s), 'its own state, this build, a proven template');
    // An amend that may have gone out with a state the plan does not name: no whole-fill end.
    s.stateUnknown = true;
    assert.equal(bot.wholeGone(s), false);
    s.stateUnknown = false;
    s.pending = { kind: 'reread', since: Date.now() };
    assert.equal(bot.wholeGone(s), false);
    s.pending = null;
    // An order placed by another CLI build may sit under another template.
    bot.state.orders[b].own.cli = 'kob 9.9.9 (another build)';
    assert.equal(bot.wholeGone(bot.state.orders[b]), false);
    // A template the indexer names differently from the proven one.
    s.own.tpl = 'ee'.repeat(32);
    assert.equal(bot.wholeGone(s), false);
    // A failed amend that may have gone out is in flight (R7T-1): its planned state is looked up
    // beside the order's own, and there is no whole-fill end until the node settles it.
    s.own.tpl = null;
    bot.kob.amend = async () => ({ ok: false, error: 'exit 1: socket closed', sentFee: 500000, doc: { plan: { inPlace: true, replacement: { ...s.own.state, state: { ...s.own.state.state, price: '4100000000' } } }, transaction: { txid: 'fe'.repeat(32) } } });
    await bot.run1({ kind: 'amend', id: a, side: 'sell', level: s.level, from: s.price, price: 4100000000, quotedFair: 40, reason: 'test' });
    assert.equal(s.own.state.state.price, String(chain().orders[a].price), 'its own state stays the one the node held');
    assert.deepEqual(bot.statesOf(s).map((x) => x.state.price), [String(chain().orders[a].price), '4100000000'], 'both are looked up');
    assert.equal(s.inflight.at(-1).exact, false, 'the plan may be the dry run\'s');
    assert.equal(bot.wholeGone(s), false);
  } finally {
    reset();
  }
});

// R7T-1: a transaction of the bot's own is in flight until the node shows it. A timeout proves
// nothing, so an order whose placement or amend confirms late is never ended for it.
/** Mines what the fake's mempool holds: placements, then amends whose input is still unspent. */
function mine(file) {
  const c = JSON.parse(readFileSync(file, 'utf8'));
  for (const [id, o] of Object.entries(c.mempool?.place ?? {})) c.orders[id] = o;
  for (const a of c.mempool?.amend ?? []) {
    const o = c.orders[a.id];
    if (!o || o.txid !== a.input) continue; // its input is spent: it can never confirm
    Object.assign(o, { price: a.price, expiresAt: a.expiresAt, txid: a.txid, expiryDaa: a.expiryDaa, value: o.value - 500000 });
  }
  c.mempool = {};
  writeFileSync(file, JSON.stringify(c));
}

test('a_placement_that_confirms_after_the_timeout_stays_tracked_and_is_found (R7T-1)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup({}, WHOLE);
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 4; i++) await bot.cycle();
    assert.ok(bot.state.templates.kinds.KobAsk?.verified, 'the ask template is proven on the node');
    // One ask is bought in full: its level is free at once (whole fills).
    const c = chain();
    const gone = askIds(bot).sort()[0];
    delete c.orders[gone];
    c.ended[gone] = 'filled';
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([gone]);
    // Its level is placed again in the same cycle; that ask waits in the mempool past
    // orderVisibleTimeoutSec.
    const before = new Set(askIds(bot));
    bot.kob.env.FAKE_KOB_PENDING = '1';
    await bot.cycle();
    delete bot.kob.env.FAKE_KOB_PENDING;
    assert.equal(bot.state.orders[gone], undefined);
    const asks = askIds(bot).filter((k) => !before.has(k));
    assert.equal(asks.length, 1, 'one new ask');
    assert.equal(asks.filter((k) => chain().orders[k]).length, 0, 'not on chain yet');
    for (let i = 0; i < 3; i++) {
      age(bot);
      for (const k of asks) for (const e of bot.state.orders[k]?.inflight ?? []) e.since -= 200_000;
      await bot.cycle();
    }
    for (const k of asks) assert.ok(bot.state.orders[k], `${k.slice(0, 8)} still tracked`);
    assert.ok(alertsOf(dir).some((x) => x.reason === 'own-tx-unconfirmed'), 'one alert says why');
    const sells = () => calls().filter((a) => a[0] === 'place' && a[a.indexOf('--side') + 1] === 'sell').length;
    assert.equal(sells(), 4, 'its level is not placed twice');
    mine(file);
    await bot.cycle();
    for (const k of asks) {
      assert.ok(chain().orders[k], 'live on chain');
      assert.ok(bot.state.orders[k], 'and tracked');
      assert.deepEqual(bot.state.orders[k].inflight, [], 'the node settled its placement');
    }
  } finally {
    reset();
  }
});

test('an_amend_rolled_back_then_confirmed_leaves_the_ask_tracked_at_its_new_state (R7T-1)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain } = setup({}, WHOLE);
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const asks = askIds(bot);
    const old = Object.fromEntries(asks.map((k) => [k, chain().orders[k].price]));
    // A move amends every quote; the amends wait in the mempool past the timeout. The bot finds
    // the asks at their earlier terms ("not landed") and amends again; those wait too.
    bot.kob.env.FAKE_KOB_PENDING = '1';
    kasUsd = 0.025 / 1.004;
    await bot.cycle();
    for (let i = 0; i < 2; i++) {
      age(bot);
      await bot.cycle();
    }
    delete bot.kob.env.FAKE_KOB_PENDING;
    for (const k of asks) assert.equal(chain().orders[k].price, old[k], 'nothing landed yet');
    // The first amend of each confirms; the later ones spend the same input and never can.
    mine(file);
    for (let i = 0; i < 2; i++) {
      age(bot);
      await bot.cycle();
    }
    for (const k of asks) {
      assert.ok(chain().orders[k], 'live on chain');
      assert.ok(bot.state.orders[k], 'and tracked');
      assert.notEqual(chain().orders[k].price, old[k], 'at the amended price');
    }
  } finally {
    reset();
  }
});

// R7T-2: a cancel-replace's replacement is looked up every cycle until the node has held it, so a
// replacement filled in full a cycle later still settles the old record.
test('a cancel-replace whose replacement is filled in full later still ends the old record (R7T-2)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup();
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const asks = askIds(bot);
    // A lagging indexer keeps listing the old asks where they were, so their records are not
    // looked up before orderVisibleTimeoutSec; it shows none of the replacements.
    const was = chain();
    staleViews = new Map(asks.map((k) => [k, { covenant_id: k, contract: 'KobAsk', side: 1, token: TOKEN, scale: 1_000_000, template_hash: TPL.KobAsk, state_known: true,
      state: stateOf(was.orders[k]), price: String(was.orders[k].price), amount_left: String(was.orders[k].amount), current: { txid: was.orders[k].txid, index: 0, value: String(was.orders[k].value) }, status: 'open', expired: false }]));
    bot.kob.env.FAKE_KOB_REPLACE_NEW = 'ok';
    kasUsd = 0.025 / 1.004;
    await bot.cycle();
    delete bot.kob.env.FAKE_KOB_REPLACE_NEW;
    const repl = Object.fromEntries(asks.map((k) => [k, bot.state.orders[k].replacedBy]));
    hiddenIds = new Set(Object.values(repl));
    // The next cycle looks the replacements up, though the old records are not due yet.
    await bot.cycle();
    // Each replacement is now bought in full; the old records are checked only later.
    const c = chain();
    for (const n of Object.values(repl)) { delete c.orders[n]; c.ended[n] = 'filled'; }
    writeFileSync(file, JSON.stringify(c));
    staleViews = new Map();
    for (let i = 0; i < 2; i++) {
      age(bot);
      await bot.cycle();
    }
    for (const k of asks) {
      assert.equal(bot.state.orders[k], undefined, 'the old record ended on the node having held its replacement');
      assert.ok(bot.state.recent[k]);
    }
  } finally {
    reset();
  }
});

// R7T-5: the CLI build is read every cycle and before a drop, so a `kob` swapped under a running
// bot never lets the whole-fill end touch the orders the earlier build placed.
test('a kob binary swapped under the running bot: no whole-fill end for the earlier build\'s asks; drop reads the build too (R7T-5, R7T-I1)', { timeout: 120_000 }, async () => {
  reset();
  const { bot, chain } = setup({}, WHOLE);
  const file = process.env.FAKE_KOB_CHAIN;
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 4; i++) await bot.cycle();
    const a = askIds(bot).sort()[0];
    bot.kob.env.FAKE_KOB_VERSION = 'kob 9.9.9 (another build)';
    const c = chain();
    delete c.orders[a];
    c.ended[a] = 'filled';
    writeFileSync(file, JSON.stringify(c));
    hiddenIds = new Set([a]);
    await bot.cycle();
    assert.equal(bot.cliVersion, 'kob 9.9.9 (another build)', 'read again');
    assert.ok(bot.state.orders[a], 'not ended on a whole-fill lookup by another build');
    assert.ok(!bot.state.recent[a]);
    // The earlier build is back; a stopped bot's drop reads it and applies the whole-fill rule.
    delete bot.kob.env.FAKE_KOB_VERSION;
    const again = new Bot(bot.cfg);
    again.log.quiet = true;
    again.kob = bot.kob;
    again.nodeOutputs = async () => [];
    again.state.templates = bot.state.templates;
    again.state.templates.cli = VERSION_LINE;
    again.state.templates.kinds = { KobAsk: { hash: TPL.KobAsk, verified: true }, KobBid: { hash: TPL.KobBid, verified: true } };
    const r = await again.operatorDrop(a);
    assert.equal(r.ok, true, r.message);
  } finally {
    reset();
  }
});
// R8T-1: a stray on an order makes kob replace refuse for good; the bot cancels that order through
// the node after AMEND_REFUSALS refusals, with an alert, and places its level again.
test('a_refused_amend_with_strays_cancels_and_requotes (R8T-1)', { timeout: 180_000 }, async () => {
  reset();
  const { bot, chain, calls, dir } = setup();
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    const a = askIds(bot).sort()[0];
    const level = bot.state.orders[a].level;
    bot.kob.env.FAKE_KOB_STRAYS = a;
    const placed = places(calls).length;
    kasUsd = 0.025 / 1.004; // a move past repriceThreshold: every quote is amended
    for (let i = 0; i < 4 && chain().orders[a]; i++) await bot.cycle();
    assert.equal(chain().ended[a], 'cancelled', 'cancelled through the node, strays or not');
    const recovers = readFileSync(process.env.FAKE_KOB_LOG, 'utf8').trim().split('\n').map((l) => JSON.parse(l)).filter((x) => x[0] === 'recover' && x.includes('--cancel'));
    assert.ok(recovers.length >= 1);
    const al = alertsOf(dir).filter((x) => x.reason === 'amend-refused');
    assert.equal(al.length, 1, 'one alert');
    assert.match(al[0].message, /kob sweep/, 'it names the strays (the CLI\'s words)');
    for (let i = 0; i < 2; i++) {
      age(bot);
      await bot.cycle();
    }
    assert.equal(bot.state.orders[a], undefined, 'the old order ended on its confirmed cancel');
    assert.ok(places(calls).length > placed, 'its level is placed again');
    const c = chain();
    assert.ok(Object.entries(bot.state.orders).some(([k, s]) => k !== a && s.side === 'sell' && s.level === level && c.orders[k]), 'a new ask holds the level');
  } finally {
    delete bot.kob.env.FAKE_KOB_STRAYS;
    reset();
  }
});

// R8T-2: the real kob replace names the replacement's state in plan.replacement only.
test('cancel_replace_record_carries_the_replacement_state (R8T-2)', { timeout: 120_000 }, async () => {
  reset();
  const { bot } = setup();
  try {
    bot.nodeOutputs = async () => [];
    for (let i = 0; i < 3; i++) await bot.cycle();
    bot.kob.env.FAKE_KOB_REPLACE_NEW = 'ok';
    kasUsd = 0.025 / 1.004;
    await bot.cycle();
    const repl = askIds(bot).map((k) => bot.state.orders[k].replacedBy).filter(Boolean);
    assert.ok(repl.length > 0);
    for (const n of repl) {
      assert.equal(bot.state.orders[n].own?.state?.kind, 'KobAsk', 'its own state, from plan.replacement');
      assert.ok(!bot.state.orders[n].own.fromIndexer);
    }
  } finally {
    delete bot.kob.env.FAKE_KOB_REPLACE_NEW;
    reset();
  }
});

