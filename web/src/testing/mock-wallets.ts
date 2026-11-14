// Browser-side fakes of window.kasware / window.kaspire / window.kastle for e2e tests (NEVER shipped: the app never sees a key).
//
// They reproduce the response shapes proven on TN10 with the real extensions (tools/wallet-gate/RESULTS.md, web/wallets.mjs):
//   * KasWare  signPskt({txJsonString, options:{signInputs}}) -> tx safe-JSON STRING; P2SH inputs come back with only `push(sig65)`;
//              getNetwork() reports `kaspa_testnet_10`; getPublicKey() is a 33-byte key.
//   * Kaspire  request({method:'signPskt', params:{psktTransactionJson, signInputs, scripts}}) -> {psktTransactionJson}; with a per-input
//              `scripts` entry the wallet emits the whole sigscript itself (ordered-args: args + redeem; wrap-signature: sig + redeem).
//   * Kastle   connect / getAccount / request('kas:...') / signTx(network, txJson, scripts?) -> tx OBJECT; the public key has 33 bytes;
//              WITHOUT `scripts` a P2SH input is silently left unsigned (upstream issue #353); `scriptHex:""` returns just push(sig65),
//              a redeem script returns `push(sig65) push(redeem)`.
//
// Signing happens IN THE PAGE with the official SDK exposed by the app as `window.__kobKaspa` (features.test): the same
// `createInputSignature(tx, index, privateKey, SighashType.All)` a real wallet runs. Everything is driven by `mockWalletPageScript`,
// a SELF-CONTAINED function (no imports, no closures): Playwright serialises it with `.toString()` (`mockWalletInitScript`).
// Tests steer the wallet at runtime through `window.__mockWallet` (options, call log, events).
//
// The wallet's OWN node (`checkInputs`, default on for KasWare and Kaspire): like KasWare 0.11.5, the wallet looks every input it is asked to
// sign up on its node and refuses one that node does not hold unspent, with KasWare's message. The mock server plays that node
// (`POST /mock/wallet-node/outpoints`, web/mock/server.mjs); `walletNodeLagMs` makes it see a new output only that long after KOB's node did,
// unless the transaction was pushed to it (KasWare `pushTx` -> `POST /mock/wallet-node/push`). KOB assumes every extension wallet checks this.
// `pushTxOutcome` reproduces an announce that does not help: 'error' rejects with a message the
// app cannot read as "already there"; 'lost' resolves although the wallet's node still cannot find
// the output (a transaction moving from the mempool into a block).

export type MockWalletId = 'kasware' | 'kaspire' | 'kastle';

export interface MockWalletConfig {
  wallet: MockWalletId;
  /** secret key (64 hex) the wallet signs with */
  secretKey: string;
  /** the matching x-only public key (64 hex) */
  pubkey: string;
  /** account address per network name (`testnet-10`, `mainnet`): the address prefix follows the wallet's network */
  addresses: Record<string, string>;
  /** the network the wallet starts on (`testnet-10` | `mainnet`) */
  network: string;
  /** false: `switchNetwork` fails and the wallet stays where it is */
  allowNetworkSwitch: boolean;
  /** false: every sign request is rejected the way a user clicking "Reject" does */
  approve: boolean;
  rejectMessage: string;
  /** every sign request answers this many ms late (a popup the user has not answered yet) */
  delayMs: number;
  /** returns well-formed but INVALID signatures (a wallet that signed another digest) */
  wrongSignature: boolean;
  /** Kaspire / Kastle: ignore the `scripts` argument (a dApp that forgot it): Kastle leaves P2SH inputs unsigned, Kaspire returns bare signatures */
  dropScripts: boolean;
  /** input indexes the wallet silently leaves unsigned */
  omitInputs: number[];
  /** connection (requestAccounts / connect) fails */
  failConnect: boolean;
  /** extensions inject late: the provider appears this long after page start */
  injectDelayMs: number;
  /** 'compressed' = 33-byte key (02 prefix), 'xonly' = 32 bytes; default per wallet as observed */
  pubkeyFormat: 'compressed' | 'xonly';
  version: string;
  /** refuse to sign an input the wallet's own node does not hold unspent (KasWare 0.11.5) */
  checkInputs: boolean;
  /** the wallet's node sees a new output this long after KOB's node did (unless pushed to it) */
  walletNodeLagMs: number;
  /** KasWare `pushTx`: 'ok' hands the tx to the wallet's node; 'error' and 'lost' do not */
  pushTxOutcome: 'ok' | 'error' | 'lost';
  /** base URL of the mock server playing the wallet's node ('' = the app's `window.__KOB_CONFIG__.nodeUrl`) */
  nodeUrl: string;
}

export interface MockWalletCall {
  wallet: MockWalletId;
  method: string;
  /** Date.now() at the request */
  at: number;
  /** the tx the wallet was asked to sign (parsed safe JSON) */
  tx: any;
  /** the raw tx JSON string exactly as passed */
  txJson: string;
  /** input indexes the wallet was asked to sign (Kastle: derived) */
  inputs: number[];
  sighashType: number | string | null;
  /** Kaspire / Kastle `scripts` argument as passed */
  scripts: any[] | null;
  /** Kastle: network id argument */
  network: string | null;
  /** 'signed' | 'rejected' | 'error', set when the call settles */
  status: 'pending' | 'signed' | 'rejected' | 'error';
  signedInputs: number[];
  unsignedInputs: number[];
  error: string | null;
  /** the exact value the wallet returned to the page */
  response: any;
}

export const DEFAULT_WALLET_CONFIG: Omit<MockWalletConfig, 'wallet' | 'secretKey' | 'pubkey' | 'addresses'> = {
  network: 'testnet-10',
  allowNetworkSwitch: true,
  approve: true,
  rejectMessage: 'User rejected the request.',
  delayMs: 0,
  wrongSignature: false,
  dropScripts: false,
  omitInputs: [],
  failConnect: false,
  injectDelayMs: 0,
  pubkeyFormat: 'compressed',
  version: '',
  checkInputs: false,
  walletNodeLagMs: 0,
  pushTxOutcome: 'ok',
  nodeUrl: '',
};

/** Per-wallet defaults observed on the real extensions. */
export const WALLET_DEFAULTS: Record<MockWalletId, Partial<MockWalletConfig>> = {
  kasware: { pubkeyFormat: 'compressed', version: '0.11.5', checkInputs: true },
  kaspire: { pubkeyFormat: 'xonly', version: '1.2.0', checkInputs: true },
  kastle: { pubkeyFormat: 'compressed', version: '2.60.1' },
};

/**
 * Runs IN THE PAGE (Playwright `addInitScript`). Must stay self-contained: no imports, no references to module scope.
 * Installs `window.<wallet>` and `window.__mockWallet`.
 */
export function mockWalletPageScript(config: MockWalletConfig): void {
  const w = window as any;
  const opts: any = { ...config };
  const calls: any[] = [];
  /** `pushTx` requests (not sign requests: `calls` holds those only) */
  const pushes: any[] = [];
  const listeners: Record<string, any[]> = {};
  const account = { network: opts.network as string, lost: false };

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const rejection = () => Object.assign(new Error(opts.rejectMessage), { code: 4001 });
  // the app publishes the SDK asynchronously (it is a 10 MB wasm loaded on first use): wait for it like a wallet waits for its own wasm
  const kaspa = async () => {
    for (let i = 0; i < 400 && !w.__kobKaspa; i++) await sleep(25);
    if (!w.__kobKaspa) throw new Error('mock wallet: window.__kobKaspa is missing (the app exposes the SDK there when features.test is set)');
    return w.__kobKaspa;
  };

  // --- script push encoding (what a wallet emits into a signature script)
  const lenHex = (n: number, bytes: number) => {
    let s = '';
    for (let i = 0; i < bytes; i++) s += ((n >> (8 * i)) & 0xff).toString(16).padStart(2, '0');
    return s;
  };
  const pushHex = (hex: string) => {
    const n = hex.length / 2;
    if (n === 0) return '00';
    if (n < 0x4c) return lenHex(n, 1) + hex;
    if (n <= 0xff) return '4c' + lenHex(n, 1) + hex;
    if (n <= 0xffff) return '4d' + lenHex(n, 2) + hex;
    return '4e' + lenHex(n, 4) + hex;
  };
  const i64Hex = (v: any) => {
    let x = BigInt.asUintN(64, BigInt(v));
    let s = '';
    for (let i = 0; i < 8; i++) {
      s += Number(x & 0xffn).toString(16).padStart(2, '0');
      x >>= 8n;
    }
    return s;
  };

  // --- network naming per wallet
  const toWalletNet = (n: string) => (opts.wallet === 'kasware' ? (n === 'mainnet' ? 'kaspa_mainnet' : 'kaspa_testnet_10') : n);
  const fromWalletNet = (n: string) => (n === 'kaspa_mainnet' || n === 'mainnet' ? 'mainnet' : n === 'kaspa_testnet_10' || n === 'testnet-10' ? 'testnet-10' : n);
  const address = () => opts.addresses[account.network];
  const publicKey = () => (opts.pubkeyFormat === 'compressed' ? '02' + opts.pubkey : opts.pubkey);
  const emit = (event: string, ...args: any[]) => (listeners[event] ?? []).slice().forEach((f) => f(...args));
  // the EventEmitter subset all three real providers have (`on` / `removeListener`)
  const on = (ev: string, f: any) => void (listeners[ev] = [...(listeners[ev] ?? []), f]);
  const removeListener = (ev: string, f: any) => void (listeners[ev] = (listeners[ev] ?? []).filter((x) => x !== f));
  const accounts = () => (account.lost ? [] : [address()]);
  const needAccount = () => {
    if (account.lost) throw new Error('This site is not connected to the wallet');
  };

  const connect = () => {
    if (opts.failConnect) throw rejection();
  };
  const switchNetwork = (name: string) => {
    const target = fromWalletNet(name);
    if (!opts.allowNetworkSwitch) throw new Error('Network switching is disabled in this wallet');
    if (target !== 'testnet-10' && target !== 'mainnet') throw new Error('Unsupported network: ' + name);
    account.network = target;
    emit('networkChanged', toWalletNet(target));
    return toWalletNet(target);
  };

  // --- signing core: sign the requested inputs of a safe-JSON tx with the official SDK
  const corrupt = (sigScriptHex: string) => {
    // flip one bit inside the 64-byte signature (byte 1 is the push opcode 0x41): a well-formed but invalid signature
    const b = sigScriptHex.slice(0, 2 + 2 * 20) + (parseInt(sigScriptHex.slice(42, 44), 16) ^ 0x01).toString(16).padStart(2, '0') + sigScriptHex.slice(44);
    return b;
  };
  const isP2sh = (tx: any, i: number) => String(tx.inputs?.[i]?.utxo?.scriptPublicKey ?? '').startsWith('0000aa20');

  /**
   * @param shape decides what goes into the input's signatureScript, given `push(sig65)` (66 bytes): return null to leave it unsigned.
   */
  const signInputs = async (call: any, txJson: string, indexes: number[], shape: (sigPush: string, index: number) => string | null) => {
    const k = await kaspa();
    const tx = k.Transaction.deserializeFromSafeJSON(txJson);
    const key = new k.PrivateKey(opts.secretKey);
    for (const i of indexes) {
      if ((opts.omitInputs as number[]).includes(i)) {
        call.unsignedInputs.push(i);
        continue;
      }
      let sig: string = k.createInputSignature(tx, i, key, k.SighashType.All);
      if (opts.wrongSignature) sig = corrupt(sig);
      const script = shape(sig, i);
      if (script === null) {
        call.unsignedInputs.push(i);
        continue;
      }
      tx.inputs[i].signatureScript = script;
      call.signedInputs.push(i);
    }
    return tx.serializeToSafeJSON() as string;
  };

  const newCall = (method: string, txJson: string, inputs: number[], sighashType: any, scripts: any, network: string | null) => {
    const call: any = {
      wallet: opts.wallet,
      method,
      at: Date.now(),
      tx: JSON.parse(txJson),
      txJson,
      inputs,
      sighashType,
      scripts: scripts ? JSON.parse(JSON.stringify(scripts)) : null,
      network,
      status: 'pending',
      signedInputs: [],
      unsignedInputs: [],
      error: null,
      response: null,
    };
    calls.push(call);
    return call;
  };

  /** Common request lifecycle: log, popup delay, approve / reject, sign, settle the log entry. */
  const run = async (call: any, work: () => any) => {
    try {
      if (opts.delayMs > 0) await sleep(opts.delayMs);
      if (!opts.approve) throw rejection();
      const response = await work();
      call.response = response;
      call.status = 'signed';
      return response;
    } catch (e: any) {
      call.status = e && e.code === 4001 ? 'rejected' : 'error';
      call.error = String((e && e.message) || e);
      throw e;
    }
  };

  // --- the wallet's own node (KasWare 0.11.5 `verifyPsktInputsOnActiveNetwork`): every input asked to be signed must be held there, unspent
  const walletNode = (): string => {
    const base = String(opts.nodeUrl || w.__KOB_CONFIG__?.nodeUrl || '');
    return /^https?:\/\//.test(base) ? base.replace(/\/+$/, '') : '';
  };
  const checkInputsOnNode = async (txJson: string, indexes: number[]) => {
    if (!opts.checkInputs || indexes.length === 0) return;
    const tx = JSON.parse(txJson);
    const outpoints = indexes.map((i) => ({ transactionId: String(tx.inputs?.[i]?.transactionId ?? ''), index: Number(tx.inputs?.[i]?.index ?? 0) }));
    const base = walletNode();
    if (!base) throw new Error('signPskt: Cannot verify inputs: the active network node is unreachable.');
    let entries: any[];
    try {
      const res = await fetch(base + '/mock/wallet-node/outpoints', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ outpoints, lag_ms: opts.walletNodeLagMs }) });
      entries = (await res.json()).entries;
      if (!Array.isArray(entries)) throw new Error('bad answer');
    } catch {
      throw new Error('signPskt: Input verification failed: the active network node did not respond.');
    }
    const bad = entries.map((e, k) => ({ e, k })).filter(({ e }) => e?.state !== 'unspent');
    if (bad.length) throw new Error('signPskt: ' + bad.map(({ e, k }) => `Input #${indexes[k]} (${e.transactionId}:${e.index}) is not an unspent output on this network.`).join(' '));
  };
  const pushTx = async (rawtx: string) => {
    const base = walletNode();
    if (!base) throw new Error('pushTx: the active network node is unreachable');
    const txid = String(JSON.parse(rawtx).id ?? '');
    if (opts.pushTxOutcome === 'error') {
      pushes.push({ at: Date.now(), txid, ok: false, error: 'RPC client is not connected' });
      throw new Error('RPC client is not connected');
    }
    if (opts.pushTxOutcome === 'lost') {
      pushes.push({ at: Date.now(), txid, ok: true, error: null });
      return txid;
    }
    const res = await fetch(base + '/mock/wallet-node/push', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ transaction: JSON.parse(rawtx) }) });
    const body = await res.json().catch(() => ({}));
    pushes.push({ at: Date.now(), txid: String(JSON.parse(rawtx).id ?? ''), ok: res.ok, error: res.ok ? null : String(body?.error ?? res.status) });
    if (!res.ok) throw new Error(String(body?.error ?? 'pushTx failed'));
    return body.transactionId;
  };

  const scriptFor = (scripts: any[] | null, i: number) => (scripts && !opts.dropScripts ? scripts.find((s: any) => Number(s.inputIndex) === i) : undefined);

  // ------------------------------------------------------------------ KasWare
  const kasware = {
    isKasWare: true,
    requestAccounts: async () => {
      connect();
      account.lost = false;
      return [address()];
    },
    getAccounts: async () => accounts(),
    getNetwork: async () => toWalletNet(account.network),
    switchNetwork: async (n: string) => switchNetwork(n),
    getPublicKey: async () => (needAccount(), publicKey()),
    getVersion: async () => opts.version,
    getBalance: async () => ({ total: 0, confirmed: 0, unconfirmed: 0 }),
    disconnect: async () => undefined,
    on,
    removeListener,
    signPskt: async (params: any) => {
      const txJson: string = params.txJsonString;
      const sign: any[] = params.options?.signInputs ?? [];
      const indexes = sign.map((s) => Number(s.index));
      const call = newCall('signPskt', txJson, indexes, sign[0]?.sighashType ?? null, null, null);
      // KasWare returns only push(sig65) for every signed input, P2SH or not: the dApp assembles the full script
      return run(call, async () => {
        await checkInputsOnNode(txJson, indexes);
        return signInputs(call, txJson, indexes, (sig) => sig);
      });
    },
    pushTx: async (rawtx: string) => pushTx(rawtx),
  };

  // ------------------------------------------------------------------ Kaspire
  const kaspireArgs = (sc: any, sigPush: string, redeem: string) => {
    const sig = sigPush.slice(2); // the 65 signature bytes
    if (sc.signatureScript?.mode === 'wrap-signature') return sigPush + pushHex(redeem);
    if (sc.signatureScript?.mode === 'ordered-args') {
      let out = '';
      for (const a of sc.signatureScript.args ?? []) {
        if (a.type === 'signature') out += pushHex((a.prefixHex ?? '') + sig);
        else if (a.type === 'i64') out += pushHex(i64Hex(a.value ?? a.hex ?? 0));
        else out += pushHex(String(a.hex ?? ''));
      }
      return out + pushHex(redeem);
    }
    return sigPush;
  };
  const kaspire = {
    isKaspire: true,
    version: opts.version,
    on,
    removeListener,
    request: async (req: any) => {
      const params = req.params ?? {};
      switch (req.method) {
        case 'requestAccounts':
          connect();
          account.lost = false;
          return [address()];
        case 'getAccounts':
          return accounts();
        case 'getNetwork':
          return toWalletNet(account.network);
        case 'switchNetwork':
          return switchNetwork(params.network);
        case 'getPublicKey':
          needAccount();
          return publicKey();
        case 'signPskt': {
          const txJson: string = params.psktTransactionJson;
          const sign: any[] = params.signInputs ?? [];
          const indexes = sign.map((s) => Number(s.index));
          const call = newCall('signPskt', txJson, indexes, sign[0]?.sighashType ?? null, params.scripts ?? null, null);
          return run(call, async () => ({
            psktTransactionJson: await checkInputsOnNode(txJson, indexes).then(() => signInputs(call, txJson, indexes, (sig, i) => {
              const sc = scriptFor(params.scripts ?? null, i);
              return sc ? kaspireArgs(sc, sig, sc.scriptHex) : sig;
            })),
          }));
        }
        default:
          throw new Error('Method not supported: ' + req.method);
      }
    },
  };

  // ------------------------------------------------------------------ Kastle
  const kastle = {
    on,
    removeListener,
    connect: async () => {
      connect();
      account.lost = false;
      return true;
    },
    getAccount: async () => (needAccount(), { address: address(), publicKey: '02' + opts.pubkey }),
    request: async (method: string, ...args: any[]) => {
      switch (method) {
        case 'kas:get_network':
          return toWalletNet(account.network);
        case 'kas:switch_network':
          return switchNetwork(args[0]);
        case 'kas:get_version':
          return opts.version;
        case 'kas:disconnect':
          return true;
        default:
          throw new Error('Method not supported: ' + method);
      }
    },
    signTx: async (network: string, txJson: string, scripts?: any[]) => {
      const parsed = JSON.parse(txJson);
      // Kastle has no `signInputs`: it signs what it recognises (its own P2PK inputs, and P2SH inputs named in `scripts`)
      const indexes: number[] = parsed.inputs.map((_: any, i: number) => i);
      const call = newCall('signTx', txJson, indexes, 'All', scripts ?? null, network);
      return run(call, async () => {
        if (fromWalletNet(network) !== account.network) throw new Error('Network mismatch: wallet is on ' + account.network + ', dApp asked for ' + network);
        const signed = await signInputs(call, txJson, indexes, (sig, i) => {
          if (!isP2sh(parsed, i)) return sig;
          const sc = scriptFor(scripts ?? null, i);
          if (!sc) return null; // issue #353: silently unsigned, no error
          return sc.scriptHex ? sig + pushHex(sc.scriptHex) : sig;
        });
        return JSON.parse(signed);
      });
    },
  };

  const providers: Record<string, any> = { kasware, kaspire, kastle };

  // --- test handle
  w.__mockWallet = {
    wallet: opts.wallet,
    pubkey: opts.pubkey,
    get address() {
      return address();
    },
    get network() {
      return account.network;
    },
    options: opts,
    calls,
    pushes,
    /** last recorded sign request */
    lastCall: () => calls[calls.length - 1] ?? null,
    /** changes behaviour at runtime, e.g. `configure({approve: false})` */
    configure: (patch: Record<string, any>) => Object.assign(opts, patch),
    clearCalls: () => void ((calls.length = 0), (pushes.length = 0)),
    /** fires a provider event (KasWare: accountsChanged / networkChanged) */
    emit,
    setNetwork: (n: string) => {
      account.network = n;
      emit('networkChanged', toWalletNet(n));
    },
    /** the user picks another account in the wallet: another key (and its per-network addresses); emits accountsChanged */
    setAccount: (a: { secretKey: string; pubkey: string; addresses: Record<string, string> }) => {
      opts.secretKey = a.secretKey;
      opts.pubkey = a.pubkey;
      opts.addresses = a.addresses;
      account.lost = false;
      emit('accountsChanged', [address()]);
      emit('kas:account_changed', address());
    },
    /** the wallet locks / the site loses its connection: no account is exposed, accountsChanged([]) */
    lose: () => {
      account.lost = true;
      emit('accountsChanged', []);
      emit('kas:account_changed', null);
    },
    /** number of registered provider listeners (leak checks) */
    listenerCount: (ev?: string) => (ev ? (listeners[ev] ?? []).length : Object.values(listeners).reduce((n, l) => n + l.length, 0)),
  };

  const inject = () => {
    w[opts.wallet] = providers[opts.wallet];
  };
  if (opts.injectDelayMs > 0) setTimeout(inject, opts.injectDelayMs);
  else inject();
}

/** The init-script source Playwright injects (`page.addInitScript({content})`): the function above, called with its config. */
export function mockWalletInitScript(config: MockWalletConfig): string {
  return `(${mockWalletPageScript.toString()})(${JSON.stringify(config)});`;
}
