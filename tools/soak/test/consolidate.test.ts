// Token-UTXO consolidation: the pure selection (which UTXOs to merge) and the rate limit. No network, no wallet.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { consolidateConfig, DEFAULT_CONSOLIDATE, outpointKey, RateLimiter, selectMergeChain, selectMerges, type MergeCandidate } from '../src/bots/consolidate-math.ts';

const txid = (n: number) => n.toString(16).padStart(4, '0').repeat(16);
const u = (n: number, amount: number | bigint, index = 1): MergeCandidate => ({ transactionId: txid(n), index, state: { amount: amount.toString() } });
/** `count` UTXOs with amounts 1..count (shuffled order) */
const many = (count: number): MergeCandidate[] => Array.from({ length: count }, (_, i) => u(i + 1, (i * 7919) % count + 1)).map((x, i) => ({ ...x, state: { amount: String(((i * 37) % count) + 1) } }));
const base = { keep: 8, slack: 8, maxInputs: 8, maxBatches: 10 };
const amounts = (b: MergeCandidate[]) => b.map((x) => BigInt(x.state.amount));
const total = (bs: MergeCandidate[][]) => bs.reduce((s, b) => s + b.length, 0);

test('no-op at or below keep + slack', () => {
  assert.deepEqual(selectMerges(many(16), base), []);
  assert.deepEqual(selectMerges(many(8), base), []);
  assert.deepEqual(selectMerges([], base), []);
});

test('plans a merge just above the threshold', () => {
  const bs = selectMerges(many(17), base);
  assert.equal(bs.length, 1);
  assert.equal(bs[0].length, 8);
});

test('smallest first, batch after batch', () => {
  const us = many(60);
  const bs = selectMerges(us, base);
  const flat = bs.flat().map((x) => BigInt(x.state.amount));
  const sorted = us.map((x) => BigInt(x.state.amount)).sort((a, b) => (a < b ? -1 : 1));
  assert.deepEqual(flat, sorted.slice(0, flat.length));
  for (const b of bs) assert.deepEqual(amounts(b), [...amounts(b)].sort((x, y) => (x < y ? -1 : 1)));
});

test('never merges below the fan-out target, even before the outputs are indexed', () => {
  for (const n of [17, 20, 31, 64, 200, 700]) {
    for (const keep of [3, 4, 8]) {
      const bs = selectMerges(many(n), { ...base, keep, maxBatches: 1000 });
      const inputs = total(bs);
      assert.ok(n - inputs >= keep, `n=${n} keep=${keep}: visible ${n - inputs} < keep`);
      assert.ok(n - (inputs - bs.length) >= keep);
    }
  }
});

test('one pass drains the excess to keep + one merged UTXO per batch; later passes converge on keep', () => {
  const bs = selectMerges(many(100), { ...base, maxBatches: 1000 });
  assert.equal(100 - (total(bs) - bs.length), base.keep + bs.length);
  // the next pass on the result (merge outputs included) plans fewer batches, until the threshold is no longer exceeded
  let n = 100 - (total(bs) - bs.length);
  for (let i = 0; i < 5 && n > base.keep + base.slack; i++) {
    const b = selectMerges(many(n), { ...base, maxBatches: 1000 });
    n -= total(b) - b.length;
  }
  assert.ok(n <= base.keep + base.slack, `settled at ${n}`);
});

test('respects the program input limit', () => {
  for (const maxInputs of [2, 3, 8]) {
    const bs = selectMerges(many(90), { ...base, maxInputs, maxBatches: 100 });
    assert.ok(bs.length > 0);
    for (const b of bs) assert.ok(b.length >= 2 && b.length <= maxInputs);
  }
  assert.deepEqual(selectMerges(many(90), { ...base, maxInputs: 1 }), []);
});

test('batches are disjoint and capped by maxBatches', () => {
  const bs = selectMerges(many(200), { ...base, maxBatches: 3 });
  assert.equal(bs.length, 3);
  const keys = bs.flat().map(outpointKey);
  assert.equal(new Set(keys).size, keys.length);
  assert.deepEqual(selectMerges(many(200), { ...base, maxBatches: 0 }), []);
});

test('deterministic: independent of input order, ties broken by outpoint', () => {
  const us = [...many(40), u(900, 5), u(901, 5), u(902, 5, 0)];
  const a = selectMerges(us, base);
  const b = selectMerges([...us].reverse(), base);
  assert.deepEqual(a.map((x) => x.map(outpointKey)), b.map((x) => x.map(outpointKey)));
  assert.deepEqual(selectMerges(us, base).map((x) => x.map(outpointKey)), a.map((x) => x.map(outpointKey)));
});

test('ignores reserved UTXOs and zero amounts', () => {
  const us = many(20);
  const smallest = [...us].sort((a, b) => Number(BigInt(a.state.amount) - BigInt(b.state.amount)))[0];
  const reserved = new Set([outpointKey(smallest)]);
  const bs = selectMerges(us, { ...base, reserved });
  assert.ok(bs.length > 0);
  assert.ok(!bs.flat().some((x) => outpointKey(x) === outpointKey(smallest)));
  // the threshold counts only eligible UTXOs: 17 held, 2 reserved -> 15 eligible -> no-op
  const held = many(17);
  assert.deepEqual(selectMerges(held, { ...base, reserved: new Set([outpointKey(held[0]), outpointKey(held[1])]) }), []);
  assert.deepEqual(selectMerges([...many(16), u(500, 0)], base), []);
});

test('per-token separation: each token is planned from its own UTXOs only', () => {
  const tusd = many(30);
  const teth = many(10).map((x) => ({ ...x, transactionId: txid(1000 + Number(BigInt('0x' + x.transactionId.slice(0, 4)))) }));
  assert.ok(selectMerges(tusd, base).length > 0);
  assert.deepEqual(selectMerges(teth, base), []);
  // a mixed list is not separated by the function: callers pass one token's UTXOs (wallet.tokenUtxos(token)); the plan never spans two calls
  const a = selectMerges(tusd, base).flat().map(outpointKey);
  const b = selectMerges(teth, base).flat().map(outpointKey);
  assert.equal(b.length, 0);
  assert.ok(a.every((k) => tusd.some((x) => outpointKey(x) === k)));
});

test('consolidateConfig: defaults, partial overrides, clamping', () => {
  assert.deepEqual(consolidateConfig(), DEFAULT_CONSOLIDATE);
  assert.deepEqual(consolidateConfig({}), DEFAULT_CONSOLIDATE);
  const c = consolidateConfig({ slack: 0, maxTxPerInterval: -2, intervalSec: 0, traders: false });
  assert.equal(c.slack, 1);
  assert.equal(c.maxTxPerInterval, 0);
  assert.equal(c.intervalSec, 1);
  assert.equal(c.traders, false);
  assert.equal(c.enabled, true);
});

test('rate limit: at most N per window per key, sliding', () => {
  const rl = new RateLimiter(3, 60_000);
  const t0 = 1_000_000;
  assert.equal(rl.allowance('mm:TUSD', t0), 3);
  rl.record('mm:TUSD', t0);
  rl.record('mm:TUSD', t0 + 1_000, 2);
  assert.equal(rl.allowance('mm:TUSD', t0 + 2_000), 0);
  assert.equal(rl.allowance('mm:TETH', t0 + 2_000), 3, 'other key is independent');
  assert.equal(rl.allowance('mm:TUSD', t0 + 59_999), 0);
  assert.equal(rl.allowance('mm:TUSD', t0 + 60_001), 1, 'the first event left the window');
  assert.equal(rl.allowance('mm:TUSD', t0 + 61_001 + 1), 3);
});

test('rate limit: allowance feeds maxBatches', () => {
  const rl = new RateLimiter(2, 60_000);
  const us = many(200);
  let now = 0;
  const round = () => {
    const bs = selectMerges(us, { ...base, maxBatches: rl.allowance('k', now) });
    rl.record('k', now, bs.length);
    return bs.length;
  };
  assert.equal(round(), 2);
  assert.equal(round(), 0);
  now += 61_000;
  assert.equal(round(), 2);
});

test('a disabled limit (0 per interval) plans nothing', () => {
  const rl = new RateLimiter(0, 60_000);
  assert.deepEqual(selectMerges(many(50), { ...base, maxBatches: rl.allowance('k', 0) }), []);
});

// ---------------------------------------------------------------- the chained pass (KOB's standard program: 3 token inputs)

const std = { ...base, maxInputs: 3 };

test('chain: 3 -> 1, then the previous output plus 2 more, every link within the 3 token inputs', () => {
  const links = selectMergeChain(many(30), { ...std, maxBatches: 4 });
  assert.deepEqual(links.map((l) => l.length), [3, 2, 2, 2]);
  // with the carried output every link spends at most 3 token inputs into one output
  links.forEach((l, i) => assert.ok(l.length + (i === 0 ? 0 : 1) <= 3));
  // smallest first, across the links, and disjoint
  const flat = links.flat();
  const sorted = many(30).map((x) => BigInt(x.state.amount)).sort((a, b) => (a < b ? -1 : 1));
  assert.deepEqual(amounts(flat), sorted.slice(0, flat.length));
  assert.equal(new Set(flat.map(outpointKey)).size, flat.length);
});

test('chain: one pass ends with one merged UTXO and never below keep', () => {
  for (const n of [17, 20, 31, 64, 200]) {
    for (const keep of [3, 4, 8]) {
      const links = selectMergeChain(many(n), { ...std, keep, maxBatches: 1000 });
      const fresh = total(links);
      assert.ok(n - fresh >= keep, `n=${n} keep=${keep}: visible ${n - fresh} < keep`);
      if (links.length > 0) assert.equal(n - fresh + 1, keep + 1, `n=${n} keep=${keep}: the pass merges the excess into one UTXO`);
    }
  }
});

test('chain: the same threshold, limit and rate rules as the disjoint plan', () => {
  assert.deepEqual(selectMergeChain(many(16), std), []);
  assert.deepEqual(selectMergeChain(many(50), { ...std, maxInputs: 1 }), []);
  assert.deepEqual(selectMergeChain(many(50), { ...std, maxBatches: 0 }), []);
  assert.equal(selectMergeChain(many(200), { ...std, maxBatches: 8 }).length, 8);
  // just above the threshold: 17 eligible, keep 8 -> 9 fresh inputs: 3 + 2 + 2 + 2
  assert.deepEqual(selectMergeChain(many(17), { ...std, maxBatches: 100 }).map((l) => l.length), [3, 2, 2, 2]);
  // the 8 / 8 prototype: 8, then 7 per link
  assert.deepEqual(selectMergeChain(many(40), { ...base, maxBatches: 3 }).map((l) => l.length), [8, 7, 7]);
  // a reserved UTXO is never selected
  const us = many(30);
  const smallest = [...us].sort((a, b) => Number(BigInt(a.state.amount) - BigInt(b.state.amount)))[0];
  assert.ok(!selectMergeChain(us, { ...std, reserved: new Set([outpointKey(smallest)]) }).flat().some((x) => outpointKey(x) === outpointKey(smallest)));
});

test('the default rate keeps up with the standard program: 8 merges a minute', () => {
  assert.equal(DEFAULT_CONSOLIDATE.maxTxPerInterval, 8);
  assert.equal(DEFAULT_CONSOLIDATE.intervalSec, 60);
});
