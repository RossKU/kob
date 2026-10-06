// Thin helpers over the OFFICIAL kaspa-wasm v2.1.0 SDK (rusty-kaspa release zip; `npm run fetch-sdk` in
// tools/wallet-gate, vendored to tools/wallet-gate/vendor/kaspa-node). The npm package `kaspa-wasm` is stale
// (0.13.0, no tx v1 / covenants): never depend on it.
//
// The SDK is NEVER imported at module top level: callers pass a `KaspaSdk` (any object with the members below), or
// load the vendored build with `loadKaspaNodeSdk(dir)` at run time.

import { createRequire } from 'node:module';
import { join } from 'node:path';
import { KobX402Error } from './errors.ts';
import type { ChainContextProvider } from './client.ts';
import type { SwapQuote, TokenUtxoJson } from './wasm.ts';
import type { NetworkId, PayerUtxo } from './types.ts';

/** The subset of the kaspa-wasm RpcClient the SDK uses. */
export interface KaspaRpc {
  connect(options?: unknown): Promise<unknown>;
  disconnect(): Promise<unknown>;
  getUtxosByAddresses(request: { addresses: string[] }): Promise<{ entries: unknown[] }>;
  getServerInfo?(): Promise<{ virtualDaaScore?: bigint | string | number }>;
  getBlockDagInfo?(): Promise<{ virtualDaaScore?: bigint | string | number }>;
  submitTransaction?(request: { transaction: unknown; allowOrphan: boolean }): Promise<{ transactionId: string }>;
}

/** The subset of the kaspa-wasm module the SDK uses. */
export interface KaspaSdk {
  RpcClient: new (options: { url?: string; networkId: string; encoding?: unknown; resolver?: unknown }) => KaspaRpc;
  Encoding: { SerdeJson: unknown };
  Resolver?: new () => unknown;
  PrivateKey: new (hex: string) => {
    toAddress(network: string): { toString(): string };
    toPublicKey(): { toXOnlyPublicKey(): { toString(): string } };
  };
  payToAddressScript(address: string): { version: number; script: string };
  Transaction?: { deserializeFromSafeJSON(json: string): unknown };
}

/** Loads the vendored nodejs build of the official SDK (CommonJS) at call time. */
export function loadKaspaNodeSdk(vendorDir: string): KaspaSdk {
  const require = createRequire(import.meta.url);
  try {
    return require(join(vendorDir, 'kaspa.js')) as KaspaSdk;
  } catch (e) {
    throw new KobX402Error('unsupported', `cannot load kaspa-wasm from ${vendorDir} (run "npm run fetch-sdk" in tools/wallet-gate)`, { cause: e });
  }
}

/** `kaspa:testnet-10` -> `testnet-10` (RpcClient networkId). */
export function rpcNetworkId(network: NetworkId): string {
  return network === 'kaspa:mainnet' ? 'mainnet' : 'testnet-10';
}

/** Address prefix name for `PrivateKey.toAddress`. */
export function addressNetwork(network: NetworkId): string {
  return network === 'kaspa:mainnet' ? 'mainnet' : 'testnet';
}

export interface ConnectOptions {
  /** wRPC JSON url, e.g. `ws://127.0.0.1:18210`. Empty: the public Resolver. */
  url?: string;
  network: NetworkId;
  timeoutMs?: number;
}

export async function connectRpc(sdk: KaspaSdk, opts: ConnectOptions): Promise<KaspaRpc> {
  const networkId = rpcNetworkId(opts.network);
  let rpcOpts: ConstructorParameters<KaspaSdk['RpcClient']>[0];
  if (opts.url) {
    rpcOpts = { url: opts.url, networkId, encoding: sdk.Encoding.SerdeJson };
  } else {
    if (!sdk.Resolver) throw new KobX402Error('unsupported', 'this kaspa-wasm build has no Resolver; pass a node url');
    rpcOpts = { resolver: new sdk.Resolver(), networkId };
  }
  const rpc = new sdk.RpcClient(rpcOpts);
  await rpc.connect({ timeoutDuration: opts.timeoutMs ?? 15000, blockAsyncConnect: true });
  return rpc;
}

/** Serialized spk of an address: `version u16 BE hex || script hex`. */
export function addressToSpkHex(sdk: KaspaSdk, address: string): string {
  const spk = sdk.payToAddressScript(address);
  return spk.version.toString(16).padStart(4, '0') + spk.script;
}

/** Address and x-only public key of a dev secret key. */
export function deriveAddress(sdk: KaspaSdk, privateKeyHex: string, network: NetworkId): { address: string; xOnlyPublicKey: string } {
  const key = new sdk.PrivateKey(privateKeyHex);
  return {
    address: key.toAddress(addressNetwork(network)).toString(),
    xOnlyPublicKey: key.toPublicKey().toXOnlyPublicKey().toString(),
  };
}

function str(v: unknown): string {
  if (v === undefined || v === null) return '';
  return typeof v === 'object' && 'toString' in v ? (v as { toString(): string }).toString() : String(v);
}

/** Normalizes one `UtxoEntryReference` (or its plain JSON form) into a `PayerUtxo`. */
export function normalizeUtxo(entry: unknown): PayerUtxo {
  const e = (entry as { entry?: unknown }).entry ?? entry;
  const u = e as {
    outpoint: { transactionId: unknown; index: number };
    amount: bigint | string | number;
    scriptPublicKey: { version?: number; script?: string } | string;
    blockDaaScore?: bigint | string | number;
    isCoinbase?: boolean;
    covenantId?: unknown;
    address?: unknown;
  };
  const spk = u.scriptPublicKey;
  const spkHex = typeof spk === 'string' ? spk : (spk.version ?? 0).toString(16).padStart(4, '0') + (spk.script ?? '');
  const out: PayerUtxo = {
    txid: str(u.outpoint.transactionId),
    index: Number(u.outpoint.index),
    amount: str(u.amount),
    scriptPublicKey: spkHex,
    blockDaaScore: str(u.blockDaaScore ?? 0),
    isCoinbase: Boolean(u.isCoinbase),
  };
  const cov = str(u.covenantId);
  if (cov) out.covenantId = cov;
  const addr = str(u.address);
  if (addr) out.address = addr;
  return out;
}

/** Lists the unspent outputs of `address` over the node's wRPC. */
export async function listUtxos(rpc: KaspaRpc, address: string): Promise<PayerUtxo[]> {
  const r = await rpc.getUtxosByAddresses({ addresses: [address] });
  return r.entries.map(normalizeUtxo);
}

export async function virtualDaaScore(rpc: KaspaRpc): Promise<string | undefined> {
  const info = rpc.getServerInfo ? await rpc.getServerInfo() : rpc.getBlockDagInfo ? await rpc.getBlockDagInfo() : undefined;
  return info?.virtualDaaScore === undefined ? undefined : String(info.virtualDaaScore);
}

/**
 * A chain context over a connected RpcClient: KAS UTXOs by address, the virtual DAA score. Token UTXOs and KOB
 * orders are owned through covenants (P2SH), so they cannot be found by the payer's address: pass `tokens` and
 * `orders` sources (an indexer) for the kcc20 and swap-and-pay profiles.
 */
export function rpcContextProvider(
  rpc: KaspaRpc,
  extra: {
    tokens?: (q: { payerAddress: string; asset: string }) => Promise<TokenUtxoJson[]>;
    quote?: (q: { payAsset: string; asset: string; amount: string }) => Promise<SwapQuote>;
  } = {},
): ChainContextProvider {
  return {
    async load(q) {
      const ctx: Awaited<ReturnType<ChainContextProvider['load']>> = { utxos: await listUtxos(rpc, q.payerAddress) };
      const daa = await virtualDaaScore(rpc).catch(() => undefined);
      if (daa !== undefined) ctx.virtualDaaScore = daa;
      const tokenAsset = q.kind === 'kcc20' ? q.asset : q.payAsset;
      if (q.kind !== 'native' && tokenAsset && tokenAsset !== 'KAS') {
        if (!extra.tokens) throw new KobX402Error('unsupported', 'kcc20 / swap payments need a token UTXO source (indexer): pass `tokens` to rpcContextProvider');
        ctx.tokenUtxos = await extra.tokens({ payerAddress: q.payerAddress, asset: tokenAsset });
      }
      if (q.kind === 'swap') {
        if (!extra.quote) throw new KobX402Error('unsupported', 'swap-and-pay needs an order source (indexer): pass `quote` to rpcContextProvider');
        ctx.quote = await extra.quote({ payAsset: q.payAsset ?? '', asset: q.asset, amount: q.amount });
      }
      return ctx;
    },
  };
}

/** Submits a signed safe-JSON transaction over RPC (used by `KobX402Client.revoke`). */
export function rpcSubmitter(sdk: KaspaSdk, rpc: KaspaRpc): (safeJsonTx: string) => Promise<string> {
  return async (safeJsonTx) => {
    if (!sdk.Transaction || !rpc.submitTransaction) throw new KobX402Error('unsupported', 'this kaspa-wasm build cannot submit transactions');
    const tx = sdk.Transaction.deserializeFromSafeJSON(safeJsonTx);
    const r = await rpc.submitTransaction({ transaction: tx, allowOrphan: false });
    return r.transactionId;
  };
}
