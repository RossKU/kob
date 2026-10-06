import { describe, expect, it } from 'vitest';
import { SignFlowError, expectedOutputs, signAndSubmit, waitForAcceptance, type SignStage } from './sign';
import { createKaswareAdapter } from './kasware';
import { createKaspireAdapter, dispatchTagFrom } from './kaspire';
import { createKastleAdapter } from './kastle';
import { fakeKasware, fakeKaspire, fakeKastle } from '../testing/fake-wallet-providers';
import { MockNode } from '../testing/mock-node';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { spkStringToAddress } from '../data/kaspa-sdk';
import { MAKER_PK, MAKER_SK, OTHER_SK } from '../testing/token-fixtures';
import { buildCancel, buildSend, kob } from '../testing/wallet-fixtures';
import type { WalletAdapter } from './types';
import type { BuiltTx } from '../kob/types';
import type { NodeUtxo } from '../data/node';

const sdk = loadKaspaSdkNode();
const NET = 'testnet-10';

const node = () => new MockNode({ network: NET, addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });
const kasware = (b = {}, sk = MAKER_SK): WalletAdapter => {
  const p = fakeKasware({ sdk, sk, behavior: b });
  return createKaswareAdapter({ getProvider: () => p });
};
const seeded = (built: BuiltTx) => {
  const n = node();
  n.seedFromInputs(built.tx);
  return n;
};

describe('signAndSubmit: the full flow', () => {
  it('signing -> finalizing -> validating -> submitting -> submitted, then the node holds the outputs', async () => {
    const built = buildSend();
    const n = seeded(built);
    const stages: [SignStage, string | undefined][] = [];
    const res = await signAndSubmit({ kob, node: n, adapter: kasware(), built, network: NET, onStage: (s, i) => stages.push([s, i?.txid]) });
    expect(stages.map((s) => s[0])).toEqual(['signing', 'finalizing', 'validating', 'submitting', 'submitted']);
    expect(stages[4]![1]).toBe(res.txid);
    expect(res.txid).toBe(built.tx.id);
    expect(res.signed.tx.id).toBe(built.tx.id);
    expect(res.fee).toBe(BigInt(built.fee.fee));
    expect(typeof res.fee).toBe('bigint');
    expect(n.submissions).toHaveLength(1);
    expect(n.submissions[0]).toEqual(res.signed.tx);
    // signature scripts were assembled by kob-wasm, not by the wallet
    expect(res.signed.tx.inputs.every((i) => i.signatureScript.length > 0)).toBe(true);
    // inputs were spent, outputs exist
    expect(n.hasUtxo(built.tx.inputs[0]!.transactionId, built.tx.inputs[0]!.index)).toBe(false);
    expect(n.hasUtxo(res.txid, 0)).toBe(true);
  });

  it('works for every wallet shape on both a token send and an order cancel', async () => {
    const wallets: [string, () => WalletAdapter][] = [
      ['kasware', () => kasware()],
      ['kaspire ordered-args', () => { const p = fakeKaspire({ sdk, sk: MAKER_SK, network: NET }); return createKaspireAdapter({ getProvider: () => p, dispatchTag: dispatchTagFrom(kob) }); }],
      ['kaspire wrap-signature', () => { const p = fakeKaspire({ sdk, sk: MAKER_SK, network: NET }); return createKaspireAdapter({ getProvider: () => p }); }],
      ['kastle', () => { const p = fakeKastle({ sdk, sk: MAKER_SK, network: NET }); return createKastleAdapter({ getProvider: () => p }); }],
    ];
    for (const [label, mkAdapter] of wallets) {
      for (const [name, build] of [['send', buildSend], ['cancel', buildCancel]] as const) {
        const built = build();
        const res = await signAndSubmit({ kob, node: seeded(built), adapter: mkAdapter(), built, network: NET });
        expect(res.txid, `${label} ${name}`).toBe(built.tx.id);
      }
    }
  });

  it('passes tightenBudgets to finalize (default true), and validate=false skips the script check stage', async () => {
    const built = buildSend();
    const opts: unknown[] = [];
    const spy = { ...kob, finalize: (b: BuiltTx, s: never, o?: unknown) => { opts.push(o); return kob.finalize(b, s, o as never); } } as typeof kob;
    const stages: string[] = [];
    await signAndSubmit({ kob: spy, node: seeded(built), adapter: kasware(), built, network: NET, tighten: false, validate: false, onStage: (s) => stages.push(s) });
    expect(opts).toEqual([{ tightenBudgets: false }]);
    expect(stages).toEqual(['signing', 'finalizing', 'submitting', 'submitted']);
    const built2 = buildSend();
    const opts2: unknown[] = [];
    const spy2 = { ...kob, finalize: (b: BuiltTx, s: never, o?: unknown) => { opts2.push(o); return kob.finalize(b, s, o as never); } } as typeof kob;
    await signAndSubmit({ kob: spy2, node: seeded(built2), adapter: kasware(), built: built2, network: NET });
    expect(opts2).toEqual([{ tightenBudgets: true }]);
  });

  it('a throwing onStage callback does not abort the flow', async () => {
    const built = buildSend();
    const res = await signAndSubmit({ kob, node: seeded(built), adapter: kasware(), built, network: NET, onStage: () => { throw new Error('ui bug'); } });
    expect(res.txid).toBe(built.tx.id);
  });
});

describe('signAndSubmit: failures stop at the right stage and nothing is broadcast', () => {
  it('wallet rejection: stage signing, code rejected, rejectedByUser, node untouched', async () => {
    const built = buildSend();
    const n = seeded(built);
    const stages: string[] = [];
    const err = await signAndSubmit({ kob, node: n, adapter: kasware({ rejectSign: true }), built, network: NET, onStage: (s) => stages.push(s) }).catch((e) => e);
    expect(err).toBeInstanceOf(SignFlowError);
    expect(err).toMatchObject({ stage: 'signing', code: 'rejected', rejectedByUser: true });
    expect(err.cause).toMatchObject({ code: 'rejected' });
    expect(stages).toEqual(['signing']);
    expect(n.attempts).toHaveLength(0);
  });

  it('wallet timeout and other wallet failures keep their code', async () => {
    const built = buildSend();
    const t = await signAndSubmit({ kob, node: seeded(built), adapter: kasware({ neverAnswer: true }), built, network: NET, timeoutMs: 20 }).catch((e) => e);
    expect(t).toMatchObject({ stage: 'signing', code: 'timeout', rejectedByUser: false });
    const o = await signAndSubmit({ kob, node: seeded(built), adapter: kasware({ failWith: new Error('locked') }), built, network: NET }).catch((e) => e);
    expect(o).toMatchObject({ stage: 'signing', code: 'other' });
  });

  it('a missing signature (wallet skipped an input) is a signing-stage no-signature error', async () => {
    const built = buildSend();
    const err = await signAndSubmit({ kob, node: seeded(built), adapter: kasware({ onlyInputs: [0] }), built, network: NET }).catch((e) => e);
    expect(err).toMatchObject({ stage: 'signing', code: 'no-signature' });
  });

  it('a signature from the WRONG key is rejected by finalize before anything is validated or sent', async () => {
    const built = buildSend();
    const n = seeded(built);
    const stages: string[] = [];
    const err = await signAndSubmit({ kob, node: n, adapter: kasware({ signKey: OTHER_SK }), built, network: NET, onStage: (s) => stages.push(s) }).catch((e) => e);
    expect(err).toBeInstanceOf(SignFlowError);
    expect(err).toMatchObject({ stage: 'finalizing', code: 'signature' });
    expect(err.message).toMatch(/does not match/);
    expect(stages).toEqual(['signing', 'finalizing']);
    expect(n.attempts).toHaveLength(0);
  });

  it('a wallet that signed a different transaction than the one shown is caught the same way', async () => {
    const built = buildSend();
    // the wallet is handed a tampered copy (an output value changed) and signs THAT: its signatures cannot match the digests of ours
    const tampered = JSON.parse(JSON.stringify(built)) as BuiltTx;
    tampered.tx.outputs[2]!.value = (BigInt(tampered.tx.outputs[2]!.value) - 1000n).toString();
    const evil: WalletAdapter = { ...kasware(), signTx: (_b, o) => kasware().signTx(tampered, o) };
    const err = await signAndSubmit({ kob, node: seeded(built), adapter: evil, built, network: NET }).catch((e) => e);
    expect(err).toMatchObject({ stage: 'finalizing', code: 'signature' });
  });

  it('script-engine failure blocks the broadcast', async () => {
    const built = buildSend();
    const n = seeded(built);
    const stages: string[] = [];
    const failing = { ...kob, validate: () => { throw new Error('covenant rule violated'); } } as unknown as typeof kob;
    const err = await signAndSubmit({ kob: failing, node: n, adapter: kasware(), built, network: NET, onStage: (s) => stages.push(s) }).catch((e) => e);
    expect(err).toMatchObject({ stage: 'validating', code: 'validation' });
    expect(err.message).toMatch(/not sent.*covenant rule violated/);
    expect(stages).toEqual(['signing', 'finalizing', 'validating']);
    expect(n.attempts).toHaveLength(0);
  });

  it('node rejection is surfaced with its classified code and readable text', async () => {
    const built = buildSend();
    const n = seeded(built).rejectNext('Rejected transaction ab: script ran, but verification failed');
    const err = await signAndSubmit({ kob, node: n, adapter: kasware(), built, network: NET }).catch((e) => e);
    expect(err).toMatchObject({ stage: 'submitting', code: 'script' });
    expect(err.cause).toMatchObject({ name: 'NodeError', code: 'script' });
    expect(err.message).not.toMatch(/Rejected transaction/);
    expect(n.attempts).toHaveLength(1);
    expect(n.submissions).toHaveLength(0);
  });

  it('an input the node does not know (already spent) -> orphan; a lost link -> unavailable', async () => {
    const built = buildSend();
    const orphan = await signAndSubmit({ kob, node: node(), adapter: kasware(), built, network: NET }).catch((e) => e);
    expect(orphan).toMatchObject({ stage: 'submitting', code: 'orphan' });
    const down = seeded(built).setUnavailable(true);
    const lost = await signAndSubmit({ kob, node: down, adapter: kasware(), built, network: NET }).catch((e) => e);
    expect(lost).toMatchObject({ stage: 'submitting', code: 'unavailable' });
  });

  it('after a successful submit the same tx cannot be sent twice (its inputs are spent)', async () => {
    const built = buildSend();
    const n = seeded(built);
    await signAndSubmit({ kob, node: n, adapter: kasware(), built, network: NET });
    const again = await signAndSubmit({ kob, node: n, adapter: kasware(), built, network: NET }).catch((e) => e);
    expect(again).toMatchObject({ stage: 'submitting', code: 'orphan' });
  });
});

describe('signAndSubmit: what the node says is checked', () => {
  const toAddress = (spk: string) => spkStringToAddress(sdk, spk, NET);

  it('a node that answers with another transaction id is not believed: the flow fails and nothing is reported as submitted', async () => {
    const built = buildSend();
    class LyingNode extends MockNode {
      override async submitTransaction(tx: Parameters<MockNode['submitTransaction']>[0]): Promise<string> {
        await super.submitTransaction(tx);
        return 'ab'.repeat(32);
      }
    }
    const n = new LyingNode({ network: NET, addressOf: toAddress });
    n.seedFromInputs(built.tx);
    const stages: string[] = [];
    const err = await signAndSubmit({ kob, node: n, adapter: kasware(), built, network: NET, onStage: (s) => stages.push(s) }).catch((e) => e);
    expect(err).toBeInstanceOf(SignFlowError);
    expect(err).toMatchObject({ stage: 'submitting', code: 'txid-mismatch' });
    expect(stages).not.toContain('submitted');
  });

  it('with inputAddress every input is re-read from the node before the wallet is asked; matching inputs sign as usual', async () => {
    const built = buildSend();
    const n = seeded(built);
    const res = await signAndSubmit({ kob, node: n, adapter: kasware(), built, network: NET, inputAddress: toAddress });
    expect(res.txid).toBe(built.tx.id);
    expect(n.utxoQueries.length).toBeGreaterThan(0);
  });

  it('an input whose KAS amount differs on the node aborts before the wallet popup (inputs-changed)', async () => {
    const built = buildCancel();
    const onNode = JSON.parse(JSON.stringify(built.tx)) as typeof built.tx;
    // the node holds 3 KAS less on the unsigned covenant input than the transaction claims
    const cov = onNode.inputs.findIndex((i, k) => i.utxo.covenantId && !built.sign.some((x) => x.inputIndex === k));
    expect(cov).toBeGreaterThanOrEqual(0);
    onNode.inputs[cov].utxo.amount = (BigInt(onNode.inputs[cov].utxo.amount) - 300_000_000n).toString();
    const n = node();
    n.seedFromInputs(onNode);
    let asked = 0;
    const adapter = kasware();
    const signTx = adapter.signTx.bind(adapter);
    adapter.signTx = async (...a) => {
      asked++;
      return signTx(...a);
    };
    const err = await signAndSubmit({ kob, node: n, adapter, built, network: NET, inputAddress: toAddress }).catch((e) => e);
    expect(err).toMatchObject({ stage: 'signing', code: 'inputs-changed' });
    expect(asked).toBe(0);
    expect(n.submissions).toHaveLength(0);
  });

  it('an input the node does not hold at all aborts the same way; a node that cannot be asked aborts with inputs-unconfirmed', async () => {
    const built = buildSend();
    const empty = await signAndSubmit({ kob, node: node(), adapter: kasware(), built, network: NET, inputAddress: toAddress }).catch((e) => e);
    expect(empty).toMatchObject({ stage: 'signing', code: 'inputs-changed' });
    const down = seeded(built).setUnavailable(true);
    const lost = await signAndSubmit({ kob, node: down, adapter: kasware(), built, network: NET, inputAddress: toAddress }).catch((e) => e);
    expect(lost).toMatchObject({ stage: 'signing', code: 'inputs-unconfirmed' });
  });
});

describe('expectedOutputs + waitForAcceptance', () => {
  const out = (address: string, txid: string, index: number): { address: string; transactionId: string; index: number } => ({ address, transactionId: txid, index });
  const utxo = (address: string, txid: string, index: number): NodeUtxo => ({ address, transactionId: txid, index, amount: '1', scriptPublicKey: '', blockDaaScore: '1', isCoinbase: false, covenantId: null });

  it('derives the addresses of a transaction\'s outputs (all or selected)', () => {
    const built = buildSend();
    const all = expectedOutputs(sdk, NET, built.tx);
    expect(all).toHaveLength(built.tx.outputs.length);
    expect(all[0]).toEqual({ address: spkStringToAddress(sdk, built.tx.outputs[0]!.scriptPublicKey, NET), transactionId: built.tx.id, index: 0 });
    expect(expectedOutputs(sdk, NET, built.tx, [2]).map((o) => o.index)).toEqual([2]);
    expect(() => expectedOutputs(sdk, NET, built.tx, [9])).toThrow(RangeError);
  });

  it('confirms once the submitted transaction\'s outputs appear on the node', async () => {
    const built = buildSend();
    const n = seeded(built);
    const res = await signAndSubmit({ kob, node: n, adapter: kasware(), built, network: NET });
    const acc = await waitForAcceptance(n, expectedOutputs(sdk, NET, res.signed.tx), { pollMs: 1 });
    expect(acc.accepted).toBe(true);
    expect(acc.missing).toEqual([]);
    expect(acc.found).toHaveLength(built.tx.outputs.length);
  });

  it('polls until ALL expected outputs are present (fake clock)', async () => {
    let t = 0;
    let polls = 0;
    const seen: NodeUtxo[] = [];
    const n = new MockNode();
    n.getUtxosByAddresses = async () => {
      polls++;
      if (polls === 3) seen.push(utxo('a', 'tx', 0));
      if (polls === 5) seen.push(utxo('b', 'tx', 1));
      return [...seen];
    };
    const res = await waitForAcceptance(n, [out('a', 'tx', 0), out('b', 'tx', 1)], { pollMs: 1000, timeoutMs: 60_000, now: () => t, sleep: async (ms) => { t += ms; } });
    expect(res).toMatchObject({ accepted: true, elapsedMs: 4000 });
    expect(polls).toBe(5);
  });

  it('times out with the missing outputs listed, and tolerates node errors while polling', async () => {
    let t = 0;
    let polls = 0;
    const n = new MockNode();
    n.getUtxosByAddresses = async () => {
      polls++;
      if (polls % 2) throw new Error('node hiccup');
      return [utxo('a', 'tx', 0)];
    };
    const res = await waitForAcceptance(n, [out('a', 'tx', 0), out('a', 'tx', 7)], { pollMs: 1000, timeoutMs: 5000, now: () => t, sleep: async (ms) => { t += ms; } });
    expect(res.accepted).toBe(false);
    expect(res.missing).toEqual([out('a', 'tx', 7)]);
    expect(res.found).toHaveLength(1);
    expect(res.elapsedMs).toBeLessThanOrEqual(5000);
    expect(polls).toBeGreaterThanOrEqual(4);
  });

  it('reports the last node error when it never got an answer, groups addresses into one query, honours abort', async () => {
    let t = 0;
    const n = new MockNode().setUnavailable(true);
    const res = await waitForAcceptance(n, out('a', 'tx', 0), { pollMs: 1000, timeoutMs: 2500, now: () => t, sleep: async (ms) => { t += ms; } });
    expect(res).toMatchObject({ accepted: false, found: [] });
    expect(res.lastError).toMatch(/unavailable/);

    const n2 = new MockNode();
    await waitForAcceptance(n2, [out('a', 'x', 0), out('a', 'x', 1), out('b', 'x', 2)], { pollMs: 1, timeoutMs: 0, now: () => 0, sleep: async () => undefined });
    expect(n2.utxoQueries[0]).toEqual(['a', 'b']);

    const ctl = new AbortController();
    ctl.abort();
    const n3 = new MockNode();
    const r3 = await waitForAcceptance(n3, out('a', 'x', 0), { signal: ctl.signal, pollMs: 1, timeoutMs: 60_000 });
    expect(r3.accepted).toBe(false);
    expect(n3.utxoQueries).toHaveLength(1); // one look, then it stops
  });
});
