import { test } from 'node:test';
import assert from 'node:assert/strict';
import { requoteThreshold } from '../src/bots/mm-math.ts';

test('requote threshold: one value without requoteMinBps (the wide ladder)', () => {
  const c = { innerBps: 25, stepBps: 30, requoteBps: 35 };
  assert.equal(requoteThreshold(c, 0), 35);
  assert.equal(requoteThreshold(c, 5), 35);
});

test('requote threshold: a tight ladder moves a level once it drifts by its own distance (min requoteMinBps, max requoteBps)', () => {
  const c = { innerBps: 2, stepBps: 4, requoteBps: 20, requoteMinBps: 3 };
  assert.equal(requoteThreshold(c, 0), 3);
  assert.equal(requoteThreshold(c, 1), 6);
  assert.equal(requoteThreshold(c, 3), 14);
  assert.equal(requoteThreshold(c, 5), 20);
});
