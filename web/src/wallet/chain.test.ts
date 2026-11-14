// The chain runner (a step spends an output of the step before): one approval for the private-key signer; for an extension wallet each step
// signed, checked, recorded and sent before the next is asked for, and the next asked for only once the wallet's own node sees its input.
import { beforeEach, describe, expect, it } from 'vitest';
import { ChainError, runSignChain, spendsOutputOf } from './chain';
import { createKaswareAdapter } from './kasware';
import { SETTLE_MS, resetVisibility } from './visibility';
import type { WalletAdapter } from './types';
import { fakeKasware, laggingWalletNode } from '../testing/fake-wallet-providers';
import { MockNode } from '../testing/mock-node';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { spkStringToAddress } from '../data/kaspa-sdk';
import { MAKER_PK, MAKER_SK } from '../testing/token-fixtures';
import { kob } from '../testing/wallet-fixtures';
import { signBuilt } from '../testing/local-signer';
import { emptyIssueForm, planIssue } from '../kob/issue';
import type { BuiltTx, SignedTx } from '../kob/types';

const sdk = loadKaspaSdkNode();
const NET = 'testnet-10';
const addressOf = (spk: string) => spkStringToAddress(sdk, spk, NET);
const OTHER_SK = '22'.repeat(32);

function setup() {
  const plan = planIssue(kob, { ...emptyIssueForm(), name: 'Chain Coin', ticker: 'CHAIN', decimals: 0, supply: '10' }, {
    funding: [{ transactionId: '0c'.repeat(32), index: 0, amount: (50n * 100_000_000n).toString(), blockDaaScore: '100', pubkey: MAKER_PK }],
    maker: MAKER_PK,
    network: NET,
  });
  const node = new MockNode({ network: NET, addressOf });
  node.seedFromInputs(plan.prep.tx);
  return { steps: [plan.prep, plan.built], node };
}

const base = (s: ReturnType<typeof setup>, adapter: WalletAdapter) => ({ kob, node: s.node, adapter, network: NET, steps: s.steps, addressOf, sleep: async () => undefined });

beforeEach(() => resetVisibility());

describe('runSignChain', () => {
  it('refuses a chain whose step does not spend the step before, before any signature', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK });
    const e = (await runSignChain({ ...base(s, createKaswareAdapter({ getProvider: () => p })), steps: [s.steps[1]!, s.steps[0]!] }).catch((x) => x)) as ChainError;
    expect(e).toBeInstanceOf(ChainError);
    expect(e.sent).toBe(0);
    expect(p.calls.filter((c) => c.method === 'signPskt')).toHaveLength(0);
    expect(spendsOutputOf(s.steps[0]!, s.steps[1]!)).toBe(true);
  });

  it('one approval (signChain): every step signed and recorded before the first is sent', async () => {
    const s = setup();
    let approvals = 0;
    const recorded: number[] = [];
    const adapter: WalletAdapter = {
      id: 'local-key', label: 'pk', inputPolicy: 'offline', detect: () => true, connect: async () => { throw new Error('unused'); },
      signTx: async () => { throw new Error('unused'); },
      signChain: async ([a, b]) => (approvals++, [signBuilt(a, [MAKER_SK]), signBuilt(b, [MAKER_SK])]),
    };
    const res = await runSignChain({ ...base(s, adapter), onSigned: (signed) => recorded.push(signed.length * 10 + s.node.submissions.length) });
    expect(approvals).toBe(1);
    expect(recorded).toEqual([20]);
    expect(res.signed.map((x) => x.tx.id)).toEqual(s.steps.map((b) => b.tx.id));
  });

  it('sequential: each step recorded before it is sent and sent before the next is asked for; the record sees every step signed so far', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK, walletNode: laggingWalletNode(s.node) });
    const log: string[] = [];
    const sign = p.signPskt.bind(p);
    p.signPskt = async (req) => (log.push(`sign@${s.node.submissions.length}`), sign(req));
    const res = await runSignChain({
      ...base(s, createKaswareAdapter({ getProvider: () => p })),
      onSigned: (signed: SignedTx[]) => log.push(`record ${signed.length}@${s.node.submissions.length}`),
      onSent: (k) => log.push(`sent ${k}`),
    });
    expect(log).toEqual(['sign@0', 'record 1@0', 'sent 0', 'sign@1', 'record 2@1', 'sent 1']);
    expect(res.fee).toBe(BigInt(s.steps[0]!.fee.fee) + BigInt(s.steps[1]!.fee.fee));
  });

  it('a record that cannot be written stops the chain before anything is sent', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK });
    const e = (await runSignChain({ ...base(s, createKaswareAdapter({ getProvider: () => p })), onSigned: () => { throw new Error('quota'); } }).catch((x) => x)) as ChainError;
    expect([e.step, e.sent]).toEqual([0, 0]);
    expect(s.node.attempts).toEqual([]);
  });

  it('an account switched between the steps: step 2 is not signed (sent: 1)', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK });
    const e = (await runSignChain({ ...base(s, createKaswareAdapter({ getProvider: () => p })), onSent: () => p.switchAccount(OTHER_SK) }).catch((x) => x)) as ChainError;
    expect(e.code).toBe('account-changed');
    expect([e.step, e.sent]).toEqual([1, 1]);
    expect(p.calls.filter((c) => c.method === 'signPskt')).toHaveLength(1);
  });

  it('a declined first step sends nothing; a declined second step leaves the first one out (sent: 1)', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK, behavior: { rejectSign: true } });
    const first = (await runSignChain(base(s, createKaswareAdapter({ getProvider: () => p }))).catch((x) => x)) as ChainError;
    expect([first.code, first.step, first.sent, first.rejectedByUser]).toEqual(['rejected', 0, 0, true]);
    const s2 = setup();
    const q = fakeKasware({ sdk, sk: MAKER_SK });
    const second = (await runSignChain({ ...base(s2, createKaswareAdapter({ getProvider: () => q })), onSent: () => void (q.behavior.rejectSign = true) }).catch((x) => x)) as ChainError;
    expect([second.code, second.step, second.sent]).toEqual(['rejected', 1, 1]);
    expect(s2.node.submissions.map((t) => t.id)).toEqual([s2.steps[0]!.tx.id]);
  });

  it('a refused step: the node error with its step; orphan answers to a later step are offered again', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK });
    s.node.rejectNext('Rejected transaction x: fee too low, below minimum');
    const e = (await runSignChain(base(s, createKaswareAdapter({ getProvider: () => p }))).catch((x) => x)) as ChainError;
    expect([e.stage, e.code, e.step, e.sent]).toEqual(['submitting', 'fee', 0, 0]);
    const s2 = setup();
    const q = fakeKasware({ sdk, sk: MAKER_SK });
    const real = s2.node.submitTransaction.bind(s2.node);
    let n = 0;
    s2.node.submitTransaction = async (tx) => (++n === 2 ? Promise.reject(new Error(`Rejected transaction ${tx.id}: transaction ${tx.id} is an orphan where orphan is disallowed`)) : real(tx));
    const res = await runSignChain(base(s2, createKaswareAdapter({ getProvider: () => q })));
    expect(res.signed).toHaveLength(2);
    expect(n).toBe(3);
  });

  it('a step whose signature does not verify is never sent', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK, behavior: { signKey: OTHER_SK } });
    const e = (await runSignChain(base(s, createKaswareAdapter({ getProvider: () => p }))).catch((x) => x)) as ChainError;
    expect([e.stage, e.sent]).toEqual(['finalizing', 0]);
    expect(s.node.attempts).toEqual([]);
  });

  it('the steps planned are the steps sent: a single step resumes a chain (alsoFresh)', async () => {
    const s = setup();
    const p = fakeKasware({ sdk, sk: MAKER_SK });
    const adapter = createKaswareAdapter({ getProvider: () => p });
    await runSignChain({ ...base(s, adapter), steps: [s.steps[0]!] });
    resetVisibility();
    const slept: number[] = [];
    const res = await runSignChain({ ...base(s, adapter), steps: [s.steps[1]! as BuiltTx], alsoFresh: new Map([[s.steps[0]!.tx.id, Date.now()]]), sleep: async (ms) => void slept.push(ms) });
    expect(res.signed[0]!.tx.id).toBe(s.steps[1]!.tx.id);
    // the gate looked the first step up on the node and waited the margin before asking
    expect(slept).toEqual([SETTLE_MS]);
  });
});
