// The token creation flow (KCC-23: preparation, then the genesis spending it) against the mock node and the fake wallets: both signed before
// anything is sent, the preparation first, the creation record kept exactly while the genesis may still be missing, and resumed without a
// new signature.
import { describe, expect, it } from 'vitest';
import { signAndSubmitCreation, resumeCreation } from './create';
import { SignFlowError, type SignStage } from './sign';
import { createKaswareAdapter } from './kasware';
import { WalletError, type WalletAdapter } from './types';
import { fakeKasware } from '../testing/fake-wallet-providers';
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

function memory(): StorageLike & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v), removeItem: (k) => void data.delete(k) };
}

function setup() {
  const plan: IssuePlan = planIssue(kob, { ...emptyIssueForm(), name: 'Flow Coin', ticker: 'FLOW', decimals: 2, supply: '1000' }, {
    funding: [{ transactionId: '0a'.repeat(32), index: 3, amount: (200n * KAS).toString(), blockDaaScore: '100', pubkey: MAKER_PK }],
    maker: MAKER_PK,
    network: NET,
  });
  const node = new MockNode({ network: NET, addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });
  node.seedFromInputs(plan.prep.tx);
  const storage = memory();
  return { plan, node, storage };
}

const kasware = (behavior = {}): { adapter: WalletAdapter; calls: () => number } => {
  const p = fakeKasware({ sdk, sk: MAKER_SK, behavior });
  return { adapter: createKaswareAdapter({ getProvider: () => p }), calls: () => p.calls.filter((c) => c.method === 'signPskt').length };
};

/** A wallet with one approval for both (as the private-key signer): signs locally, counts its approvals. */
function oneApproval(): { adapter: WalletAdapter; approvals: () => number } {
  let n = 0;
  const sign = (b: BuiltTx): InputSignature[] => signBuilt(b, [MAKER_SK]);
  return {
    approvals: () => n,
    adapter: {
      id: 'local-key', label: 'test', detect: () => true,
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
  kob, node: s.node, adapter, prep: s.plan.prep, built: s.plan.built, network: NET, maker: MAKER_PK, storage: s.storage,
  record: { token: s.plan.token, declaration: s.plan.declaration, docs: s.plan.docs }, sleep: async () => undefined,
});

describe('signAndSubmitCreation', () => {
  it('one approval: both signed, the preparation sent first, the genesis spending it; nothing left to resume', async () => {
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

  it('an extension wallet asks twice (its API signs one transaction per request): both before anything is sent', async () => {
    const s = setup();
    const w = kasware();
    await signAndSubmitCreation(args(s, w.adapter));
    expect(w.calls()).toBe(2);
    expect(s.node.submissions.map((t) => t.id)).toEqual([s.plan.prep.tx.id, s.plan.built.tx.id]);
  });

  it('a declined signature sends nothing and keeps no record', async () => {
    const s = setup();
    const e = await signAndSubmitCreation(args(s, kasware({ rejectSign: true }).adapter)).catch((x) => x);
    expect(e).toBeInstanceOf(SignFlowError);
    expect((e as SignFlowError).rejectedByUser).toBe(true);
    expect(s.node.attempts).toEqual([]);
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
    const [rec] = listCreations(NET, MAKER_PK, s.storage) as [CreationRecord];
    expect(rec).toMatchObject({ covenantId: s.plan.token.covenantId, prepTxid: s.plan.prep.tx.id, genesisTxid: s.plan.built.tx.id, prepSent: true });
    expect(rec.genesis.inputs.every((i) => i.signatureScript.length > 0)).toBe(true);
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
