// Loader and typed subset of the official kaspa-wasm v2.1.0 SDK, plus the small address helpers
// the app needs. The SDK is vendored in web/vendor/kaspa-web as a browser ESM build. The rest of
// the code depends on the `KaspaSdk` interface, never on the concrete module. So node tests
// (vendor/kaspa-node via kaspa-sdk.node.ts), the browser and in-memory fakes are interchangeable.
//
// The SDK is used for exactly three things:
// - wRPC, for node access,
// - addresses,
// - sighash signing, in tests and mock wallets only.
// Transaction layout, sighash and fees come from kob-wasm, never from here.
import type { Hex } from '../kob/types';

// --- Typed subset ---

/** `ScriptPublicKey` of the SDK: `script` is hex. */
export interface SdkScriptPublicKey {
  readonly version: number;
  readonly script: string;
}

/** `Address` of the SDK. */
export interface SdkAddress {
  readonly prefix: string;
  readonly payload: string;
  /** 'PubKey', 'PubKeyECDSA' or 'ScriptHash'. */
  readonly version: string;
  toString(): string;
}

/** `PrivateKey` of the SDK, for tests and mock wallets. */
export interface SdkPrivateKey {
  toString(): string;
}

/** An input of an SDK `Transaction`. */
export interface SdkTransactionInput {
  signatureScript: string;
}

/** wasm `Transaction`, with only what the tests and the mock wallets touch. */
export interface SdkTransaction {
  readonly inputs: SdkTransactionInput[];
  serializeToSafeJSON(): string;
}

/** `UtxoEntryReference` of the SDK, as `getUtxosByAddresses` returns it. */
export interface SdkUtxoEntryReference {
  readonly address?: SdkAddress;
  readonly outpoint: { readonly transactionId: string; readonly index: number };
  readonly amount: bigint;
  readonly scriptPublicKey: SdkScriptPublicKey;
  readonly blockDaaScore: bigint;
  readonly isCoinbase: boolean;
  /**
   * The inner `UtxoEntry`, which is where 2.1.0 exposes the covenant id. It is a `Hash`, and
   * `undefined` for plain UTXOs.
   */
  readonly entry?: { readonly covenantId?: { toString(): string } | string | null };
  readonly covenantId?: { toString(): string } | string | null;
}

/** The config of an SDK `RpcClient`. */
export interface SdkRpcConfig {
  url?: string;
  resolver?: unknown;
  encoding?: number;
  networkId?: string;
}

/** Options of `RpcClient.connect`. */
export interface SdkConnectOptions {
  blockAsyncConnect?: boolean;
  timeoutDuration?: number;
  retryInterval?: number;
  strategy?: string;
  url?: string;
}

/** What `getBlockDagInfo` gives. */
export interface SdkBlockDagInfo {
  network: string;
  virtualDaaScore: bigint;
  /** The past median time (unix ms) the node checks a lock time against. */
  pastMedianTime?: bigint;
  /**
   * The selected tip (`sink` in 2.1.0) and the pruning point, as hex block hashes. They are the
   * ends of the node's long DAA-rate window.
   */
  sink?: string;
  pruningPointHash?: string;
}

/**
 * `getBlock({ hash, includeTransactions: false })`: the header fields the DAA rate needs, with the
 * timestamp in ms.
 */
export interface SdkBlockHeaderInfo {
  block?: { header?: { timestamp?: bigint | number; daaScore?: bigint | number } };
}

/** The `RpcClient` calls the app makes. */
export interface SdkRpcClient {
  readonly isConnected: boolean;
  connect(options?: SdkConnectOptions): Promise<void>;
  disconnect(): Promise<void>;
  addEventListener(event: 'connect' | 'disconnect' | 'block-added', callback: (event?: unknown) => void): void;
  removeEventListener?(event: 'connect' | 'disconnect' | 'block-added', callback?: (event?: unknown) => void): void;
  /** `block-added` notifications for standalone mode (data/block-stream.ts). Test fakes lack it. */
  subscribeBlockAdded?(): Promise<void>;
  unsubscribeBlockAdded?(): Promise<void>;
  getBlockDagInfo(): Promise<SdkBlockDagInfo>;
  /**
   * A block by hash, headers only with `includeTransactions: false`. It is used for the
   * long-window DAA rate. Test fakes lack it.
   */
  getBlock?(request: { hash: string; includeTransactions: boolean }): Promise<SdkBlockHeaderInfo>;
  getServerInfo(): Promise<{ serverVersion: string; networkId?: string; isSynced?: boolean }>;
  getUtxosByAddresses(request: { addresses: string[] }): Promise<{ entries: SdkUtxoEntryReference[] }>;
  submitTransaction(request: { transaction: SdkTransaction; allowOrphan?: boolean }): Promise<{ transactionId: string }>;
  /**
   * `getFeeEstimate({})`: `{ estimate: { priorityBucket, normalBuckets, lowBuckets } }`, with
   * feerates in sompi per gram, parsed by kob/fee-policy.ts. Older SDKs lack it.
   */
  getFeeEstimate?(request?: Record<string, never>): Promise<unknown>;
}

/** The typed subset of the SDK namespace the app depends on. */
export interface KaspaSdk {
  Address: {
    new (address: string): SdkAddress;
    validate(address: string): boolean;
  };
  ScriptPublicKey: new (version: number, script: string) => SdkScriptPublicKey;
  addressFromScriptPublicKey(spk: SdkScriptPublicKey | string, network: string): SdkAddress | undefined;
  payToAddressScript(address: SdkAddress | string): SdkScriptPublicKey;
  Transaction: { deserializeFromSafeJSON(json: string): SdkTransaction };
  PrivateKey: new (hex: string) => SdkPrivateKey;
  /** The hex of `push(sig65)`, which is 0x41 || sig64 || sighashByte. */
  createInputSignature(tx: SdkTransaction, inputIndex: number, key: SdkPrivateKey, sighash?: number): string;
  /**
   * The SDK's own enum, with All = 0. It is not the wallet-facing SIGHASH_ALL = 1 of
   * `SignRequest.sighashType`.
   */
  SighashType: { All: number };
  Encoding: { Borsh: number; SerdeJson: number };
  Resolver: new () => unknown;
  RpcClient: new (config: SdkRpcConfig) => SdkRpcClient;
}

// --- Networks ---

export const MAINNET = 'mainnet';
export const TESTNET_10 = 'testnet-10';

/**
 * The canonical network name. Wallets and nodes disagree on spelling. KasWare reports
 * `kaspa_testnet_10`, nodes report `kaspa-mainnet` or `testnet-10`, and the SDK takes `mainnet`
 * or `testnet-10`. Unknown names come back lower-cased and otherwise unchanged, for callers to
 * compare.
 */
export function normalizeNetwork(raw: unknown): string {
  if (typeof raw !== 'string') return '';
  const s = raw.trim().toLowerCase();
  const compact = s.replace(/^kaspa[:_\-\s]?/, '').replace(/[_\s]+/g, '-');
  if (compact === 'mainnet' || compact === 'main') return MAINNET;
  const t = /^testnet-?(\d+)$/.exec(compact);
  if (t) return `testnet-${t[1]}`;
  if (compact === 'testnet') return 'testnet';
  if (compact === 'devnet' || compact === 'simnet') return compact;
  return s;
}

/** The address prefix of a network name. */
export function addressPrefix(network: string): string {
  switch (normalizeNetwork(network)) {
    case MAINNET: return 'kaspa';
    case 'devnet': return 'kaspadev';
    case 'simnet': return 'kaspasim';
    default: return 'kaspatest';
  }
}

// --- Address helpers, pure given an SDK ---

const isHex = (s: string, len?: number) => /^[0-9a-f]*$/i.test(s) && s.length % 2 === 0 && (len === undefined || s.length === len);

/**
 * The P2PK (Schnorr, x-only key) address of a 32-byte public key, with the script
 * `OP_DATA32 <pk> OP_CHECKSIG`.
 */
export function pubkeyToAddress(sdk: KaspaSdk, pubkeyHex: Hex, network: string): string {
  if (!isHex(pubkeyHex, 64)) throw new Error(`pubkeyToAddress: expected a 32-byte x-only key (64 hex chars), got ${pubkeyHex.length} chars`);
  const spk = new sdk.ScriptPublicKey(0, '20' + pubkeyHex.toLowerCase() + 'ac');
  const addr = sdk.addressFromScriptPublicKey(spk, normalizeNetwork(network));
  if (!addr) throw new Error('pubkeyToAddress: the SDK could not derive an address');
  return addr.toString();
}

/**
 * The address of a script public key in kaspa string form, `version u16 BE hex` + `script hex`,
 * as kob-wasm returns it.
 */
export function spkStringToAddress(sdk: KaspaSdk, spk: string, network: string): string {
  const { version, script } = splitSpkString(spk);
  const addr = sdk.addressFromScriptPublicKey(new sdk.ScriptPublicKey(version, script), normalizeNetwork(network));
  if (!addr) throw new Error('spkStringToAddress: not a standard script (no address form)');
  return addr.toString();
}

/**
 * The inverse of `spkStringToAddress`: the script public key string (`version + script`) that
 * pays to `address`.
 */
export function addressToSpkString(sdk: KaspaSdk, address: string): string {
  const s = sdk.payToAddressScript(address);
  return s.version.toString(16).padStart(4, '0') + s.script;
}

/** Splits a script public key string into its version and script hex. */
export function splitSpkString(spk: string): { version: number; script: string } {
  if (spk.length < 4 || !isHex(spk)) throw new Error(`invalid script public key string: ${spk.slice(0, 24)}`);
  return { version: parseInt(spk.slice(0, 4), 16), script: spk.slice(4).toLowerCase() };
}

/** Joins a version and script hex into a script public key string. */
export const joinSpkString = (version: number, script: string): string => version.toString(16).padStart(4, '0') + script;

// --- Browser loader ---

let cached: Promise<KaspaSdk> | null = null;

/** Options of `loadKaspaSdk`. */
export interface LoadKaspaSdkOptions {
  /**
   * For tests only (`features.test`). It publishes the namespace as `window.__kobKaspa`, so the
   * mock wallets can sign with the official SDK.
   */
  exposeForTests?: boolean;
}

/**
 * Imports the vendored ESM SDK lazily, initialises it once and returns the namespace. It is a
 * ~10 MB wasm, so it loads only when a node or address feature is first needed. Vite emits the
 * wasm binary as an asset (`?url`).
 */
export function loadKaspaSdk(opts: LoadKaspaSdkOptions = {}): Promise<KaspaSdk> {
  cached ??= (async () => {
    const mod = (await import('../../vendor/kaspa-web/kaspa.js')) as unknown as { default: (input?: unknown) => Promise<unknown> } & KaspaSdk;
    const wasmUrl = (await import('../../vendor/kaspa-web/kaspa_bg.wasm?url')).default as string;
    await mod.default({ module_or_path: wasmUrl });
    return mod as KaspaSdk;
  })();
  const p = cached;
  if (opts.exposeForTests) {
    void p.then((sdk) => {
      if (typeof window !== 'undefined') (window as unknown as { __kobKaspa?: KaspaSdk }).__kobKaspa = sdk;
    });
  }
  return p;
}
