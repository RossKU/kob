import { describe, expect, it } from 'vitest';
import { createKastleAdapter } from './kastle';
import { fakeKastle } from '../testing/fake-wallet-providers';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAKER_PK, MAKER_SK, OTHER_SK } from '../testing/token-fixtures';
import { assertConsensusValid, buildCancel, buildSend } from '../testing/wallet-fixtures';
import { extractSignature } from './sigs';

const sdk = loadKaspaSdkNode();
const mk = (o: Partial<Parameters<typeof fakeKastle>[0]> = {}) => {
  const provider = fakeKastle({ sdk, sk: MAKER_SK, ...o });
  return { provider, adapter: createKastleAdapter({ getProvider: () => provider }) };
};

describe('Kastle adapter: connect', () => {
  it('takes the last 32 bytes of the 33-byte public key and reads network and version', async () => {
    const { adapter, provider } = mk({ network: 'testnet-10' });
    expect((await provider.getAccount()).publicKey).toBe('02' + MAKER_PK); // the wallet really returns 33 bytes
    const info = await adapter.connect('testnet-10');
    expect(info).toEqual({ id: 'kastle', label: 'Kastle', address: 'kaspa:qfake-kastle', pubkey: MAKER_PK, network: 'testnet-10', version: '2.60.1' });
  });

  it('switches network through kas:switch_network', async () => {
    const { adapter, provider } = mk({ network: 'mainnet' });
    expect((await adapter.connect('testnet-10')).network).toBe('testnet-10');
    expect(provider.calls.find((c) => c.method === 'kas:switch_network')!.args[0]).toBe('testnet-10');
    const stuck = mk({ network: 'mainnet', switchAccepts: [] });
    expect((await stuck.adapter.connect('testnet-10')).network).toBe('mainnet');
  });

  it('connect() returning false is a refusal; a missing provider is unsupported', async () => {
    const { adapter, provider } = mk();
    provider.connect = async () => false;
    await expect(adapter.connect('mainnet')).rejects.toMatchObject({ code: 'rejected' });
    await expect(createKastleAdapter({ getProvider: () => null }).connect('mainnet')).rejects.toMatchObject({ code: 'unsupported' });
    expect(createKastleAdapter({ getProvider: () => null }).detect()).toBe(false);
  });
});

describe('Kastle adapter: signTx', () => {
  it('passes `scripts` with the empty-script variant for covenant inputs only, and the network name', async () => {
    const { adapter, provider } = mk();
    const built = buildSend();
    await adapter.signTx(built, { network: 'kaspa_testnet_10' });
    expect(provider.lastScripts()).toEqual([
      { inputIndex: 0, scriptHex: '', signType: 'All' },
      { inputIndex: 1, scriptHex: '', signType: 'All' },
    ]);
    expect(provider.calls.find((c) => c.method === 'signTx')!.args[0]).toBe('testnet-10');
  });

  it('is consensus-valid for send and cancel', async () => {
    const { adapter } = mk();
    for (const built of [buildSend(), buildCancel()]) {
      const sigs = await adapter.signTx(built, { network: 'testnet-10' });
      expect(() => assertConsensusValid(built, sigs)).not.toThrow();
    }
  });

  it('a P2PK-only transaction is signed by a plain call (no scripts)', async () => {
    const { adapter, provider } = mk();
    const built = buildSend();
    // keep only the funding input's request: the covenant inputs are not this wallet's business here
    const p2pkOnly = { ...built, sign: built.sign.filter((s) => s.redeemScript === null) };
    await adapter.signTx(p2pkOnly, { network: 'testnet-10' });
    expect(provider.lastScripts()).toBeUndefined();
  });

  it('reference behaviour of the wallet: without `scripts` a P2SH input comes back UNSIGNED with no error (issue #353)', async () => {
    const { provider } = mk();
    const built = buildSend();
    const resp = await provider.signTx('testnet-10', JSON.stringify(built.tx));
    expect(() => extractSignature(resp, 0)).toThrow(/no signature/i);
    expect(extractSignature(resp, 2)).toMatch(/^[0-9a-f]{130}$/); // the P2PK input was signed
  });

  it('when the wallet still returns a covenant input unsigned the adapter says so precisely', async () => {
    const { adapter } = mk({ behavior: { onlyInputs: [2] } });
    const err = await adapter.signTx(buildSend(), { network: 'testnet-10' }).catch((e) => e);
    expect(err).toMatchObject({ name: 'WalletError', code: 'no-signature' });
    expect(err.message).toMatch(/covenant input.*unsigned/);
  });

  it('needs the network name', async () => {
    await expect(mk().adapter.signTx(buildSend())).rejects.toMatchObject({ code: 'unsupported' });
  });

  it('declined / unanswered / failed / wrong key', async () => {
    await expect(mk({ behavior: { rejectSign: true } }).adapter.signTx(buildSend(), { network: 'testnet-10' })).rejects.toMatchObject({ code: 'rejected' });
    await expect(mk({ behavior: { neverAnswer: true } }).adapter.signTx(buildSend(), { network: 'testnet-10', timeoutMs: 20 })).rejects.toMatchObject({ code: 'timeout' });
    await expect(mk({ behavior: { failWith: new Error('locked') } }).adapter.signTx(buildSend(), { network: 'testnet-10' })).rejects.toMatchObject({ code: 'other', message: 'Kastle: locked' });
    const built = buildSend();
    const sigs = await mk({ behavior: { signKey: OTHER_SK } }).adapter.signTx(built, { network: 'testnet-10' });
    expect(() => assertConsensusValid(built, sigs)).toThrow(/signature/i);
  });
});
