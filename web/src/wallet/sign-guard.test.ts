// signAndSubmit re-reads the wallet right before asking it to sign: a plan built for one account / network is never put in front of
// another (the confirmation screen may have been open while the user switched in the wallet).
import { describe, expect, it } from 'vitest';
import { SignFlowError, signAndSubmit } from './sign';
import { createKaswareAdapter } from './kasware';
import { createKaspireAdapter } from './kaspire';
import { createKastleAdapter } from './kastle';
import { fakeKasware, fakeKaspire, fakeKastle } from '../testing/fake-wallet-providers';
import { MockNode } from '../testing/mock-node';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { spkStringToAddress } from '../data/kaspa-sdk';
import { MAKER_SK, OTHER_SK } from '../testing/token-fixtures';
import { buildSend, kob } from '../testing/wallet-fixtures';
import type { WalletAdapter } from './types';
import { classifyFailure } from '../ui/confirm/confirm-flow';

const sdk = loadKaspaSdkNode();
const NET = 'testnet-10';
const mkNode = () => new MockNode({ network: NET, addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });

interface Rig {
  adapter: WalletAdapter;
  signCalls(): number;
  switchAccount(): void;
  moveToNetwork(n: string): void;
  lose(): void;
}
const rigs: [string, () => Rig][] = [
  ['KasWare', () => {
    const p = fakeKasware({ sdk, sk: MAKER_SK });
    return { adapter: createKaswareAdapter({ getProvider: () => p }), signCalls: () => p.calls.filter((c) => c.method === 'signPskt').length, switchAccount: () => p.switchAccount(OTHER_SK), moveToNetwork: (n) => p.moveToNetwork(n === 'mainnet' ? 'kaspa_mainnet' : n), lose: () => p.lose() };
  }],
  ['Kaspire', () => {
    const p = fakeKaspire({ sdk, sk: MAKER_SK, network: NET });
    return { adapter: createKaspireAdapter({ getProvider: () => p }), signCalls: () => p.calls.filter((c) => c.method === 'signPskt').length, switchAccount: () => p.switchAccount(OTHER_SK), moveToNetwork: (n) => p.moveToNetwork(n), lose: () => p.lose() };
  }],
  ['Kastle', () => {
    const p = fakeKastle({ sdk, sk: MAKER_SK, network: NET });
    return { adapter: createKastleAdapter({ getProvider: () => p }), signCalls: () => p.calls.filter((c) => c.method === 'signTx').length, switchAccount: () => p.switchAccount(OTHER_SK), moveToNetwork: (n) => p.moveToNetwork(n), lose: () => p.lose() };
  }],
];

describe.each(rigs)('%s: signAndSubmit guards the account and network', (_name, make) => {
  const run = async (rig: Rig) => {
    const built = buildSend();
    const node = mkNode();
    node.seedFromInputs(built.tx);
    return signAndSubmit({ kob, node, adapter: rig.adapter, built, network: NET });
  };

  it('signs when the wallet is still the one the plan was built for', async () => {
    const rig = make();
    await rig.adapter.connect(NET);
    await expect(run(rig)).resolves.toMatchObject({ txid: expect.any(String) });
  });

  it('refuses after an account switch: nothing is sent to the wallet, and the failure is a re-plan', async () => {
    const rig = make();
    await rig.adapter.connect(NET);
    rig.switchAccount();
    const e = await run(rig).catch((x) => x);
    expect(e).toBeInstanceOf(SignFlowError);
    expect(e).toMatchObject({ stage: 'signing', code: 'account-changed' });
    expect(rig.signCalls()).toBe(0);
    expect(classifyFailure(e).kind).toBe('replan');
  });

  it('refuses after a network switch', async () => {
    const rig = make();
    await rig.adapter.connect(NET);
    rig.moveToNetwork('mainnet');
    await expect(run(rig)).rejects.toMatchObject({ stage: 'signing', code: 'network' });
    expect(rig.signCalls()).toBe(0);
  });

  it('refuses when the wallet lost its account', async () => {
    const rig = make();
    await rig.adapter.connect(NET);
    rig.lose();
    await expect(run(rig)).rejects.toMatchObject({ stage: 'signing', code: 'wallet-lost' });
    expect(rig.signCalls()).toBe(0);
  });

  it('checkWallet: false skips the re-read (tests / adapters without refresh keep working)', async () => {
    const rig = make();
    await rig.adapter.connect(NET);
    rig.switchAccount();
    const built = buildSend();
    const node = mkNode();
    node.seedFromInputs(built.tx);
    // the wallet signs with the other key: finalize catches it, which is the second line of defence
    await expect(signAndSubmit({ kob, node, adapter: rig.adapter, built, network: NET, checkWallet: false })).rejects.toMatchObject({ stage: 'finalizing' });
  });
});
