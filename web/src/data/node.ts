// The node access contract. It has two implementations:
// - `KaspaRpcNode` (data/node-rpc.ts), the official kaspa-wasm v2.1.0 SDK `RpcClient` over wRPC.
//   That is a `ws://...` node, such as a testnet-10 node of your own at `ws://127.0.0.1:18210`, or
//   a public node the resolvers name when the node URL is empty (data/public-node.ts).
// - `HttpMockNode` (data/node-http.ts), JSON over HTTP against the mock server (mock/server.mjs)
//   for offline tests. It is selected when `nodeUrl` starts with http:// or https://.
// On a real node, submission always goes through node RPC, never REST, because REST drops the
// per-input compute budget.
import type { FeeEstimate } from '../kob/fee-policy';
import type { Hex, TxJson } from '../kob/types';
import type { StreamBlock } from './block-stream';

/** A UTXO as the node lists it. */
export interface NodeUtxo {
  /**
   * The address the UTXO was found under, the P2PK address of a key or the P2SH address of a
   * covenant script.
   */
  address: string;
  transactionId: Hex;
  index: number;
  /** Sompi, as a decimal string. */
  amount: string;
  /**
   * In kaspa string form, `version (u16 BE hex) + script hex`, the same as kob-wasm
   * `scriptPublicKey`.
   */
  scriptPublicKey: string;
  blockDaaScore: string;
  isCoinbase: boolean;
  covenantId: Hex | null;
}

/** What `NodeApi.connect` reports of the node. */
export interface NodeInfo {
  network: string;
  virtualDaaScore: string;
  serverVersion?: string;
  /**
   * The measured DAA advance in milli-DAA per second over a long window, when the implementation
   * can measure it (data/node-rpc.ts `nodeDaaRate`, `DaaRateEstimator`).
   */
  daaRateMilli?: number | null;
}

/** Everything the app asks of a Kaspa node. */
export interface NodeApi {
  readonly kind: 'rpc' | 'http-mock';
  connect(): Promise<NodeInfo>;
  disconnect(): Promise<void>;
  /**
   * The virtual DAA score and the UTC clock read together, for expiry and day orders
   * (`matcher.md` 10.10).
   */
  getClock(): Promise<{ daa: bigint; unixSeconds: bigint; rateMilli: number | null }>;
  getUtxosByAddresses(addresses: string[]): Promise<NodeUtxo[]>;
  /** Submits a fully signed transaction (safe JSON) and resolves with the accepted id. */
  submitTransaction(tx: TxJson): Promise<string>;
  /**
   * The node's fee estimate (`getFeeEstimate`), sompi per gram for the priority, normal and low
   * buckets, for the fee policy (kob/fee-policy.ts). It never rejects. A node or transport that
   * cannot answer gives null, and the policy pays the floor. That covers a missing method, a
   * malformed answer and connection trouble. An implementation without it is treated as null.
   */
  getFeeEstimate?(): Promise<FeeEstimate | null>;
  /**
   * The node's past median time in unix ms. A transaction with a time lock time is accepted once
   * that time is past, as for a creator lock's unlock. Null when the node does not say. It is
   * optional.
   */
  getPastMedianTime?(): Promise<bigint | null>;
  /**
   * Follows the node's new blocks for standalone mode (data/standalone.ts), by wRPC `block-added`
   * or, on the mock node, `GET /node/blocks`. `onBlock` gets every block from now on. `onGap` is
   * called when blocks may have been missed, because the link dropped and came back. It rejects
   * when the node cannot be asked. It is optional, and a node without it has no standalone mode.
   */
  subscribeBlocks?(onBlock: (block: StreamBlock) => void, onGap?: () => void): Promise<BlockSubscription>;
}

/** A running block subscription (`NodeApi.subscribeBlocks`). */
export interface BlockSubscription {
  close(): Promise<void>;
}
