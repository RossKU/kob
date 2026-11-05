// `NodeApi.subscribeBlocks` of both node implementations (standalone mode follows the node's new blocks): the wRPC client's `block-added`
// notifications with a subscription made again after a lost link, and the mock node's `/node/blocks` poll.
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { StreamBlock } from './block-stream';
import { HttpMockNode } from './node-http';
import { KaspaRpcNode } from './node-rpc';
import type { KaspaSdk, SdkRpcClient } from './kaspa-sdk';

const blockOf = (n: number) => ({
  header: { hash: n.toString(16).padStart(64, '0'), daaScore: BigInt(1000 + n), timestamp: 1_790_000_000_000n + BigInt(n) },
  transactions: [{ version: 1, inputs: [], outputs: [{ value: 5n, scriptPublicKey: '0000ab' }], lockTime: 0n, subnetworkId: '00'.repeat(20), gas: 0n, payload: '', verboseData: { transactionId: n.toString(16).padStart(64, 'f') } }],
});

class FakeRpc {
  isConnected = false;
  subscribed = 0;
  unsubscribed = 0;
  private ls = new Map<string, Set<(e?: unknown) => void>>();
  async connect() {
    this.isConnected = true;
    this.emit('connect');
  }
  async disconnect() {
    this.isConnected = false;
  }
  addEventListener(ev: string, cb: (e?: unknown) => void) {
    (this.ls.get(ev) ?? this.ls.set(ev, new Set()).get(ev)!).add(cb);
  }
  removeEventListener(ev: string, cb?: (e?: unknown) => void) {
    if (cb) this.ls.get(ev)?.delete(cb);
    else this.ls.delete(ev);
  }
  emit(ev: string, data?: unknown) {
    for (const cb of [...(this.ls.get(ev) ?? [])]) cb(data);
  }
  async getBlockDagInfo() {
    return { network: 'testnet-10', virtualDaaScore: 1000n };
  }
  async getServerInfo() {
    return { serverVersion: '2.1.0' };
  }
  async getUtxosByAddresses() {
    return { entries: [] };
  }
  async submitTransaction() {
    return { transactionId: '00' };
  }
  async subscribeBlockAdded() {
    this.subscribed++;
  }
  async unsubscribeBlockAdded() {
    this.unsubscribed++;
  }
}

function rpcNode() {
  const clients: FakeRpc[] = [];
  const sdk = {
    Encoding: { Borsh: 0, SerdeJson: 1 },
    Resolver: class {},
    RpcClient: class {
      constructor() {
        const c = new FakeRpc();
        clients.push(c);
        return c as unknown as SdkRpcClient;
      }
    },
  } as unknown as KaspaSdk;
  return { node: new KaspaRpcNode({ sdk, network: 'testnet-10', url: 'ws://127.0.0.1:18210' }), clients };
}

afterEach(() => vi.useRealTimers());

describe('KaspaRpcNode.subscribeBlocks', () => {
  it('hands over every block-added notification, subscribes again after a lost link and reports the gap', async () => {
    const { node, clients } = rpcNode();
    const got: StreamBlock[] = [];
    const gap = vi.fn();
    const sub = await node.subscribeBlocks((b) => got.push(b), gap);
    const c = clients[0]!;
    expect(c.subscribed).toBe(1);
    c.emit('block-added', { type: 'block-added', data: { block: blockOf(1) } });
    c.emit('block-added', { type: 'block-added', data: { block: { header: {} } } }); // unreadable: skipped
    expect(got.map((b) => b.daaScore)).toEqual([1001n]);
    expect(got[0]!.transactions).toHaveLength(1);
    // the SDK's automatic reconnect: the node forgot the subscription
    c.isConnected = false;
    c.emit('disconnect');
    c.isConnected = true;
    c.emit('connect');
    await vi.waitFor(() => expect(gap).toHaveBeenCalledTimes(1));
    expect(c.subscribed).toBe(2);
    c.emit('block-added', { data: { block: blockOf(2) } });
    expect(got).toHaveLength(2);
    await sub.close();
    expect(c.unsubscribed).toBe(1);
    c.emit('block-added', { data: { block: blockOf(3) } });
    expect(got).toHaveLength(2);
  });
});

describe('HttpMockNode.subscribeBlocks', () => {
  it('polls /node/blocks from the tip on, in order, and reports a gap after failed polls', async () => {
    vi.useFakeTimers();
    let tip = 3;
    let fail = false;
    const asked: string[] = [];
    const fetchFn = vi.fn(async (url: string) => {
      asked.push(url);
      if (fail) throw new Error('offline');
      const m = /after=(\d+)/.exec(url);
      const after = m ? Number(m[1]) : null;
      const blocks = after === null ? [] : Array.from({ length: tip - after }, (_, k) => JSON.parse(JSON.stringify(blockOf(after + k + 1), (_x, v) => (typeof v === 'bigint' ? v.toString() : v))));
      return new Response(JSON.stringify({ tip, blocks }), { status: 200 });
    });
    const node = new HttpMockNode({ baseUrl: 'http://mock', network: 'testnet-10', fetch: fetchFn as unknown as typeof fetch, blockPollMs: 100 });
    const got: bigint[] = [];
    const gap = vi.fn();
    const sub = await node.subscribeBlocks((b) => got.push(b.daaScore), gap);
    expect(asked).toEqual(['http://mock/node/blocks']);
    tip = 5;
    await vi.advanceTimersByTimeAsync(110);
    expect(got).toEqual([1004n, 1005n]);
    expect(asked[1]).toBe('http://mock/node/blocks?after=3');
    fail = true;
    await vi.advanceTimersByTimeAsync(220);
    fail = false;
    tip = 6;
    await vi.advanceTimersByTimeAsync(110);
    expect(gap).toHaveBeenCalledTimes(1);
    expect(got).toEqual([1004n, 1005n, 1006n]);
    await sub.close();
    const n = asked.length;
    await vi.advanceTimersByTimeAsync(500);
    expect(asked.length).toBe(n);
  });
});
