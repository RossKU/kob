// Unit tests of the checker state's order-memo pruning (store.ts, `npm test`).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { emptyState, loadState, ORDER_MEMO_WINDOW_DAA, pruneOrders, prunedKey, saveState, type OrderMemo } from '../src/checker/store.ts';
import { memoOrderRows, ordersSummary } from '../src/checker/report-model.ts';

const memo = (over: Partial<OrderMemo>): OrderMemo => ({
  c: 'KobAsk', side: 1, tif: 0, status: 'filled', lastDaa: 0, genesisDaa: 0, initialAmount: null, terms: { price: '1' }, ev: [], active: false, watch: false, tk: [0], ...over,
});

test('pruneOrders drops ended memos older than the window and counts each once', () => {
  const st = emptyState();
  const ref = 1_000_000;
  st.orders = {
    live: memo({ active: true, status: 'open', lastDaa: 1 }),
    old: memo({ lastDaa: ref - ORDER_MEMO_WINDOW_DAA - 1 }),
    old2: memo({ lastDaa: 5 }),
    edge: memo({ lastDaa: ref - ORDER_MEMO_WINDOW_DAA }),
    recent: memo({ lastDaa: ref - 10 }),
    kept: memo({ lastDaa: 5 }),
    other: memo({ c: 'KobBid', side: 2, tif: null, status: 'cancelled', lastDaa: 7, tk: undefined }),
  };
  assert.equal(pruneOrders(st, ref, new Set(['kept'])), 3);
  assert.deepEqual(Object.keys(st.orders).sort(), ['edge', 'kept', 'live', 'recent']);
  assert.deepEqual(st.prunedOrders, { 'KobAsk|0|filled|1|0': 2, 'KobBid|-|cancelled|2|': 1 });
  // a second pass finds nothing new: the counts do not grow
  assert.equal(pruneOrders(st, ref, new Set(['kept'])), 0);
  assert.equal(st.prunedOrders['KobAsk|0|filled|1|0'], 2);
  // the report's totals are the same before and after pruning
  const before = emptyState();
  before.orders = {
    live: memo({ active: true, status: 'open', lastDaa: 1 }),
    old: memo({ lastDaa: 5 }),
    old2: memo({ lastDaa: 5 }),
  };
  const rowsBefore = ordersSummary(memoOrderRows(before.orders, ['tusd'], ['tusd'], true).rows);
  pruneOrders(before, ref, new Set());
  const rowsAfter = ordersSummary(memoOrderRows(before.orders, ['tusd'], ['tusd'], true, before.prunedOrders).rows);
  assert.deepEqual(rowsAfter, rowsBefore);
  assert.equal(prunedKey(memo({ tif: null, tk: [1, 0] })), 'KobAsk|-|filled|1|1,0');
});

test('loadState prunes a state written before the pruning, saveState writes the small one', () => {
  const dir = mkdtempSync(join(tmpdir(), 'kob-ck-'));
  try {
    const p = join(dir, 'checker.json');
    const st = emptyState();
    st.scanDaa = 2_000_000;
    st.retry = ['deferred'];
    st.orders = { live: memo({ active: true, status: 'open', lastDaa: 1 }), deferred: memo({ lastDaa: 1 }) };
    for (let i = 0; i < 100; i++) st.orders[`o${i}`] = memo({ lastDaa: 1_000 + i });
    st.orders.fresh = memo({ lastDaa: 2_000_000 });
    writeFileSync(p, JSON.stringify(st));
    const got = loadState(p);
    assert.deepEqual(Object.keys(got.orders).sort(), ['deferred', 'fresh', 'live']);
    assert.equal(got.prunedOrders?.['KobAsk|0|filled|1|0'], 100);
    saveState(p, got);
    const again = loadState(p);
    assert.equal(again.prunedOrders?.['KobAsk|0|filled|1|0'], 100);
    assert.equal(Object.keys(again.orders).length, 3);
    // a state with no scan yet is left as it is
    const fresh = emptyState();
    fresh.orders = { a: memo({ lastDaa: 1 }) };
    writeFileSync(p, JSON.stringify(fresh));
    assert.equal(Object.keys(loadState(p).orders).length, 1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
