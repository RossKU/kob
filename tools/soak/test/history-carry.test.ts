// Market history across a re-issue (src/history-carry.ts), on a database made from the indexer's own schema.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import {
  carryHistory,
  carryPairs,
  chartProblems,
  firstFillTs,
  historyFloors,
  importHistory,
  importPairs,
  pairMarkets,
  type SqlDb,
  type SqlSrc,
} from '../src/history-carry.ts';

const SCHEMA = readFileSync(new URL('../../../crates/kob-executor/src/indexer/schema.sql', import.meta.url), 'utf8');
const id = (c: string): string => c.repeat(64);
const b = (h: string): Uint8Array => Uint8Array.from(Buffer.from(h, 'hex'));
const OLD = { TUSD: id('1'), TETH: id('2'), TBTC: id('3') };
const NEW = { TUSD: id('a'), TETH: id('b'), TBTC: id('c') };
const reg = (m: Record<string, string>) => ({ tokens: Object.entries(m).map(([ticker, covenant_id]) => ({ ticker, covenant_id })) });

let seq = 0;
function order(db: DatabaseSync, cov: string, token: string, status: string, quote: string | null = null): void {
  db.prepare(
    `INSERT INTO orders (covenant_id, contract, template_hash, family, side, token_cov_id, scale, price, in_book, genesis_state, genesis_block, genesis_daa, listed, quote_cov_id)
     VALUES (?, ?, ?, 1, 1, ?, 100000000, 5000, 1, ?, 1, 1, 1, ?)`,
  ).run(b(cov), quote ? 'KobPair' : 'KobAsk', b(id('f')), b(token), b('00'), quote ? b(quote) : null);
  db.prepare('INSERT INTO order_state (covenant_id, status, filled_amount, state_known, last_block, last_daa) VALUES (?, ?, 0, 1, 1, 1)').run(b(cov), status);
}
function event(db: DatabaseSync, cov: string, token: string, kind: string, ts: number): void {
  db.prepare(
    `INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price) VALUES (?, 1, ?, ?, ?, 0, ?, ?, 1, 10, 5000)`,
  ).run(b(cov), ts, ts, b(id(String(++seq % 10))), kind, b(token));
}
function pairFill(db: DatabaseSync, cov: string, base: string, quote: string, ts: number): void {
  db.prepare(
    `INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, covenant_id, contract, base_cov_id, quote_cov_id, side, amount_a, a_scale, counterparty)
     VALUES (1, ?, ?, ?, 0, ?, 'KobPair', ?, ?, 1, 10, 100000000, 'route')`,
  ).run(ts, ts, b(id('9')), b(cov), b(base), b(quote));
}
const count = (db: DatabaseSync, sql: string, ...p: unknown[]): number => Number((db.prepare(sql).get(...(p as [])) as { n: number }).n);

function fixture(): DatabaseSync {
  const db = new DatabaseSync(':memory:');
  db.exec(SCHEMA);
  // an ended ask of the old TUSD with a create, two fills and a cancel; an ended pair order TETH/TUSD; a fill of the old TBTC
  order(db, id('d'), OLD.TUSD, 'cancelled');
  event(db, id('d'), OLD.TUSD, 'create', 1);
  event(db, id('d'), OLD.TUSD, 'fill', 2);
  event(db, id('d'), OLD.TUSD, 'fill', 3);
  event(db, id('d'), OLD.TUSD, 'cancel', 4);
  order(db, id('e'), OLD.TETH, 'filled', OLD.TUSD);
  event(db, id('e'), OLD.TETH, 'fill', 5);
  pairFill(db, id('e'), OLD.TETH, OLD.TUSD, 5);
  order(db, id('8'), OLD.TBTC, 'filled');
  event(db, id('8'), OLD.TBTC, 'fill', 6);
  return db;
}

test('carryPairs: by ticker, only the ids that changed; a ticker without a new market or an id reused across tickers is refused', () => {
  assert.deepEqual(carryPairs(reg(OLD), reg({ ...NEW, TBTC: OLD.TBTC })), [
    { ticker: 'TUSD', from: OLD.TUSD, to: NEW.TUSD },
    { ticker: 'TETH', from: OLD.TETH, to: NEW.TETH },
  ]);
  assert.throws(() => carryPairs(reg(OLD), reg({ TUSD: NEW.TUSD, TETH: NEW.TETH })), /TBTC .* not in the new one/);
  assert.throws(() => carryPairs(reg(OLD), reg({ ...NEW, TETH: OLD.TUSD })), /old id of another token/);
  assert.throws(() => carryPairs(reg({ TUSD: 'xyz' }), reg(NEW)), /32-byte covenant id/);
  assert.throws(() => carryPairs({ tokens: [{ ticker: 'A', covenant_id: id('1') }, { ticker: 'A', covenant_id: id('2') }] }, reg(NEW)), /twice/);
});

test('carryHistory: the fills and pair fills move to the new id of their ticker; orders and the other events keep the old id', () => {
  const db = fixture();
  const r = carryHistory(db as unknown as SqlDb, carryPairs(reg(OLD), reg(NEW)), { now: () => 7 });
  assert.deepEqual(
    r.map((x) => [x.ticker, x.fills, x.pairBase, x.pairQuote, x.already]),
    [
      ['TUSD', 2, 0, 1, false],
      ['TETH', 1, 1, 0, false],
      ['TBTC', 1, 0, 0, false],
    ],
  );
  // what the market reads use (market.rs FILL_SELECT, pairs.rs pair_fills) now answers for the new ids, in the same order
  assert.equal(count(db, "SELECT COUNT(*) n FROM order_events WHERE kind = 'fill' AND token_cov_id = ?", b(NEW.TUSD)), 2);
  assert.equal(count(db, "SELECT COUNT(*) n FROM order_events WHERE kind = 'fill' AND token_cov_id IN (?, ?, ?)", b(OLD.TUSD), b(OLD.TETH), b(OLD.TBTC)), 0);
  assert.equal(count(db, 'SELECT COUNT(*) n FROM pair_fills WHERE base_cov_id = ? AND quote_cov_id = ?', b(NEW.TETH), b(NEW.TUSD)), 1);
  assert.deepEqual(
    (db.prepare("SELECT ts FROM order_events WHERE kind = 'fill' AND token_cov_id = ? ORDER BY id").all(b(NEW.TUSD)) as Array<{ ts: number }>).map((x) => x.ts),
    [2, 3],
  );
  // untouched: the orders (their token template is the old one) and the non-fill events
  assert.equal(count(db, 'SELECT COUNT(*) n FROM orders WHERE token_cov_id IN (?, ?, ?)', b(NEW.TUSD), b(NEW.TETH), b(NEW.TBTC)), 0);
  assert.equal(count(db, "SELECT COUNT(*) n FROM order_events WHERE kind != 'fill' AND token_cov_id = ?", b(OLD.TUSD)), 2);
  const meta = JSON.parse((db.prepare('SELECT v FROM meta WHERE k = ?').get(`carried_history:${OLD.TUSD}`) as { v: string }).v);
  assert.deepEqual(meta, { ticker: 'TUSD', to: NEW.TUSD, at: 7, fills: 2, pairBase: 0, pairQuote: 1 });
});

test('carryHistory: a re-run is a no-op that says so; carrying an id to another id is refused', () => {
  const db = fixture();
  const pairs = carryPairs(reg(OLD), reg(NEW));
  carryHistory(db as unknown as SqlDb, pairs);
  const again = carryHistory(db as unknown as SqlDb, pairs);
  assert.ok(again.every((x) => x.already && x.fills === 0 && x.pairBase === 0 && x.pairQuote === 0));
  assert.equal(count(db, "SELECT COUNT(*) n FROM order_events WHERE kind = 'fill' AND token_cov_id = ?", b(NEW.TUSD)), 2);
  assert.throws(() => carryHistory(db as unknown as SqlDb, [{ ticker: 'TUSD', from: OLD.TUSD, to: id('5') }]), /carried to .* before/);
});

test('carryHistory: an open order of an old id refuses the whole carry and nothing moves', () => {
  const db = fixture();
  order(db, id('7'), OLD.TBTC, 'partial');
  assert.throws(() => carryHistory(db as unknown as SqlDb, carryPairs(reg(OLD), reg(NEW))), /TBTC .* 1 open \/ partial/);
  assert.equal(count(db, "SELECT COUNT(*) n FROM order_events WHERE kind = 'fill' AND token_cov_id IN (?, ?, ?)", b(NEW.TUSD), b(NEW.TETH), b(NEW.TBTC)), 0);
  assert.equal(count(db, "SELECT COUNT(*) n FROM meta WHERE k LIKE 'carried_history:%'"), 0);
  // a pair order quoting the old TUSD counts as well
  const db2 = fixture();
  order(db2, id('6'), OLD.TETH, 'open', OLD.TUSD);
  assert.throws(() => carryHistory(db2 as unknown as SqlDb, [{ ticker: 'TUSD', from: OLD.TUSD, to: NEW.TUSD }]), /TUSD .* open/);
});

test('carryHistory: a database without the indexer tables is refused', () => {
  const db = new DatabaseSync(':memory:');
  assert.throws(() => carryHistory(db as unknown as SqlDb, []), /not a KOB indexer database/);
});

test('historyFloors: the first KAS trade under the current id; fills left under a carried id are counted (a replay undid the carry)', () => {
  const db = fixture();
  carryHistory(db as unknown as SqlDb, carryPairs(reg(OLD), reg(NEW)));
  const floors = historyFloors(db as unknown as SqlDb, reg(NEW));
  assert.deepEqual(floors.map((f) => [f.ticker, f.firstTrade, f.leftover]), [['TUSD', 2, 0], ['TETH', 5, 0], ['TBTC', 6, 0]]);
  assert.deepEqual(pairMarkets(db as unknown as SqlDb, reg(NEW)), [[NEW.TETH, NEW.TUSD]]);
  // a replay rebuilds the tables from the record log: the fills are back under the old id, the meta record stays
  db.exec("UPDATE order_events SET token_cov_id = X'" + OLD.TUSD + "' WHERE token_cov_id = X'" + NEW.TUSD + "'");
  db.exec("UPDATE pair_fills SET quote_cov_id = X'" + OLD.TUSD + "'");
  const after = historyFloors(db as unknown as SqlDb, reg(NEW));
  assert.deepEqual([after[0]!.firstTrade, after[0]!.leftover], [null, 3]);
  // a token without any trade
  assert.deepEqual(historyFloors(db as unknown as SqlDb, { tokens: [{ ticker: 'X', covenant_id: id('4') }] }), [{ ticker: 'X', id: id('4'), firstTrade: null, leftover: 0 }]);
});

test('chartProblems: each interval must start at the bucket of the first trade', () => {
  const t = Date.UTC(2026, 9, 6, 2, 52, 30);
  const ok = { '1d': Date.UTC(2026, 9, 6), '1h': Date.UTC(2026, 9, 6, 2), '5m': Date.UTC(2026, 9, 6, 2, 50), '1m': Date.UTC(2026, 9, 6, 2, 52) };
  assert.deepEqual(chartProblems('TUSD', t, ok), []);
  // the 2026-10-14 case: the 1d chart began on 10-11
  assert.deepEqual(chartProblems('TUSD', t, { ...ok, '1d': Date.UTC(2026, 9, 11) }), ['TUSD 1d: the chart starts at 2026-10-11T00:00Z, the first trade\x27s bucket is 2026-10-06T00:00Z']);
  assert.equal(chartProblems('TUSD', t, { '1h': null }).length, 1);
  assert.deepEqual(chartProblems('X', null, { '1d': null }), []);
  assert.equal(chartProblems('X', null, { '1d': 0 }).length, 1);
});

// a database of the lot layouts (protocol v2, schema 4 and older): the columns the import reads
const LOT_SCHEMA = `
CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE orders (covenant_id BLOB PRIMARY KEY, contract TEXT NOT NULL, template_hash BLOB NOT NULL, family INTEGER NOT NULL, side INTEGER NOT NULL,
  maker BLOB, token_cov_id BLOB, token_tpl_hash BLOB, ext_commit BLOB, unit INTEGER, lot_units INTEGER, price INTEGER, tip_lot INTEGER, tif INTEGER,
  expiry_daa INTEGER, active_from INTEGER, in_book INTEGER NOT NULL, lot_max INTEGER, reserve INTEGER, initial_lots INTEGER, deadline INTEGER,
  genesis_state BLOB NOT NULL, genesis_txid BLOB, genesis_out INTEGER, genesis_block INTEGER NOT NULL, genesis_daa INTEGER NOT NULL, parent BLOB,
  listed INTEGER NOT NULL, unlisted_reason TEXT, origin TEXT NOT NULL DEFAULT 'chain');
CREATE TABLE order_state (covenant_id BLOB PRIMARY KEY, status TEXT NOT NULL, filled_lots INTEGER NOT NULL, remaining_lots INTEGER, lots_exact INTEGER NOT NULL DEFAULT 0,
  cur_txid BLOB, cur_idx INTEGER, cur_value INTEGER, state_known INTEGER NOT NULL, last_block INTEGER NOT NULL, last_daa INTEGER NOT NULL);
CREATE TABLE order_events (id INTEGER PRIMARY KEY AUTOINCREMENT, covenant_id BLOB NOT NULL, block_seq INTEGER NOT NULL, daa INTEGER NOT NULL, ts INTEGER NOT NULL,
  txid BLOB NOT NULL, tx_pos INTEGER NOT NULL, kind TEXT NOT NULL, token_cov_id BLOB, side INTEGER, lots INTEGER, price INTEGER, payout INTEGER,
  closes INTEGER NOT NULL DEFAULT 0, detail TEXT);`;

function lotSource(): DatabaseSync {
  const db = new DatabaseSync(':memory:');
  db.exec(LOT_SCHEMA);
  // an ask of the old TUSD (unit 10^8, 1 lot = 1 TUSD) and a bid of the old TETH (unit 10^4, lots of 3 units), both 'open' at the end
  const ord = db.prepare(
    `INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, unit, lot_units, price, in_book, initial_lots, genesis_state,
       genesis_block, genesis_daa, listed) VALUES (?, 'KobAsk', ?, 1, ?, ?, ?, ?, ?, ?, 1, 9, ?, 5, ?, 1)`,
  );
  ord.run(b(id('4')), b(id('f')), 1, b(id('e')), b(OLD.TUSD), 100000000, 1, 2500000000, b('00'), 40);
  ord.run(b(id('5')), b(id('f')), 2, b(id('e')), b(OLD.TETH), 10000, 3, 70000, b('00'), 41);
  db.prepare("INSERT INTO order_state (covenant_id, status, filled_lots, state_known, last_block, last_daa) VALUES (?, 'filled', 9, 1, 9, 90)").run(b(id('4')));
  db.prepare("INSERT INTO order_state (covenant_id, status, filled_lots, state_known, last_block, last_daa) VALUES (?, 'partial', 2, 1, 9, 91)").run(b(id('5')));
  const ev = db.prepare(
    `INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, lots, price) VALUES (?, 7, ?, ?, ?, 0, ?, ?, ?, ?, ?)`,
  );
  ev.run(b(id('4')), 100, 1000, b(id('1')), 'fill', b(OLD.TUSD), 1, 2, 2500000000);
  ev.run(b(id('4')), 101, 2000, b(id('2')), 'fill', b(OLD.TUSD), 1, 3, 2600000000);
  ev.run(b(id('4')), 102, 9000, b(id('3')), 'fill', b(OLD.TUSD), 1, 4, 2700000000); // after `until`: the next run was live
  ev.run(b(id('4')), 99, 900, b(id('6')), 'create', b(OLD.TUSD), 1, 9, 2500000000);
  ev.run(b(id('5')), 103, 2000, b(id('2')), 'fill', b(OLD.TETH), 2, 2, 70000);
  // a 'never' expiry of 2^62: more than a JavaScript number holds exactly
  db.exec('UPDATE orders SET expiry_daa = 4611686018427387904');
  return db;
}
const OLD_TICKERS = new Map([[OLD.TUSD, 'TUSD'], [OLD.TETH, 'TETH'], [OLD.TBTC, 'TBTC']]);

test('importHistory: the fills of an older run of the lot layouts land, converted, on the current ids; once; the next run wins after `until`', () => {
  const src = lotSource();
  const dst = new DatabaseSync(':memory:');
  dst.exec(SCHEMA);
  const pairs = importPairs(src as unknown as SqlSrc, OLD_TICKERS, reg(NEW));
  assert.deepEqual(pairs, [{ ticker: 'TETH', from: OLD.TETH, to: NEW.TETH }, { ticker: 'TUSD', from: OLD.TUSD, to: NEW.TUSD }]);
  assert.equal(firstFillTs(src as unknown as SqlSrc), 1000);
  const r = importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'm5', until: 5000, now: () => 7 });
  assert.equal(r.already, false);
  assert.deepEqual(r.results.map((x) => [x.ticker, x.fills, x.later, x.duplicates, x.first, x.last]), [['TETH', 1, 0, 0, 2000, 2000], ['TUSD', 2, 1, 0, 1000, 2000]]);
  // amounts in base units (lots x lot_units x unit), the price per `unit` base units = the order's scale; block 0, the current ids
  const rows = dst
    .prepare("SELECT lower(hex(e.token_cov_id)) t, e.amount, e.price, e.block_seq, o.scale, o.genesis_daa, e.ts FROM order_events e JOIN orders o ON o.covenant_id = e.covenant_id WHERE e.kind = 'fill' ORDER BY e.ts, e.amount DESC")
    .all() as Array<Record<string, unknown>>;
  assert.deepEqual(rows.map((x) => [x.t, x.amount, x.price, x.block_seq, x.scale, x.genesis_daa, x.ts]), [
    [NEW.TUSD, 200000000, 2500000000, 0, 100000000, 40, 1000],
    [NEW.TUSD, 300000000, 2600000000, 0, 100000000, 40, 2000],
    [NEW.TETH, 60000, 70000, 0, 10000, 41, 2000],
  ]);
  // the orders: never reverted, nobody's, unlisted with the label, the old token id; a final state (the open one closed)
  const ords = dst.prepare('SELECT o.genesis_block, o.maker, o.listed, o.unlisted_reason, lower(hex(o.token_cov_id)) t, s.status, s.filled_amount FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id ORDER BY o.side').all() as Array<Record<string, unknown>>;
  assert.deepEqual(ords.map((o) => [o.genesis_block, o.maker, o.listed, o.unlisted_reason, o.t, o.status, o.filled_amount]), [
    [0, null, 0, 'history_import:m5', OLD.TUSD, 'filled', 900000000],
    [0, null, 0, 'history_import:m5', OLD.TETH, 'closed', 60000],
  ]);
  // a re-run is a no-op; another label copies nothing twice
  assert.deepEqual(importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'm5', until: 5000 }), { already: true, results: [] });
  const again = importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'm5b', until: 99999 });
  assert.deepEqual(again.results.map((x) => [x.ticker, x.fills, x.duplicates]), [['TETH', 0, 1], ['TUSD', 1, 2]]);
  assert.equal(count(dst, "SELECT COUNT(*) n FROM order_events WHERE kind = 'fill'"), 4);
  assert.equal(count(dst, 'SELECT COUNT(*) n FROM orders'), 2);
  assert.equal(count(dst, 'SELECT COUNT(*) n FROM orders WHERE expiry_daa = 4611686018427387904'), 2);
  const floors = historyFloors(dst as unknown as SqlDb, reg(NEW));
  assert.deepEqual(floors.map((f) => [f.ticker, f.firstTrade]), [['TUSD', 1000], ['TETH', 2000], ['TBTC', null]]);
});

test('importHistory: a database of the current layout, with its pair fills; a bad label or target is refused and nothing moves', () => {
  const src = fixture(); // the old ids, fills at ts 2, 3, 5, 6 and a pair fill TETH/TUSD at 5
  const dst = new DatabaseSync(':memory:');
  dst.exec(SCHEMA);
  const pairs = importPairs(src as unknown as SqlSrc, OLD_TICKERS, reg(NEW));
  const r = importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'm10-pre', until: 100 });
  assert.deepEqual(r.results.map((x) => [x.ticker, x.fills]), [['TBTC', 1], ['TETH', 1], ['TUSD', 2]]);
  assert.equal(count(dst, 'SELECT COUNT(*) n FROM pair_fills WHERE base_cov_id = ? AND quote_cov_id = ? AND block_seq = 0', b(NEW.TETH), b(NEW.TUSD)), 1);
  assert.throws(() => importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'a b', until: 1 }), /bad label/);
  assert.throws(() => importHistory(lotSource() as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'x', until: 1 }), /not a KOB indexer database|lot layouts/);
  // a failure inside the transaction leaves nothing behind
  const dst2 = new DatabaseSync(':memory:');
  dst2.exec(SCHEMA);
  assert.throws(() => importHistory(dst2 as unknown as SqlDb, src as unknown as SqlSrc, [...pairs, { ticker: 'X', from: 'zz', to: NEW.TUSD }], { label: 'y', until: 100 }), /bad pair/);
  assert.equal(count(dst2, "SELECT COUNT(*) n FROM order_events"), 0);
  assert.equal(count(dst2, "SELECT COUNT(*) n FROM meta WHERE k LIKE 'history_import:%'"), 0);
});
