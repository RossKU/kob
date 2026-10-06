// `NodeApi` over HTTP against the offline mock server (web/mock/server.mjs), selected when the node URL is http(s).
// Contract (the server side is written by the mock-server work item):
//   GET  {base}/node/info    -> { network, virtualDaaScore, serverVersion, daaRateMilli }
//   POST {base}/node/utxos   body { addresses }     -> { entries: NodeUtxo[] }
//   POST {base}/node/submit  body { transaction }   -> { transactionId }   or HTTP 400 { error }
//   GET  {base}/node/fee-estimate                    -> the node's `getFeeEstimate` answer ({ estimate: { priorityBucket, normalBuckets, lowBuckets } }); optional: any failure = no estimate
// Never used against a real node: submission there must go through wRPC (see node-rpc.ts).
import { parseFeeEstimate, type FeeEstimate } from '../kob/fee-policy';
import type { TxJson } from '../kob/types';
import type { NodeApi, NodeInfo, NodeUtxo } from './node';
import { normalizeNetwork } from './kaspa-sdk';
import { NodeError, describeNodeError, rawErrorText } from './node-error';

export interface HttpMockNodeOptions {
  /** base URL, e.g. `http://127.0.0.1:8899` */
  baseUrl: string;
  /** network the app is configured for; `connect` fails on a mismatch */
  network: string;
  fetch?: typeof fetch;
  now?: () => number;
  timeoutMs?: number;
}

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null;

export class HttpMockNode implements NodeApi {
  readonly kind = 'http-mock' as const;
  private readonly base: string;
  private readonly network: string;
  private readonly f: typeof fetch;
  private readonly now: () => number;
  private readonly timeoutMs: number;

  constructor(o: HttpMockNodeOptions) {
    this.base = o.baseUrl.replace(/\/+$/, '');
    this.network = normalizeNetwork(o.network);
    this.f = o.fetch ?? ((...a) => fetch(...a));
    this.now = o.now ?? Date.now;
    this.timeoutMs = o.timeoutMs ?? 10_000;
  }

  private async request(method: 'GET' | 'POST', path: string, body?: unknown): Promise<{ status: number; json: unknown }> {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), this.timeoutMs);
    try {
      const res = await this.f(this.base + path, {
        method,
        headers: body === undefined ? undefined : { 'content-type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: ctl.signal,
      });
      let json: unknown = null;
      try {
        json = await res.json();
      } catch {
        json = null;
      }
      return { status: res.status, json };
    } catch (e) {
      const raw = rawErrorText(e);
      const timedOut = ctl.signal.aborted;
      throw new NodeError('unavailable', timedOut ? `The mock node did not answer within ${this.timeoutMs / 1000}s.` : `The mock node is unreachable: ${raw}`, raw);
    } finally {
      clearTimeout(timer);
    }
  }

  private failure(status: number, json: unknown): NodeError {
    const err = isRecord(json) ? json.error : undefined;
    const text = typeof err === 'string' ? err : isRecord(err) && typeof err.message === 'string' ? err.message : `HTTP ${status}`;
    // the server may forward the node's own rejection text: reuse the same classification as the wRPC node
    return status === 400 || status === 422 ? describeNodeError(text) : new NodeError('other', text, text);
  }

  private async info(): Promise<{ info: NodeInfo; rate: number | null }> {
    const { status, json } = await this.request('GET', '/node/info');
    if (status !== 200 || !isRecord(json)) throw this.failure(status, json);
    if (typeof json.network !== 'string' || (typeof json.virtualDaaScore !== 'string' && typeof json.virtualDaaScore !== 'number')) {
      throw new NodeError('bad-response', 'The mock node answered /node/info with an unexpected shape.');
    }
    const rate = typeof json.daaRateMilli === 'number' && Number.isFinite(json.daaRateMilli) ? json.daaRateMilli : null;
    return {
      rate,
      info: {
        network: normalizeNetwork(json.network),
        virtualDaaScore: String(json.virtualDaaScore),
        serverVersion: typeof json.serverVersion === 'string' ? json.serverVersion : undefined,
        daaRateMilli: rate,
      },
    };
  }

  async connect(): Promise<NodeInfo> {
    const { info } = await this.info();
    if (info.network !== this.network) {
      throw new NodeError('network-mismatch', `The node is on ${info.network || 'an unknown network'} but the app is set to ${this.network}.`);
    }
    return info;
  }

  async disconnect(): Promise<void> {
    /* stateless */
  }

  async getClock(): Promise<{ daa: bigint; unixSeconds: bigint; rateMilli: number | null }> {
    const t0 = this.now();
    const { info, rate } = await this.info();
    const tMs = (t0 + this.now()) / 2;
    return { daa: BigInt(info.virtualDaaScore), unixSeconds: BigInt(Math.floor(tMs / 1000)), rateMilli: rate };
  }

  async getUtxosByAddresses(addresses: string[]): Promise<NodeUtxo[]> {
    const uniq = [...new Set(addresses)];
    if (uniq.length === 0) return [];
    const { status, json } = await this.request('POST', '/node/utxos', { addresses: uniq });
    if (status !== 200) throw this.failure(status, json);
    if (!isRecord(json) || !Array.isArray(json.entries)) throw new NodeError('bad-response', 'The mock node answered /node/utxos with an unexpected shape.');
    return (json.entries as NodeUtxo[]).map((e) => ({ ...e, covenantId: e.covenantId ?? null }));
  }

  /** GET /node/fee-estimate; a mock without the route (or any failure) is "no estimate" */
  async getFeeEstimate(): Promise<FeeEstimate | null> {
    try {
      const { status, json } = await this.request('GET', '/node/fee-estimate');
      return status === 200 ? parseFeeEstimate(json) : null;
    } catch {
      return null;
    }
  }

  async submitTransaction(tx: TxJson): Promise<string> {
    const { status, json } = await this.request('POST', '/node/submit', { transaction: tx });
    if (status === 200 && isRecord(json) && typeof json.transactionId === 'string') return json.transactionId;
    if (status === 200) throw new NodeError('bad-response', 'The mock node answered /node/submit without a transaction id.');
    throw this.failure(status, json);
  }
}
