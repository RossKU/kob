import { describe, expect, it } from 'vitest';
import { createKaswareAdapter } from './kasware';
import { WalletError } from './types';
import { fakeKasware } from '../testing/fake-wallet-providers';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAKER_PK, MAKER_SK, OTHER_SK } from '../testing/token-fixtures';
import { assertConsensusValid, buildCancel, buildSend } from '../testing/wallet-fixtures';
import { extractSignature, parsePushes, hexToBytes } from './sigs';

const sdk = loadKaspaSdkNode();
const mk = (o: Partial<Parameters<typeof fakeKasware>[0]> = {}) => {
  const provider = fakeKasware({ sdk, sk: MAKER_SK, ...o });
  return { provider, adapter: createKaswareAdapter({ getProvider: () => provider }) };
};

describe('KasWare adapter: connect', () => {
  it('reads account, x-only key, version; normalises kaspa_testnet_10 without switching when already on the wanted network', async () => {
    const { adapter, provider } = mk();
    expect(adapter.id).toBe('kasware');
    expect(adapter.detect()).toBe(true);
    const info = await adapter.connect('testnet-10');
    expect(info).toEqual({ id: 'kasware', label: 'KasWare', address: 'kaspatest:qfake-kasware', pubkey: MAKER_PK, network: 'testnet-10', version: '0.10.0' });
    expect(provider.calls.some((c) => c.method === 'switchNetwork')).toBe(false);
  });

  it('switches network with the wallet-native name first, and the result tells whether it worked', async () => {
    const { adapter, provider } = mk({ network: 'kaspa_mainnet' });
    const info = await adapter.connect('kaspa_testnet_10');
    expect(info.network).toBe('testnet-10');
    expect(provider.calls.filter((c) => c.method === 'switchNetwork').map((c) => c.args[0])).toEqual(['kaspa_testnet_10']);
  });

  it('falls back to the plain SDK spelling when the wallet rejects the native one', async () => {
    const { adapter, provider } = mk({ network: 'kaspa_mainnet', switchAccepts: ['testnet-10'] });
    expect((await adapter.connect('testnet-10')).network).toBe('testnet-10');
    expect(provider.calls.filter((c) => c.method === 'switchNetwork').map((c) => c.args[0])).toEqual(['kaspa_testnet_10', 'testnet-10']);
  });

  it('reports the wallet\'s network when it cannot switch (no throw: the caller decides)', async () => {
    const { adapter } = mk({ network: 'kaspa_mainnet', switchAccepts: [] });
    expect((await adapter.connect('testnet-10')).network).toBe('mainnet');
  });

  it('maps a declined connection to WalletError(rejected) and a missing provider to unsupported', async () => {
    const provider = fakeKasware({ sdk, sk: MAKER_SK });
    provider.requestAccounts = async () => { throw { code: 4001, message: 'User rejected the request.' }; };
    await expect(createKaswareAdapter({ getProvider: () => provider }).connect('testnet-10')).rejects.toMatchObject({ name: 'WalletError', code: 'rejected' });
    const none = createKaswareAdapter({ getProvider: () => undefined });
    expect(none.detect()).toBe(false);
    await expect(none.connect('testnet-10')).rejects.toMatchObject({ code: 'unsupported' });
  });

  it('detect() reads the provider at call time (extensions inject late)', () => {
    let p: ReturnType<typeof fakeKasware> | undefined;
    const adapter = createKaswareAdapter({ getProvider: () => p });
    expect(adapter.detect()).toBe(false);
    p = fakeKasware({ sdk, sk: MAKER_SK });
    expect(adapter.detect()).toBe(true);
  });
});

describe('KasWare adapter: signTx', () => {
  it('asks signPskt for every input in built.sign with the unsigned tx JSON and SIGHASH_ALL, and returns consensus-valid signatures (send)', async () => {
    const { adapter, provider } = mk();
    const built = buildSend();
    const sigs = await adapter.signTx(built, { network: 'testnet-10' });
    const call = provider.calls.find((c) => c.method === 'signPskt')!;
    const req = call.args[0] as { txJsonString: string; options: { signInputs: { index: number; sighashType: number }[] } };
    expect(JSON.parse(req.txJsonString)).toEqual(built.tx);
    expect(req.options.signInputs).toEqual(built.sign.map((s) => ({ index: s.inputIndex, sighashType: 1 })));
    expect(sigs.map((s) => s.inputIndex)).toEqual(built.sign.map((s) => s.inputIndex));
    for (const s of sigs) expect(s.signature).toMatch(/^[0-9a-f]{128}01$/); // 65 bytes, SIGHASH_ALL
    // covenant inputs 0 and 1 and P2PK input 2: finalize verifies each signature, the script engine runs the covenants
    expect(() => assertConsensusValid(built, sigs)).not.toThrow();
  });

  it('works for a covenant entry input (cancel)', async () => {
    const { adapter } = mk();
    const built = buildCancel();
    expect(() => assertConsensusValid(built, [])).toThrow(); // sanity: unsigned is not valid
    const sigs = await adapter.signTx(built);
    expect(sigs).toHaveLength(built.sign.length);
    expect(() => assertConsensusValid(built, sigs)).not.toThrow();
  });

  it('KasWare returns exactly push(sig65) (66 bytes) for P2SH inputs, which is what the extractor locates', async () => {
    const { provider } = mk();
    const built = buildSend();
    const resp = await provider.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs: [{ index: 0, sighashType: 1 }] } });
    const ss = (JSON.parse(resp as string) as { inputs: { signatureScript: string }[] }).inputs[0]!.signatureScript;
    expect(ss.length / 2).toBe(66);
    expect(parsePushes(hexToBytes(ss)).map((p) => p.length)).toEqual([65]);
    expect(extractSignature(resp, 0)).toBe(ss.slice(2));
  });

  it('accepts an object response as well as a JSON string', async () => {
    const { adapter } = mk({ behavior: { respondAsObject: true } });
    const built = buildSend();
    expect(() => assertConsensusValid(built, [])).toThrow();
    expect(await adapter.signTx(built)).toHaveLength(3);
  });

  it('a wallet that omits a signature -> WalletError(no-signature) naming the wallet', async () => {
    const { adapter } = mk({ behavior: { onlyInputs: [0, 1] } });
    await expect(adapter.signTx(buildSend())).rejects.toMatchObject({ code: 'no-signature', message: expect.stringMatching(/^KasWare: .*input 2/) });
  });

  it('a declined popup -> WalletError(rejected)', async () => {
    const { adapter } = mk({ behavior: { rejectSign: true } });
    const err = await adapter.signTx(buildSend()).catch((e) => e);
    expect(err).toBeInstanceOf(WalletError);
    expect(err.code).toBe('rejected');
  });

  it('an unanswered popup -> WalletError(timeout) after timeoutMs', async () => {
    const { adapter } = mk({ behavior: { neverAnswer: true } });
    await expect(adapter.signTx(buildSend(), { timeoutMs: 20 })).rejects.toMatchObject({ code: 'timeout' });
  });

  it('other wallet failures keep their text', async () => {
    const { adapter } = mk({ behavior: { failWith: { code: -32603, message: 'wallet locked' } } });
    await expect(adapter.signTx(buildSend())).rejects.toMatchObject({ code: 'other', message: expect.stringContaining('wallet locked') });
  });

  it('signatures from the wrong key are returned as is (finalize is what rejects them)', async () => {
    const { adapter } = mk({ behavior: { signKey: OTHER_SK } });
    const built = buildSend();
    const sigs = await adapter.signTx(built);
    expect(() => assertConsensusValid(built, sigs)).toThrow(/signature/i);
  });
});
