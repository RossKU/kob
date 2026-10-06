import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { MockNode } from './mock-node';
import type { TxJson } from '../kob/types';

const golden = JSON.parse(readFileSync(fileURLToPath(new URL('../../../crates/kob-protocol/vectors/golden.json', import.meta.url)), 'utf8'));
const signed: TxJson = golden.transactions.find((t: { name: string }) => t.name === 'send.tokens').signed.tx;

describe('MockNode', () => {
  it('serves a settable UTXO set filtered by address, and records the queries', async () => {
    const n = new MockNode();
    n.addUtxo({ address: 'a', transactionId: '11'.repeat(32), index: 0, amount: '5' });
    n.addUtxo({ address: 'b', transactionId: '22'.repeat(32), index: 1, amount: '6', covenantId: '33'.repeat(32) });
    expect((await n.getUtxosByAddresses(['a'])).map((u) => u.amount)).toEqual(['5']);
    expect((await n.getUtxosByAddresses(['a', 'b'])).map((u) => u.covenantId)).toEqual([null, '33'.repeat(32)]);
    expect(n.utxoQueries).toEqual([['a'], ['a', 'b']]);
    n.removeUtxo('11'.repeat(32), 0);
    expect(await n.getUtxosByAddresses(['a'])).toEqual([]);
  });

  it('clock and info are settable and advance together', async () => {
    const n = new MockNode({ daa: 100n, unixSeconds: 1000n, rateMilli: null });
    expect(await n.getClock()).toEqual({ daa: 100n, unixSeconds: 1000n, rateMilli: null });
    n.advance(50n);
    expect(await n.getClock()).toEqual({ daa: 150n, unixSeconds: 1005n, rateMilli: null });
    expect((await n.connect()).virtualDaaScore).toBe('150');
  });

  it('a submitted transaction spends its inputs and creates its outputs (with covenant ids)', async () => {
    const n = new MockNode({ addressOf: (spk) => 'addr:' + spk.slice(0, 12) });
    n.seedFromInputs(signed);
    expect(n.all()).toHaveLength(signed.inputs.length);
    const id = await n.submitTransaction(signed);
    expect(id).toBe(signed.id);
    expect(n.submissions).toHaveLength(1);
    const now = n.all();
    expect(now).toHaveLength(signed.outputs.length);
    expect(now.map((u) => [u.transactionId, u.index])).toEqual(signed.outputs.map((_, i) => [signed.id, i]));
    expect(now[0]!.covenantId).toBe(signed.outputs[0]!.covenant!.covenantId);
    expect(now[signed.outputs.length - 1]!.covenantId).toBeNull();
    // the same transaction again: its inputs are gone -> orphan, like a real node
    await expect(n.submitTransaction(signed)).rejects.toMatchObject({ code: 'orphan' });
    expect(n.attempts).toHaveLength(2);
    expect(n.submissions).toHaveLength(1);
  });

  it('an unknown input is an orphan; checkInputs=false and applyOnSubmit=false relax that', async () => {
    await expect(new MockNode().submitTransaction(signed)).rejects.toMatchObject({ code: 'orphan' });
    const lax = new MockNode({ checkInputs: false, applyOnSubmit: false });
    expect(await lax.submitTransaction(signed)).toBe(signed.id);
    expect(lax.all()).toEqual([]);
  });

  it('rejectNext injects failures once (classified) and setUnavailable simulates a lost link', async () => {
    const n = new MockNode({ checkInputs: false });
    n.rejectNext('script ran, but verification failed');
    await expect(n.submitTransaction(signed)).rejects.toMatchObject({ code: 'script' });
    expect(n.submissions).toHaveLength(0);
    expect(await n.submitTransaction(signed)).toBe(signed.id);
    n.setUnavailable(true);
    await expect(n.getUtxosByAddresses(['a'])).rejects.toMatchObject({ code: 'unavailable' });
    await expect(n.getClock()).rejects.toMatchObject({ code: 'unavailable' });
    n.setUnavailable(false);
    await expect(n.getClock()).resolves.toBeDefined();
  });
});
