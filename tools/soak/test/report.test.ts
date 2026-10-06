// Unit tests of the report model and the incident log (`npm test`).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { IncidentLog, readIncidents } from '../src/checker/incidents.ts';
import { botActions, buildReport, fmtDur, fmtKas, fmtUnits, ordersSummary, resourcePeaks, type ReportData } from '../src/checker/report-model.ts';

test('bot counters per action, with the top refusal codes', () => {
  const rows = botActions({
    'try:limit': 10,
    'placed:limit': 7,
    'plan_refused:limit:INSUFFICIENT_KAS': 2,
    'plan_refused:limit:BOOK_EMPTY': 1,
    'tx_ok:limit': 7,
    'tx_conflict:limit': 1,
    'plan_refused:cancel:KobAsk:cancel.custody-missing': 3,
    'tx_ok:cancel:KobAsk': 4,
    tx_ok: 11,
  });
  const limit = rows.find((r) => r.action === 'limit')!;
  assert.equal(limit.tries, 10);
  assert.equal(limit.placed, 7);
  assert.equal(limit.refused, 3);
  assert.deepEqual(limit.refusals[0], ['INSUFFICIENT_KAS', 2]);
  assert.equal(limit.conflicts, 1);
  const cancel = rows.find((r) => r.action === 'cancel:KobAsk')!;
  assert.equal(cancel.refused, 3);
  assert.equal(cancel.txOk, 4);
});

test('orders by contract, status and tif', () => {
  const s = ordersSummary([
    { contract: 'KobAsk', tif: 0, status: 'open', side: 1 },
    { contract: 'KobAsk', tif: 1, status: 'killed', side: 1 },
    { contract: 'KobBid', tif: 0, status: 'partial', side: 2 },
  ]);
  assert.equal(s.total, 3);
  assert.equal(s.active, 2);
  assert.deepEqual(s.byContract.KobAsk.byTif, { '0': 1, '1': 1 });
});

test('resource peaks since the soak start', () => {
  const l = [
    { ts: 1, procs: { a: { wsMb: 999 } }, totalWsMb: 999 },
    { ts: 10, procs: { a: { wsMb: 50 }, b: { wsMb: 20 } }, totalWsMb: 70 },
    { ts: 20, procs: { a: { wsMb: 40 }, b: { wsMb: 30 } }, totalWsMb: 70 },
  ]
    .map((x) => JSON.stringify(x))
    .join('\n');
  assert.deepEqual(resourcePeaks(l + '\n{torn', 5), { peak: { a: 50, b: 30 }, peakTotal: 70 });
});

test('formatting helpers', () => {
  assert.equal(fmtKas(123_456_789n), '1.2345');
  assert.equal(fmtKas('-50000000', 2), '-0.50');
  assert.equal(fmtUnits('250000000', 8), '2.50');
  assert.equal(fmtDur(90_061_000), '1d 01:01');
});

test('the text report is compact and complete', () => {
  const d: ReportData = {
    now: Date.UTC(2026, 8, 30, 12, 0),
    soakStartMs: Date.UTC(2026, 8, 29, 12, 0),
    token: { ticker: 'TUSD', covenantId: '7cfe8aa0'.padEnd(64, '0'), decimals: 8 },
    fills: { count: 10, sell: 5, buy: 5, trades: 5, volumeTokens: '500000000', volumeKas: '11000000000', asOf: null },
    openAsks: 6,
    openBids: 7,
    orders: [{ contract: 'KobAsk', tif: 0, status: 'open', side: 1 }],
    ordersComplete: true,
    bots: { counters: { tx_ok: 3, tx_conflict: 1, 'try:market': 2, 'placed:market': 1, fees_sompi: 1_000_000 }, gauges: {}, ts: null },
    executors: [
      { name: 'exec-a', health: { state: 'following', cursor_daa: 5, lag_daa: 1 }, metrics: { kob_matcher_finalized_total: 4, kob_matcher_finalized_profit_sompi: 5_000_000, kob_matcher_conflicts_total: 2 } },
      { name: 'exec-b', health: null, metrics: null },
    ],
    x402: { paidByPath: { '/native': 2 }, errors: { '/swap:unsupported': 1 }, ledger: { ledgerAccepted: 2, byKind: { native: 2 } } },
    incidents: [{ ts: '2026-09-30T11:00:00.000Z', invariant: 'fee', severity: 'error', subject: 'tx1', detail: { problem: 'fee off' } }],
    checks: { fills: 10, rounds: 3 },
    checkerTs: Date.UTC(2026, 8, 30, 11, 59),
    resources: { current: { ts: 0, procs: { miner: { wsMb: 10, cpuSec: 5 } }, totalWsMb: 10 }, peak: { miner: 12 }, peakTotal: 12 },
    supervisor: [{ name: 'miner', running: true, restarts: 1 }],
  };
  const { text, json } = buildReport(d);
  assert.match(text, /uptime 1d 00:00/);
  assert.match(text, /fills 10 \(sell 5 \/ buy 5\)  trades 5  volume 5\.00 TUSD \/ 110\.00 KAS/);
  assert.match(text, /exec-a  finalized 4 \(profit 0\.0500 KAS\)/);
  assert.match(text, /matcher conflicts 2 \+ bot tx conflicts 1/);
  assert.match(text, /incidents 1 \(errors 1\)/);
  assert.match(text, /restarts miner 1/);
  assert.ok(text.split('\n').length < 40);
  assert.equal((json.incidents as { total: number }).total, 1);
});

test('incident log: appended once per (invariant, subject), also across restarts', () => {
  const dir = mkdtempSync(join(tmpdir(), 'soak-inc-'));
  try {
    const p = join(dir, 'incidents.jsonl');
    const got: string[] = [];
    const log = new IncidentLog(p, (i) => got.push(i.subject));
    const v = { invariant: 'fee', severity: 'error' as const, subject: 'tx1', detail: { fee: 5n } };
    assert.equal(log.report(v), true);
    assert.equal(log.report(v), false);
    assert.equal(log.report({ ...v, invariant: 'all-in' }), true);
    const again = new IncidentLog(p);
    assert.equal(again.report(v), false);
    assert.equal(readIncidents(p).length, 2);
    assert.equal(JSON.parse(readFileSync(p, 'utf8').split('\n')[0]).detail.fee, '5');
    assert.deepEqual(got, ['tx1', 'tx1']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
