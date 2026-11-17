// Retired token instances (recovery only) and the USDC peg band.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { initialMode, nextMode } from '../src/modes.mjs';
import { pegCheck } from '../src/reference.mjs';
import { retiredMessage, retiredPlan } from '../src/retired.mjs';
import { DEFAULTS, validate } from '../src/config.mjs';

const OLD = '03'.repeat(32);
const NEW = '04'.repeat(32);
const order = (id, extra = {}) => ({ id: id.repeat(64).slice(0, 64), side: 'sell', ...extra });

test('a retired token: cancel every order, then merge, then done; never a place or an amend', () => {
  const live = [order('a'), order('b', { side: 'buy' })];
  let p = retiredPlan(live, 3, 0, 180_000);
  assert.deepEqual(p.actions.map((a) => a.kind), ['cancel', 'cancel']);
  assert.equal(p.done, false);
  // the cancels are sent: not again until the resend time
  p = retiredPlan(live.map((o) => ({ ...o, cancelSentAt: 0 })), 3, 60_000, 180_000);
  assert.deepEqual(p.actions, []);
  p = retiredPlan(live.map((o) => ({ ...o, cancelSentAt: 0 })), 3, 200_000, 180_000);
  assert.equal(p.actions.length, 2, 'a cancel not seen after the resend time is sent again');
  // no order left: merge the returned UTXOs
  p = retiredPlan([], 4, 0, 180_000);
  assert.deepEqual(p.actions.map((a) => a.kind), ['merge']);
  assert.equal(p.done, false);
  // one UTXO, no order: done
  assert.deepEqual(retiredPlan([], 1, 0, 180_000), { actions: [], done: true });
  assert.deepEqual(retiredPlan([], 0, 0, 180_000), { actions: [], done: true });
  for (const n of [0, 1, 5]) for (const a of retiredPlan([order('c')], n, 0, 1).actions) assert.ok(['cancel', 'merge'].includes(a.kind));
});

test('the retired message says how much came back and that the bridge step is manual', () => {
  assert.equal(retiredMessage(OLD, 12_500_000, 6), `retired instance ${OLD}: quotes pulled, 12.5 tokens back in the wallet (moving them through the bridge is manual).`);
  assert.match(retiredMessage(OLD, 7, null), /7 base units of the token back in the wallet/);
});

test('config: retiredTokens are covenant ids and never the quoted token', () => {
  const ok = { ...DEFAULTS, token: { ...DEFAULTS.token, covenantId: NEW, decimals: 6 }, retiredTokens: [OLD] };
  assert.deepEqual(validate(ok), []);
  assert.ok(validate({ ...ok, retiredTokens: [NEW] }).some((p) => /must not contain token.covenantId/.test(p)));
  assert.ok(validate({ ...ok, retiredTokens: ['USDX'] }).some((p) => /not 64 lower-case hex/.test(p)));
});

test('peg check: a live rate outside the band fails; a fallback or fixed rate never does', () => {
  assert.deepEqual(pegCheck({ value: 0.9998, source: 'kraken USDCUSD' }, 1, 0.005), { ok: true, drift: 0.9998 - 1 });
  assert.equal(pegCheck({ value: 0.9951, source: 'kraken USDCUSD' }, 1, 0.005).ok, true);
  assert.equal(pegCheck({ value: 0.9949, source: 'kraken USDCUSD' }, 1, 0.005).ok, false);
  assert.equal(pegCheck({ value: 1.006, source: 'kraken USDCUSD' }, 1, 0.005).ok, false);
  assert.deepEqual(pegCheck({ value: 1, source: 'fallback' }, 1, 0.005), { ok: true, drift: 0 });
  assert.deepEqual(pegCheck({ value: 1, source: 'fixed' }, 1, 0.005), { ok: true, drift: 0 });
});

test('peg drift: pull the quotes and idle; resume after the rate is back in the band for the cool-down', () => {
  const cfg = { killJump: 0.02, resumeAfterSec: 180 };
  const ok = { stop: false, refOk: true, jump: 0, feeBlocked: false, pegOk: true };
  const t = (s) => 1_000_000 + s * 1000;
  let m = nextMode(initialMode(t(0)), ok, cfg, t(0));
  m = nextMode(m, ok, cfg, t(180));
  assert.equal(m.mode, 'QUOTING');
  m = nextMode(m, { ...ok, pegOk: false }, cfg, t(210));
  assert.equal(`${m.mode}/${m.reason}`, 'IDLE/peg');
  assert.equal(m.pull, true);
  m = nextMode(m, { ...ok, pegOk: false }, cfg, t(240));
  assert.equal(m.reason, 'peg');
  m = nextMode(m, ok, cfg, t(270));
  assert.equal(`${m.mode}/${m.reason}`, 'IDLE/cooldown');
  m = nextMode(m, { ...ok, pegOk: false }, cfg, t(300));
  assert.equal(m.reason, 'peg', 'a drift during the cool-down restarts it');
  m = nextMode(m, ok, cfg, t(330));
  m = nextMode(m, ok, cfg, t(509));
  assert.equal(m.mode, 'IDLE');
  m = nextMode(m, ok, cfg, t(510));
  assert.equal(m.mode, 'QUOTING');
});
