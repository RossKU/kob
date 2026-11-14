// The mock wallets (src/testing/mock-wallets.ts) run here in a node `vm` sandbox with the OFFICIAL SDK as `window.__kobKaspa`, exactly as the
// e2e page runs them. Every scenario feeds a transaction built by kob-wasm through the wallet and checks the outcome against kob-wasm:
// signatures must verify in `kob.finalize` and the finalized tx must pass the script engine, so the fakes cannot drift from the real digests.
import { createRequire } from 'node:module';
import vm from 'node:vm';
import { describe, expect, it } from 'vitest';
import { loadKobNode } from '../src/kob/wasm.node';
import { mockWalletInitScript, type MockWalletCall } from '../src/testing/mock-wallets';
import { TEST_PUBKEYS, TEST_SECRETS } from '../mock/keys.mjs';
import { buildWalletConfig, type InstallMockWalletOptions } from '../e2e/wallet';
import type { ActionRequest, BuiltTx, InputSignature } from '../src/kob/types';

const require = createRequire(import.meta.url);
const sdk = require('../vendor/kaspa-node/kaspa.js');
const golden = require('../../crates/kob-protocol/vectors/golden.json');
const kob = loadKobNode();

const ALICE = TEST_SECRETS.alice;
const GOLDEN_MAKER = '1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f';

/** create.ask of the golden vectors with alice as maker: input 0 is the KCC-20 leader (P2SH), input 1 a P2PK funding input. */
const askBuilt = (): BuiltTx => {
  const v = golden.transactions.find((t: { name: string }) => t.name === 'create.ask');
  return kob.build(JSON.parse(JSON.stringify(v.request).replaceAll(GOLDEN_MAKER, TEST_PUBKEYS.alice)) as ActionRequest);
};

/** `fetch`: the wallet's own node (POST /mock/wallet-node/*); without one the input check is off unless the test turns it on */
function loadWallet(o: Partial<InstallMockWalletOptions> & { wallet: InstallMockWalletOptions['wallet'] }, withSdk = true, fetch?: (url: string, init: any) => Promise<any>) {
  const window: any = withSdk ? { __kobKaspa: sdk } : {};
  const ctx = vm.createContext({ window, setTimeout, clearTimeout, ...(fetch ? { fetch } : {}) });
  vm.runInContext(mockWalletInitScript(buildWalletConfig({ secretKey: ALICE, checkInputs: false, ...o })), ctx);
  return window;
}

/** A wallet node holding `held` (`txid:index`), answering like the mock server; records the requests. */
function walletNodeFetch(held: Set<string>) {
  const requests: { url: string; body: any }[] = [];
  const fetch = async (url: string, init: any) => {
    const body = JSON.parse(init.body);
    requests.push({ url, body });
    if (url.endsWith('/mock/wallet-node/push')) {
      held.add(`${body.transaction.id}:0`);
      return { ok: true, status: 200, json: async () => ({ transactionId: body.transaction.id }) };
    }
    const entries = body.outpoints.map((p: any) => ({ ...p, state: held.has(`${p.transactionId}:${p.index}`) ? 'unspent' : 'unknown' }));
    return { ok: true, status: 200, json: async () => ({ entries }) };
  };
  return { fetch, requests };
}

const pushes = (hex: string): Buffer[] => {
  const b = Buffer.from(hex, 'hex');
  const out: Buffer[] = [];
  for (let i = 0; i < b.length; ) {
    const op = b[i++];
    let len = op;
    if (op === 0x4c) len = b[i++];
    else if (op === 0x4d) (len = b.readUInt16LE(i)), (i += 2);
    else if (op === 0x4e) (len = b.readUInt32LE(i)), (i += 4);
    out.push(b.subarray(i, i + len));
    i += len;
  }
  return out;
};
/** The 65-byte signature push of a returned signature script (first 65-byte push, like the wallet adapters). */
const sigOf = (sigscript: string) => pushes(sigscript).find((p) => p.length === 65)?.toString('hex');

/** Signatures of the inputs a wallet returned, as `finalize` wants them. */
const signaturesFrom = (txJson: string | any, built: BuiltTx): InputSignature[] => {
  const tx = typeof txJson === 'string' ? JSON.parse(txJson) : txJson;
  return built.sign.map((s) => ({ inputIndex: s.inputIndex, signature: sigOf(tx.inputs[s.inputIndex].signatureScript)! }));
};

const signRequests = (built: BuiltTx) => built.sign.map((s) => ({ index: s.inputIndex, sighashType: 1 }));
const finalizeAndValidate = (built: BuiltTx, sigs: InputSignature[]) => {
  const signed = kob.finalize(built, sigs, { tightenBudgets: true });
  kob.validate(signed);
  return signed;
};

describe('KasWare mock', () => {
  it('reports the shapes proven on TN10 (network label, 33-byte key, accounts, version)', async () => {
    const w = loadWallet({ wallet: 'kasware' });
    expect(await w.kasware.getNetwork()).toBe('kaspa_testnet_10');
    expect(await w.kasware.getVersion()).toBe('0.11.5');
    const [addr] = await w.kasware.requestAccounts();
    expect(addr).toMatch(/^kaspatest:q/);
    expect(await w.kasware.getPublicKey()).toBe('02' + TEST_PUBKEYS.alice);
    expect(w.__mockWallet.address).toBe(addr);
    await w.kasware.switchNetwork('kaspa_mainnet');
    expect(await w.kasware.getNetwork()).toBe('kaspa_mainnet');
    expect((await w.kasware.requestAccounts())[0]).toMatch(/^kaspa:q/);
    await w.kasware.switchNetwork('testnet-10');
    expect(await w.kasware.getNetwork()).toBe('kaspa_testnet_10');
  });

  it('signPskt returns the tx safe JSON string with push(sig65) that kob-wasm verifies and the engine accepts', async () => {
    const w = loadWallet({ wallet: 'kasware' });
    const built = askBuilt();
    const txJson = JSON.stringify(built.tx);
    const out = await w.kasware.signPskt({ txJsonString: txJson, options: { signInputs: signRequests(built) } });
    expect(typeof out).toBe('string');
    const tx = JSON.parse(out);
    expect(tx.inputs.map((i: any) => i.signatureScript.length / 2)).toEqual([66, 66]); // only push(sig65), also for the P2SH input
    expect(tx.version).toBe(1);
    expect(tx.outputs).toEqual(built.tx.outputs);
    expect(tx.inputs.map((i: any) => i.computeBudget)).toEqual(built.tx.inputs.map((i) => i.computeBudget));
    finalizeAndValidate(built, signaturesFrom(tx, built));
    // the call log records exactly what the wallet was asked to sign
    const [call] = w.__mockWallet.calls as MockWalletCall[];
    expect(call).toMatchObject({ wallet: 'kasware', method: 'signPskt', inputs: [0, 1], status: 'signed', signedInputs: [0, 1], unsignedInputs: [], sighashType: 1 });
    expect(call.txJson).toBe(txJson);
    expect(call.tx.id).toBe(built.tx.id);
  });

  it('like KasWare 0.11.5, refuses an input its own node does not hold (by default), signs once it does, and takes pushTx', async () => {
    const built = askBuilt();
    const [i0, i1] = built.tx.inputs.map((i) => `${i.transactionId}:${i.index}`);
    const held = new Set([i0!]);
    const node = walletNodeFetch(held);
    const w = loadWallet({ wallet: 'kasware', checkInputs: true, nodeUrl: 'http://mock.test' }, true, node.fetch);
    const req = { txJsonString: JSON.stringify(built.tx), options: { signInputs: signRequests(built) } };
    await expect(w.kasware.signPskt(req)).rejects.toThrow(`signPskt: Input #1 (${i1}) is not an unspent output on this network.`);
    expect(node.requests[0]).toMatchObject({ url: 'http://mock.test/mock/wallet-node/outpoints', body: { lag_ms: 0 } });
    expect(w.__mockWallet.lastCall()).toMatchObject({ status: 'error', signedInputs: [] });
    held.add(i1!);
    finalizeAndValidate(built, signaturesFrom(await w.kasware.signPskt(req), built));
    // pushTx hands a signed transaction to the wallet's node; it is logged apart from the sign requests
    expect(await w.kasware.pushTx(JSON.stringify(built.tx))).toBe(built.tx.id);
    expect(w.__mockWallet.pushes).toMatchObject([{ txid: built.tx.id, ok: true }]);
    expect(w.__mockWallet.calls).toHaveLength(2);
    // no node to ask: it fails closed, like the real wallet
    const lone = loadWallet({ wallet: 'kasware', checkInputs: true });
    await expect(lone.kasware.signPskt(req)).rejects.toThrow(/node is unreachable/);
    // the defaults: on for KasWare and Kaspire
    expect(buildWalletConfig({ wallet: 'kasware', secretKey: ALICE }).checkInputs).toBe(true);
    expect(buildWalletConfig({ wallet: 'kaspire', secretKey: ALICE }).checkInputs).toBe(true);
  });

  it('signs only the requested inputs', async () => {
    const w = loadWallet({ wallet: 'kasware' });
    const built = askBuilt();
    const out = JSON.parse(await w.kasware.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs: [{ index: 1, sighashType: 1 }] } }));
    expect(out.inputs[0].signatureScript).toBe('');
    expect(out.inputs[1].signatureScript).not.toBe('');
    expect(w.__mockWallet.lastCall()).toMatchObject({ inputs: [1], signedInputs: [1] });
  });

  it('rejects like a user clicking Reject, can be steered at runtime and logs the rejection', async () => {
    const w = loadWallet({ wallet: 'kasware', approve: false });
    const built = askBuilt();
    const req = { txJsonString: JSON.stringify(built.tx), options: { signInputs: signRequests(built) } };
    await expect(w.kasware.signPskt(req)).rejects.toMatchObject({ message: 'User rejected the request.', code: 4001 });
    expect(w.__mockWallet.lastCall()).toMatchObject({ status: 'rejected', error: 'User rejected the request.' });
    w.__mockWallet.configure({ approve: true });
    finalizeAndValidate(built, signaturesFrom(await w.kasware.signPskt(req), built));
    expect(w.__mockWallet.calls).toHaveLength(2);
    w.__mockWallet.clearCalls();
    expect(w.__mockWallet.calls).toHaveLength(0);
  });

  it('answers late when delayed', async () => {
    const w = loadWallet({ wallet: 'kasware', delayMs: 120 });
    const built = askBuilt();
    const t0 = Date.now();
    await w.kasware.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs: signRequests(built) } });
    expect(Date.now() - t0).toBeGreaterThanOrEqual(110);
  });

  it('returns well-formed but invalid signatures on demand: kob.finalize refuses them', async () => {
    const w = loadWallet({ wallet: 'kasware', wrongSignature: true });
    const built = askBuilt();
    const out = await w.kasware.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs: signRequests(built) } });
    expect(JSON.parse(out).inputs[1].signatureScript).toHaveLength(132);
    expect(() => kob.finalize(built, signaturesFrom(out, built), { tightenBudgets: true })).toThrow(/signature/i);
  });

  it('can leave inputs unsigned (omitInputs) and fail the connection', async () => {
    const w = loadWallet({ wallet: 'kasware', omitInputs: [0] });
    const built = askBuilt();
    const out = JSON.parse(await w.kasware.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs: signRequests(built) } }));
    expect(out.inputs[0].signatureScript).toBe('');
    expect(w.__mockWallet.lastCall()).toMatchObject({ signedInputs: [1], unsignedInputs: [0] });
    const f = loadWallet({ wallet: 'kasware', failConnect: true });
    await expect(f.kasware.requestAccounts()).rejects.toMatchObject({ code: 4001 });
  });

  it('refuses to switch network when the wallet is locked to one, and emits networkChanged otherwise', async () => {
    const locked = loadWallet({ wallet: 'kasware', network: 'mainnet', allowNetworkSwitch: false });
    expect(await locked.kasware.getNetwork()).toBe('kaspa_mainnet');
    await expect(locked.kasware.switchNetwork('testnet-10')).rejects.toThrow(/disabled/);
    const w = loadWallet({ wallet: 'kasware' });
    const seen: string[] = [];
    w.kasware.on('networkChanged', (n: string) => seen.push(n));
    await w.kasware.switchNetwork('mainnet');
    w.__mockWallet.setNetwork('testnet-10');
    expect(seen).toEqual(['kaspa_mainnet', 'kaspa_testnet_10']);
  });

  it('injects late, the way extensions do', async () => {
    const w = loadWallet({ wallet: 'kasware', injectDelayMs: 80 });
    expect(w.kasware).toBeUndefined();
    expect(w.__mockWallet).toBeDefined();
    await new Promise((r) => setTimeout(r, 160));
    expect(w.kasware).toBeDefined();
  });
});

describe('Kaspire mock', () => {
  it('uses plain network names and x-only keys, and rejects unknown methods', async () => {
    const w = loadWallet({ wallet: 'kaspire' });
    expect(w.kaspire.version).toBe('1.2.0');
    expect(await w.kaspire.request({ method: 'getNetwork' })).toBe('testnet-10');
    expect(await w.kaspire.request({ method: 'getPublicKey' })).toBe(TEST_PUBKEYS.alice);
    expect(await w.kaspire.request({ method: 'requestAccounts' })).toEqual([w.__mockWallet.address]);
    await w.kaspire.request({ method: 'switchNetwork', params: { network: 'mainnet' } });
    expect((await w.kaspire.request({ method: 'requestAccounts' }))[0]).toMatch(/^kaspa:q/);
    await expect(w.kaspire.request({ method: 'mineBlocks' })).rejects.toThrow(/not supported/);
  });

  it('signPskt without scripts returns {psktTransactionJson} with bare signatures', async () => {
    const w = loadWallet({ wallet: 'kaspire' });
    const built = askBuilt();
    const res = await w.kaspire.request({ method: 'signPskt', params: { psktTransactionJson: JSON.stringify(built.tx), submitTransaction: false, signInputs: signRequests(built) } });
    expect(Object.keys(res)).toEqual(['psktTransactionJson']);
    finalizeAndValidate(built, signaturesFrom(res.psktTransactionJson, built));
    expect(w.__mockWallet.lastCall().scripts).toBeNull();
  });

  it('with scripts emits the whole signature script itself (wrap-signature: sig + redeem; ordered-args: args + redeem)', async () => {
    const built = askBuilt();
    const redeem = built.sign[0].redeemScript!;
    const params = (sc: unknown) => ({ psktTransactionJson: JSON.stringify(built.tx), submitTransaction: false, signInputs: signRequests(built), scripts: [{ inputIndex: 0, scriptHex: redeem, signType: 1, signatureScript: sc }] });
    const w = loadWallet({ wallet: 'kaspire' });
    const wrapped = JSON.parse((await w.kaspire.request({ method: 'signPskt', params: params({ mode: 'wrap-signature' }) })).psktTransactionJson);
    const p = pushes(wrapped.inputs[0].signatureScript);
    expect(p.map((x) => x.length)).toEqual([65, redeem.length / 2]);
    expect(p[1].toString('hex')).toBe(redeem);
    expect(wrapped.inputs[1].signatureScript).toHaveLength(132); // the P2PK input keeps push(sig65)
    const ordered = JSON.parse(
      (await w.kaspire.request({ method: 'signPskt', params: params({ mode: 'ordered-args', args: [{ type: 'data', hex: 'aabb' }, { type: 'signature', prefixHex: '00' }, { type: 'data', hex: '79c71c23' }] }) })).psktTransactionJson,
    );
    const q = pushes(ordered.inputs[0].signatureScript);
    expect(q.map((x) => x.length)).toEqual([2, 66, 4, redeem.length / 2]);
    expect(q[1][0]).toBe(0x00); // witness = 0x00 || sig
    expect(q[3].toString('hex')).toBe(redeem);
    // the signature inside is the real one: kob.finalize verifies it against the digest
    const sig0 = q[1].subarray(1).toString('hex');
    finalizeAndValidate(built, [{ inputIndex: 0, signature: sig0 }, { inputIndex: 1, signature: sigOf(ordered.inputs[1].signatureScript)! }]);
    expect(w.__mockWallet.calls[1].scripts[0]).toMatchObject({ inputIndex: 0, scriptHex: redeem });
    // a dApp that drops the scripts gets bare signatures back
    w.__mockWallet.configure({ dropScripts: true });
    const dropped = JSON.parse((await w.kaspire.request({ method: 'signPskt', params: params({ mode: 'wrap-signature' }) })).psktTransactionJson);
    expect(dropped.inputs[0].signatureScript).toHaveLength(132);
  });

  it('rejects when told to', async () => {
    const w = loadWallet({ wallet: 'kaspire', approve: false, rejectMessage: 'Rejected by user' });
    const built = askBuilt();
    await expect(w.kaspire.request({ method: 'signPskt', params: { psktTransactionJson: JSON.stringify(built.tx), signInputs: signRequests(built) } })).rejects.toThrow('Rejected by user');
    expect(w.__mockWallet.lastCall().status).toBe('rejected');
  });
});

describe('Kastle mock', () => {
  it('connect / getAccount / request reproduce the proven shapes (33-byte key)', async () => {
    const w = loadWallet({ wallet: 'kastle' });
    expect(await w.kastle.connect()).toBe(true);
    const acc = await w.kastle.getAccount();
    expect(acc.publicKey).toBe('02' + TEST_PUBKEYS.alice);
    expect(acc.address).toMatch(/^kaspatest:q/);
    expect(await w.kastle.request('kas:get_network')).toBe('testnet-10');
    expect(await w.kastle.request('kas:get_version')).toBe('2.60.1');
    await expect(w.kastle.request('kas:nope')).rejects.toThrow(/not supported/);
  });

  it('WITHOUT scripts a P2SH input is silently left unsigned (issue #353) while P2PK inputs are signed', async () => {
    const w = loadWallet({ wallet: 'kastle' });
    const built = askBuilt();
    const out = await w.kastle.signTx('testnet-10', JSON.stringify(built.tx));
    expect(typeof out).toBe('object');
    expect(out.inputs[0].signatureScript).toBe('');
    expect(out.inputs[1].signatureScript).toHaveLength(132);
    expect(w.__mockWallet.lastCall()).toMatchObject({ method: 'signTx', network: 'testnet-10', signedInputs: [1], unsignedInputs: [0], scripts: null });
  });

  it('with scripts signs: scriptHex "" returns only push(sig65), a redeem script returns push(sig65) push(redeem)', async () => {
    const w = loadWallet({ wallet: 'kastle' });
    const built = askBuilt();
    const redeem = built.sign[0].redeemScript!;
    const empty = await w.kastle.signTx('testnet-10', JSON.stringify(built.tx), [{ inputIndex: 0, scriptHex: '', signType: 'All' }]);
    expect(empty.inputs[0].signatureScript).toHaveLength(132);
    finalizeAndValidate(built, signaturesFrom(empty, built));
    const full = await w.kastle.signTx('testnet-10', JSON.stringify(built.tx), [{ inputIndex: 0, scriptHex: redeem, signType: 'All' }]);
    const p = pushes(full.inputs[0].signatureScript);
    expect(p.map((x) => x.length)).toEqual([65, redeem.length / 2]);
    finalizeAndValidate(built, signaturesFrom(full, built));
    // a dApp that forgot the scripts (dropScripts) is back to the unsigned behaviour
    w.__mockWallet.configure({ dropScripts: true });
    const dropped = await w.kastle.signTx('testnet-10', JSON.stringify(built.tx), [{ inputIndex: 0, scriptHex: '', signType: 'All' }]);
    expect(dropped.inputs[0].signatureScript).toBe('');
  });

  it('refuses a network id that differs from the wallet network and rejects when told to', async () => {
    const w = loadWallet({ wallet: 'kastle' });
    const built = askBuilt();
    await expect(w.kastle.signTx('mainnet', JSON.stringify(built.tx))).rejects.toThrow(/Network mismatch/);
    expect(w.__mockWallet.lastCall().status).toBe('error');
    w.__mockWallet.configure({ approve: false });
    await expect(w.kastle.signTx('testnet-10', JSON.stringify(built.tx))).rejects.toMatchObject({ code: 4001 });
  });
});

describe('mock wallet plumbing', () => {
  it('waits for window.__kobKaspa and reports a clear error when the app never publishes it', async () => {
    const w = loadWallet({ wallet: 'kasware' }, false);
    const built = askBuilt();
    const pending = w.kasware.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs: signRequests(built) } });
    setTimeout(() => (w.__kobKaspa = sdk), 100); // the app finishes loading the SDK a little later
    const out = await pending;
    finalizeAndValidate(built, signaturesFrom(out, built));
  }, 20_000);

  it('derives the public key and both addresses from the secret key on the node side', () => {
    const c = buildWalletConfig({ wallet: 'kastle', secretKey: TEST_SECRETS.bob });
    expect(c.pubkey).toBe(TEST_PUBKEYS.bob);
    expect(c.addresses['testnet-10']).toBe(new sdk.PrivateKey(TEST_SECRETS.bob).toPublicKey().toAddress('testnet-10').toString());
    expect(c.addresses.mainnet).toBe(new sdk.PrivateKey(TEST_SECRETS.bob).toPublicKey().toAddress('mainnet').toString());
    expect(c).toMatchObject({ approve: true, pubkeyFormat: 'compressed', version: '2.60.1', network: 'testnet-10' });
    expect(buildWalletConfig({ wallet: 'kaspire', secretKey: TEST_SECRETS.bob, delayMs: 5 })).toMatchObject({ pubkeyFormat: 'xonly', delayMs: 5 });
  });
});
