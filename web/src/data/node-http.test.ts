import { afterEach, describe, expect, it } from 'vitest';
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { HttpMockNode } from './node-http';
import { createNode, nodeKindFor } from './node-factory';
import { KaspaRpcNode } from './node-rpc';
import { loadKaspaSdkNode } from './kaspa-sdk.node';
import type { NodeUtxo } from './node';

// An in-process server speaking the documented mock-server contract, over real HTTP.
interface Mock {
  info: Record<string, unknown>;
  utxos: NodeUtxo[];
  submits: unknown[];
  submitReply: { status: number; body: unknown };
  delayMs: number;
  feeEstimate?: unknown;
}

let server: Server | null = null;
async function start(m: Mock): Promise<string> {
  server = createServer((req, res) => {
    let raw = '';
    req.on('data', (c) => (raw += c));
    req.on('end', () => {
      const reply = (status: number, body: unknown) => {
        res.writeHead(status, { 'content-type': 'application/json' });
        res.end(typeof body === 'string' ? body : JSON.stringify(body));
      };
      const go = () => {
        if (req.method === 'GET' && req.url === '/node/info') return reply(200, m.info);
        if (req.method === 'GET' && req.url === '/node/fee-estimate') return m.feeEstimate === undefined ? reply(404, { error: 'no such route' }) : reply(200, m.feeEstimate);
        if (req.method === 'POST' && req.url === '/node/utxos') {
          const { addresses } = JSON.parse(raw) as { addresses: string[] };
          return reply(200, { entries: m.utxos.filter((u) => addresses.includes(u.address)) });
        }
        if (req.method === 'POST' && req.url === '/node/submit') {
          m.submits.push(JSON.parse(raw));
          return reply(m.submitReply.status, m.submitReply.body);
        }
        return reply(404, { error: 'no such route' });
      };
      if (m.delayMs) setTimeout(go, m.delayMs);
      else go();
    });
  });
  await new Promise<void>((r) => server!.listen(0, '127.0.0.1', r));
  return `http://127.0.0.1:${(server!.address() as AddressInfo).port}`;
}
afterEach(async () => {
  if (server) await new Promise<void>((r) => server!.close(() => r()));
  server = null;
});

const utxo = (addr: string, i: number, over: Partial<NodeUtxo> = {}): NodeUtxo => ({
  address: addr, transactionId: 'ab'.repeat(32), index: i, amount: '1000', scriptPublicKey: '0000ac', blockDaaScore: '10', isCoinbase: false, covenantId: null, ...over,
});
const mock = (over: Partial<Mock> = {}): Mock => ({
  info: { network: 'testnet-10', virtualDaaScore: '12345', serverVersion: 'mock-1', daaRateMilli: 9800 },
  utxos: [], submits: [], submitReply: { status: 200, body: { transactionId: 'cc'.repeat(32) } }, delayMs: 0, ...over,
});

describe('HttpMockNode', () => {
  it('connect: reads /node/info and checks the network', async () => {
    const base = await start(mock());
    const node = new HttpMockNode({ baseUrl: base + '/', network: 'testnet-10' });
    expect(node.kind).toBe('http-mock');
    expect(await node.connect()).toEqual({ network: 'testnet-10', virtualDaaScore: '12345', serverVersion: 'mock-1', daaRateMilli: 9800 });
    await expect(new HttpMockNode({ baseUrl: base, network: 'mainnet' }).connect()).rejects.toMatchObject({ code: 'network-mismatch' });
    await node.disconnect();
  });

  it('getClock: DAA from the server, UTC from the local clock, measured rate passed through (null when absent)', async () => {
    const m = mock();
    const base = await start(m);
    const node = new HttpMockNode({ baseUrl: base, network: 'testnet-10', now: () => 1_790_000_123_400 });
    expect(await node.getClock()).toEqual({ daa: 12345n, unixSeconds: 1_790_000_123n, rateMilli: 9800 });
    delete m.info.daaRateMilli;
    m.info.virtualDaaScore = '9007199254740993'; // decimal string: the server must not send a lossy JSON number
    expect((await node.getClock()).daa).toBe(9_007_199_254_740_993n);
    expect((await node.getClock()).rateMilli).toBeNull();
  });

  it('getUtxosByAddresses posts the addresses and normalises covenantId', async () => {
    const m = mock({ utxos: [utxo('kaspatest:qa', 0), utxo('kaspatest:qb', 1, { covenantId: '77'.repeat(32) }), utxo('kaspatest:qc', 2)] });
    const base = await start(m);
    const node = new HttpMockNode({ baseUrl: base, network: 'testnet-10' });
    const out = await node.getUtxosByAddresses(['kaspatest:qa', 'kaspatest:qb', 'kaspatest:qa']);
    expect(out.map((u) => [u.address, u.covenantId])).toEqual([['kaspatest:qa', null], ['kaspatest:qb', '77'.repeat(32)]]);
    expect(await node.getUtxosByAddresses([])).toEqual([]);
  });

  it('submitTransaction posts { transaction } and returns the id; a 400 becomes a classified NodeError', async () => {
    const m = mock();
    const base = await start(m);
    const node = new HttpMockNode({ baseUrl: base, network: 'testnet-10' });
    const tx = { id: 'dd'.repeat(32), version: 1 } as never;
    expect(await node.submitTransaction(tx)).toBe('cc'.repeat(32));
    expect(m.submits[0]).toEqual({ transaction: tx });
    m.submitReply = { status: 400, body: { error: 'transaction is an orphan where orphan is disallowed' } };
    await expect(node.submitTransaction(tx)).rejects.toMatchObject({ name: 'NodeError', code: 'orphan' });
    m.submitReply = { status: 400, body: { error: { code: 'bad_tx', message: 'script ran, but verification failed' } } };
    await expect(node.submitTransaction(tx)).rejects.toMatchObject({ code: 'script' });
    m.submitReply = { status: 500, body: 'boom' };
    await expect(node.submitTransaction(tx)).rejects.toMatchObject({ code: 'other' });
    m.submitReply = { status: 200, body: { nope: 1 } };
    await expect(node.submitTransaction(tx)).rejects.toMatchObject({ code: 'bad-response' });
  });

  it('getFeeEstimate: GET /node/fee-estimate; a mock without the route, an unreadable answer or a dead server is null', async () => {
    const answer = { estimate: { priorityBucket: { feerate: 150, estimatedSeconds: 1 }, normalBuckets: [{ feerate: 120 }], lowBuckets: [{ feerate: 100 }] } };
    const base = await start(mock({ feeEstimate: answer }));
    const node = new HttpMockNode({ baseUrl: base, network: 'testnet-10' });
    expect(await node.getFeeEstimate()).toEqual({ priority: 150, normal: 120, low: 100, seconds: { priority: 1 } });
    await new Promise<void>((r) => server!.close(() => r()));
    server = null;
    expect(await node.getFeeEstimate()).toBeNull();
    const noRoute = await start(mock());
    expect(await new HttpMockNode({ baseUrl: noRoute, network: 'testnet-10' }).getFeeEstimate()).toBeNull();
    await new Promise<void>((r) => server!.close(() => r()));
    server = null;
    const junk = await start(mock({ feeEstimate: { estimate: 'x' } }));
    expect(await new HttpMockNode({ baseUrl: junk, network: 'testnet-10' }).getFeeEstimate()).toBeNull();
  });

  it('reports an unreachable or slow server as unavailable', async () => {
    const base = await start(mock());
    await new Promise<void>((r) => server!.close(() => r()));
    server = null;
    await expect(new HttpMockNode({ baseUrl: base, network: 'testnet-10' }).connect()).rejects.toMatchObject({ code: 'unavailable' });
    const slow = await start(mock({ delayMs: 300 }));
    await expect(new HttpMockNode({ baseUrl: slow, network: 'testnet-10', timeoutMs: 50 }).connect()).rejects.toMatchObject({ code: 'unavailable', message: expect.stringMatching(/did not answer/) });
  });

  it('rejects a malformed /node/info', async () => {
    const base = await start(mock({ info: { hello: 'world' } }));
    await expect(new HttpMockNode({ baseUrl: base, network: 'testnet-10' }).connect()).rejects.toMatchObject({ code: 'bad-response' });
  });
});

describe('createNode', () => {
  const sdk = loadKaspaSdkNode();
  it('picks the implementation by URL scheme', () => {
    expect(nodeKindFor('')).toBe('rpc');
    expect(nodeKindFor('ws://127.0.0.1:18210')).toBe('rpc');
    expect(nodeKindFor('WSS://n.example')).toBe('rpc');
    expect(nodeKindFor('http://127.0.0.1:8899')).toBe('http-mock');
    expect(nodeKindFor('https://mock.example')).toBe('http-mock');
    expect(() => nodeKindFor('ftp://x')).toThrow(/unsupported/);
    expect(createNode({ network: 'testnet-10', nodeUrl: 'ws://x:1' }, sdk)).toBeInstanceOf(KaspaRpcNode);
    expect(createNode({ network: 'testnet-10', nodeUrl: '' }, sdk).kind).toBe('rpc');
    expect(createNode({ network: 'testnet-10', nodeUrl: 'http://127.0.0.1:1' }, null)).toBeInstanceOf(HttpMockNode);
  });
  it('needs the SDK only for wRPC', () => {
    expect(() => createNode({ network: 'mainnet', nodeUrl: '' }, null)).toThrow(/SDK is required/);
  });
});
