// The bots' own-transaction bookkeeping (src/bots/pending.ts): a listing that lags the chain, after the wallet's own pending spends; the
// SELF_TRADE refusal's own orders; the per-key lock.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Mutex, mempoolSpent, outpointOf, selfTradeIds, splitPending } from '../src/bots/pending.ts';

const view = (id: string, txid: string | null, index = 0) => ({ covenant_id: id, current: txid === null ? null : { txid, index } });

test('an order whose listed UTXO the wallet already spends (a cancel or amend the indexer has not caught up with) is pending', () => {
  const a = view('a', 'aa'.repeat(32), 0);
  const b = view('b', 'bb'.repeat(32), 1);
  const c = view('c', null);
  const reserved = new Set([`${'bb'.repeat(32)}:1`, `${'aa'.repeat(32)}:1`]);
  const { live, pending } = splitPending([a, b, c], reserved);
  assert.deepEqual(live.map((v) => v.covenant_id), ['a', 'c']);
  assert.deepEqual(pending.map((v) => v.covenant_id), ['b']);
  assert.equal(outpointOf(b), `${'bb'.repeat(32)}:1`);
  assert.equal(outpointOf(c), null);
});

test('a SELF_TRADE refusal names every own order the plan would trade against, once each; other issues are ignored', () => {
  const ids = selfTradeIds([
    { code: 'SELF_TRADE', params: { ownCovenantId: 'x1' } },
    { code: 'MARKET_START_UNVERIFIED', params: {} },
    { code: 'SELF_TRADE', params: { ownCovenantId: 'x2' } },
    { code: 'SELF_TRADE', params: { ownCovenantId: 'x1' } },
    { code: 'SELF_TRADE' },
  ]);
  assert.deepEqual(ids, ['x1', 'x2']);
});

test('the per-key lock runs sections one at a time in call order, and a failing section does not block the next', async () => {
  const m = new Mutex();
  const log: string[] = [];
  let inside = 0;
  const section = (name: string, ms: number, fail = false) =>
    m.run(async () => {
      inside++;
      assert.equal(inside, 1, 'two sections of one key overlap');
      log.push(`${name}:start`);
      await new Promise((r) => setTimeout(r, ms));
      log.push(`${name}:end`);
      inside--;
      if (fail) throw new Error(name);
      return name;
    });
  const r = await Promise.allSettled([section('a', 20), section('b', 1, true), section('c', 5)]);
  assert.deepEqual(log, ['a:start', 'a:end', 'b:start', 'b:end', 'c:start', 'c:end']);
  assert.equal(r[0]!.status, 'fulfilled');
  assert.equal(r[1]!.status, 'rejected');
  assert.deepEqual(r[2], { status: 'fulfilled', value: 'c' });
});

test('the outpoints the mempool already spends in the transactions a key sent (however long they wait there)', () => {
  const input = (txid: string, index: number) => ({ previousOutpoint: { transactionId: txid, index } });
  const spent = mempoolSpent({
    entries: [
      { sending: [{ transaction: { inputs: [input('aa', 0), input('bb', 2)] } }, { transaction: { inputs: [input('cc', 1)] } }] },
      { sending: [] },
    ],
  });
  assert.deepEqual([...spent].sort(), ['aa:0', 'bb:2', 'cc:1']);
  assert.deepEqual([...mempoolSpent({ entries: [] })], []);
});
