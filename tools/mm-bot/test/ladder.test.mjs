// The ladder, the inventory limits, the skew and the capital sizing.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { sideBudgets, skewShift } from '../src/inventory.mjs';
import { assertNoSelfCross, bidCost, buildLadder } from '../src/ladder.mjs';
import { inventoryUsd, resolveSizing, sizedQuotes } from '../src/sizing.mjs';

const SCALE = 1e6; // 6 decimals
const Q = { levels: 3, spreadPct: 1.0, levelStepPct: 0.5, sizePerLevel: 10, maxAskTokens: 100, maxBidKas: 5000, kasReserve: 50, minOrderKas: 15 };
const FAIR = 37.5; // KAS per token

test('spreadPct is the bid-ask gap: the best levels sit spreadPct / 2 from fair, then levelStepPct further each; bids round down, asks up', () => {
  const { bids, asks } = buildLadder(FAIR, { askTokens: 100 * SCALE, bidKas: 5000e8 }, Q, SCALE);
  assert.deepEqual(asks.map((a) => a.price), [Math.ceil(37.5e8 * (1 + 0.005)), Math.ceil(37.5e8 * (1 + 0.005 + 0.005)), Math.ceil(37.5e8 * (1 + 0.005 + 0.01))]);
  assert.deepEqual(bids.map((b) => b.price), [Math.floor(37.5e8 * (1 - 0.005)), Math.floor(37.5e8 * (1 - 0.005 - 0.005)), Math.floor(37.5e8 * (1 - 0.005 - 0.01))]);
  // The gap between the best bid and the best ask is 1 % of fair.
  assert.ok(Math.abs((asks[0].price - bids[0].price) / 37.5e8 - 0.01) < 1e-9);
  assert.ok(asks.every((a) => a.amount === 10 * SCALE && a.side === 'sell'));
  assert.ok(bids.every((b) => b.amount === 10 * SCALE && b.side === 'buy'));
  assert.deepEqual(asks.map((a) => a.level), [0, 1, 2]);
});

test('a side stops where its budget ends; the last level may be partial if still worth minOrderKas', () => {
  // 25 tokens: two full asks and a 5-token third (5 x 38.06 = 190 KAS >= 15).
  const a = buildLadder(FAIR, { askTokens: 25 * SCALE, bidKas: 0 }, Q, SCALE);
  assert.deepEqual(a.asks.map((x) => x.amount / SCALE), [10, 10, 5]);
  assert.equal(a.bids.length, 0, 'no KAS: no bids (the side is idle, never borrowed)');
  // 0.3 tokens is worth about 11 KAS: below the minimum, no ask at all.
  assert.equal(buildLadder(FAIR, { askTokens: 0.3 * SCALE, bidKas: 0 }, Q, SCALE).asks.length, 0);
  // 500 KAS of bids: one full level (371 KAS) and a partial one.
  const b = buildLadder(FAIR, { askTokens: 0, bidKas: 500e8 }, Q, SCALE);
  assert.equal(b.bids.length, 2);
  const spent = b.bids.reduce((s, x) => s + bidCost(x.amount, x.price, SCALE), 0);
  assert.ok(spent <= 500e8, 'never more than the budget');
});

test('the ladder never crosses itself', () => {
  const { bids, asks } = buildLadder(FAIR, { askTokens: 100 * SCALE, bidKas: 5000e8 }, Q, SCALE, 0.004);
  assert.ok(Math.max(...bids.map((b) => b.price)) < Math.min(...asks.map((a) => a.price)));
  assert.throws(() => assertNoSelfCross([{ price: 100 }], [{ price: 100 }]), /self-cross/);
  assert.throws(() => buildLadder(0, { askTokens: 1, bidKas: 1 }, Q, SCALE));
});

test('side budgets: free plus what the own orders hold, capped, with the KAS reserve kept back', () => {
  const b = sideBudgets({ kasFree: 1000e8, tokenFree: 30 * SCALE }, { askTokens: 20 * SCALE, bidKas: 2000e8 }, Q, SCALE);
  assert.equal(b.askTokens, 50 * SCALE);
  assert.equal(b.bidKas, (1000 + 2000 - 50) * 1e8);
  const capped = sideBudgets({ kasFree: 9000e8, tokenFree: 500 * SCALE }, { askTokens: 0, bidKas: 0 }, Q, SCALE);
  assert.equal(capped.askTokens, 100 * SCALE);
  assert.equal(capped.bidKas, 5000e8);
  const broke = sideBudgets({ kasFree: 10e8, tokenFree: 0 }, { askTokens: 0, bidKas: 0 }, Q, SCALE);
  assert.deepEqual(broke, { askTokens: 0, bidKas: 0 });
});

test('skew: off by default; leans the prices against the inventory, bounded by maxShift', () => {
  assert.equal(skewShift(900, 100, undefined), 0);
  assert.equal(skewShift(900, 100, { enabled: false, maxShift: 0.005 }), 0);
  const on = { enabled: true, maxShift: 0.005, targetTokenShare: 0.5 };
  assert.equal(skewShift(500, 500, on), 0);
  assert.equal(skewShift(1000, 0, on), -0.005, 'all token: prices down to sell it');
  assert.equal(skewShift(0, 1000, on), 0.005, 'all KAS: prices up to buy');
  assert.ok(Math.abs(skewShift(750, 250, on) + 0.0025) < 1e-12);
});

test('capital sizing: percentages of capitalUsd at the live rates', () => {
  const q = { levelSizePct: 15, maxAskPct: 50, maxBidPct: 50 };
  const s = resolveSizing(1000, q, 0.9998, 0.0266);
  assert.ok(Math.abs(s.sizePerLevel - 150 / 0.9998) < 1e-9);
  assert.ok(Math.abs(s.maxAskTokens - 500 / 0.9998) < 1e-9);
  assert.ok(Math.abs(s.maxBidKas - 500 / 0.0266) < 1e-6); // 18,797 KAS
  assert.equal(sizedQuotes(1000, { ...q, levels: 3 }, 1, 0.025).maxBidKas, 20_000);
  assert.throws(() => resolveSizing(1000, q, 1, 0));
  const inv = inventoryUsd(412, 498 / 0.0266, { tokenUsd: 1, kasUsd: 0.0266 }, 1000);
  assert.equal(inv.text, 'token 412 USD (41%), KAS 498 USD (50%), total 910 USD (91%) of 1,000 USD');
});
