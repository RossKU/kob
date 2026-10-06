// The bank's payout planning: the pure retry ladder, and the real SDK generator's storage-mass dead band (the `Mass calculation error`
// root cause, see src/bank-plan.ts). The SDK part runs when web/vendor/kaspa-node is present.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { consolidationOutputs, createWithMassRetry, isMassError, KAS, marginCoins, marginFor, payoutAttempts, RecentSpends, type PayoutOutput } from '../src/bank-plan.ts';

const out = (kas: number): PayoutOutput => ({ address: 'kaspatest:x', amount: BigInt(kas) * KAS });

test('payoutAttempts: the plan first, then bumped first outputs, then fewer outputs; never an empty payout', () => {
  const a = payoutAttempts([out(100), out(100), out(100)]);
  assert.deepEqual(a[0].map((o) => o.amount), [100n * KAS, 100n * KAS, 100n * KAS]);
  assert.deepEqual(a[1].map((o) => o.amount), [101n * KAS, 100n * KAS, 100n * KAS]);
  assert.equal(a.length, 1 + 3 * 5);
  assert.ok(a.every((s) => s.length >= 1));
  assert.deepEqual(a.at(-1)!.map((o) => o.amount), [108n * KAS]);
  assert.deepEqual(payoutAttempts([]), [[]]);
  // the input is not mutated
  const plan = [out(100)];
  payoutAttempts(plan);
  assert.equal(plan[0].amount, 100n * KAS);
});

test('consolidationOutputs: one output of the total less 1 KAS, nothing for a dust total', () => {
  assert.deepEqual(consolidationOutputs('a', 250n * KAS), [{ address: 'a', amount: 249n * KAS }]);
  assert.deepEqual(consolidationOutputs('a', 2n * KAS), []);
  assert.deepEqual(consolidationOutputs('a', 0n), []);
});

test('RecentSpends: submitted inputs and paid wallets are held back until the ttl, then offered again', () => {
  const r = new RecentSpends(1000);
  const k = RecentSpends.key('ab', 1);
  assert.equal(k, 'ab:1');
  assert.equal(r.isSpent(k, 0), false);
  r.spend([k], 100);
  assert.equal(r.isSpent(k, 100), true);
  assert.equal(r.isSpent(RecentSpends.key('ab', 2), 100), false);
  assert.equal(r.isSpent(k, 1099), true);
  assert.equal(r.isSpent(k, 1100), false, 'ttl over: a transaction that never confirmed frees its coins');
  r.pay('addr', 0);
  assert.equal(r.isPaid('addr', 999), true);
  assert.equal(r.isPaid('other', 999), false);
  assert.equal(r.isPaid('addr', 1000), false);
});

test('createWithMassRetry: a refused plan is re-planned, other errors are thrown at once', async () => {
  const calls: bigint[] = [];
  const dead = (outs: PayoutOutput[]) => {
    calls.push(outs[0].amount);
    if (outs[0].amount === 100n * KAS) throw new Error('Mass calculation error');
    return Promise.resolve('ok');
  };
  const r = await createWithMassRetry(dead, [out(100), out(100)]);
  assert.equal(r.result, 'ok');
  assert.equal(r.retries, 1);
  assert.deepEqual(r.outputs.map((o) => o.amount), [101n * KAS, 100n * KAS]);

  await assert.rejects(createWithMassRetry(() => Promise.reject(new Error('RPC timeout')), [out(100)]), /RPC timeout/);
  // the plan itself unfunded: not retried
  let n = 0;
  await assert.rejects(createWithMassRetry(() => (n++, Promise.reject(new Error('Insufficient funds'))), [out(100)]), /Insufficient/);
  assert.equal(n, 1);
  // every attempt refused: the plan's own error comes back, after every alternative was tried
  n = 0;
  await assert.rejects(createWithMassRetry(() => (n++, Promise.reject(new Error(n === 1 ? 'Mass calculation error' : 'Storage mass exceeds maximum'))), [out(100), out(100)]), /Mass calculation error/);
  assert.equal(n, 1 + 2 * 5);
  // an alternative the funds do not cover is skipped, the next one is tried
  const fund = (outs: PayoutOutput[]) => {
    if (outs[0].amount === 100n * KAS) return Promise.reject(new Error('Mass calculation error'));
    if (outs[0].amount < 103n * KAS) return Promise.reject(new Error('Insufficient funds'));
    return Promise.resolve('ok');
  };
  assert.equal((await createWithMassRetry(fund, [out(100)])).retries, 3);
  assert.ok(isMassError(new Error('Storage mass exceeds maximum')));
  assert.ok(!isMassError(new Error('Insufficient funds')));
});

const SDK_DIR = fileURLToPath(new URL('../../../web/vendor/kaspa-node/', import.meta.url));

test('real SDK generator: a change in the storage-mass dead band is refused, and the retry ladder pays', { skip: !existsSync(SDK_DIR + 'kaspa.js') }, async () => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const sdk = createRequire(import.meta.url)(SDK_DIR + 'kaspa.js') as any;
  const network = 'testnet-10';
  const address = new sdk.PrivateKey('01'.repeat(32)).toPublicKey().toAddress(network).toString();
  const COIN = 308_672_142n; // a TN10 coinbase coin of the live bank (3.0867 KAS)
  const entries = Array.from({ length: 80 }, (_, i) => ({
    address,
    outpoint: { transactionId: (i + 1).toString(16).padStart(64, '0'), index: 0 },
    amount: COIN,
    scriptPublicKey: sdk.payToAddressScript(address),
    blockDaaScore: 1000n,
    isCoinbase: false,
  }));
  const create = (outputs: PayoutOutput[]) => sdk.createTransactions({ entries, outputs: outputs.map((o) => ({ address: o.address, amount: o.amount })), changeAddress: address, priorityFee: 0n, networkId: network });
  // 33 coins minus 0.05 KAS: the leftover after the fee is a change output whose storage mass is over the 100,000 limit (live bank, 10-02)
  const dead: PayoutOutput = { address, amount: 33n * COIN - 5_000_000n };
  await assert.rejects(create([dead]), (e: unknown) => typeof e === "string" && isMassError(e) && /Mass calculation error/.test(e));
  const ok = createWithMassRetry(create, [dead]);
  const r = await ok;
  assert.ok(r.retries >= 1);
  assert.equal(r.result.transactions.length, 1);
  // a change of 0.2 KAS is fine as planned
  const fine = await createWithMassRetry(create, [{ address, amount: 33n * COIN - 20_000_000n }]);
  assert.equal(fine.retries, 0);
});

test('real SDK generator: the consolidation request merges 80 coins into one transaction (no outputs would spend one coin back)', { skip: !existsSync(SDK_DIR + 'kaspa.js') }, async () => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const sdk = createRequire(import.meta.url)(SDK_DIR + 'kaspa.js') as any;
  const network = 'testnet-10';
  const address = new sdk.PrivateKey('01'.repeat(32)).toPublicKey().toAddress(network).toString();
  const entries = Array.from({ length: 80 }, (_, i) => ({
    address,
    outpoint: { transactionId: (i + 1).toString(16).padStart(64, '0'), index: 0 },
    amount: 308_672_142n,
    scriptPublicKey: sdk.payToAddressScript(address),
    blockDaaScore: 1000n,
    isCoinbase: false,
  }));
  const gen = (outputs: { address: string; amount: bigint }[]) => sdk.createTransactions({ entries, outputs, changeAddress: address, priorityFee: 0n, networkId: network });
  const none = await gen([]);
  assert.equal(none.transactions[0].serializeToObject().inputs.length, 1, 'no outputs: a single input is spent');
  const total = entries.reduce((s, e) => s + e.amount, 0n);
  const merged = await gen(consolidationOutputs(address, total));
  assert.equal(merged.transactions.length, 1);
  assert.equal(merged.transactions[0].serializeToObject().inputs.length, 80);
});

test('marginCoins: the needed coins with the largest last, one big coin alone, null when it cannot be forced', () => {
  const c = (kas: number) => ({ amount: BigInt(Math.round(kas * 1e8)) });
  const margin = marginFor(100n); // 0.5 KAS
  assert.equal(margin, 50_000_000n);
  // consolidation: 100 + 3 x 3 KAS, output = total - 1 KAS: all four coins, the 100 KAS coin last
  const coins = [c(3), c(100), c(3), c(3)];
  const set = marginCoins(coins, 109n * KAS - KAS, margin)!;
  assert.deepEqual(set.map((x) => x.amount), [3n * KAS, 3n * KAS, 3n * KAS, 100n * KAS]);
  // everything before the last coin is below the outputs: the generator cannot stop early
  assert.ok(set.slice(0, -1).reduce((s, x) => s + x.amount, 0n) < 108n * KAS);
  // a giant covers the payout alone
  assert.deepEqual(marginCoins([c(3), c(461_621), c(2)], 1200n * KAS, margin)!.map((x) => x.amount), [461_621n * KAS]);
  // only the needed coins: the smallest are left out
  assert.equal(marginCoins([c(50), c(40), c(30), c(0.01)], 60n * KAS, margin)!.length, 2);
  // not enough funds
  assert.equal(marginCoins([c(3), c(3)], 6n * KAS, margin), null);
  // all coins tiny against the margin: no order forces the stop
  assert.equal(marginCoins(Array.from({ length: 50 }, () => c(0.1)), 3n * KAS, margin), null);
  // the input is not mutated
  assert.equal(coins[0].amount, 3n * KAS);
});

test('createWithMassRetry: the coin variants come right after the plan, before the output ladder', async () => {
  const seen: string[] = [];
  const create = (outs: PayoutOutput[], coins?: string) => {
    seen.push(`${outs[0].amount / KAS}:${coins ?? '-'}`);
    return coins === 'B' ? Promise.resolve('ok') : Promise.reject(new Error('Storage mass exceeds maximum'));
  };
  const r = await createWithMassRetry(create, [out(100)], ['A', 'B']);
  assert.equal(r.result, 'ok');
  assert.deepEqual(seen, ['100:-', '100:A', '100:B']);
  assert.equal(r.retries, 2);
  assert.deepEqual(r.outputs.map((o) => o.amount), [100n * KAS], 'the plan was paid unchanged');
});

/** the TN10 bank of 10-03 10:28 (live listing, below the merged coin): 46 coinbase coins of 3.0851 KAS, then the smaller ones */
const LIVE_SMALL = [...Array.from({ length: 46 }, () => 3.0851), 2.9826, 2.8813, 2.5279, 1.9941, 1.4913, 1.3834, 1.2986, 1.2039, 1.1914, 1.1512, 1.1033, 0.9899, 0.9868, ...Array.from({ length: 20 }, () => 0.9096)];
const LIVE_GIANT = 461_621.5306;

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Gen = (outputs: PayoutOutput[], coins?: any[]) => Promise<any>;

function sdkBank(coinsKas: number[]): { address: string; entries: { amount: bigint }[]; gen: Gen } {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const sdk = createRequire(import.meta.url)(SDK_DIR + 'kaspa.js') as any;
  const network = 'testnet-10';
  const address = new sdk.PrivateKey('01'.repeat(32)).toPublicKey().toAddress(network).toString();
  const entries = coinsKas.map((k, i) => ({
    address,
    outpoint: { transactionId: (i + 1).toString(16).padStart(64, '0'), index: 0 },
    amount: BigInt(Math.round(k * 1e8)),
    scriptPublicKey: sdk.payToAddressScript(address),
    blockDaaScore: 1000n,
    isCoinbase: false,
  }));
  const gen: Gen = (outputs, coins) =>
    sdk.createTransactions({ entries: coins ?? entries, outputs: outputs.map((o) => ({ address: o.address, amount: o.amount })), changeAddress: address, priorityFee: 0n, feeRate: 100, networkId: network });
  return { address, entries, gen };
}

const sum = (os: readonly { amount: bigint }[]): bigint => os.reduce((s, o) => s + o.amount, 0n);

test('real SDK generator: the 10-03 live bank (one 461,621 KAS coin + 79 small ones) consolidates at keep 1 KAS through the margin coins', { skip: !existsSync(SDK_DIR + 'kaspa.js') }, async () => {
  const { address, entries, gen } = sdkBank([LIVE_GIANT, ...LIVE_SMALL]);
  assert.equal(entries.length, 80);
  const plan = consolidationOutputs(address, sum(entries));
  // the plan as the bank sent it (largest first, output = total - 1 KAS): refused every tick, 56 times
  await assert.rejects(gen(plan), (e: unknown) => typeof e === 'string' && isMassError(e));
  // the old ladder (the output only raised) finds nothing either
  await assert.rejects(createWithMassRetry((o) => gen(o), plan), (e: unknown) => typeof e === 'string' && isMassError(e));
  // with the margin coins: planned as asked, one transaction that spends all 80 coins, change of ~0.4 KAS
  const set = marginCoins(entries, plan[0].amount, marginFor(100n))!;
  const r = await createWithMassRetry((o, coins?: typeof set) => gen(o, coins), plan, [set]);
  assert.equal(r.retries, 1);
  assert.equal(r.result.transactions.length, 1);
  const tx = r.result.transactions[0].serializeToObject();
  assert.equal(tx.inputs.length, 80);
  assert.equal(tx.outputs.length, 2);
  assert.deepEqual(r.outputs, plan);
});

test('real SDK generator: every keep from 0.2 to 6 KAS on the live coin set pays, and so does every payout size', { skip: !existsSync(SDK_DIR + 'kaspa.js') }, async () => {
  const { address, entries, gen } = sdkBank([LIVE_GIANT, ...LIVE_SMALL]);
  const total = sum(entries);
  let retried = 0;
  for (let keepCents = 20; keepCents <= 600; keepCents += 1) {
    const plan = [{ address, amount: total - BigInt(keepCents) * 1_000_000n }];
    const set = marginCoins(entries, plan[0].amount, marginFor(100n));
    const r = await createWithMassRetry((o, coins?: unknown[]) => gen(o, coins), plan, set ? [set] : []);
    assert.equal(r.result.transactions.length, 1, `keep ${keepCents / 100}`);
    if (r.retries > 0) retried++;
  }
  assert.ok(retried > 20, 'the sweep does cross the dead bands (otherwise it proves nothing)');
  // payouts of 1..12 chunks of 100 KAS
  for (let n = 1; n <= 12; n++) {
    const outs = Array.from({ length: n }, () => ({ address, amount: 100n * KAS }));
    const set = marginCoins(entries, sum(outs), marginFor(100n));
    const r = await createWithMassRetry((o, coins?: unknown[]) => gen(o, coins), outs, set ? [set] : []);
    assert.equal(r.result.transactions.length, 1);
  }
});

test('real SDK generator: random coin sets and request sizes always find a plan (seeded)', { skip: !existsSync(SDK_DIR + 'kaspa.js') }, async () => {
  let seed = 20261003;
  const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32);
  let planned = 0;
  for (let round = 0; round < 90; round++) {
    const n = 1 + Math.floor(rnd() * 80);
    const kind = round % 3;
    const coins = Array.from({ length: n }, () => (kind === 0 ? 3.0851 : kind === 1 ? 0.5 + rnd() * 4 : rnd() < 0.05 ? 1000 + rnd() * 5000 : 0.3 + rnd() * 3));
    const { address, entries, gen } = sdkBank(coins);
    const sorted = [...entries].sort((a, b) => (b.amount > a.amount ? 1 : -1)); // the bank hands them over largest first
    const outs: PayoutOutput[] =
      rnd() < 0.5
        ? consolidationOutputs(address, sum(entries))
        : Array.from({ length: 1 + Math.floor(rnd() * 12) }, () => ({ address, amount: BigInt(Math.round((10 + rnd() * 190) * 1e8)) }));
    if (outs.length === 0 || sum(outs) + KAS > sum(entries)) continue; // not payable at all
    const set = marginCoins(sorted, sum(outs), marginFor(100n));
    const r = await createWithMassRetry((o, c2?: unknown[]) => gen(o, c2 ?? sorted), outs, set ? [set] : []).catch((e) => {
      throw new Error(`round ${round} (${n} coins, kind ${kind}, ${outs.length} outputs, total ${sum(entries)}): ${e}`);
    });
    assert.ok(r.result.transactions.length >= 1);
    planned++;
  }
  assert.ok(planned >= 30, `only ${planned} rounds were payable`);
});
