// The reprice decision: amend in place only on a real move, cancel+place only where an amend cannot
// do it.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildLadder } from '../src/ladder.mjs';
import { planActions, pullAll } from '../src/reprice.mjs';

const SCALE = 1e6;
const Q = { levels: 2, spreadPct: 1.0, levelStepPct: 0.5, sizePerLevel: 10, maxAskTokens: 100, maxBidKas: 5000, kasReserve: 50, minOrderKas: 15,
  repriceThreshold: 0.0025, resizeBelow: 0.5, askCarrierMinKas: 0.2 };
const BUDGET = { askTokens: 20 * SCALE, bidKas: 1000e8 };
const FREE = { kas: 10_000e8, tokens: 100 * SCALE };

/** Live orders exactly as quoted at `fair`. */
function quotedAt(fair) {
  const t = buildLadder(fair, BUDGET, Q, SCALE);
  return [...t.asks, ...t.bids].map((l, i) => ({
    id: `${l.side}${l.level}`.padEnd(64, String(i)), side: l.side, level: l.level, price: l.price, quotedFair: fair,
    amountLeft: l.amount, value: l.side === 'sell' ? 1e8 : Math.ceil((l.amount * l.price) / SCALE), pending: false,
  }));
}
const plan = (fair, orders, free = FREE) => planActions(buildLadder(fair, BUDGET, Q, SCALE), orders, { fair, scale: SCALE, free, q: Q });

test('no orders: place every level, inside out, asks and bids', () => {
  const a = plan(37.5, []);
  assert.equal(a.length, 4);
  assert.ok(a.every((x) => x.kind === 'place'));
  assert.deepEqual(a.map((x) => `${x.side}${x.level}`).sort(), ['buy0', 'buy1', 'sell0', 'sell1']);
});

test('a move at or below the threshold changes nothing (no timer, no churn)', () => {
  const orders = quotedAt(37.5);
  assert.deepEqual(plan(37.5, orders), []);
  assert.deepEqual(plan(37.5 * 1.0024, orders), []);
  assert.deepEqual(plan(37.5 * 0.9976, orders), []);
});

test('a move past the threshold amends every level in place to the new price', () => {
  const orders = quotedAt(37.5);
  const fair = 37.5 * 1.003;
  const a = plan(fair, orders);
  assert.equal(a.length, 4);
  assert.ok(a.every((x) => x.kind === 'amend'));
  const target = buildLadder(fair, BUDGET, Q, SCALE);
  for (const x of a) {
    const t = (x.side === 'sell' ? target.asks : target.bids).find((l) => l.level === x.level);
    assert.equal(x.price, t.price);
    assert.equal(x.quotedFair, fair);
    assert.equal(x.id, orders.find((o) => o.side === x.side && o.level === x.level).id, 'the same order, amended');
  }
});

test('an ask whose carrier cannot pay another amend is cancelled (and placed again later)', () => {
  const orders = quotedAt(37.5).map((o) => (o.side === 'sell' && o.level === 0 ? { ...o, value: 0.15e8 } : o));
  const a = plan(37.5 * 1.01, orders);
  const c = a.filter((x) => x.kind === 'cancel');
  assert.equal(c.length, 1);
  assert.equal(c[0].side, 'sell');
  assert.match(c[0].reason, /carrier/);
  assert.equal(a.filter((x) => x.kind === 'amend').length, 3);
});

test('an order left too small by a partial fill is cancelled; a large remainder is kept', () => {
  const orders = quotedAt(37.5);
  const small = orders.map((o) => (o.side === 'sell' && o.level === 0 ? { ...o, amountLeft: 4 * SCALE } : o));
  const a = plan(37.5, small);
  assert.deepEqual(a.map((x) => [x.kind, x.side, x.level]), [['cancel', 'sell', 0]]);
  const big = orders.map((o) => (o.side === 'sell' && o.level === 0 ? { ...o, amountLeft: 6 * SCALE } : o));
  assert.deepEqual(plan(37.5, big), []);
});

test('a filled level (order gone) is placed again only if the free inventory covers it now', () => {
  const orders = quotedAt(37.5).filter((o) => !(o.side === 'sell' && o.level === 1));
  assert.deepEqual(plan(37.5, orders).map((x) => [x.kind, x.side, x.level]), [['place', 'sell', 1]]);
  assert.deepEqual(plan(37.5, orders, { kas: FREE.kas, tokens: 5 * SCALE }), []);
});

test('orders past the ladder (inventory down, an unknown order) are cancelled; pending ones are left alone', () => {
  const orders = quotedAt(37.5);
  const extra = { ...orders[0], id: 'f'.repeat(64), level: 999 };
  assert.deepEqual(plan(37.5, [...orders, extra]).map((x) => [x.kind, x.id]), [['cancel', extra.id]]);
  const pending = orders.map((o) => ({ ...o, pending: true }));
  assert.deepEqual(plan(37.5 * 1.01, pending), [], 'nothing is sent for an order whose last action is not seen yet');
});

test('cancels come first, then amends, then placements', () => {
  const orders = quotedAt(37.5).filter((o) => !(o.side === 'buy' && o.level === 1));
  orders.push({ ...orders[0], id: 'e'.repeat(64), level: 7 });
  const kinds = plan(37.5 * 1.01, orders).map((x) => x.kind);
  assert.deepEqual(kinds, ['cancel', 'amend', 'amend', 'amend', 'place']);
});

test('pullAll cancels every quote', () => {
  const orders = quotedAt(37.5);
  const a = pullAll(orders, 'pull: jump');
  assert.equal(a.length, orders.length);
  assert.ok(a.every((x) => x.kind === 'cancel' && x.reason === 'pull: jump'));
});

test('a changed spread is followed on the same threshold, without a fair-value move', () => {
  const orders = quotedAt(37.5);
  const wider = { ...Q, spreadPct: 2.0 };
  const a = planActions(buildLadder(37.5, BUDGET, wider, SCALE), orders, { fair: 37.5, scale: SCALE, free: FREE, q: wider });
  assert.equal(a.length, 4);
  assert.ok(a.every((x) => x.kind === 'amend' && /from its target/.test(x.reason)));
  // A change smaller than the threshold is left alone.
  const near = { ...Q, spreadPct: 1.2 };
  assert.deepEqual(planActions(buildLadder(37.5, BUDGET, near, SCALE), orders, { fair: 37.5, scale: SCALE, free: FREE, q: near }), []);
});

test('amends that move a quote away from the other side go first (T-7)', () => {
  // Fair up 1 %: the asks move up (away from the bids) before the bids move up toward the asks.
  const up = plan(37.5 * 1.01, quotedAt(37.5)).filter((x) => x.kind === 'amend');
  const firstBid = up.findIndex((x) => x.side === 'buy');
  assert.ok(firstBid > 0 && up.slice(0, firstBid).every((x) => x.side === 'sell'), up.map((x) => x.side).join(','));
  // Fair down 1 %: the bids move down first.
  const down = plan(37.5 * 0.99, quotedAt(37.5)).filter((x) => x.kind === 'amend');
  const firstAsk = down.findIndex((x) => x.side === 'sell');
  assert.ok(firstAsk > 0 && down.slice(0, firstAsk).every((x) => x.side === 'buy'), down.map((x) => x.side).join(','));
});
