import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ladderSteps, requoteThreshold } from '../src/bots/mm-math.ts';

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

const T = (rank: number, price: bigint) => ({ rank, price, amount: 1000n });
const thr = () => 10; // bps

test('ladder: missing levels are placed, a level off by more than its threshold is amended, surplus orders are cancelled', () => {
  const targets = [T(0, 10_000n), T(1, 9_990n), T(2, 9_980n)];
  // bids best first: 10_000 (on level), 9_900 (off level 1 by ~90 bps) -> amend; level 2 missing -> place
  const steps = ladderSteps('buy', targets, [{ price: 9_900n, order: 'o2' }, { price: 10_000n, order: 'o1' }], thr, 10);
  assert.deepEqual(steps, [
    { kind: 'amend', order: 'o2', target: targets[1] },
    { kind: 'place', target: targets[2] },
  ]);
  const surplus = ladderSteps('sell', [T(0, 100n)], [{ price: 100n, order: 'a' }, { price: 101n, order: 'b' }], thr, 10);
  assert.deepEqual(surplus, [{ kind: 'cancel', order: 'b' }]);
});

test('ladder: a held slot (own pending placement or amend, not listed yet) keeps its level and is never acted on', () => {
  const targets = [T(0, 10_000n), T(1, 9_990n), T(2, 9_980n)];
  // the indexer lists none of the three placements yet: nothing is placed again
  const held = targets.map((t) => ({ price: t.price, order: null }));
  assert.deepEqual(ladderSteps('buy', targets, held, thr, 10), []);
  // an amended order still listed at its old price (far off) is not amended again; a held surplus slot is not cancelled
  assert.deepEqual(ladderSteps('buy', [T(0, 10_000n)], [{ price: 9_000n, order: null }, { price: 8_000n, order: null }], thr, 10), []);
});

test('ladder: at most `budget` transactions per side and tick', () => {
  const targets = Array.from({ length: 10 }, (_, i) => T(i, 10_000n - BigInt(i) * 10n));
  const steps = ladderSteps<string>('buy', targets, [], thr, 2);
  assert.deepEqual(steps.map((s) => s.kind), ['place', 'place']);
  assert.equal((steps[0] as { target: { rank: number } }).target.rank, 0);
});
