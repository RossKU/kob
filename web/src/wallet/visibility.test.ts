// The visibility gate: an extension wallet is asked to sign an input spending a fresh output only once that output is visible to its own node
// (announced to it, or listed by KOB's node plus a margin); the private-key signer and old inputs pass at once.
import { beforeEach, describe, expect, it } from 'vitest';
import { MockNode } from '../testing/mock-node';
import type { TxJson } from '../kob/types';
import type { WalletAdapter } from './types';
import { FRESH_MS, SETTLE_MS, awaitVisible, freshInputs, noteSent, resetVisibility, sentAt } from './visibility';

const A = 'aa'.repeat(32);
const B = 'bb'.repeat(32);
const SPK = '0000' + '20' + 'cc'.repeat(32) + 'ac';
const tx = (id: string, inputs: [string, number][] = []): TxJson =>
  ({
    id, version: 1, subnetworkId: '00'.repeat(20), lockTime: '0', gas: '0', storageMass: '0', payload: '',
    inputs: inputs.map(([t, i]) => ({ transactionId: t, index: i, sequence: '0', sigOpCount: 1, signatureScript: '', utxo: { address: null, amount: '1000', scriptPublicKey: SPK, blockDaaScore: '0', isCoinbase: false, covenantId: null } })),
    outputs: [{ value: '1000', scriptPublicKey: SPK, covenant: null }],
  }) as unknown as TxJson;

const wallet = (o: Partial<WalletAdapter> = {}): WalletAdapter & { announced: string[] } => {
  const announced: string[] = [];
  return {
    id: 'kasware', label: 'test', detect: () => true, announced,
    connect: async () => { throw new Error('unused'); },
    signTx: async () => [],
    ...o,
  } as WalletAdapter & { announced: string[] };
};

function clock() {
  let t = 1_000_000;
  const slept: number[] = [];
  return { now: () => t, sleep: async (ms: number) => void (slept.push(ms), (t += ms)), slept, advance: (ms: number) => void (t += ms) };
}

const node = () => new MockNode({ addressOf: () => 'kaspatest:addr' });
const list = (n: MockNode, txid: string, index = 0) => n.addUtxo({ address: 'kaspatest:addr', transactionId: txid, index, amount: '1000', scriptPublicKey: SPK });

beforeEach(() => resetVisibility());

describe('noteSent / freshInputs', () => {
  it('remembers what this page sent for FRESH_MS; only inputs spending those (or alsoFresh) are fresh', () => {
    const c = clock();
    void noteSent(null, tx(A), { now: c.now });
    expect(sentAt(A, c.now())).toBe(c.now());
    const spend = tx(B, [[A, 0], ['dd'.repeat(32), 1]]);
    expect(freshInputs(spend, { now: c.now() }).map((f) => f.input.transactionId)).toEqual([A]);
    expect(freshInputs(spend, { now: c.now(), alsoFresh: new Map([['dd'.repeat(32), c.now() - 5]]) })).toHaveLength(2);
    expect(freshInputs(spend, { now: c.now() + FRESH_MS + 1 })).toEqual([]);
  });

  it('announces through the adapter when it can, and never rejects', async () => {
    const ok = wallet({ announce: async (t) => void ok.announced.push(t.id) });
    expect(await noteSent(ok, tx(A))).toBe(true);
    expect(ok.announced).toEqual([A]);
    const failing = wallet({ announce: async () => { throw new Error('pushTx: boom'); } });
    expect(await noteSent(failing, tx(B))).toBe(false);
    const throwing = wallet({ announce: () => { throw new Error('sync'); } });
    expect(await noteSent(throwing, tx(B))).toBe(false);
    // the private-key signer is never asked to relay anything
    const offline = wallet({ inputPolicy: 'offline', announce: async () => void offline.announced.push('x') });
    expect(await noteSent(offline, tx(A))).toBe(false);
    expect(offline.announced).toEqual([]);
  });
});

describe('awaitVisible', () => {
  it('passes at once for an offline signer and for a transaction with no fresh input', async () => {
    const n = node();
    const c = clock();
    void noteSent(null, tx(A), { now: c.now });
    expect(await awaitVisible({ adapter: wallet({ inputPolicy: 'offline' }), node: n, tx: tx(B, [[A, 0]]), addressOf: () => 'x', ...c })).toEqual({ ok: true, waitedMs: 0 });
    expect(await awaitVisible({ adapter: wallet(), node: n, tx: tx(B, [['ee'.repeat(32), 0]]), addressOf: () => 'x', ...c })).toEqual({ ok: true, waitedMs: 0 });
    expect(n.utxoQueries).toEqual([]);
  });

  it('an output announced to this wallet is visible at once: no node poll, no margin', async () => {
    const n = node();
    const c = clock();
    const w = wallet({ announce: async () => undefined });
    await noteSent(w, tx(A), { now: c.now });
    const r = await awaitVisible({ adapter: w, node: n, tx: tx(B, [[A, 0]]), addressOf: () => 'kaspatest:addr', ...c });
    expect(r.ok).toBe(true);
    expect(n.utxoQueries).toEqual([]);
    expect(c.slept).toEqual([]);
  });

  it("another wallet's announce does not count; without one it polls KOB's node, then waits the margin", async () => {
    const n = node();
    const c = clock();
    await noteSent(wallet({ id: 'kaspire', announce: async () => undefined }), tx(A), { now: c.now });
    const waits: boolean[] = [];
    let polls = 0;
    const real = n.getUtxosByAddresses.bind(n);
    n.getUtxosByAddresses = async (a) => {
      if (++polls === 3) list(n, A);
      return real(a);
    };
    const r = await awaitVisible({ adapter: wallet(), node: n, tx: tx(B, [[A, 0]]), addressOf: () => 'kaspatest:addr', onWait: (w) => waits.push(w), ...c });
    expect(r.ok).toBe(true);
    expect(polls).toBe(3);
    expect(c.slept).toEqual([500, 500, SETTLE_MS]);
    expect(waits).toEqual([true, false]);
  });

  it('an output sent long ago and listed needs no margin', async () => {
    const n = node();
    const c = clock();
    void noteSent(null, tx(A), { now: c.now });
    c.advance(31_000);
    list(n, A);
    const r = await awaitVisible({ adapter: wallet(), node: n, tx: tx(B, [[A, 0]]), addressOf: () => 'kaspatest:addr', ...c });
    expect(r.ok).toBe(true);
    expect(c.slept).toEqual([]);
  });

  it('times out with what is missing, through node errors, and the wallet is not asked', async () => {
    const n = node();
    const c = clock();
    void noteSent(null, tx(A), { now: c.now });
    n.setUnavailable(true);
    const waits: boolean[] = [];
    const r = await awaitVisible({ adapter: wallet(), node: n, tx: tx(B, [[A, 0]]), addressOf: () => 'kaspatest:addr', timeoutMs: 5000, onWait: (w) => waits.push(w), ...c });
    expect(r).toMatchObject({ ok: false, missing: [{ transactionId: A, index: 0 }] });
    expect(r.ok === false && r.lastError).toMatch(/unavailable/);
    expect(r.waitedMs).toBeLessThanOrEqual(5000);
    expect(waits).toEqual([true, false]);
  });

  it('a creation resumed on a new page: the record names the preparation as fresh (alsoFresh)', async () => {
    const n = node();
    const c = clock();
    list(n, A);
    const r = await awaitVisible({ adapter: wallet(), node: n, tx: tx(B, [[A, 0]]), addressOf: () => 'kaspatest:addr', alsoFresh: new Map([[A, c.now() - 1000]]), ...c });
    expect(r.ok).toBe(true);
    expect(c.slept).toEqual([SETTLE_MS]);
  });
});
