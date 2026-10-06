// Loader and typed subset of the OFFICIAL kaspa-wasm v2.1.0 SDK (vendored in web/vendor/kaspa-web, ESM, browser), plus the
// small address helpers the app needs. The rest of the code depends on the `KaspaSdk` INTERFACE, never on the concrete module, so
// node tests (vendor/kaspa-node via kaspa-sdk.node.ts), the browser and in-memory fakes are interchangeable.
//
// The SDK is used for exactly three things: wRPC (node access), addresses, and (tests / mock wallets only) sighash signing.
// Transaction layout, sighash and fees come from kob-wasm, never from here.
import type { Hex } from '../kob/types';

// ------------------------------------------------------------------------------------------------ typed subset

/** `ScriptPublicKey` of the SDK: `script` is hex. */
export interface SdkScriptPublicKey {
  readonly version: number;
  readonly script: string;
}

export interface SdkAddress {
  readonly prefix: string;
  readonly payload: string;
  /** 'PubKey' | 'PubKeyECDSA' | 'ScriptHash' */
  readonly version: string;
  toString(): string;
}

export interface SdkPrivateKey {
  toString(): string;
}

export interface SdkTransactionInput {
  signatureScript: string;
}

/** wasm `Transaction`. Only what the tests and the mock wallets touch. */
export interface SdkTransaction {
  readonly inputs: SdkTransactionInput[];
  serializeToSafeJSON(): string;
}

export interface SdkUtxoEntryReference {
  readonly address?: SdkAddress;
  readonly outpoint: { readonly transactionId: string; readonly index: number };
  readonly amount: bigint;
  readonly scriptPublicKey: SdkScriptPublicKey;
  readonly blockDaaScore: bigint;
  readonly isCoinbase: boolean;
  /** the inner `UtxoEntry`, which is where 2.1.0 exposes the covenant id (a `Hash`, `undefined` for plain UTXOs) */
  readonly entry?: { readonly covenantId?: { toString(): string } | string | null };
  readonly covenantId?: { toString(): string } | string | null;
}

export interface SdkRpcConfig {
  url?: string;
  resolver?: unknown;
  encoding?: number;
  networkId?: string;
}

export interface SdkConnectOptions {
  blockAsyncConnect?: boolean;
  timeoutDuration?: number;
  retryInterval?: number;
  strategy?: string;
  url?: string;
}

export interface SdkBlockDagInfo {
  network: string;
  virtualDaaScore: bigint;
}

export interface SdkRpcClient {
  readonly isConnected: boolean;
  connect(options?: SdkConnectOptions): Promise<void>;
  disconnect(): Promise<void>;
  addEventListener(event: 'connect' | 'disconnect', callback: () => void): void;
  removeEventListener?(event: 'connect' | 'disconnect', callback?: () => void): void;
  getBlockDagInfo(): Promise<SdkBlockDagInfo>;
  getServerInfo(): Promise<{ serverVersion: string; networkId?: string; isSynced?: boolean }>;
  getUtxosByAddresses(request: { addresses: string[] }): Promise<{ entries: SdkUtxoEntryReference[] }>;
  submitTransaction(request: { transaction: SdkTransaction; allowOrphan?: boolean }): Promise<{ transactionId: string }>;
  /** `getFeeEstimate({})`: `{ estimate: { priorityBucket, normalBuckets, lowBuckets } }` (feerates in sompi per gram); parsed by kob/fee-policy.ts. Absent on older SDKs. */
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
  /** hex of `push(sig65)` (0x41 || sig64 || sighashByte) */
  createInputSignature(tx: SdkTransaction, inputIndex: number, key: SdkPrivateKey, sighash?: number): string;
  /** NOTE: the SDK's own enum (All = 0), not the wallet-facing SIGHASH_ALL = 1 of `SignRequest.sighashType` */
  SighashType: { All: number };
  Encoding: { Borsh: number; SerdeJson: number };
  Resolver: new () => unknown;
  RpcClient: new (config: SdkRpcConfig) => SdkRpcClient;
}

// ------------------------------------------------------------------------------------------------ networks

export const MAINNET = 'mainnet';
export const TESTNET_10 = 'testnet-10';

/**
 * Canonical network name. Wallets and nodes disagree on spelling: KasWare reports `kaspa_testnet_10`, nodes report `kaspa-mainnet` /
 * `testnet-10`, the SDK takes `mainnet` / `testnet-10`. Unknown names come back lower-cased and unchanged (callers compare).
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

/** Address prefix of a canonical network name. */
export function addressPrefix(network: string): string {
  switch (normalizeNetwork(network)) {
    case MAINNET: return 'kaspa';
    case 'devnet': return 'kaspadev';
    case 'simnet': return 'kaspasim';
    default: return 'kaspatest';
  }
}

// ------------------------------------------------------------------------------------------------ address helpers (pure given an sdk)

const isHex = (s: string, len?: number) => /^[0-9a-f]*$/i.test(s) && s.length % 2 === 0 && (len === undefined || s.length === len);

/** P2PK (Schnorr, x-only key) address of a 32-byte public key: script `OP_DATA32 <pk> OP_CHECKSIG`. */
export function pubkeyToAddress(sdk: KaspaSdk, pubkeyHex: Hex, network: string): string {
  if (!isHex(pubkeyHex, 64)) throw new Error(`pubkeyToAddress: expected a 32-byte x-only key (64 hex chars), got ${pubkeyHex.length} chars`);
  const spk = new sdk.ScriptPublicKey(0, '20' + pubkeyHex.toLowerCase() + 'ac');
  const addr = sdk.addressFromScriptPublicKey(spk, normalizeNetwork(network));
  if (!addr) throw new Error('pubkeyToAddress: the SDK could not derive an address');
  return addr.toString();
}

/** Address of a kaspa-string-form script public key (`version u16 BE hex` + `script hex`, as kob-wasm returns it). */
export function spkStringToAddress(sdk: KaspaSdk, spk: string, network: string): string {
  const { version, script } = splitSpkString(spk);
  const addr = sdk.addressFromScriptPublicKey(new sdk.ScriptPublicKey(version, script), normalizeNetwork(network));
  if (!addr) throw new Error('spkStringToAddress: not a standard script (no address form)');
  return addr.toString();
}

/** Inverse of `spkStringToAddress`: the script public key string (`version + script`) that pays to `address`. */
export function addressToSpkString(sdk: KaspaSdk, address: string): string {
  const s = sdk.payToAddressScript(address);
  return s.version.toString(16).padStart(4, '0') + s.script;
}

export function splitSpkString(spk: string): { version: number; script: string } {
  if (spk.length < 4 || !isHex(spk)) throw new Error(`invalid script public key string: ${spk.slice(0, 24)}`);
  return { version: parseInt(spk.slice(0, 4), 16), script: spk.slice(4).toLowerCase() };
}

export const joinSpkString = (version: number, script: string): string => version.toString(16).padStart(4, '0') + script;

// ------------------------------------------------------------------------------------------------ browser loader

let cached: Promise<KaspaSdk> | null = null;

export interface LoadKaspaSdkOptions {
  /** tests only (`features.test`): publish the namespace as `window.__kobKaspa` so the mock wallets can sign with the official SDK */
  exposeForTests?: boolean;
}

/**
 * Lazily imports the vendored ESM SDK (a ~10 MB wasm: only when a node/address feature is first needed), initialises it once and
 * returns the namespace. The wasm binary is emitted as an asset by Vite (`?url`).
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
