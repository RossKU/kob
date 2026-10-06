import { describe, expect, it } from 'vitest';
import { createKaspireAdapter, dispatchTagFrom, kaspireSignatureScript, type KaspireDeps, type KaspireScriptEntry } from './kaspire';
import { fakeKaspire } from '../testing/fake-wallet-providers';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAKER_PK, MAKER_SK, OTHER_SK } from '../testing/token-fixtures';
import { assertConsensusValid, buildCancel, buildSend, kob } from '../testing/wallet-fixtures';
import { extractSignature } from './sigs';

const sdk = loadKaspaSdkNode();
const mk = (o: Partial<Parameters<typeof fakeKaspire>[0]> = {}, deps: KaspireDeps = { dispatchTag: dispatchTagFrom(kob) }) => {
  const provider = fakeKaspire({ sdk, sk: MAKER_SK, ...o });
  return { provider, adapter: createKaspireAdapter({ getProvider: () => provider, ...deps }) };
};
const scriptsOf = (p: ReturnType<typeof fakeKaspire>) => (p.lastSignParams() as { scripts?: KaspireScriptEntry[] }).scripts;

describe('Kaspire adapter: connect', () => {
  it('already on the wanted network: no switch, no second account request', async () => {
    const { adapter, provider } = mk({ network: 'testnet-10' });
    expect(adapter.detect()).toBe(true);
    const info = await adapter.connect('testnet-10');
    expect(info).toEqual({ id: 'kaspire', label: 'Kaspire', address: 'kaspatest:qfake-kaspire', pubkey: MAKER_PK, network: 'testnet-10', version: '1.2.0' });
    expect(provider.calls.map((c) => c.method)).toEqual(['requestAccounts', 'getNetwork', 'getPublicKey']);
  });

  it('switches network and REQUESTS THE ACCOUNTS AGAIN (the address prefix changes with the network)', async () => {
    const { adapter, provider } = mk({ network: 'mainnet' });
    const info = await adapter.connect('kaspa_testnet_10');
    expect(info.network).toBe('testnet-10');
    expect(info.address).toBe('kaspatest:qfake-kaspire'); // not the stale kaspa: address
    expect(provider.calls.map((c) => c.method)).toEqual(['requestAccounts', 'getNetwork', 'switchNetwork', 'getNetwork', 'requestAccounts', 'getPublicKey']);
    expect(provider.calls[2]!.args[0]).toEqual({ network: 'testnet-10' });
  });

  it('a failed switch keeps the first account and reports the real network', async () => {
    const { adapter, provider } = mk({ network: 'mainnet', switchAccepts: [] });
    const info = await adapter.connect('testnet-10');
    expect(info.network).toBe('mainnet');
    expect(info.address).toBe('kaspa:qfake-kaspire');
    expect(provider.calls.filter((c) => c.method === 'requestAccounts')).toHaveLength(1);
  });

  it('is not detected without a request function, and connect says unsupported', async () => {
    const adapter = createKaspireAdapter({ getProvider: () => ({} as never) });
    expect(adapter.detect()).toBe(false);
    await expect(adapter.connect('mainnet')).rejects.toMatchObject({ code: 'unsupported' });
  });

  it('rejection at connect -> rejected', async () => {
    const provider = fakeKaspire({ sdk, sk: MAKER_SK });
    provider.request = async () => { throw new Error('User rejected the connection'); };
    await expect(createKaspireAdapter({ getProvider: () => provider }).connect('mainnet')).rejects.toMatchObject({ code: 'rejected' });
  });
});

describe('Kaspire adapter: signTx request shape', () => {
  it('cancel (entry with only a signature): covenant input gets ordered-args [signature, dispatch tag] and the redeem script', async () => {
    const { adapter, provider } = mk();
    const built = buildCancel();
    await adapter.signTx(built, { network: 'testnet-10' });
    const p = provider.lastSignParams() as Record<string, unknown>;
    expect(p.psktTransactionJson).toBe(JSON.stringify(built.tx));
    expect(p.submitTransaction).toBe(false);
    expect(p.signInputs).toEqual(built.sign.map((s) => ({ index: s.inputIndex, sighashType: 1 })));
    const tag = kob.templates().find((t) => t.name === 'KobAsk')!.entries.cancel!;
    expect(scriptsOf(provider)).toEqual([
      {
        inputIndex: 0, scriptHex: built.sign[0]!.redeemScript, signType: 1,
        signatureScript: { mode: 'ordered-args', args: [{ type: 'signature', prefixHex: '' }, { type: 'data', hex: tag }] },
      },
    ]);
    expect(tag).toMatch(/^[0-9a-f]{8}$/);
  });

  it('cancel without a dispatch-tag source, or with the mode forced: wrap-signature', async () => {
    for (const deps of [{} as KaspireDeps, { dispatchTag: dispatchTagFrom(kob), mode: 'wrap-signature' as const }]) {
      const { adapter, provider } = mk({}, deps);
      await adapter.signTx(buildCancel());
      expect(scriptsOf(provider)![0]!.signatureScript).toEqual({ mode: 'wrap-signature' });
    }
  });

  it('KCC-20 transfers (leader / delegator witnesses) use wrap-signature; only covenant inputs get a scripts entry, P2PK funding none', async () => {
    const { adapter, provider } = mk();
    const built = buildSend();
    await adapter.signTx(built);
    const scripts = scriptsOf(provider)!;
    expect(scripts.map((s) => s.inputIndex)).toEqual([0, 1]);
    expect(scripts.every((s) => s.signatureScript.mode === 'wrap-signature' && s.signType === 1)).toBe(true);
    expect(scripts.map((s) => s.scriptHex)).toEqual([built.sign[0]!.redeemScript, built.sign[1]!.redeemScript]);
  });

  it('kaspireSignatureScript only mirrors the plan when it is certain (sole sig arg + known tag)', () => {
    const cancel = buildCancel();
    const dispatchTag = dispatchTagFrom(kob);
    expect(kaspireSignatureScript(cancel, cancel.sign[0]!, { dispatchTag }).mode).toBe('ordered-args');
    expect(kaspireSignatureScript(cancel, cancel.sign[0]!, { dispatchTag: () => null }).mode).toBe('wrap-signature');
    const send = buildSend();
    expect(kaspireSignatureScript(send, send.sign[0]!, { dispatchTag }).mode).toBe('wrap-signature');
    expect(dispatchTagFrom(kob)('KobAsk', 'nope')).toBeNull();
    expect(dispatchTagFrom(kob)('KobBid', 'cancel')).toMatch(/^[0-9a-f]{8}$/);
  });
});

describe('Kaspire adapter: signatures are consensus-valid in every mode', () => {
  const cases: [string, KaspireDeps][] = [
    ['ordered-args', { dispatchTag: dispatchTagFrom(kob) }],
    ['wrap-signature', { mode: 'wrap-signature' }],
  ];
  for (const [label, deps] of cases) {
    it(`cancel (${label}) and send`, async () => {
      const { adapter } = mk({}, deps);
      for (const built of [buildCancel(), buildSend()]) {
        const sigs = await adapter.signTx(built);
        expect(() => assertConsensusValid(built, sigs)).not.toThrow();
      }
    });
  }

  it('the wallet-emitted ordered-args script contains the signature the extractor takes (and the redeem script after it)', async () => {
    const { provider } = mk();
    const built = buildCancel();
    const resp = (await provider.request({
      method: 'signPskt',
      params: {
        psktTransactionJson: JSON.stringify(built.tx), submitTransaction: false, signInputs: [{ index: 0, sighashType: 1 }],
        scripts: [{ inputIndex: 0, scriptHex: built.sign[0]!.redeemScript, signType: 1, signatureScript: { mode: 'ordered-args', args: [{ type: 'signature', prefixHex: '' }, { type: 'data', hex: 'a0893109' }] } }],
      },
    })) as { psktTransactionJson: string };
    const ss = (JSON.parse(resp.psktTransactionJson) as { inputs: { signatureScript: string }[] }).inputs[0]!.signatureScript;
    expect(ss.startsWith('41')).toBe(true); // push(sig65) first
    expect(ss.includes('04a0893109')).toBe(true); // then the dispatch tag
    expect(ss.endsWith(built.sign[0]!.redeemScript!)).toBe(true); // and the redeem script last
    expect(extractSignature(resp, 0)).toBe(ss.slice(2, 132));
  });

  it('without scripts a covenant input comes back with a BARE signature: still extractable', async () => {
    const { provider } = mk();
    const built = buildSend();
    const resp = await provider.request({ method: 'signPskt', params: { psktTransactionJson: JSON.stringify(built.tx), submitTransaction: false, signInputs: [{ index: 0, sighashType: 1 }] } });
    expect(extractSignature(resp, 0)).toMatch(/^[0-9a-f]{130}$/);
  });
});

describe('Kaspire adapter: failures', () => {
  it('declined, unanswered, failed and wrong-key', async () => {
    await expect(mk({ behavior: { rejectSign: true } }).adapter.signTx(buildCancel())).rejects.toMatchObject({ code: 'rejected' });
    await expect(mk({ behavior: { neverAnswer: true } }).adapter.signTx(buildCancel(), { timeoutMs: 20 })).rejects.toMatchObject({ code: 'timeout' });
    await expect(mk({ behavior: { failWith: new Error('hub unreachable') } }).adapter.signTx(buildCancel())).rejects.toMatchObject({ code: 'other', message: 'Kaspire: hub unreachable' });
    const { adapter } = mk({ behavior: { signKey: OTHER_SK } });
    const built = buildCancel();
    expect(() => assertConsensusValid(built, [])).toThrow();
    await expect(adapter.signTx(built).then((s) => assertConsensusValid(built, s))).rejects.toThrow(/signature/i);
  });

  it('a response without the psktTransactionJson wrapper is read as the transaction itself; a missing signature is reported', async () => {
    const { adapter, provider } = mk();
    const orig = provider.request.bind(provider);
    provider.request = async (a) => {
      const r = await orig(a);
      return a.method === 'signPskt' ? JSON.parse((r as { psktTransactionJson: string }).psktTransactionJson) : r;
    };
    expect(await adapter.signTx(buildCancel())).toHaveLength(1);
    const partial = mk({ behavior: { onlyInputs: [] } });
    await expect(partial.adapter.signTx(buildCancel())).rejects.toMatchObject({ code: 'no-signature' });
  });
});
