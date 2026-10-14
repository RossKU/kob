// How far the traders let a taker or an auction go from the reference (src/bots/bands.ts).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { bandsOf, capToDepth, DEFAULT_BANDS, depthWithin } from '../src/bots/bands.ts';

test('the default bounds are a few tens of bps, not the protocol default 3 %', () => {
  assert.ok(DEFAULT_BANDS.marketBps <= 50 && DEFAULT_BANDS.stopBps <= 50 && DEFAULT_BANDS.streamingBps <= 50);
  assert.ok(DEFAULT_BANDS.stopLimitBps <= 50 && DEFAULT_BANDS.entryBandBps <= 50 && DEFAULT_BANDS.crossBps <= 50);
  assert.ok(DEFAULT_BANDS.dutchStartBps[1] <= 150);
});

test('bandsOf: no config is the defaults, a subset overrides, an invalid value keeps its default', () => {
  assert.deepEqual(bandsOf(undefined), DEFAULT_BANDS);
  assert.deepEqual(bandsOf(null), DEFAULT_BANDS);
  const b = bandsOf({ marketBps: 10, stopBps: 0, streamingBps: Number.NaN, depthFraction: 2, dutchStartBps: [5, 1], dutchEndBps: [1, 2] });
  assert.equal(b.marketBps, 10);
  assert.equal(b.stopBps, DEFAULT_BANDS.stopBps);
  assert.equal(b.streamingBps, DEFAULT_BANDS.streamingBps);
  assert.equal(b.depthFraction, DEFAULT_BANDS.depthFraction);
  assert.deepEqual(b.dutchStartBps, DEFAULT_BANDS.dutchStartBps);
  assert.deepEqual(b.dutchEndBps, [1, 2]);
  assert.equal(bandsOf({ depthFraction: 0 }).depthFraction, 0);
});

const asks = [
  { price: 10_000n, amount: 100n },
  { price: 10_010n, amount: 200n }, // +10 bps
  { price: 10_030n, amount: 400n }, // +30 bps
  { price: 10_031n, amount: 800n }, // +31 bps
];

test('depthWithin sums the levels within the band of the first level (both directions)', () => {
  assert.equal(depthWithin(asks, 30), 700n);
  assert.equal(depthWithin(asks, 10), 300n);
  assert.equal(depthWithin(asks, 0), 100n);
  assert.equal(depthWithin([], 30), 0n);
  const bids = [{ price: 10_000n, amount: 5n }, { price: 9_970n, amount: 7n }, { price: 9_969n, amount: 9n }];
  assert.equal(depthWithin(bids, 30), 12n);
});

test('capToDepth: an order within the share is kept, a larger one is cut to the share, never below the floor', () => {
  assert.equal(capToDepth(100n, asks, 0.25, 30, 10n), 100n); // cap 175
  assert.equal(capToDepth(500n, asks, 0.25, 30, 10n), 175n);
  assert.equal(capToDepth(500n, asks, 0.01, 30, 10n), 10n); // cap 7 -> the floor
  assert.equal(capToDepth(5n, asks, 0.01, 30, 10n), 5n); // never above the order itself
});

test('capToDepth: no cap without a fraction, depth or opposite side', () => {
  assert.equal(capToDepth(500n, asks, 0, 30, 10n), 500n);
  assert.equal(capToDepth(500n, [], 0.25, 30, 10n), 500n);
  assert.equal(capToDepth(500n, [{ price: 1n, amount: 0n }], 0.25, 30, 10n), 500n);
});
