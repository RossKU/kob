// Market history across a re-issue (src/history-carry.ts), on a database made from the indexer's own schema.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { carryHistory, carryPairs, type SqlDb } from '../src/history-carry.ts';

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
