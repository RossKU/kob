// Node access contract. Two implementations:
//   * `KaspaRpcNode` (data/node-rpc.ts): the official kaspa-wasm v2.1.0 SDK `RpcClient` over wRPC (`ws://...`, a testnet-10 node of your own,
//     `ws://127.0.0.1:18210`, or the SDK's public `Resolver` when the node URL is empty);
//   * `HttpMockNode` (data/node-http.ts): JSON over HTTP against the mock server (mock/server.mjs) for offline tests
//     (selected when `nodeUrl` starts with http:// or https://).
// Submission always goes through node RPC (never REST: REST drops the per-input compute budget).
import type { FeeEstimate } from '../kob/fee-policy';
import type { Hex, TxJson } from '../kob/types';

export interface NodeUtxo {
  /** address the UTXO was found under (P2PK address of a key, or the P2SH address of a covenant script) */
  address: string;
  transactionId: Hex;
  index: number;
  /** sompi, decimal string */
  amount: string;
  /** kaspa string form: `version (u16 BE hex) + script hex` (same as kob-wasm `scriptPublicKey`) */
  scriptPublicKey: string;
  blockDaaScore: string;
  isCoinbase: boolean;
  covenantId: Hex | null;
}

export interface NodeInfo {
  network: string;
  virtualDaaScore: string;
  serverVersion?: string;
  /** measured DAA advance per second over the last minutes (milli-DAA/s), when the implementation can measure it */
  daaRateMilli?: number | null;
}

export interface NodeApi {
  readonly kind: 'rpc' | 'http-mock';
  connect(): Promise<NodeInfo>;
  disconnect(): Promise<void>;
  /** virtual DAA score and the UTC clock read together (for expiry / day orders, `matcher.md` 10.10) */
  getClock(): Promise<{ daa: bigint; unixSeconds: bigint; rateMilli: number | null }>;
  getUtxosByAddresses(addresses: string[]): Promise<NodeUtxo[]>;
  /** submits a fully signed tx (safe JSON); resolves with the accepted tx id */
  submitTransaction(tx: TxJson): Promise<string>;
  /**
   * The node's fee estimate (`getFeeEstimate`: sompi per gram for the priority / normal / low buckets), for the fee policy (kob/fee-policy.ts).
   * NEVER rejects: a node or transport that cannot answer (method missing, malformed answer, connection trouble) gives null and the policy pays the floor.
   * Optional: an implementation without it is treated as null.
   */
  getFeeEstimate?(): Promise<FeeEstimate | null>;
}
