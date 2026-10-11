// A trader limit that is marketable when planned is a taker (IOC at its own price); a passive limit rests (src/bots/taker.ts).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { crossesBest, takerIntent } from '../src/bots/taker.ts';

const book = { asks: [{ price: 1000n }, { price: 1010n }], bids: [{ price: 990n }, { price: 980n }] };
const limit = (side: 'buy' | 'sell', price: bigint, over: Record<string, unknown> = {}) => ({ type: 'limit', side, amount: 5n, price, ...over }) as never;

test('a buy at or above the best ask and a sell at or below the best bid cross', () => {
  assert.equal(crossesBest('buy', 1000n, 0n, book), true);
  assert.equal(crossesBest('buy', 1050n, 0n, book), true);
  assert.equal(crossesBest('buy', 999n, 0n, book), false);
  assert.equal(crossesBest('sell', 990n, 0n, book), true);
  assert.equal(crossesBest('sell', 900n, 0n, book), true);
  assert.equal(crossesBest('sell', 991n, 0n, book), false);
});

test('tips count on both sides of the comparison; an empty opposite side never crosses', () => {
  assert.equal(crossesBest('buy', 995n, 5n, book), true);
  assert.equal(crossesBest('buy', 995n, 4n, book), false);
  assert.equal(crossesBest('sell', 995n, 0n, { asks: [], bids: [{ price: 990n, tip: 5n }] }), true);
  assert.equal(crossesBest('buy', 5000n, 0n, { asks: [], bids: [{ price: 1n }] }), false);
});

test('a marketable limit becomes an IOC at the same price (any lifetime, maxFills and minFill dropped)', () => {
  const r = takerIntent(limit('buy', 1050n, { maxFills: 3n, minFill: 2n, lifetime: { kind: 'day' } }), book);
  assert.equal(r.taker, true);
  assert.deepEqual(r.intent, { type: 'ioc', side: 'buy', amount: 5n, price: 1050n });
  const s = takerIntent(limit('sell', 900n, { tip: 2n }), book);
  assert.deepEqual(s.intent, { type: 'ioc', side: 'sell', amount: 5n, price: 900n, tip: 2n });
});

test('a passive limit, a timed limit and every other intent are unchanged', () => {
  const passive = limit('buy', 990n);
  assert.equal(takerIntent(passive, book).intent, passive);
  assert.equal(takerIntent(passive, book).taker, false);
  const timed = limit('buy', 1050n, { activeFrom: { unixSeconds: 1n } });
  assert.equal(takerIntent(timed, book).intent, timed);
  const other = { type: 'market', side: 'buy', amount: 1n } as never;
  assert.equal(takerIntent(other, book).intent, other);
});

test('a pair book has no KAS tips: the intent tip does not make a pair limit cross', () => {
  assert.equal(takerIntent(limit('buy', 995n, { tip: 5n }), book, true).taker, false);
  assert.equal(takerIntent(limit('buy', 995n, { tip: 5n }), book, false).taker, true);
});
