// The token creation flow (KCC-23: preparation, then the genesis spending it) against the mock node and the fake wallets.
//
//   * one approval (the private-key signer, `signChain`): both signed before anything is sent, the preparation first;
//   * an extension wallet that looks every input up on its own node (KasWare 0.11.5, the fakes' `walletNode`): the preparation is signed and
//     SENT, handed to the wallet's node, and only then is the wallet asked for the genesis;
//   * the creation record kept exactly while the genesis may still be missing (a declined second approval, a reload, a refused genesis), and
//     finished later: with no signature when the genesis was signed, with one when it was not; a v1 record still resumes.
import { beforeEach, describe, expect, it } from 'vitest';
import { finishCreation, resumeCreation, signAndSubmitCreation, CREATION_SPENT } from './create';
import { SignFlowError, type SignStage } from './sign';
import { createKaswareAdapter } from './kasware';
import { createKaspireAdapter } from './kaspire';
import { WalletError, type WalletAdapter } from './types';
import { resetVisibility } from './visibility';
import { fakeKaspire, fakeKasware, laggingWalletNode, type FakeWalletNode } from '../testing/fake-wallet-providers';
import { MockNode } from '../testing/mock-node';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { spkStringToAddress } from '../data/kaspa-sdk';
import { MAKER_PK, MAKER_SK } from '../testing/token-fixtures';
import { kob } from '../testing/wallet-fixtures';
import { signBuilt } from '../testing/local-signer';
import { emptyIssueForm, planIssue, type IssuePlan } from '../kob/issue';
import { listCreations, type CreationRecord } from '../data/creations';
import type { StorageLike } from '../config';
import type { BuiltTx, InputSignature } from '../kob/types';

const sdk = loadKaspaSdkNode();
const NET = 'testnet-10';
const KAS = 100_000_000n;
const addressOf = (spk: string) => spkStringToAddress(sdk, spk, NET);

function memory(): StorageLike & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v), removeItem: (k) => void data.delete(k) };
}

function setup(o: { applyOnSubmit?: boolean } = {}) {
  const plan: IssuePlan = planIssue(kob, { ...emptyIssueForm(), name: 'Flow Coin', ticker: 'FLOW', decimals: 2, supply: '1000' }, {
    funding: [{ transactionId: '0a'.repeat(32), index: 3, amount: (200n * KAS).toString(), blockDaaScore: '100', pubkey: MAKER_PK }],
    maker: MAKER_PK,
    network: NET,
  });
  const node = new MockNode({ network: NET, addressOf, ...(o.applyOnSubmit === false ? { applyOnSubmit: false } : {}) });
  node.seedFromInputs(plan.prep.tx);
  const storage = memory();
  return { plan, node, storage };
}

/** The fake KasWare 0.11.5 with its own node (`walletNode`, default: a lagging view of the test's node that `pushTx` feeds). */
const kasware = (s: ReturnType<typeof setup>, behavior = {}, walletNode: FakeWalletNode = laggingWalletNode(s.node)) => {
  const p = fakeKasware({ sdk, sk: MAKER_SK, behavior, walletNode });
  return {
    provider: p,
    adapter: createKaswareAdapter({ getProvider: () => p }),
    signs: () => p.calls.filter((c) => c.method === 'signPskt').length,
    order: () => p.calls.filter((c) => c.method === 'signPskt' || c.method === 'pushTx').map((c) => c.method),
  };
};

/** A wallet with one approval for both (as the private-key signer): signs locally, counts its approvals. */
function oneApproval(): { adapter: WalletAdapter; approvals: () => number } {
  let n = 0;
  const sign = (b: BuiltTx): InputSignature[] => signBuilt(b, [MAKER_SK]);
  return {
    approvals: () => n,
    adapter: {
      id: 'local-key', label: 'test', inputPolicy: 'offline', detect: () => true,
      connect: async () => { throw new Error('unused'); },
      signTx: async () => { throw new Error('a creation is signed with signChain'); },
      signChain: async ([prep, built]) => {
        n++;
        return [sign(prep), sign(built)];
      },
    },
  };
}

const args = (s: ReturnType<typeof setup>, adapter: WalletAdapter) => ({
  kob, node: s.node, adapter, prep: s.plan.prep, built: s.plan.built, network: NET, maker: MAKER_PK, storage: s.storage, addressOf,
  record: { token: s.plan.token, declaration: s.plan.declaration, docs: s.plan.docs }, sleep: async () => undefined,
});

const finishArgs = (s: ReturnType<typeof setup>, adapter: WalletAdapter, rec: CreationRecord) => ({
  kob, node: s.node, adapter, network: NET, rec, addressOf, storage: s.storage, sleep: async () => undefined,
});

const onlyRecord = (s: ReturnType<typeof setup>): CreationRecord => {
  const all = listCreations(NET, MAKER_PK, s.storage);
  expect(all).toHaveLength(1);
  return all[0]!;
};

beforeEach(() => resetVisibility());

describe('signAndSubmitCreation: one approval (the private-key signer)', () => {
  it('both signed, the preparation sent first, the genesis spending it; nothing left to resume', async () => {
    const s = setup();
    const w = oneApproval();
    const stages: SignStage[] = [];
    const res = await signAndSubmitCreation({ ...args(s, w.adapter), onStage: (st) => stages.push(st) });
    expect(w.approvals()).toBe(1);
    expect(stages).toEqual(['signing', 'finalizing', 'validating', 'submitting', 'submitting', 'submitted']);
    expect(s.node.submissions.map((t) => t.id)).toEqual([s.plan.prep.tx.id, s.plan.built.tx.id]);
    expect(res).toMatchObject({ prepTxid: s.plan.prep.tx.id, txid: s.plan.built.tx.id });
    expect(res.fee).toBe(s.plan.fee);
    expect(s.node.hasUtxo(s.plan.prep.tx.id, 0)).toBe(false); // spent by the genesis
    expect(s.node.hasUtxo(s.plan.built.tx.id, 0)).toBe(true);
    expect(s.node.submissions[0]!.payload).toBe(s.plan.declaration.payload);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('a refused preparation sends nothing more and leaves no record', async () => {
    const s = setup();
    s.node.rejectNext('Rejected transaction x: fee too low, below minimum');
    const e = (await signAndSubmitCreation(args(s, oneApproval().adapter)).catch((x) => x)) as SignFlowError;
    expect(e.stage).toBe('submitting');
    expect(s.node.submissions).toEqual([]);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('a genesis the node refuses after the preparation went out: the record stays (signed genesis, preparation sent) and resumes without a signature', async () => {
    const s = setup();
    // the preparation goes through, the genesis is refused (not an orphan: no retry)
    const real = s.node.submitTransaction.bind(s.node);
    let n = 0;
    s.node.submitTransaction = async (tx) => {
      if (++n === 2) throw new Error('Rejected transaction x: the node is not taking it now (invalid)');
      return real(tx);
    };
    const e = (await signAndSubmitCreation(args(s, oneApproval().adapter)).catch((x) => x)) as SignFlowError;
    expect(e).toBeInstanceOf(SignFlowError);
    expect(e.code).toBe('genesis-pending');
    expect(e.message).toMatch(/stays in your wallet/);
    const rec = onlyRecord(s);
    expect(rec).toMatchObject({ covenantId: s.plan.token.covenantId, prepTxid: s.plan.prep.tx.id, genesisTxid: s.plan.built.tx.id, prepSent: true });
    expect(rec.genesis!.inputs.every((i) => i.signatureScript.length > 0)).toBe(true);
    // the preparation's output 0 is the wallet's own coin meanwhile
    expect(s.node.hasUtxo(s.plan.prep.tx.id, 0)).toBe(true);
    // resume: the saved pair, nothing signed
    s.node.submitTransaction = real;
    const txid = await resumeCreation(s.node, rec, { storage: s.storage, sleep: async () => undefined });
    expect(txid).toBe(s.plan.built.tx.id);
    expect(s.node.hasUtxo(s.plan.built.tx.id, 0)).toBe(true);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('a genesis the node does not see the input of yet is offered again', async () => {
    const s = setup();
    const real = s.node.submitTransaction.bind(s.node);
    let n = 0;
    s.node.submitTransaction = async (tx) => {
      n++;
      if (n === 2 || n === 3) throw new Error(`Rejected transaction ${tx.id}: transaction ${tx.id} is an orphan where orphan is disallowed`);
      return real(tx);
    };
    const res = await signAndSubmitCreation(args(s, oneApproval().adapter));
    expect(res.txid).toBe(s.plan.built.tx.id);
    expect(n).toBe(4);
  });

  it('refuses a genesis that does not spend the preparation, before any signature', async () => {
    const s = setup();
    const w = oneApproval();
    const bad = JSON.parse(JSON.stringify(s.plan.built)) as BuiltTx;
    bad.tx.inputs[0]!.transactionId = 'ee'.repeat(32);
    const e = await signAndSubmitCreation({ ...args(s, w.adapter), built: bad }).catch((x) => x);
    expect(e).toBeInstanceOf(SignFlowError);
    expect(w.approvals()).toBe(0);
    expect(s.node.attempts).toEqual([]);
  });

  it('a wallet whose signatures do not verify sends nothing', async () => {
    const s = setup();
    const flip = (sigs: InputSignature[]) => sigs.map((x, i) => (i === 0 ? { ...x, signature: (x.signature[0] === '0' ? '1' : '0') + x.signature.slice(1) } : x));
    const wrong: WalletAdapter = { ...oneApproval().adapter, signChain: async ([prep, built]) => [flip(signBuilt(prep, [MAKER_SK])), signBuilt(built, [MAKER_SK])] };
    const e = (await signAndSubmitCreation(args(s, wrong)).catch((x) => x)) as SignFlowError;
    expect(e.stage).toBe('finalizing');
    expect(s.node.attempts).toEqual([]);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('a wallet error keeps its code', async () => {
    const s = setup();
    const failing: WalletError = new WalletError('timeout', 'no answer');
    const w: WalletAdapter = { ...oneApproval().adapter, signChain: async () => { throw failing; } };
    const e = (await signAndSubmitCreation(args(s, w)).catch((x) => x)) as SignFlowError;
    expect([e.stage, e.code]).toEqual(['signing', 'timeout']);
  });
});

describe('signAndSubmitCreation: an extension wallet that checks its inputs on its own node (KasWare 0.11.5)', () => {
  it('signs the preparation, sends it and hands it to the wallet, and only then asks for the genesis', async () => {
    const s = setup();
    const w = kasware(s);
    // what the node held when the wallet was asked for each signature
    const heldAtSign: boolean[] = [];
    const sign = w.provider.signPskt.bind(w.provider);
    w.provider.signPskt = async (req) => {
      heldAtSign.push(s.node.hasUtxo(s.plan.prep.tx.id, 0));
      return sign(req);
    };
    const infos: { stage: SignStage; step?: string; waiting?: boolean }[] = [];
    const res = await signAndSubmitCreation({ ...args(s, w.adapter), onStage: (stage, info) => infos.push({ stage, ...(info?.step ? { step: info.step } : {}), ...(info?.waiting !== undefined ? { waiting: info.waiting } : {}) }) });
    expect(res.txid).toBe(s.plan.built.tx.id);
    expect(w.signs()).toBe(2);
    // the genesis is handed over too (its outputs are fresh for whatever comes next)
    expect(w.order()).toEqual(['signPskt', 'pushTx', 'signPskt', 'pushTx']);
    // the second request came once the preparation was on the node: the wallet's own check passed
    expect(heldAtSign).toEqual([false, true]);
    expect(s.node.submissions.map((t) => t.id)).toEqual([s.plan.prep.tx.id, s.plan.built.tx.id]);
    expect(infos.filter((i) => i.stage === 'submitting').map((i) => i.step)).toEqual(['prep', 'genesis']);
    expect(infos.at(-1)).toMatchObject({ stage: 'submitted' });
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('an accepted pushTx is not enough: the genesis waits for the node and the margin; a refusal is asked again and passes', async () => {
    const s = setup();
    // pushTx is accepted, yet the wallet's lookup does not find the preparation until it moved into its node's UTXO index (seen on TN10)
    const view = laggingWalletNode(s.node, { pushLost: true });
    const w = kasware(s, {}, view);
    const slept: number[] = [];
    let refused = 0;
    const sign = w.provider.signPskt.bind(w.provider);
    w.provider.signPskt = async (req) => sign(req).catch((e: Error) => Promise.reject((refused++, e)));
    // the wallet's node takes the block only after the margin: the first ask is refused, the one 2 s later passes
    const sleep = async (ms: number) => void (slept.push(ms), ms === 2000 && slept.filter((x) => x === 2000).length === 2 && view.reveal());
    const res = await signAndSubmitCreation({ ...args(s, w.adapter), sleep });
    expect(res.txid).toBe(s.plan.built.tx.id);
    expect(slept.slice(0, 1)).toEqual([2000]);
    expect(refused).toBe(1);
    expect(w.signs()).toBe(3);
  });

  it('without pushTx (Kaspire): the genesis is asked for once the node lists the preparation and the wallet node had its margin', async () => {
    const s = setup();
    const view = laggingWalletNode(s.node);
    const p = fakeKaspire({ sdk, sk: MAKER_SK, network: NET, walletNode: { hasUtxo: view.hasUtxo } });
    const adapter = createKaspireAdapter({ getProvider: () => p });
    const slept: number[] = [];
    // the wallet's node sees the preparation when the settle margin is over
    const sleep = async (ms: number) => {
      slept.push(ms);
      view.reveal();
    };
    const res = await signAndSubmitCreation({ ...args(s, adapter), sleep, settleMs: 1500 });
    expect(res.txid).toBe(s.plan.built.tx.id);
    expect(slept).toContain(1500);
    expect(p.calls.filter((c) => c.method === 'signPskt')).toHaveLength(2);
  });

  it('a wallet that still cannot see the input is asked again after 2 s, at most 3 times', async () => {
    const s = setup();
    const view = laggingWalletNode(s.node);
    const p = fakeKaspire({ sdk, sk: MAKER_SK, network: NET, walletNode: { hasUtxo: view.hasUtxo } });
    const adapter = createKaspireAdapter({ getProvider: () => p });
    const slept: number[] = [];
    let retries = 0;
    const sleep = async (ms: number) => {
      slept.push(ms);
      if (ms === 2000 && ++retries === 2) view.reveal(); // relayed during the second wait
    };
    // the margin is not one of the 2 s retries
    const res = await signAndSubmitCreation({ ...args(s, adapter), sleep, settleMs: 1500 });
    expect(res.txid).toBe(s.plan.built.tx.id);
    expect(p.calls.filter((c) => c.method === 'signPskt')).toHaveLength(4); // preparation, then the genesis three times
    expect(slept.filter((ms) => ms === 2000)).toHaveLength(2);
  });

  it('a wallet that never sees it: after 3 asks the flow stops, the preparation is out, the record stays to finish later', async () => {
    const s = setup();
    const view = laggingWalletNode(s.node);
    const p = fakeKaspire({ sdk, sk: MAKER_SK, network: NET, walletNode: { hasUtxo: view.hasUtxo } });
    const e = (await signAndSubmitCreation(args(s, createKaspireAdapter({ getProvider: () => p }))).catch((x) => x)) as SignFlowError;
    expect(e.code).toBe('genesis-pending');
    expect(p.calls.filter((c) => c.method === 'signPskt')).toHaveLength(4);
    expect(s.node.submissions.map((t) => t.id)).toEqual([s.plan.prep.tx.id]);
    expect(onlyRecord(s).prepSent).toBe(true);
    expect(onlyRecord(s).genesis).toBeUndefined();
  });

  it('a declined preparation sends nothing and keeps no record', async () => {
    const s = setup();
    const e = await signAndSubmitCreation(args(s, kasware(s, { rejectSign: true }).adapter)).catch((x) => x);
    expect(e).toBeInstanceOf(SignFlowError);
    expect((e as SignFlowError).rejectedByUser).toBe(true);
    expect(s.node.attempts).toEqual([]);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('a declined genesis: the preparation is on chain, its output 0 is the wallet coin, the record (unsigned genesis) finishes with one more approval', async () => {
    const s = setup();
    const w = kasware(s);
    const sign = w.provider.signPskt.bind(w.provider);
    let n = 0;
    w.provider.signPskt = async (req) => (++n === 2 ? Promise.reject({ code: 4001, message: 'User rejected the request.' }) : sign(req));
    const e = (await signAndSubmitCreation(args(s, w.adapter)).catch((x) => x)) as SignFlowError;
    expect(e.code).toBe('genesis-pending');
    expect(e.rejectedByUser).toBe(false);
    expect(e.message).toMatch(/second approval was declined/);
    expect(s.node.submissions.map((t) => t.id)).toEqual([s.plan.prep.tx.id]);
    expect(s.node.hasUtxo(s.plan.prep.tx.id, 0)).toBe(true);
    const rec = onlyRecord(s);
    expect(rec).toMatchObject({ prepSent: true, genesisTxid: s.plan.built.tx.id });
    expect(rec.genesis).toBeUndefined();
    expect(rec.genesisBuilt!.tx.id).toBe(s.plan.built.tx.id);
    expect(typeof rec.prepSentAt).toBe('number');
    // a v1 reader would have dropped this record: it is in v2 only
    expect(s.storage.data.has('kob.creations.v1')).toBe(false);

    // "Finish the creation": one approval, the genesis sent, the record gone
    const txid = await finishCreation(finishArgs(s, w.adapter, rec));
    expect(txid).toBe(s.plan.built.tx.id);
    expect(n).toBe(3);
    expect(s.node.submissions.map((t) => t.id)).toEqual([s.plan.prep.tx.id, s.plan.built.tx.id]);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('a reload between the steps: a fresh page finishes the record with one approval once the wallet node sees the preparation', async () => {
    const s = setup();
    const first = kasware(s);
    const sign = first.provider.signPskt.bind(first.provider);
    let n = 0;
    // the page dies while the second popup is open
    first.provider.signPskt = async (req) => (++n === 2 ? Promise.reject(new Error('the page went away')) : sign(req));
    await signAndSubmitCreation(args(s, first.adapter)).catch(() => undefined);
    const rec = onlyRecord(s);
    resetVisibility(); // the new page knows nothing of what the old one sent
    const view = laggingWalletNode(s.node);
    const again = kasware(s, {}, view);
    const res = await finishCreation({ ...finishArgs(s, again.adapter, rec) });
    expect(res).toBe(s.plan.built.tx.id);
    expect(again.signs()).toBe(1);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });

  it('a preparation that never went out (the connection dropped): finishing sends it again, then asks for the genesis', async () => {
    const s = setup();
    const w = kasware(s);
    s.node.rejectNext(new Error('websocket closed'));
    const e = (await signAndSubmitCreation(args(s, w.adapter)).catch((x) => x)) as SignFlowError;
    expect(e.stage).toBe('submitting');
    expect(w.signs()).toBe(1);
    const rec = onlyRecord(s);
    expect(rec.prepSent).toBe(false);
    const txid = await finishCreation(finishArgs(s, w.adapter, rec));
    expect(txid).toBe(s.plan.built.tx.id);
    expect(s.node.submissions.map((t) => t.id)).toEqual([s.plan.prep.tx.id, s.plan.built.tx.id]);
  });

  it('a preparation whose coin was spent elsewhere cannot be finished: creation-spent, nothing signed', async () => {
    const s = setup();
    const w = kasware(s);
    s.node.rejectNext(new Error('websocket closed'));
    await signAndSubmitCreation(args(s, w.adapter)).catch(() => undefined);
    const rec = onlyRecord(s);
    // the wallet spent its coin in another transaction meanwhile
    for (const i of s.plan.prep.tx.inputs) s.node.removeUtxo(i.transactionId, i.index);
    const e = (await finishCreation(finishArgs(s, w.adapter, rec)).catch((x) => x)) as SignFlowError;
    expect(e.code).toBe(CREATION_SPENT);
    expect(e.message).toMatch(/cannot be finished/);
    expect(w.signs()).toBe(1);
  });

  it('the gate times out (the preparation is not listed): the wallet is not asked, the record stays', async () => {
    const s = setup({ applyOnSubmit: false });
    const p = fakeKaspire({ sdk, sk: MAKER_SK, network: NET });
    let t = 0;
    const now = () => t;
    const sleep = async (ms: number) => void (t += ms);
    const e = (await signAndSubmitCreation({ ...args(s, createKaspireAdapter({ getProvider: () => p })), sleep, now, visibleTimeoutMs: 60_000 }).catch((x) => x)) as SignFlowError;
    expect(e.code).toBe('genesis-pending');
    expect(e.message).toMatch(/cannot see the coin/);
    expect(t).toBeGreaterThanOrEqual(59_000);
    expect(p.calls.filter((c) => c.method === 'signPskt')).toHaveLength(1);
    expect(onlyRecord(s).prepSent).toBe(true);
  });

  it('a record of the old format (v1, signed genesis) is moved to v2 and resumes without a signature', async () => {
    const s = setup();
    const w = oneApproval();
    // the genesis is refused once: a record with a signed genesis is left
    const real = s.node.submitTransaction.bind(s.node);
    let n = 0;
    s.node.submitTransaction = async (tx) => {
      if (++n === 2) throw new Error('Rejected transaction x: the node is not taking it now (invalid)');
      return real(tx);
    };
    await signAndSubmitCreation(args(s, w.adapter)).catch(() => undefined);
    s.node.submitTransaction = real;
    const { genesisBuilt: _drop, prepSentAt: _at, ...v1 } = onlyRecord(s);
    s.storage.data.delete('kob.creations.v2');
    s.storage.setItem('kob.creations.v1', JSON.stringify([v1]));
    const rec = onlyRecord(s);
    expect(s.storage.data.has('kob.creations.v1')).toBe(false);
    expect(rec.genesisBuilt).toBeUndefined();
    const txid = await finishCreation(finishArgs(s, kasware(s).adapter, rec));
    expect(txid).toBe(s.plan.built.tx.id);
    expect(listCreations(NET, MAKER_PK, s.storage)).toEqual([]);
  });
});
