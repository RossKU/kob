// Market history across a re-issue (src/history-carry.ts), on databases made from the indexer's own schemas: the current one
// (schema 9: ids interned as integers, in the child tables and in `orders`), and the schema-8 (integer children, blobs in `orders`) and
// schema-7 (blobs everywhere) fixtures of the Rust tests.
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
  isSchema9,
  pairMarkets,
  storeSchema,
  type SqlDb,
  type SqlSrc,
} from '../src/history-carry.ts';

const SCHEMAS = {
  v9: readFileSync(new URL('../../../crates/kobd/src/indexer/schema.sql', import.meta.url), 'utf8'),
  v8: readFileSync(new URL('../../../crates/kobd/tests/fixtures/schema8.sql', import.meta.url), 'utf8'),
  v7: readFileSync(new URL('../../../crates/kobd/tests/fixtures/schema7.sql', import.meta.url), 'utf8'),
};
type Layout = keyof typeof SCHEMAS;
const empty = (layout: Layout = 'v9'): DatabaseSync => {
  const db = new DatabaseSync(':memory:');
  db.exec(SCHEMAS[layout]);
  return db;
};
const id = (c: string): string => c.repeat(64);
const b = (h: string): Uint8Array => Uint8Array.from(Buffer.from(h, 'hex'));
const OLD = { TUSD: id('1'), TETH: id('2'), TBTC: id('3') };
const NEW = { TUSD: id('a'), TETH: id('b'), TBTC: id('c') };
const reg = (m: Record<string, string>) => ({ tokens: Object.entries(m).map(([ticker, covenant_id]) => ({ ticker, covenant_id })) });

let seq = 0;
/** ids.id of a 32-byte id in a database with an `ids` table (interned on the way: an id is never deleted) */
function intern(db: DatabaseSync, h: string): number {
  db.prepare('INSERT OR IGNORE INTO ids (h) VALUES (?)').run(b(h));
  return Number((db.prepare('SELECT id FROM ids WHERE h = ?').get(b(h)) as { id: number }).id);
}
// the values every fixture order names besides its token: template, maker, token program, extension commitment
const TEMPLATE = id('f');
const MAKER = id('e');
const TOKEN_TPL = id('0');
const EXT = id('9');
function order(db: DatabaseSync, cov: string, token: string, status: string, quote: string | null = null): void {
  if (isSchema9(db)) {
    // schema 9: the six 32-byte values are `ids` rows
    db.prepare(
      `INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, token_tpl_id, ext_id, scale, price, in_book, genesis_state, genesis_block,
         genesis_daa, listed, quote_id) VALUES (?, ?, ?, 1, 1, ?, ?, ?, ?, 100000000, 5000, 1, ?, 1, 1, 1, ?)`,
    ).run(b(cov), quote ? 'KobPair' : 'KobAsk', intern(db, TEMPLATE), intern(db, MAKER), intern(db, token), intern(db, TOKEN_TPL), intern(db, EXT), b('00'), quote ? intern(db, quote) : null);
  } else {
    db.prepare(
      `INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, ext_commit, scale, price, in_book, genesis_state,
         genesis_block, genesis_daa, listed, quote_cov_id) VALUES (?, ?, ?, 1, 1, ?, ?, ?, ?, 100000000, 5000, 1, ?, 1, 1, 1, ?)`,
    ).run(b(cov), quote ? 'KobPair' : 'KobAsk', b(TEMPLATE), b(MAKER), b(token), b(TOKEN_TPL), b(EXT), b('00'), quote ? b(quote) : null);
  }
  if (storeSchema(db) >= 8) {
    db.prepare(
      'INSERT INTO order_state (order_id, status, filled_amount, state_known, last_block, last_daa) VALUES ((SELECT id FROM orders WHERE covenant_id = ?), ?, 0, 1, 1, 1)',
    ).run(b(cov), status);
  } else {
    db.prepare('INSERT INTO order_state (covenant_id, status, filled_amount, state_known, last_block, last_daa) VALUES (?, ?, 0, 1, 1, 1)').run(b(cov), status);
  }
}
function event(db: DatabaseSync, cov: string, token: string, kind: string, ts: number): void {
  const txid = b(id(String(++seq % 10)));
  if (storeSchema(db) >= 8) {
    db.prepare(
      `INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price)
       VALUES ((SELECT id FROM orders WHERE covenant_id = ?), 1, ?, ?, ?, 0, ?, ?, 1, 10, 5000)`,
    ).run(b(cov), ts, ts, txid, kind, intern(db, token));
  } else {
    db.prepare(
      `INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price) VALUES (?, 1, ?, ?, ?, 0, ?, ?, 1, 10, 5000)`,
    ).run(b(cov), ts, ts, txid, kind, b(token));
  }
}
function pairFill(db: DatabaseSync, cov: string, base: string, quote: string, ts: number): void {
  db.prepare(
    `INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, covenant_id, contract, base_cov_id, quote_cov_id, side, amount_a, a_scale, counterparty)
     VALUES (1, ?, ?, ?, 0, ?, 'KobPair', ?, ?, 1, 10, 100000000, 'route')`,
  ).run(ts, ts, b(id('9')), b(cov), b(base), b(quote));
}
const count = (db: DatabaseSync, sql: string, ...p: unknown[]): number => Number((db.prepare(sql).get(...(p as [])) as { n: number }).n);
/** events of a token id (default: its fills; `cmp` is the SQL comparison on `kind`), in every layout */
const eventsOf = (db: DatabaseSync, token: string, cmp = "= 'fill'"): number =>
  storeSchema(db) >= 8
    ? count(db, `SELECT COUNT(*) n FROM order_events WHERE kind ${cmp} AND token_id = (SELECT id FROM ids WHERE h = ?)`, b(token))
    : count(db, `SELECT COUNT(*) n FROM order_events WHERE kind ${cmp} AND token_cov_id = ?`, b(token));

/** SQL: the lower-case hex of the 32-byte value an `orders` row (alias o) names in a schema-9 store (`''` for NULL) */
const idHex = (col: string): string => `lower(hex((SELECT h FROM ids WHERE id = o.${col})))`;

function fixture(layout: Layout = 'v9'): DatabaseSync {
  const db = empty(layout);
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

test('the fixtures are of the layouts they claim', () => {
  for (const l of ['v9', 'v8', 'v7'] as const) {
    const n = Number(l.slice(1));
    assert.equal(storeSchema(empty(l)), n);
    assert.equal(storeSchema(fixture(l)), n);
    assert.equal(isSchema9(fixture(l)), l === 'v9');
  }
});

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
  assert.equal(eventsOf(db, NEW.TUSD), 2);
  assert.equal(count(db, "SELECT COUNT(*) n FROM order_events WHERE kind = 'fill' AND token_id IN (SELECT id FROM ids WHERE h IN (?, ?, ?))", b(OLD.TUSD), b(OLD.TETH), b(OLD.TBTC)), 0);
  assert.equal(count(db, 'SELECT COUNT(*) n FROM pair_fills WHERE base_cov_id = ? AND quote_cov_id = ?', b(NEW.TETH), b(NEW.TUSD)), 1);
  assert.deepEqual(
    (db.prepare("SELECT ts FROM order_events WHERE kind = 'fill' AND token_id = (SELECT id FROM ids WHERE h = ?) ORDER BY id").all(b(NEW.TUSD)) as Array<{ ts: number }>).map((x) => x.ts),
    [2, 3],
  );
  // untouched: the orders (their token template is the old one) and the non-fill events
  assert.equal(count(db, 'SELECT COUNT(*) n FROM orders WHERE token_id IN (SELECT id FROM ids WHERE h IN (?, ?, ?))', b(NEW.TUSD), b(NEW.TETH), b(NEW.TBTC)), 0);
  assert.equal(eventsOf(db, OLD.TUSD, "!= 'fill'"), 2);
  const meta = JSON.parse((db.prepare('SELECT v FROM meta WHERE k = ?').get(`carried_history:${OLD.TUSD}`) as { v: string }).v);
  assert.deepEqual(meta, { ticker: 'TUSD', to: NEW.TUSD, at: 7, fills: 2, pairBase: 0, pairQuote: 1 });
});

test('carryHistory: a re-run is a no-op that says so; carrying an id to another id is refused', () => {
  const db = fixture();
  const pairs = carryPairs(reg(OLD), reg(NEW));
  carryHistory(db as unknown as SqlDb, pairs);
  const again = carryHistory(db as unknown as SqlDb, pairs);
  assert.ok(again.every((x) => x.already && x.fills === 0 && x.pairBase === 0 && x.pairQuote === 0));
  assert.equal(eventsOf(db, NEW.TUSD), 2);
  assert.throws(() => carryHistory(db as unknown as SqlDb, [{ ticker: 'TUSD', from: OLD.TUSD, to: id('5') }]), /carried to .* before/);
});

test('carryHistory: an old id with no fills (no ids row) moves nothing and the target is interned; an older-layout destination is refused', () => {
  const db = fixture();
  const r = carryHistory(db as unknown as SqlDb, [{ ticker: 'TNEW', from: id('4'), to: id('5') }]);
  assert.deepEqual(r.map((x) => [x.fills, x.pairBase, x.pairQuote, x.already]), [[0, 0, 0, false]]);
  assert.equal(count(db, 'SELECT COUNT(*) n FROM ids WHERE h = ?', b(id('5'))), 1);
  assert.equal(count(db, 'SELECT COUNT(*) n FROM ids WHERE h = ?', b(id('4'))), 0);
  assert.equal(eventsOf(db, OLD.TUSD), 2);
  // a store of an older layout (schema 8 included: its orders keep blobs) is a source of reads, never a destination
  for (const l of ['v8', 'v7'] as const) {
    const old = fixture(l);
    assert.throws(() => carryHistory(old as unknown as SqlDb, carryPairs(reg(OLD), reg(NEW))), /older layout/);
    assert.equal(eventsOf(old, OLD.TUSD), 2);
    assert.equal(count(old, "SELECT COUNT(*) n FROM meta WHERE k LIKE 'carried_history:%'"), 0);
  }
});

test('carryHistory: an open order of an old id refuses the whole carry and nothing moves', () => {
  const db = fixture();
  order(db, id('7'), OLD.TBTC, 'partial');
  assert.throws(() => carryHistory(db as unknown as SqlDb, carryPairs(reg(OLD), reg(NEW))), /TBTC .* 1 open \/ partial/);
  assert.equal(count(db, 'SELECT COUNT(*) n FROM ids WHERE h IN (?, ?, ?)', b(NEW.TUSD), b(NEW.TETH), b(NEW.TBTC)), 0); // the interning rolled back too
  assert.equal(eventsOf(db, OLD.TUSD), 2);
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
  db.exec(`UPDATE order_events SET token_id = (SELECT id FROM ids WHERE h = X'${OLD.TUSD}') WHERE token_id = (SELECT id FROM ids WHERE h = X'${NEW.TUSD}')`);
  db.exec("UPDATE pair_fills SET quote_cov_id = X'" + OLD.TUSD + "'");
  const after = historyFloors(db as unknown as SqlDb, reg(NEW));
  assert.deepEqual([after[0]!.firstTrade, after[0]!.leftover], [null, 3]);
  // a token without any trade (and without an ids row)
  assert.deepEqual(historyFloors(db as unknown as SqlDb, { tokens: [{ ticker: 'X', covenant_id: id('4') }] }), [{ ticker: 'X', id: id('4'), firstTrade: null, leftover: 0 }]);
});

for (const layout of ['v8', 'v7'] as const) test(`historyFloors / pairMarkets: the same answers on a schema-${layout.slice(1)} store (the chart check reads any layout)`, () => {
  const db = fixture(layout);
  assert.deepEqual(historyFloors(db as unknown as SqlDb, reg(OLD)).map((f) => [f.ticker, f.firstTrade, f.leftover]), [['TUSD', 2, 0], ['TETH', 5, 0], ['TBTC', 6, 0]]);
  assert.deepEqual(pairMarkets(db as unknown as SqlDb, reg(OLD)), [[OLD.TETH, OLD.TUSD]]);
  // a carry record whose old id still has its fills: counted from the old columns
  db.prepare('INSERT INTO meta (k, v) VALUES (?, ?)').run(`carried_history:${OLD.TUSD}`, JSON.stringify({ ticker: 'TUSD', to: NEW.TUSD }));
  assert.deepEqual(historyFloors(db as unknown as SqlDb, reg(NEW)).map((f) => [f.ticker, f.firstTrade, f.leftover]), [['TUSD', null, 3], ['TETH', null, 0], ['TBTC', null, 0]]);
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
  const dst = empty();
  const pairs = importPairs(src as unknown as SqlSrc, OLD_TICKERS, reg(NEW));
  assert.deepEqual(pairs, [{ ticker: 'TETH', from: OLD.TETH, to: NEW.TETH }, { ticker: 'TUSD', from: OLD.TUSD, to: NEW.TUSD }]);
  assert.equal(firstFillTs(src as unknown as SqlSrc), 1000);
  const r = importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'm5', until: 5000, now: () => 7 });
  assert.equal(r.already, false);
  assert.deepEqual(r.results.map((x) => [x.ticker, x.fills, x.later, x.duplicates, x.first, x.last]), [['TETH', 1, 0, 0, 2000, 2000], ['TUSD', 2, 1, 0, 1000, 2000]]);
  // amounts in base units (lots x lot_units x unit), the price per `unit` base units = the order's scale; block 0, the current ids
  const rows = dst
    .prepare(
      "SELECT lower(hex(i.h)) t, e.amount, e.price, e.block_seq, o.scale, o.genesis_daa, e.ts FROM order_events e JOIN ids i ON i.id = e.token_id JOIN orders o ON o.id = e.order_id WHERE e.kind = 'fill' ORDER BY e.ts, e.amount DESC",
    )
    .all() as Array<Record<string, unknown>>;
  assert.deepEqual(rows.map((x) => [x.t, x.amount, x.price, x.block_seq, x.scale, x.genesis_daa, x.ts]), [
    [NEW.TUSD, 200000000, 2500000000, 0, 100000000, 40, 1000],
    [NEW.TUSD, 300000000, 2600000000, 0, 100000000, 40, 2000],
    [NEW.TETH, 60000, 70000, 0, 10000, 41, 2000],
  ]);
  // the orders: never reverted, nobody's, unlisted with the label, the old token id; a final state (the open one closed)
  const ords = dst.prepare(`SELECT o.genesis_block, o.maker_id, o.listed, o.unlisted_reason, ${idHex('token_id')} t, ${idHex('template_id')} tpl, s.status, s.filled_amount FROM orders o JOIN order_state s ON s.order_id = o.id ORDER BY o.side`).all() as Array<Record<string, unknown>>;
  assert.deepEqual(ords.map((o) => [o.genesis_block, o.maker_id, o.listed, o.unlisted_reason, o.t, o.tpl, o.status, o.filled_amount]), [
    [0, null, 0, 'history_import:m5', OLD.TUSD, id('f'), 'filled', 900000000],
    [0, null, 0, 'history_import:m5', OLD.TETH, id('f'), 'closed', 60000],
  ]);
  // every state row belongs to an order (a NULL order_id would have taken a fresh rowid) and the target ids are interned once
  assert.equal(count(dst, 'SELECT COUNT(*) n FROM order_state'), 2);
  assert.equal(count(dst, 'SELECT COUNT(*) n FROM order_events e JOIN orders o ON o.id = e.order_id'), 3);
  assert.equal(count(dst, 'SELECT COUNT(*) n FROM ids WHERE h IN (?, ?)', b(NEW.TUSD), b(NEW.TETH)), 2);
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

// the same import from a source of each layout (schema 7 blob-keyed, schema 8 integer children, schema 9 interned orders)
for (const layout of ['v7', 'v8', 'v9'] as const) {
  test(`importHistory: a schema-${layout.slice(1)} source, with its pair fills; a bad label or target is refused and nothing moves`, () => {
    const src = fixture(layout); // the old ids, fills at ts 2, 3, 5, 6 and a pair fill TETH/TUSD at 5
    const dst = empty();
    const pairs = importPairs(src as unknown as SqlSrc, OLD_TICKERS, reg(NEW));
    assert.deepEqual(pairs, [{ ticker: 'TBTC', from: OLD.TBTC, to: NEW.TBTC }, { ticker: 'TETH', from: OLD.TETH, to: NEW.TETH }, { ticker: 'TUSD', from: OLD.TUSD, to: NEW.TUSD }]);
    assert.equal(firstFillTs(src as unknown as SqlSrc), 2);
    const r = importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'm10-pre', until: 100 });
    assert.deepEqual(r.results.map((x) => [x.ticker, x.fills]), [['TBTC', 1], ['TETH', 1], ['TUSD', 2]]);
    assert.equal(count(dst, 'SELECT COUNT(*) n FROM pair_fills WHERE base_cov_id = ? AND quote_cov_id = ? AND block_seq = 0', b(NEW.TETH), b(NEW.TUSD)), 1);
    // the fills sit on the new ids, behind imported orders with a state of their own (the three source orders had fills)
    assert.equal(eventsOf(dst, NEW.TUSD), 2);
    assert.equal(count(dst, 'SELECT COUNT(*) n FROM order_events e JOIN orders o ON o.id = e.order_id WHERE e.block_seq = 0'), 4);
    assert.equal(count(dst, 'SELECT COUNT(*) n FROM orders o JOIN order_state s ON s.order_id = o.id'), 3);
    assert.equal(count(dst, 'SELECT COUNT(*) n FROM order_state'), 3);
    assert.deepEqual(
      (dst.prepare('SELECT s.status st FROM orders o JOIN order_state s ON s.order_id = o.id ORDER BY o.id').all() as Array<{ st: string }>).map((x) => x.st),
      ['filled', 'filled', 'cancelled'], // in import order: TBTC, TETH, TUSD
    );
    // the 32-byte values of each imported order are interned in `ids` and named by integer; the maker is not carried (NULL); the old token id stays
    const rows = dst
      .prepare(
        `SELECT ${idHex('template_id')} tpl, ${idHex('token_id')} tok, ${idHex('token_tpl_id')} ttpl, ${idHex('ext_id')} ext, ${idHex('quote_id')} quote, o.maker_id mk, o.quote_id qid
         FROM orders o ORDER BY o.id`,
      )
      .all() as Array<Record<string, unknown>>;
    assert.deepEqual(rows.map((x) => [x.tpl, x.tok, x.ttpl, x.ext, x.quote, x.mk, x.qid === null]), [
      [TEMPLATE, OLD.TBTC, TOKEN_TPL, EXT, '', null, true],
      [TEMPLATE, OLD.TETH, TOKEN_TPL, EXT, OLD.TUSD, null, false],
      [TEMPLATE, OLD.TUSD, TOKEN_TPL, EXT, '', null, true],
    ]);
    assert.equal(count(dst, 'SELECT COUNT(*) n FROM ids WHERE h = ?', b(MAKER)), 0); // nobody's: the maker is not interned
    assert.equal(count(dst, 'SELECT COUNT(*) n FROM ids WHERE h IN (?, ?, ?, ?)', b(TEMPLATE), b(TOKEN_TPL), b(EXT), b(OLD.TUSD)), 4);
    assert.throws(() => importHistory(dst as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'a b', until: 1 }), /bad label/);
    assert.throws(() => importHistory(lotSource() as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'x', until: 1 }), /not a KOB indexer database|lot layouts/);
    // an older-layout store (schema 8 included) is never the target
    for (const l of ['v8', 'v7'] as const) {
      const old = empty(l);
      assert.throws(() => importHistory(old as unknown as SqlDb, src as unknown as SqlSrc, pairs, { label: 'x', until: 1 }), /older layout/);
      assert.equal(count(old, 'SELECT COUNT(*) n FROM orders'), 0);
    }
    // a failure inside the transaction leaves nothing behind
    const dst2 = empty();
    assert.throws(() => importHistory(dst2 as unknown as SqlDb, src as unknown as SqlSrc, [...pairs, { ticker: 'X', from: 'zz', to: NEW.TUSD }], { label: 'y', until: 100 }), /bad pair/);
    assert.equal(count(dst2, 'SELECT COUNT(*) n FROM order_events'), 0);
    assert.equal(count(dst2, 'SELECT COUNT(*) n FROM orders'), 0);
    assert.equal(count(dst2, 'SELECT COUNT(*) n FROM ids'), 0);
    assert.equal(count(dst2, "SELECT COUNT(*) n FROM meta WHERE k LIKE 'history_import:%'"), 0);
  });
}
