// `NodeApi` over the official kaspa-wasm SDK `RpcClient` (wRPC, JSON encoding): a testnet-10 node of your own (e.g. `ws://127.0.0.1:18210`),
// any other wRPC node, or the SDK's public `Resolver` when no URL is configured. Submission ALWAYS goes through node RPC: the REST
// endpoint drops the per-input compute budget of tx v1, which the covenants need.
import { parseFeeEstimate, type FeeEstimate } from '../kob/fee-policy';
import type { Hex, TxJson } from '../kob/types';
import type { NodeApi, NodeInfo, NodeUtxo } from './node';
import { joinSpkString, normalizeNetwork, spkStringToAddress, type KaspaSdk, type SdkRpcClient, type SdkUtxoEntryReference } from './kaspa-sdk';
import { NodeError, describeNodeError, isConnectionError, rawErrorText } from './node-error';

// ------------------------------------------------------------------------------------------------ DAA rate

/**
 * Sliding history of (virtual DAA, wall-clock) samples -> measured DAA advance in milli-DAA per second.
 * `kob.dayOrder` needs the measured rate to turn "00:00 UTC" into a DAA score (matcher.md 10.10); the nominal 10 DAA/s is only a fallback.
 * `rateMilli()` is null until the history spans `minSpanMs` (a rate from a few hundred ms of samples is noise).
 */
export class DaaRateEstimator {
  private samples: { daa: bigint; tMs: number }[] = [];
  constructor(
    private readonly windowMs = 10 * 60_000,
    private readonly minSpanMs = 20_000,
    private readonly maxSamples = 64,
    private readonly minIntervalMs = 1_000,
  ) {}

  add(daa: bigint, tMs: number): void {
    const last = this.samples[this.samples.length - 1];
    if (last) {
      if (tMs - last.tMs < this.minIntervalMs) {
        // too close to the previous sample: keep the newer reading only if it is not older (DAA never runs backwards in a stable DAG)
        if (tMs >= last.tMs && daa >= last.daa) this.samples[this.samples.length - 1] = { daa, tMs };
        return;
      }
      if (daa < last.daa) this.samples = []; // reorg / node switch: the old history is meaningless
    }
    this.samples.push({ daa, tMs });
    const cutoff = tMs - this.windowMs;
    while (this.samples.length > this.maxSamples || (this.samples.length > 2 && this.samples[0]!.tMs < cutoff)) this.samples.shift();
  }

  get size(): number {
    return this.samples.length;
  }

  reset(): void {
    this.samples = [];
  }

  rateMilli(): number | null {
    if (this.samples.length < 2) return null;
    const a = this.samples[0]!;
    const b = this.samples[this.samples.length - 1]!;
    const spanMs = b.tMs - a.tMs;
    if (spanMs < this.minSpanMs || b.daa <= a.daa) return null;
    return Number(((b.daa - a.daa) * 1_000_000n) / BigInt(Math.round(spanMs)));
  }
}

// ------------------------------------------------------------------------------------------------ utxo mapping

const covenantIdOf = (e: SdkUtxoEntryReference): Hex | null => {
  const c = e.entry?.covenantId ?? e.covenantId;
  if (c === undefined || c === null) return null;
  const s = String(c).toLowerCase();
  return /^[0-9a-f]{64}$/.test(s) ? s : null;
};

/** SDK `UtxoEntryReference` -> `NodeUtxo`. The SDK does not always attach the address: derive it from the script then. */
export function mapUtxoEntry(sdk: KaspaSdk, network: string, e: SdkUtxoEntryReference): NodeUtxo {
  const spk = joinSpkString(e.scriptPublicKey.version, e.scriptPublicKey.script);
  return {
    address: e.address ? e.address.toString() : spkStringToAddress(sdk, spk, network),
    transactionId: e.outpoint.transactionId,
    index: e.outpoint.index,
    amount: e.amount.toString(),
    scriptPublicKey: spk,
    blockDaaScore: e.blockDaaScore.toString(),
    isCoinbase: !!e.isCoinbase,
    covenantId: covenantIdOf(e),
  };
}

// ------------------------------------------------------------------------------------------------ the node

export type RpcNodeStatus = 'idle' | 'connecting' | 'connected' | 'disconnected' | 'closed';

export interface KaspaRpcNodeOptions {
  sdk: KaspaSdk;
  /** network the app is configured for; `connect` fails if the node is on another one */
  network: string;
  /** `ws://` / `wss://` wRPC endpoint; '' = the SDK's public Resolver */
  url?: string;
  connectTimeoutMs?: number;
  /** extra time on top of `connectTimeoutMs` before the connect attempt is abandoned by force, ms */
  connectGraceMs?: number;
  /** hard limit of one RPC call (a socket that died silently never answers), ms */
  callTimeoutMs?: number;
  /** how long a call waits for the SDK's automatic reconnect before failing */
  reconnectWaitMs?: number;
  /** wall clock, ms since the epoch */
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  estimator?: DaaRateEstimator;
}

/** Rejects with an `unavailable` NodeError when `p` does not settle in time (the SDK's own timeouts do not cover every path, e.g. a Resolver lookup). */
function timed<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  let t: ReturnType<typeof setTimeout>;
  const limit = new Promise<never>((_, rej) => {
    t = setTimeout(() => rej(new NodeError('unavailable', `The node did not answer (${what}) within ${Math.round(ms / 1000)}s.`)), ms);
  });
  return Promise.race([p, limit]).finally(() => clearTimeout(t));
}

const realSleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

export class KaspaRpcNode implements NodeApi {
  readonly kind = 'rpc' as const;
  private readonly sdk: KaspaSdk;
  private readonly network: string;
  private readonly url: string;
  private readonly connectTimeoutMs: number;
  private readonly callTimeoutMs: number;
  private readonly connectGraceMs: number;
  private readonly reconnectWaitMs: number;
  private readonly now: () => number;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly estimator: DaaRateEstimator;
  private rpc: SdkRpcClient | null = null;
  private connecting: Promise<NodeInfo> | null = null;
  private state: RpcNodeStatus = 'idle';
  private readonly listeners = new Set<(s: RpcNodeStatus) => void>();

  constructor(o: KaspaRpcNodeOptions) {
    this.sdk = o.sdk;
    this.network = normalizeNetwork(o.network);
    this.url = o.url ?? '';
    this.connectTimeoutMs = o.connectTimeoutMs ?? 15_000;
    this.callTimeoutMs = o.callTimeoutMs ?? 20_000;
    this.connectGraceMs = o.connectGraceMs ?? 5_000;
    this.reconnectWaitMs = o.reconnectWaitMs ?? 10_000;
    this.now = o.now ?? Date.now;
    this.sleep = o.sleep ?? realSleep;
    this.estimator = o.estimator ?? new DaaRateEstimator();
  }

  get status(): RpcNodeStatus {
    return this.state;
  }

  /** Connection state changes (`connected` / `disconnected`, the latter is followed by the SDK's automatic reconnect). */
  onStatus(cb: (s: RpcNodeStatus) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  private setState(s: RpcNodeStatus): void {
    if (this.state === s) return;
    this.state = s;
    for (const cb of [...this.listeners]) {
      try {
        cb(s);
      } catch {
        /* a listener must not break the connection handling */
      }
    }
  }

  connect(): Promise<NodeInfo> {
    if (this.rpc?.isConnected && this.state === 'connected') return this.info();
    this.connecting ??= this.doConnect().finally(() => {
      this.connecting = null;
    });
    return this.connecting;
  }

  private async doConnect(): Promise<NodeInfo> {
    const { sdk } = this;
    await this.dispose();
    this.setState('connecting');
    const cfg = this.url
      ? { url: this.url, networkId: this.network, encoding: sdk.Encoding.SerdeJson }
      : { resolver: new sdk.Resolver(), networkId: this.network, encoding: sdk.Encoding.SerdeJson };
    let rpc: SdkRpcClient;
    try {
      rpc = new sdk.RpcClient(cfg);
    } catch (e) {
      this.setState('idle');
      throw new NodeError('unavailable', `Cannot create the node client: ${rawErrorText(e)}`, rawErrorText(e));
    }
    rpc.addEventListener('connect', () => this.state !== 'closed' && this.setState('connected'));
    rpc.addEventListener('disconnect', () => this.state !== 'closed' && this.setState('disconnected'));
    this.rpc = rpc;
    try {
      await timed(rpc.connect({ blockAsyncConnect: true, timeoutDuration: this.connectTimeoutMs }), this.connectTimeoutMs + this.connectGraceMs, 'connect');
      const info = await this.info();
      this.setState('connected');
      return info;
    } catch (e) {
      await this.dispose();
      this.setState('idle');
      throw e instanceof NodeError ? e : describeNodeError(e, 'read');
    }
  }

  /** Reads the dag info, checks the network and records a clock sample. Assumes a connected client. */
  private async info(): Promise<NodeInfo> {
    const rpc = this.rpc;
    if (!rpc) throw new NodeError('unavailable', 'Not connected to a node.');
    const t0 = this.now();
    let dag;
    try {
      dag = await timed(rpc.getBlockDagInfo(), this.callTimeoutMs, 'getBlockDagInfo');
    } catch (e) {
      throw describeNodeError(e, 'read');
    }
    this.estimator.add(dag.virtualDaaScore, (t0 + this.now()) / 2);
    const net = normalizeNetwork(dag.network);
    if (net !== this.network) {
      throw new NodeError('network-mismatch', `The node is on ${net || 'an unknown network'} but the app is set to ${this.network}.`);
    }
    let serverVersion: string | undefined;
    try {
      serverVersion = (await timed(rpc.getServerInfo(), this.callTimeoutMs, 'getServerInfo')).serverVersion;
    } catch {
      /* optional */
    }
    return { network: net, virtualDaaScore: dag.virtualDaaScore.toString(), serverVersion, daaRateMilli: this.estimator.rateMilli() };
  }

  private async dispose(): Promise<void> {
    const rpc = this.rpc;
    this.rpc = null;
    if (!rpc) return;
    try {
      rpc.removeEventListener?.('connect');
      rpc.removeEventListener?.('disconnect');
    } catch {
      /* ignore */
    }
    try {
      await rpc.disconnect();
    } catch {
      /* already down */
    }
  }

  async disconnect(): Promise<void> {
    this.setState('closed');
    await this.dispose();
    this.estimator.reset();
  }

  /** A connected client: connects on first use, then waits for the SDK's automatic reconnect after a drop. */
  private async ready(): Promise<SdkRpcClient> {
    if (this.state === 'closed') throw new NodeError('unavailable', 'The node connection was closed.');
    if (!this.rpc) await this.connect();
    const rpc = this.rpc;
    if (!rpc) throw new NodeError('unavailable', 'Not connected to a node.');
    if (rpc.isConnected) return rpc;
    const deadline = this.now() + this.reconnectWaitMs;
    while (this.now() < deadline) {
      if (this.rpc !== rpc) return this.ready();
      if (rpc.isConnected) return rpc;
      await this.sleep(100);
    }
    // the automatic retry did not bring the link back in time: start over once
    await this.connect();
    if (this.rpc?.isConnected) return this.rpc;
    throw new NodeError('unavailable', 'The node connection was lost and could not be restored.');
  }

  /** Runs an idempotent read; on a dropped link waits for the reconnect and retries once. */
  private async read<T>(fn: (rpc: SdkRpcClient) => Promise<T>): Promise<T> {
    let rpc = await this.ready();
    try {
      return await timed(fn(rpc), this.callTimeoutMs, 'request');
    } catch (e) {
      if (!isConnectionError(e)) throw describeNodeError(e, 'read');
    }
    rpc = await this.ready();
    try {
      return await timed(fn(rpc), this.callTimeoutMs, 'request');
    } catch (e) {
      throw describeNodeError(e, 'read');
    }
  }

  async getClock(): Promise<{ daa: bigint; unixSeconds: bigint; rateMilli: number | null }> {
    const t0 = this.now();
    const dag = await this.read((rpc) => rpc.getBlockDagInfo());
    const tMs = (t0 + this.now()) / 2;
    this.estimator.add(dag.virtualDaaScore, tMs);
    return { daa: dag.virtualDaaScore, unixSeconds: BigInt(Math.floor(tMs / 1000)), rateMilli: this.estimator.rateMilli() };
  }

  async getUtxosByAddresses(addresses: string[]): Promise<NodeUtxo[]> {
    const uniq = [...new Set(addresses)];
    if (uniq.length === 0) return [];
    const res = await this.read((rpc) => rpc.getUtxosByAddresses({ addresses: uniq }));
    return res.entries.map((e) => mapUtxoEntry(this.sdk, this.network, e));
  }

  /** `getFeeEstimate`: null on any failure (an older node without the method, a dropped link, an unreadable answer): the fee policy then pays the floor. */
  async getFeeEstimate(): Promise<FeeEstimate | null> {
    try {
      const raw = await this.read((rpc) => {
        if (typeof rpc.getFeeEstimate !== 'function') throw new NodeError('unavailable', 'This SDK has no getFeeEstimate.');
        return rpc.getFeeEstimate({});
      });
      return parseFeeEstimate(raw);
    } catch {
      return null;
    }
  }

  async submitTransaction(tx: TxJson): Promise<string> {
    const rpc = await this.ready();
    let transaction;
    try {
      transaction = this.sdk.Transaction.deserializeFromSafeJSON(JSON.stringify(tx));
    } catch (e) {
      throw new NodeError('invalid', `The transaction could not be read by the SDK: ${rawErrorText(e)}`, rawErrorText(e));
    }
    try {
      const r = await timed(rpc.submitTransaction({ transaction, allowOrphan: false }), this.callTimeoutMs, 'submit');
      return r.transactionId;
    } catch (e) {
      const err = describeNodeError(e);
      // NOT retried: after a dropped link the node may have accepted it. Say so instead of inviting a blind resubmit.
      if (err.code === 'unavailable') {
        throw new NodeError('unavailable', 'The connection dropped while sending. The transaction may or may not have been accepted: check before sending again.', err.raw);
      }
      throw err;
    }
  }
}
