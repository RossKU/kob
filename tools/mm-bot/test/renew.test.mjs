// The renewal of the quotes' short expiry (review finding E-L4): when it is due, that it rides on a
// reprice, and what it never does.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DEFAULTS, validate } from '../src/config.mjs';
import { gtdArg } from '../src/kob.mjs';
import { buildLadder } from '../src/ladder.mjs';
import { planActions, renewDue } from '../src/reprice.mjs';

const SCALE = 1e6;
const Q = { levels: 2, spreadPct: 1.0, levelStepPct: 0.5, sizePerLevel: 10, maxAskTokens: 100, maxBidKas: 5000, kasReserve: 50, minOrderKas: 15,
  repriceThreshold: 0.0025, resizeBelow: 0.5, askCarrierMinKas: 0.2 };
const BUDGET = { askTokens: 20 * SCALE, bidKas: 1000e8 };
const FREE = { kas: 10_000e8, tokens: 100 * SCALE };
const MIN = 60_000;
const LIFE = { lifeMs: 90 * MIN, renewAfterMs: 45 * MIN };
const T0 = Date.parse('2026-11-20T12:00:00Z');

/** Live orders as quoted at `fair` at `at`, each with an expiry LIFE.lifeMs later. */
function quotedAt(fair, at = T0) {
  const t = buildLadder(fair, BUDGET, Q, SCALE);
  return [...t.asks, ...t.bids].map((l, i) => ({
    id: `${l.side}${l.level}`.padEnd(64, String(i)), side: l.side, level: l.level, price: l.price, quotedFair: fair,
    amountLeft: l.amount, value: l.side === 'sell' ? 1e8 : Math.ceil((l.amount * l.price) / SCALE) + 1e8, pending: false,
    expiresAt: at + LIFE.lifeMs, expired: false,
  }));
}
const plan = (fair, orders, now, life = LIFE) => planActions(buildLadder(fair, BUDGET, Q, SCALE), orders, { fair, scale: SCALE, free: FREE, q: Q, now, life });

test('renewDue: renewAfterMs after the expiry was set, at once when past it or unknown, never without a life', () => {
  const o = { expiresAt: T0 + LIFE.lifeMs, expired: false };
  assert.equal(renewDue(o, T0, LIFE), false);
  assert.equal(renewDue(o, T0 + 45 * MIN - 1, LIFE), false);
  assert.equal(renewDue(o, T0 + 45 * MIN, LIFE), true);
  assert.equal(renewDue({ ...o, expired: true }, T0, LIFE), true, 'the indexer says it is past its expiry');
  assert.equal(renewDue({ expiresAt: null }, T0, LIFE), true, 'an order of a run before the expiry existed');
  assert.equal(renewDue({ expiresAt: null }, T0, null), false);
});

test('steady market: nothing is sent before renewAfterMin; then every quote is amended in place once, at its target', () => {
  const orders = quotedAt(37.5);
  assert.deepEqual(plan(37.5, orders, T0 + 10 * MIN), []);
  assert.deepEqual(plan(37.5 * 1.002, orders, T0 + 45 * MIN - 1000), [], 'a move under the threshold is still not a reprice');
  const a = plan(37.5, orders, T0 + 45 * MIN);
  assert.equal(a.length, orders.length);
  assert.ok(a.every((x) => x.kind === 'amend' && x.renewOnly && /^renew: expiry due/.test(x.reason)));
  // The same ids and the same prices: the total gap stays at the spread.
  for (const x of a) {
    const o = orders.find((y) => y.id === x.id);
    assert.equal(x.price, o.price);
  }
  // A renewal under a small move takes the quote to its new target in the same transaction.
  const moved = plan(37.5 * 1.002, orders, T0 + 45 * MIN);
  const target = buildLadder(37.5 * 1.002, BUDGET, Q, SCALE);
  for (const x of moved) {
    const t = (x.side === 'sell' ? target.asks : target.bids).find((l) => l.level === x.level);
    assert.equal(x.price, t.price);
    assert.equal(x.quotedFair, 37.5 * 1.002);
  }
});

test('a reprice renews too: a moving market pays no extra renewal', () => {
  const orders = quotedAt(37.5);
  const a = plan(37.5 * 1.004, orders, T0 + 10 * MIN);
  assert.equal(a.length, orders.length);
  assert.ok(a.every((x) => x.kind === 'amend' && !x.renewOnly), 'a reprice, counted as an amend, not a renewal');
  // After the reprice the bot records a fresh expiry; renewAfterMin later the quote is due again.
  const after = orders.map((o) => ({ ...o, price: a.find((x) => x.id === o.id).price, quotedFair: 37.5 * 1.004, expiresAt: T0 + 10 * MIN + LIFE.lifeMs }));
  assert.deepEqual(plan(37.5 * 1.004, after, T0 + 54 * MIN), []);
  assert.equal(plan(37.5 * 1.004, after, T0 + 55 * MIN).length, orders.length);
});

test('a quote past its expiry (the bot was down) is renewed at once; a pending one waits', () => {
  const orders = quotedAt(37.5).map((o, i) => (i === 0 ? { ...o, expired: true } : o));
  const a = plan(37.5, orders, T0 + MIN);
  assert.deepEqual(a.map((x) => [x.kind, x.id, x.reason]), [['amend', orders[0].id, 'renew: past its expiry']]);
  const pending = orders.map((o) => ({ ...o, pending: true }));
  assert.deepEqual(plan(37.5, pending, T0 + 80 * MIN), []);
});

test('a renewal that an amend cannot carry is a cancel, and the level is placed again later', () => {
  const orders = quotedAt(37.5).map((o) => (o.side === 'sell' && o.level === 0 ? { ...o, value: 0.15e8 } : o));
  const a = plan(37.5, orders, T0 + 45 * MIN);
  assert.equal(a.filter((x) => x.kind === 'cancel').length, 1);
  assert.match(a.find((x) => x.kind === 'cancel').reason, /carrier/);
  assert.equal(a.filter((x) => x.kind === 'amend' && x.renewOnly).length, orders.length - 1);
});

test('renewals never cross the book: every renewed bid stays below every renewed ask', () => {
  for (const fair of [37.5, 37.5 * 1.002, 37.5 * 0.998]) {
    const a = plan(fair, quotedAt(37.5), T0 + 45 * MIN);
    const bids = a.filter((x) => x.side === 'buy').map((x) => x.price);
    const asks = a.filter((x) => x.side === 'sell').map((x) => x.price);
    assert.ok(Math.max(...bids) < Math.min(...asks));
    // The gap between the best bid and the best ask is the 1 % spread of the fair value.
    const gap = (Math.min(...asks) - Math.max(...bids)) / (fair * 1e8);
    assert.ok(Math.abs(gap - 0.01) < 1e-6, `${gap}`);
  }
});

test('the --gtd argument is UTC to the second', () => {
  assert.equal(gtdArg(T0 + 90 * MIN + 999), '2026-11-20T13:30:00Z');
});

test('config: lifeMin 30..1440 min, renewAfterMin at least 15 min below it', () => {
  const cfg = (quotes) => ({ ...DEFAULTS, token: { ...DEFAULTS.token, covenantId: '01'.repeat(32), decimals: 6 }, quotes: { ...DEFAULTS.quotes, ...quotes } });
  assert.deepEqual(validate(cfg({})), []);
  assert.equal(DEFAULTS.quotes.lifeMin, 90);
  assert.equal(DEFAULTS.quotes.renewAfterMin, 45);
  assert.deepEqual(validate(cfg({ lifeMin: 120, renewAfterMin: 60 })), []);
  assert.ok(validate(cfg({ lifeMin: 20 })).some((p) => /lifeMin/.test(p)));
  assert.ok(validate(cfg({ lifeMin: 90, renewAfterMin: 80 })).some((p) => /renewAfterMin must be at least 15 minutes below/.test(p)));
});
