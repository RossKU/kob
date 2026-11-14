// In-memory fakes of the three wallet extensions' `window.*` providers, for unit tests of the adapters and the signing pipeline. They
// SIGN for real (official SDK `createInputSignature` over the safe-JSON transaction, local secret key) and reproduce the RESPONSE SHAPES
// documented in tools/wallet-gate/RESULTS.md: KasWare's 66-byte `push(sig65)` for P2SH inputs and `kaspa_testnet_10` network name,
// Kaspire's `psktTransactionJson` with `ordered-args` / `wrap-signature` / bare-signature modes, Kastle's 33-byte key and its silently
// unsigned covenant inputs without `scripts` (issue #353). NOT used by the production app.
//
// The wallet's own node (`walletNode`): like KasWare 0.11.5, a fake given one refuses to sign an input that node does not hold, with KasWare's
// message, and `pushTx` hands it a transaction. KOB assumes every extension wallet checks this, so the Kaspire fake takes one too.
import type { Hex, TxJson } from '../kob/types';
import type { KaspaSdk } from '../data/kaspa-sdk';
import type { KaswareProvider } from '../wallet/kasware';
import type { KaspireProvider, KaspireScriptEntry } from '../wallet/kaspire';
import type { KastleProvider, KastleScript } from '../wallet/kastle';
import { bytesToHex, hexToBytes } from '../wallet/sigs';
import { pubkeyOf } from './local-signer';

export interface FakeBehavior {
  /** the user declines the popup (EIP-1193 style `{code: 4001}`) */
  rejectSign?: boolean;
  /** signs with this key instead of the account key (a wrong-key wallet) */
  signKey?: Hex;
  /** never answers (unanswered popup) */
  neverAnswer?: boolean;
  /** throws this instead of signing */
  failWith?: unknown;
  /** answer with the signed tx as an object instead of a JSON string */
  respondAsObject?: boolean;
  /** signs only these input indices, whatever was asked */
  onlyInputs?: number[];
}

/** What a fake wallet sees of the network: its OWN node (not necessarily KOB's). */
export interface FakeWalletNode {
  hasUtxo(txid: string, index: number): boolean;
  /** `pushTx`: the wallet's node takes a transaction (resolves its id; rejects like a node) */
  push?(tx: TxJson): Promise<string>;
}

/**
 * The wallet's view of `node` (a MockNode KOB's flow also talks to) lagging behind it: an output of a transaction `node` accepted after this
 * view was made is not seen until it is pushed to the wallet (`pushTx`) or `reveal`ed (it reached the wallet's node by relay).
 */
export function laggingWalletNode(node: { hasUtxo(txid: string, index: number): boolean; submissions: readonly TxJson[] }): FakeWalletNode & { reveal(txid?: string): void; pushed: string[] } {
  const before = node.submissions.length;
  const seen = new Set<string>();
  const pushed: string[] = [];
  const fresh = (txid: string) => node.submissions.slice(before).some((t) => t.id === txid);
  return {
    pushed,
    hasUtxo: (txid, index) => node.hasUtxo(txid, index) && (!fresh(txid) || seen.has(txid)),
    async push(tx) {
      pushed.push(tx.id);
      if (!node.submissions.some((t) => t.id === tx.id)) throw new Error(`Rejected transaction ${tx.id}: transaction ${tx.id} is an orphan where orphan is disallowed`);
      seen.add(tx.id);
      return tx.id;
    },
    reveal(txid) {
      for (const t of node.submissions.slice(before)) if (txid === undefined || t.id === txid) seen.add(t.id);
    },
  };
}

/** KasWare 0.11.5's refusal (`verifyPsktInputsOnActiveNetwork`) of the first requested input its node does not hold; null when all are held. */
function unknownInputError(node: FakeWalletNode | undefined, tx: SafeTx, indexes: number[]): Error | null {
  if (!node) return null;
  for (const i of indexes) {
    const input = tx.inputs[i] as SafeInput & { transactionId?: string; index?: number };
    const txid = String(input?.transactionId ?? '');
    const index = Number(input?.index ?? 0);
    if (!node.hasUtxo(txid, index)) return new Error(`signPskt: Input #${i} (${txid}:${index}) is not an unspent output on this network.`);
  }
  return null;
}

export interface FakeWalletOptions {
  sdk: KaspaSdk;
  /** account secret key (hex) */
  sk: Hex;
  address?: string;
  version?: string;
  /** the wallet's own name of its current network */
  network?: string;
  behavior?: FakeBehavior;
  /** network names `switchNetwork` accepts (default: the wallet-native and the plain SDK spelling) */
  switchAccepts?: string[];
  /** the wallet's own node: given, every input asked to be signed must be held there (KasWare 0.11.5), and `pushTx` feeds it */
  walletNode?: FakeWalletNode;
}

export interface CallLog { method: string; args: unknown[] }

/** What a test can do to a fake wallet after connect: switch the account, the network, or make the wallet lose its account. */
export interface FakeControls {
  /** fires a provider event to every registered listener */
  emit(event: string, ...args: unknown[]): void;
  /** number of listeners registered for an event (leak checks) */
  listenerCount(event?: string): number;
  /** the wallet switches to another account (other key; optional address) and emits `accountsChanged` */
  switchAccount(sk: Hex, address?: string): void;
  /** the wallet moves to another network (its OWN spelling for KasWare) and emits `networkChanged` */
  moveToNetwork(network: string): void;
  /** the site loses its connection / the wallet locks: no account is exposed any more, `accountsChanged([])` */
  lose(): void;
  /** false: the provider has NO event emitter (`on` / `removeListener` are removed): the adapter must poll */
  removeEmitter(): void;
}

function emitterOf() {
  const listeners = new Map<string, Set<(...a: any[]) => void>>();
  return {
    on(event: string, h: (...a: any[]) => void) {
      if (!listeners.has(event)) listeners.set(event, new Set());
      listeners.get(event)!.add(h);
      return this;
    },
    removeListener(event: string, h: (...a: any[]) => void) {
      listeners.get(event)?.delete(h);
      return this;
    },
    emit(event: string, ...args: unknown[]) {
      for (const h of [...(listeners.get(event) ?? [])]) h(...args);
    },
    listenerCount(event?: string) {
      return event === undefined ? [...listeners.values()].reduce((n, s) => n + s.size, 0) : (listeners.get(event)?.size ?? 0);
    },
  };
}

const REJECTION = { code: 4001, message: 'User rejected the request.' };

/** Canonical data push of arbitrary bytes. */
export function pushData(bytes: Uint8Array): string {
  const n = bytes.length;
  const body = bytesToHex(bytes);
  if (n === 0) return '00';
  if (n <= 0x4b) return n.toString(16).padStart(2, '0') + body;
  if (n <= 0xff) return '4c' + n.toString(16).padStart(2, '0') + body;
  if (n <= 0xffff) return '4d' + (n & 0xff).toString(16).padStart(2, '0') + (n >> 8).toString(16).padStart(2, '0') + body;
  return '4e' + [n & 0xff, (n >> 8) & 0xff, (n >> 16) & 0xff, (n >>> 24) & 0xff].map((b) => b.toString(16).padStart(2, '0')).join('') + body;
}

interface SafeInput { signatureScript: string; utxo?: { scriptPublicKey?: string } }
interface SafeTx { inputs: SafeInput[] }

/** `push(sig65)` of an input, signed with the official SDK. */
function sigPush(sdk: KaspaSdk, txJson: string, index: number, sk: Hex): string {
  const tx = sdk.Transaction.deserializeFromSafeJSON(txJson);
  return sdk.createInputSignature(tx, index, new sdk.PrivateKey(sk), sdk.SighashType.All);
}

const isP2sh = (input: SafeInput): boolean => /^0000aa20[0-9a-f]{64}87$/i.test(input.utxo?.scriptPublicKey ?? '');

const hang = <T>(): Promise<T> => new Promise<T>(() => undefined);

function checkBehavior(b: FakeBehavior): Promise<never> | null {
  if (b.rejectSign) return Promise.reject(REJECTION);
  if (b.neverAnswer) return hang<never>();
  if (b.failWith !== undefined) return Promise.reject(b.failWith);
  return null;
}

const out = (tx: SafeTx, asObject?: boolean): unknown => (asObject ? tx : JSON.stringify(tx));

// ------------------------------------------------------------------------------------------------ KasWare

export interface FakeKasware extends KaswareProvider, FakeControls {
  calls: CallLog[];
  behavior: FakeBehavior;
  network: string;
}

export function fakeKasware(o: FakeWalletOptions): FakeKasware {
  let sk = o.sk;
  let address: string | null = o.address ?? 'kaspatest:qfake-kasware';
  const em = emitterOf();
  const w: FakeKasware = {
    ...em,
    calls: [],
    behavior: o.behavior ?? {},
    network: o.network ?? 'kaspa_testnet_10',
    async requestAccounts() {
      w.calls.push({ method: 'requestAccounts', args: [] });
      return address ? [address] : [];
    },
    async getAccounts() {
      w.calls.push({ method: 'getAccounts', args: [] });
      return address ? [address] : [];
    },
    async getNetwork() {
      w.calls.push({ method: 'getNetwork', args: [] });
      return w.network;
    },
    async switchNetwork(n: string) {
      w.calls.push({ method: 'switchNetwork', args: [n] });
      if (!(o.switchAccepts ?? ['kaspa_testnet_10', 'testnet-10', 'kaspa_mainnet', 'mainnet']).includes(n)) throw new Error(`unknown network ${n}`);
      w.network = n === 'testnet-10' ? 'kaspa_testnet_10' : n === 'mainnet' ? 'kaspa_mainnet' : n;
      // like the real wallet, the account's address prefix follows the network
      if (address) address = address.replace(/^kaspa(test)?:/, w.network.includes('mainnet') ? 'kaspa:' : 'kaspatest:');
      return w.network;
    },
    async getPublicKey() {
      w.calls.push({ method: 'getPublicKey', args: [] });
      if (!address) throw new Error('not connected');
      return pubkeyOf(sk);
    },
    async getVersion() {
      return o.version ?? '0.11.5';
    },
    switchAccount(next, addr) {
      sk = next;
      address = addr ?? `kaspatest:qfake-kasware-${pubkeyOf(next).slice(0, 8)}`;
      em.emit('accountsChanged', [address]);
    },
    moveToNetwork(n) {
      w.network = n;
      em.emit('networkChanged', n);
    },
    lose() {
      address = null;
      em.emit('accountsChanged', []);
    },
    removeEmitter() {
      delete (w as Partial<FakeKasware>).on;
      delete (w as Partial<FakeKasware>).removeListener;
    },
    async pushTx(rawtx: string) {
      w.calls.push({ method: 'pushTx', args: [rawtx] });
      if (!o.walletNode?.push) throw new Error('pushTx: no node');
      return o.walletNode.push(JSON.parse(rawtx) as TxJson);
    },
    async signPskt(req) {
      w.calls.push({ method: 'signPskt', args: [req] });
      const stop = checkBehavior(w.behavior);
      if (stop) return stop;
      const tx = JSON.parse(req.txJsonString) as SafeTx;
      const unknown = unknownInputError(o.walletNode, tx, req.options.signInputs.map((s) => s.index));
      if (unknown) throw unknown;
      const key = w.behavior.signKey ?? sk;
      for (const si of req.options.signInputs) {
        if (w.behavior.onlyInputs && !w.behavior.onlyInputs.includes(si.index)) continue;
        // KasWare: P2PK inputs and covenant inputs alike get exactly `push(sig65)`
        tx.inputs[si.index]!.signatureScript = sigPush(o.sdk, req.txJsonString, si.index, key);
      }
      return out(tx, w.behavior.respondAsObject);
    },
  };
  return w;
}

// ------------------------------------------------------------------------------------------------ Kaspire

export interface FakeKaspire extends KaspireProvider, FakeControls {
  calls: CallLog[];
  behavior: FakeBehavior;
  network: string;
  /** the `signPskt` params of the last call */
  lastSignParams: () => Record<string, unknown> | null;
}

export function fakeKaspire(o: FakeWalletOptions): FakeKaspire {
  let sk = o.sk;
  let lost = false;
  let account = o.address;
  const em = emitterOf();
  const addr = () => (lost ? null : account ?? (w.network === 'testnet-10' ? 'kaspatest:qfake-kaspire' : 'kaspa:qfake-kaspire'));
  let last: Record<string, unknown> | null = null;
  const w: FakeKaspire = {
    ...em,
    calls: [],
    behavior: o.behavior ?? {},
    network: o.network ?? 'mainnet',
    version: o.version ?? '1.2.0',
    lastSignParams: () => last,
    async request({ method, params }) {
      w.calls.push({ method, args: [params] });
      switch (method) {
        case 'requestAccounts': {
          lost = false;
          const accounts = [addr()!];
          em.emit('accountsChanged', accounts); // the real provider echoes its own requestAccounts
          return accounts;
        }
        case 'getAccounts': {
          const a = addr();
          return a ? [a] : [];
        }
        case 'getNetwork':
          return w.network;
        case 'switchNetwork': {
          const n = (params as { network: string }).network;
          if (!(o.switchAccepts ?? ['testnet-10', 'mainnet']).includes(n)) throw new Error(`unknown network ${n}`);
          w.network = n;
          em.emit('networkChanged', n); // the real provider echoes its own switchNetwork
          return true;
        }
        case 'getPublicKey':
          if (lost) throw new Error('not connected');
          return pubkeyOf(sk);
        case 'signPskt': {
          const p = params as { psktTransactionJson: string; signInputs: { index: number; sighashType: number }[]; scripts?: KaspireScriptEntry[]; submitTransaction?: boolean };
          last = p as unknown as Record<string, unknown>;
          const stop = checkBehavior(w.behavior);
          if (stop) return stop;
          const tx = JSON.parse(p.psktTransactionJson) as SafeTx;
          const unknown = unknownInputError(o.walletNode, tx, p.signInputs.map((s) => s.index));
          if (unknown) throw unknown;
          const key = w.behavior.signKey ?? sk;
          for (const si of p.signInputs) {
            if (w.behavior.onlyInputs && !w.behavior.onlyInputs.includes(si.index)) continue;
            const push = sigPush(o.sdk, p.psktTransactionJson, si.index, key); // 0x41 || sig65
            const sig65 = hexToBytes(push).subarray(1);
            const sc = p.scripts?.find((s) => s.inputIndex === si.index);
            if (!sc) {
              tx.inputs[si.index]!.signatureScript = push; // no scripts entry: a bare signature
            } else if (sc.signatureScript.mode === 'ordered-args') {
              // args in order (signature with its prefix, or data), then the redeem script: what the wallet emits itself
              const parts = sc.signatureScript.args.map((a) => (a.type === 'signature' ? pushData(hexToBytes(a.prefixHex + bytesToHex(sig65))) : pushData(hexToBytes(a.hex))));
              tx.inputs[si.index]!.signatureScript = parts.join('') + pushData(hexToBytes(sc.scriptHex));
            } else {
              tx.inputs[si.index]!.signatureScript = push + pushData(hexToBytes(sc.scriptHex)); // <sig><redeem>, no tag
            }
          }
          return { psktTransactionJson: w.behavior.respondAsObject ? tx : JSON.stringify(tx) };
        }
        default:
          throw new Error(`unsupported method ${method}`);
      }
    },
    switchAccount(next, address) {
      sk = next;
      account = address ?? `${w.network === 'testnet-10' ? 'kaspatest' : 'kaspa'}:qfake-kaspire-${pubkeyOf(next).slice(0, 8)}`;
      lost = false;
      em.emit('accountsChanged', [account]);
    },
    moveToNetwork(n) {
      w.network = n;
      account = undefined; // the address prefix follows the network
      em.emit('networkChanged', n);
    },
    lose() {
      lost = true;
      em.emit('accountsChanged', []);
      em.emit('disconnect', { code: 4900, message: 'Kaspire disconnected this site.' });
    },
    removeEmitter() {
      delete (w as Partial<FakeKaspire>).on;
      delete (w as Partial<FakeKaspire>).removeListener;
    },
  };
  return w;
}

// ------------------------------------------------------------------------------------------------ Kastle

export interface FakeKastle extends KastleProvider, FakeControls {
  calls: CallLog[];
  behavior: FakeBehavior;
  network: string;
  /** `scripts` passed to the last `signTx` (undefined when none) */
  lastScripts: () => KastleScript[] | undefined;
}

export function fakeKastle(o: FakeWalletOptions): FakeKastle {
  let sk = o.sk;
  let address: string | null = o.address ?? 'kaspa:qfake-kastle';
  const em = emitterOf();
  let lastScripts: KastleScript[] | undefined;
  const w: FakeKastle = {
    ...em,
    calls: [],
    behavior: o.behavior ?? {},
    network: o.network ?? 'mainnet',
    lastScripts: () => lastScripts,
    async connect() {
      w.calls.push({ method: 'connect', args: [] });
      return true;
    },
    async getAccount() {
      if (!address) throw new Error('Kastle: this site is not connected');
      return { address, publicKey: '02' + pubkeyOf(sk) }; // compressed, as Kastle returns it
    },
    switchAccount(next, addr) {
      sk = next;
      address = addr ?? `kaspa:qfake-kastle-${pubkeyOf(next).slice(0, 8)}`;
      em.emit('accountsChanged', [address]);
      em.emit('kas:account_changed', address);
    },
    moveToNetwork(n) {
      w.network = n;
      em.emit('networkChanged', n);
      em.emit('kas:network_changed', n);
    },
    lose() {
      address = null;
      em.emit('accountsChanged', []);
      em.emit('kas:account_changed', null);
    },
    removeEmitter() {
      delete (w as Partial<FakeKastle>).on;
      delete (w as Partial<FakeKastle>).removeListener;
    },
    async request(method: string, params?: unknown) {
      w.calls.push({ method, args: [params] });
      if (method === 'kas:get_network') return w.network;
      if (method === 'kas:switch_network') {
        if (!(o.switchAccepts ?? ['testnet-10', 'mainnet']).includes(params as string)) throw new Error('unknown network');
        w.network = params as string;
        return true;
      }
      if (method === 'kas:get_version') return o.version ?? '2.60.1';
      throw new Error(`unsupported method ${method}`);
    },
    async signTx(network: string, txJson: string, scripts?: KastleScript[]) {
      w.calls.push({ method: 'signTx', args: [network, scripts] });
      lastScripts = scripts;
      const stop = checkBehavior(w.behavior);
      if (stop) return stop;
      const tx = JSON.parse(txJson) as SafeTx;
      const key = w.behavior.signKey ?? sk;
      tx.inputs.forEach((input, i) => {
        if (w.behavior.onlyInputs && !w.behavior.onlyInputs.includes(i)) return;
        const listed = scripts?.some((s) => s.inputIndex === i);
        // issue #353: a P2SH input without a `scripts` entry is left unsigned and NO error is reported
        if (isP2sh(input) && !listed) return;
        input.signatureScript = sigPush(o.sdk, txJson, i, key); // empty-script variant: just push(sig65)
      });
      return out(tx, w.behavior.respondAsObject);
    },
  };
  return w;
}
