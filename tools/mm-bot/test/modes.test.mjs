// The mode state machine: the bot idles on every safety condition, resumes by itself, and stops
// only for the operator.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { backoffMs, feeGate, IDLE, initialMode, nextMode, QUOTING, sideStatus, STOPPING } from '../src/modes.mjs';

const CFG = { killJump: 0.02, resumeAfterSec: 180 };
const OK = { stop: false, refOk: true, jump: 0.001, feeBlocked: false };
const T0 = Date.parse('2026-11-17T10:00:00Z');
const sec = (s) => T0 + s * 1000;

/** Runs a sequence of [seconds, input] steps from `start`; returns every mode. */
function walk(start, steps) {
  let m = start;
  const out = [];
  for (const [s, input] of steps) {
    m = nextMode(m, { ...OK, ...input }, CFG, sec(s));
    out.push(m);
  }
  return out;
}

test('the bot starts idle and quotes only after the reference was steady for the cool-down', () => {
  const ms = walk(initialMode(T0), [[0, {}], [90, {}], [179, {}], [180, {}]]);
  assert.deepEqual(ms.map((m) => `${m.mode}/${m.reason}`), ['IDLE/cooldown', 'IDLE/cooldown', 'IDLE/cooldown', 'QUOTING/null']);
  assert.ok(ms.slice(0, 3).every((m) => m.pull), 'nothing is quoted while idle');
  assert.equal(ms[3].pull, false);
  assert.equal(ms[3].changed, true);
  assert.equal(ms[3].since, sec(180));
});

test('a stale reference pulls the quotes, idles, and resumes after a fresh steady cool-down', () => {
  const q = { mode: QUOTING, reason: null, since: T0, steadySince: T0, feeDay: null };
  const ms = walk(q, [[30, { refOk: false }], [60, { refOk: false }], [90, {}], [200, {}], [270, {}]]);
  assert.deepEqual(ms.map((m) => `${m.mode}/${m.reason}`), ['IDLE/reference', 'IDLE/reference', 'IDLE/cooldown', 'IDLE/cooldown', 'QUOTING/null']);
  assert.equal(ms[0].pull, true);
  assert.equal(ms[0].changed, true);
  assert.equal(ms[1].changed, false);
  assert.equal(ms[1].since, sec(30), 'since = when the state last changed');
});

test('a one-minute jump above killJump pulls the quotes; another jump during the cool-down restarts it', () => {
  const q = { mode: QUOTING, reason: null, since: T0, steadySince: T0, feeDay: null };
  const ms = walk(q, [[30, { jump: 0.025 }], [60, {}], [150, { jump: 0.03 }], [180, {}], [300, {}], [360, {}]]);
  assert.deepEqual(ms.map((m) => `${m.mode}/${m.reason}`), ['IDLE/jump', 'IDLE/cooldown', 'IDLE/jump', 'IDLE/cooldown', 'IDLE/cooldown', 'QUOTING/null']);
  // A jump at exactly the threshold is not a jump.
  assert.equal(walk(q, [[30, { jump: 0.02 }]])[0].mode, QUOTING);
});

test('the fee cap idles the bot until the UTC day rolls over, whatever the reference does', () => {
  const late = Date.parse('2026-11-17T23:59:00Z');
  const q = { mode: QUOTING, reason: null, since: T0, steadySince: T0, feeDay: null };
  let m = nextMode(q, { ...OK, feeBlocked: true }, CFG, late);
  assert.equal(`${m.mode}/${m.reason}`, 'IDLE/fee-cap');
  assert.equal(m.feeDay, '2026-11-17');
  assert.equal(m.pull, true);
  // Still blocked the same day even if the gate's reading clears.
  m = nextMode(m, OK, CFG, late + 30_000);
  assert.equal(m.reason, 'fee-cap');
  // The next UTC day: cool-down, then quoting.
  m = nextMode(m, OK, CFG, Date.parse('2026-11-18T00:00:10Z'));
  assert.equal(`${m.mode}/${m.reason}`, 'IDLE/cooldown');
  m = nextMode(m, OK, CFG, Date.parse('2026-11-18T00:03:10Z'));
  assert.equal(m.mode, QUOTING);
});

test('only the operator stops the bot, and STOPPING is final', () => {
  const q = { mode: QUOTING, reason: null, since: T0, steadySince: T0, feeDay: null };
  let m = nextMode(q, { ...OK, stop: true }, CFG, sec(10));
  assert.equal(m.mode, STOPPING);
  assert.equal(m.pull, true);
  m = nextMode(m, OK, CFG, sec(20));
  assert.equal(m.mode, STOPPING, 'no way back from STOPPING');
  // No safety condition ever yields STOPPING.
  for (const input of [{ refOk: false }, { jump: 0.5 }, { feeBlocked: true }]) {
    assert.equal(nextMode(q, { ...OK, ...input }, CFG, sec(10)).mode, IDLE);
  }
});

test('a side without inventory is idle on its own', () => {
  assert.deepEqual(sideStatus([{ level: 0 }], 10), { quoting: true, reason: null });
  assert.deepEqual(sideStatus([], 0), { quoting: false, reason: 'no inventory' });
  assert.deepEqual(sideStatus([], 5), { quoting: false, reason: 'inventory below one level' });
});

test('backoff doubles from the base and is capped', () => {
  assert.deepEqual([0, 1, 2, 3, 4, 5, 6, 50].map((n) => backoffMs(n, 15_000, 300_000)), [0, 15_000, 30_000, 60_000, 120_000, 240_000, 300_000, 300_000]);
});

test('fee gate: amends and placements stop at cap minus the cancel reserve; cancels always go', () => {
  const cfg = { maxFeesKasPerDay: 50, cancelReserveKas: 5 };
  assert.deepEqual(feeGate(0, cfg), { canTrade: true, canCancel: true, left: 45e8, overCap: false });
  assert.equal(feeGate(44.99e8, cfg).canTrade, true);
  assert.equal(feeGate(45e8, cfg).canTrade, false);
  assert.equal(feeGate(45e8, cfg).canCancel, true);
  assert.equal(feeGate(51e8, cfg).overCap, true);
  assert.equal(feeGate(51e8, cfg).canCancel, true);
});
