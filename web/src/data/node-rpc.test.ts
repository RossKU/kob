import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { DaaRateEstimator, KaspaRpcNode, mapUtxoEntry, nodeDaaRate } from './node-rpc';
import { NodeError, describeNodeError } from './node-error';
import { loadKaspaSdkNode } from './kaspa-sdk.node';
import type { KaspaSdk, SdkConnectOptions, SdkRpcClient, SdkRpcConfig, SdkTransaction, SdkUtxoEntryReference } from './kaspa-sdk';

const realSdk = loadKaspaSdkNode();
const golden = JSON.parse(readFileSync(fileURLToPath(new URL('../../../crates/kob-protocol/vectors/golden.json', import.meta.url)), 'utf8'));
const signedTx = golden.transactions.find((t: { name: string }) => t.name === 'send.tokens').signed.tx;

// ------------------------------------------------------------------------------------------------ fake wRPC client

interface FakeState {
  configs: SdkRpcConfig[];
  clients: FakeRpc[];
  daa: bigint;
  network: string;
  entries: SdkUtxoEntryReference[];
  failConnect?: string;
  utxoCalls: string[][];
  submitted: SdkTransaction[];
  submitError?: unknown;
  /** errors thrown by the next N getUtxosByAddresses calls */
  utxoErrors: unknown[];
  /** when set, a fresh client starts disconnected and comes up after this many `isConnected` reads */
  reconnectAfterReads?: number;
  /** never settle these calls (a socket that died silently) */
  hang?: Set<'connect' | 'utxos' | 'submit'>;
  /** answer of getFeeEstimate: a value, or an Error to throw; undefined = the SDK has no such method */
  feeEstimate?: unknown;
  feeCalls?: number;
  /** headers by hash (timestamp ms, DAA score) and the dag info's sink and pruning point: the node's long DAA-rate window */
  headers?: Record<string, { timestamp: bigint; daaScore: bigint }>;
  sink?: string;
  pruningPoint?: string;
}

function makeFake(state: FakeState): KaspaSdk {
  class FakeRpc implements SdkRpcClient {
    isConnected = false;
    private listeners = new Map<string, (() => void)[]>();
    disconnected = false;
    reads = 0;
    constructor(cfg: SdkRpcConfig) {
      state.configs.push(cfg);
      state.clients.push(this);
    }
    async connect(_o?: SdkConnectOptions) {
      if (state.hang?.has('connect')) return new Promise<void>(() => undefined);
      if (state.failConnect) throw new Error(state.failConnect);
      this.isConnected = true;
      this.emit('connect');
    }
    async disconnect() {
      this.isConnected = false;
      this.disconnected = true;
    }
    addEventListener(ev: 'connect' | 'disconnect', cb: () => void) {
      this.listeners.set(ev, [...(this.listeners.get(ev) ?? []), cb]);
    }
    removeEventListener() {
      this.listeners.clear();
    }
    emit(ev: string) {
      for (const cb of this.listeners.get(ev) ?? []) cb();
    }
    drop() {
      this.isConnected = false;
      this.emit('disconnect');
    }
    async getBlockDagInfo() {
      return { network: state.network, virtualDaaScore: state.daa, sink: state.sink, pruningPointHash: state.pruningPoint };
    }
    getBlock = state.headers === undefined ? undefined : async (req: { hash: string; includeTransactions: boolean }) => {
      const h = state.headers![req.hash];
      if (!h) throw new Error('block not found');
      return { block: { header: h } };
    };
    async getServerInfo() {
      return { serverVersion: '2.1.0' };
    }
    getFeeEstimate = state.feeEstimate === undefined && !('feeEstimate' in state) ? undefined : async (_req?: Record<string, never>) => {
      state.feeCalls = (state.feeCalls ?? 0) + 1;
      if (state.feeEstimate instanceof Error) throw state.feeEstimate;
      return state.feeEstimate;
    };
    async getUtxosByAddresses(req: { addresses: string[] }) {
      state.utxoCalls.push(req.addresses);
      if (state.hang?.has('utxos')) return new Promise<never>(() => undefined);
      const err = state.utxoErrors.shift();
      if (err) throw err;
      return { entries: state.entries };
    }
    async submitTransaction(req: { transaction: SdkTransaction }) {
      if (state.hang?.has('submit')) return new Promise<never>(() => undefined);
      if (state.submitError) throw state.submitError;
      state.submitted.push(req.transaction);
      return { transactionId: 'aa'.repeat(32) };
    }
  }
  return { ...realSdk, RpcClient: FakeRpc as unknown as KaspaSdk['RpcClient'], Resolver: class {} as unknown as KaspaSdk['Resolver'] };
}
type FakeRpc = SdkRpcClient & { drop(): void; disconnected: boolean };

const newState = (over: Partial<FakeState> = {}): FakeState => ({
  configs: [], clients: [], daa: 1_000n, network: 'testnet-10', entries: [], utxoCalls: [], submitted: [], utxoErrors: [], ...over,
});

const P2PK_SPK = { version: 0, script: '20' + '11'.repeat(32) + 'ac' };
const entry = (over: Partial<SdkUtxoEntryReference> = {}): SdkUtxoEntryReference => ({
  address: undefined,
  outpoint: { transactionId: 'ab'.repeat(32), index: 2 },
  amount: 123_456_789n,
  scriptPublicKey: P2PK_SPK,
  blockDaaScore: 555n,
  isCoinbase: false,
  ...over,
});

// ------------------------------------------------------------------------------------------------ estimator

describe('DaaRateEstimator', () => {
  it('is null until the history spans an hour (matcher.md 10.10)', () => {
    const e = new DaaRateEstimator();
    expect(e.rateMilli()).toBeNull();
    e.add(1000n, 0);
    expect(e.rateMilli()).toBeNull();
    e.add(1200n, 20_000); // 20 s: noise
    expect(e.rateMilli()).toBeNull();
    for (let s = 60; s < 3_600; s += 60) e.add(1000n + BigInt(s * 10), s * 1000);
    expect(e.rateMilli()).toBeNull();
    e.add(37_000n, 3_600_000);
    expect(e.rateMilli()).toBe(10_000);
  });

  it('measures milli-DAA per second (9.7 DAA/s over an hour)', () => {
    const e = new DaaRateEstimator();
    e.add(0n, 0);
    e.add(34_920n, 3_600_000);
    expect(e.rateMilli()).toBe(9_700);
  });

  it('keeps a sliding window and drops old samples', () => {
    const e = new DaaRateEstimator(60_000, 5_000, 64, 1_000);
    e.add(0n, 0); // 10 DAA/s until t=30s, then 5 DAA/s
    e.add(300n, 30_000);
    e.add(450n, 60_000);
    e.add(600n, 90_000);
    e.add(750n, 120_000); // t=0/30s samples are now outside the 60 s window
    expect(e.rateMilli()).toBe(5_000);
    expect(e.size).toBeLessThanOrEqual(3);
  });

  it('ignores samples closer than the minimum interval (keeps the newer reading) and resets on a DAA regression', () => {
    const e = new DaaRateEstimator(600_000, 1_000, 64, 1_000);
    e.add(100n, 0);
    e.add(101n, 200);
    expect(e.size).toBe(1);
    e.add(120n, 2_000);
    expect(e.rateMilli()).toBe(Number(((120n - 101n) * 1_000_000n) / 1_800n));
    e.add(50n, 5_000); // went backwards: another node / reorg
    expect(e.size).toBe(1);
    expect(e.rateMilli()).toBeNull();
  });

  it('never reports a non-positive rate', () => {
    const e = new DaaRateEstimator(600_000, 1_000);
    e.add(100n, 0);
    e.add(100n, 5_000);
    expect(e.rateMilli()).toBeNull();
  });
});

describe('nodeDaaRate', () => {
  const header = (timestamp: bigint, daaScore: bigint) => ({ block: { header: { timestamp, daaScore } } });
  const blocks: Record<string, ReturnType<typeof header>> = {
    pp: header(1_790_000_000_000n, 1_000_000n),
    tip: header(1_790_000_000_000n + 30n * 3_600_000n, 1_000_000n + 30n * 36_000n - 540n),
    near: header(1_790_000_000_000n + 600_000n, 1_006_000n),
  };
  const getBlock = async (r: { hash: string }) => blocks[r.hash]!;

  it('is the DAA advance per second between the pruning point and the selected tip (hours apart)', async () => {
    expect(await nodeDaaRate({ sink: 'tip', pruningPointHash: 'pp' }, getBlock)).toBe(9_995);
  });

  it('is null when the node gives no window, or one shorter than an hour', async () => {
    expect(await nodeDaaRate({ sink: 'tip', pruningPointHash: 'pp' }, undefined)).toBeNull();
    expect(await nodeDaaRate({ sink: 'tip' }, getBlock)).toBeNull();
    expect(await nodeDaaRate({ sink: 'near', pruningPointHash: 'pp' }, getBlock)).toBeNull();
    expect(await nodeDaaRate({ sink: 'pp', pruningPointHash: 'pp' }, getBlock)).toBeNull();
  });
});

// ------------------------------------------------------------------------------------------------ entry mapping

describe('mapUtxoEntry', () => {
  it('maps a P2PK entry: derives the address when the SDK gives none, no covenant', () => {
    const u = mapUtxoEntry(realSdk, 'testnet-10', entry());
    expect(u).toMatchObject({
      transactionId: 'ab'.repeat(32), index: 2, amount: '123456789', blockDaaScore: '555', isCoinbase: false, covenantId: null,
      scriptPublicKey: '0000' + P2PK_SPK.script,
    });
    expect(u.address).toMatch(/^kaspatest:q/);
  });

  it('reads the covenant id from the inner UtxoEntry (Hash object) or the reference, and keeps a given address', () => {
    const cov = 'cd'.repeat(32);
    const addr = new realSdk.Address(realSdk.addressFromScriptPublicKey(P2PK_SPK as never, 'testnet-10')!.toString());
    expect(mapUtxoEntry(realSdk, 'testnet-10', entry({ entry: { covenantId: { toString: () => cov.toUpperCase() } }, address: addr })).covenantId).toBe(cov);
    expect(mapUtxoEntry(realSdk, 'testnet-10', entry({ covenantId: cov })).covenantId).toBe(cov);
    expect(mapUtxoEntry(realSdk, 'testnet-10', entry({ entry: { covenantId: undefined } })).covenantId).toBeNull();
    expect(mapUtxoEntry(realSdk, 'testnet-10', entry({ entry: { covenantId: 'nothex' } })).covenantId).toBeNull();
    expect(mapUtxoEntry(realSdk, 'testnet-10', entry({ address: addr })).address).toBe(addr.toString());
  });

  it('keeps amounts above 2^53 exact (bigint -> string)', () => {
    expect(mapUtxoEntry(realSdk, 'mainnet', entry({ amount: 9_007_199_254_740_993n })).amount).toBe('9007199254740993');
  });
});

// ------------------------------------------------------------------------------------------------ the node

describe('KaspaRpcNode', () => {
  const mk = (state: FakeState, extra: Partial<ConstructorParameters<typeof KaspaRpcNode>[0]> = {}) =>
    new KaspaRpcNode({ sdk: makeFake(state), network: 'testnet-10', url: 'ws://127.0.0.1:18210', ...extra });

  it('connects over wRPC JSON to the configured URL and reports network, DAA and server version', async () => {
    const st = newState();
    const node = mk(st);
    const info = await node.connect();
    expect(info).toMatchObject({ network: 'testnet-10', virtualDaaScore: '1000', serverVersion: '2.1.0', daaRateMilli: null });
    expect(st.configs[0]).toMatchObject({ url: 'ws://127.0.0.1:18210', networkId: 'testnet-10', encoding: realSdk.Encoding.SerdeJson });
    expect(node.status).toBe('connected');
    expect(node.kind).toBe('rpc');
    // a second connect on a live link reuses it
    await node.connect();
    expect(st.clients).toHaveLength(1);
    await node.disconnect();
    expect(st.clients[0]!.disconnected).toBe(true);
    expect(node.status).toBe('closed');
  });

  it('uses the SDK public Resolver when no URL is configured', async () => {
    const st = newState();
    await mk(st, { url: '' }).connect();
    expect(st.configs[0]!.resolver).toBeDefined();
    expect(st.configs[0]!.url).toBeUndefined();
    expect(st.configs[0]!.networkId).toBe('testnet-10');
  });

  it('refuses a node on another network (accepting the node spelling of mainnet)', async () => {
    const st = newState({ network: 'mainnet' });
    await expect(mk(st).connect()).rejects.toMatchObject({ name: 'NodeError', code: 'network-mismatch' });
    expect(st.clients[0]!.disconnected).toBe(true);
    const ok = newState({ network: 'kaspa-mainnet' });
    await expect(new KaspaRpcNode({ sdk: makeFake(ok), network: 'mainnet', url: 'ws://x:1' }).connect()).resolves.toMatchObject({ network: 'mainnet' });
  });

  it('turns a failed connection into a readable NodeError and cleans up', async () => {
    const st = newState({ failConnect: 'WebSocket connection timed out' });
    const err = await mk(st).connect().catch((e) => e);
    expect(err).toBeInstanceOf(NodeError);
    expect(err.code).toBe('unavailable');
    expect(st.clients[0]!.disconnected).toBe(true);
  });

  it('getClock: virtual DAA + UTC seconds read together, rate null until the history is long enough, then measured', async () => {
    const st = newState();
    let t = 1_790_000_000_000;
    const node = mk(st, { now: () => t });
    await node.connect();
    let c = await node.getClock();
    expect(c).toEqual({ daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: null });
    t += 30_000;
    st.daa += 297n; // 9.9 DAA/s
    c = await node.getClock();
    expect(c.daa).toBe(1297n);
    expect(c.unixSeconds).toBe(1_790_000_030n);
    expect(c.rateMilli).toBeNull(); // 30 s of samples: not a rate
    for (let k = 0; k < 59; k++) {
      t += 60_000;
      st.daa += 600n;
      c = await node.getClock();
    }
    expect(c.rateMilli).toBeNull(); // 59.5 min
    t += 30_000;
    st.daa += 303n;
    expect((await node.getClock()).rateMilli).toBe(10_000); // 36,000 DAA over an hour
  });

  it("getClock: the node's own long window (pruning point to selected tip) is the rate when the node gives it", async () => {
    const st = newState({
      sink: 'tip',
      pruningPoint: 'pp',
      headers: { pp: { timestamp: 1_789_900_000_000n, daaScore: 0n }, tip: { timestamp: 1_789_900_000_000n + 30n * 3_600_000n, daaScore: 30n * 36_000n - 540n } },
    });
    const t = 1_790_000_000_000;
    const node = mk(st, { now: () => t });
    await node.connect();
    expect((await node.getClock()).rateMilli).toBe(9_995);
    // a node whose window is shorter than an hour gives no rate: the wallet's own samples (none yet)
    const short = newState({ sink: 'tip', pruningPoint: 'pp', headers: { pp: { timestamp: 0n, daaScore: 0n }, tip: { timestamp: 600_000n, daaScore: 6_000n } } });
    const n2 = mk(short, { now: () => t });
    await n2.connect();
    expect((await n2.getClock()).rateMilli).toBeNull();
  });

  it('getUtxosByAddresses maps entries, dedupes addresses and skips the call for an empty list', async () => {
    const st = newState({ entries: [entry(), entry({ outpoint: { transactionId: 'cd'.repeat(32), index: 0 }, entry: { covenantId: '77'.repeat(32) } })] });
    const node = mk(st);
    expect(await node.getUtxosByAddresses([])).toEqual([]);
    expect(st.utxoCalls).toHaveLength(0);
    const out = await node.getUtxosByAddresses(['kaspatest:qa', 'kaspatest:qa', 'kaspatest:qb']);
    expect(st.utxoCalls).toEqual([['kaspatest:qa', 'kaspatest:qb']]);
    expect(out.map((u) => [u.transactionId.slice(0, 2), u.index, u.covenantId?.slice(0, 2) ?? null])).toEqual([['ab', 2, null], ['cd', 0, '77']]);
  });

  it('a read that hits a dropped link waits for the SDK reconnect and retries once', async () => {
    const st = newState({ entries: [entry()] });
    const node = mk(st, { sleep: async () => undefined });
    await node.connect();
    const c = st.clients[0]!;
    st.utxoErrors.push(new Error('WebSocket disconnected'));
    // the reconnect completes while the call is waiting
    let reads = 0;
    Object.defineProperty(c, 'isConnected', { get: () => ++reads > 2, configurable: true });
    const out = await node.getUtxosByAddresses(['kaspatest:qa']);
    expect(out).toHaveLength(1);
    expect(st.utxoCalls).toHaveLength(2);
  });

  it('gives up with an unavailable error when the link does not come back', async () => {
    const st = newState();
    let t = 0;
    const node = mk(st, { now: () => t, sleep: async () => { t += 5_000; }, reconnectWaitMs: 10_000, connectTimeoutMs: 1 });
    await node.connect();
    const c = st.clients[0]!;
    c.drop();
    st.failConnect = 'refused';
    const err = await node.getUtxosByAddresses(['kaspatest:qa']).catch((e) => e);
    expect(err).toMatchObject({ name: 'NodeError', code: 'unavailable' });
  });

  it('a non-connection read failure is surfaced without a retry', async () => {
    const st = newState();
    const node = mk(st);
    st.utxoErrors.push(new Error('RPC Server (remote error) -> code:0  message:`invalid address` data:None'));
    await expect(node.getUtxosByAddresses(['x'])).rejects.toMatchObject({ code: 'other' });
    expect(st.utxoCalls).toHaveLength(1);
  });

  it('submitTransaction deserializes the safe JSON with the SDK and submits with allowOrphan=false', async () => {
    const st = newState();
    let allowOrphan: unknown = 'unset';
    const sdk = makeFake(st);
    const node = new KaspaRpcNode({ sdk, network: 'testnet-10', url: 'ws://x:1' });
    await node.connect();
    const orig = st.clients[0]!.submitTransaction.bind(st.clients[0]!);
    st.clients[0]!.submitTransaction = async (req) => {
      allowOrphan = req.allowOrphan;
      return orig(req);
    };
    const id = await node.submitTransaction(signedTx);
    expect(id).toBe('aa'.repeat(32));
    expect(allowOrphan).toBe(false);
    const sent = JSON.parse(st.submitted[0]!.serializeToSafeJSON());
    expect(sent.version).toBe(1);
    expect(sent.inputs).toHaveLength(signedTx.inputs.length);
    expect(sent.inputs[0].computeBudget).toBe(signedTx.inputs[0].computeBudget);
    expect(sent.inputs[0].signatureScript).toBe(signedTx.inputs[0].signatureScript);
    expect(sent.outputs).toHaveLength(signedTx.outputs.length);
    expect(sent.outputs[0].covenant).toEqual(signedTx.outputs[0].covenant);
    expect(sent.payload).toBe(signedTx.payload);
  });

  it('maps node rejections to readable errors', async () => {
    const st = newState();
    const node = mk(st);
    st.submitError = new Error('RPC Server (remote error) -> code:0  message:`Rejected transaction ad9490cfc55ec289b4006b640066cabdd0af8424638f36401559707c83253579: transaction ad9490cfc55ec289b4006b640066cabdd0af8424638f36401559707c83253579 is an orphan where orphan is disallowed` data:None');
    const e1 = await node.submitTransaction(signedTx).catch((e) => e);
    expect(e1).toMatchObject({ name: 'NodeError', code: 'orphan' });
    expect(e1.message).not.toMatch(/RPC Server/);
    expect(e1.raw).toMatch(/orphan/);
    st.submitError = 'Rejected transaction ab: script ran, but verification failed';
    expect((await node.submitTransaction(signedTx).catch((e) => e)).code).toBe('script');
  });

  it('a malformed transaction is refused before it reaches the node', async () => {
    const st = newState();
    const node = mk(st);
    const err = await node.submitTransaction({ ...signedTx, inputs: 'nope' } as never).catch((e) => e);
    expect(err).toMatchObject({ name: 'NodeError', code: 'invalid' });
    expect(st.submitted).toHaveLength(0);
  });

  it('does NOT retry a submit after the link dropped, and says the outcome is unknown', async () => {
    const st = newState();
    const node = mk(st);
    st.submitError = new Error('WebSocket disconnected');
    const err = await node.submitTransaction(signedTx).catch((e) => e);
    expect(err.code).toBe('unavailable');
    expect(err.message).toMatch(/may or may not/);
    expect(st.submitted).toHaveLength(0);
  });

  it('hard timeouts: a hanging connect, read or submit ends in an unavailable NodeError', async () => {
    const st = newState({ hang: new Set(['connect']) });
    await expect(mk(st, { connectTimeoutMs: 20, connectGraceMs: 20 }).connect()).rejects.toMatchObject({ code: 'unavailable', message: expect.stringMatching(/did not answer/) });
    expect(st.clients[0]!.disconnected).toBe(true);

    const st2 = newState();
    const node = mk(st2, { callTimeoutMs: 30, reconnectWaitMs: 50 });
    await node.connect();
    st2.hang = new Set(['utxos']);
    await expect(node.getUtxosByAddresses(['kaspatest:qa'])).rejects.toMatchObject({ code: 'unavailable' });
    expect(st2.utxoCalls).toHaveLength(2); // one retry
    st2.hang = new Set(['submit']);
    const err = await node.submitTransaction(signedTx).catch((e) => e);
    expect(err).toMatchObject({ code: 'unavailable', message: expect.stringMatching(/may or may not/) });
  });

  it('calls after disconnect() fail cleanly', async () => {
    const st = newState();
    const node = mk(st);
    await node.connect();
    await node.disconnect();
    await expect(node.getClock()).rejects.toMatchObject({ code: 'unavailable' });
  });

  it('emits status changes for connect and drop', async () => {
    const st = newState();
    const node = mk(st);
    const seen: string[] = [];
    node.onStatus((s) => seen.push(s));
    await node.connect();
    (st.clients[0] as unknown as FakeRpc).drop();
    expect(seen).toEqual(['connecting', 'connected', 'disconnected']);
  });
});

describe('KaspaRpcNode.getFeeEstimate', () => {
  const mk = (state: FakeState) => new KaspaRpcNode({ sdk: makeFake(state), network: 'testnet-10', url: 'ws://127.0.0.1:18210', callTimeoutMs: 500, reconnectWaitMs: 50 });
  const answer = { estimate: { priorityBucket: { feerate: 194.4, estimatedSeconds: 0.5 }, normalBuckets: [{ feerate: 140, estimatedSeconds: 40 }], lowBuckets: [{ feerate: 115, estimatedSeconds: 1800 }] } };

  it('asks the node with {} and returns the three bucket feerates with their times', async () => {
    const st = newState({ feeEstimate: answer });
    const node = mk(st);
    expect(await node.getFeeEstimate()).toEqual({ priority: 194.4, normal: 140, low: 115, seconds: { priority: 0.5, normal: 40, low: 1800 } });
    expect(st.feeCalls).toBe(1);
  });

  it('is null (never a rejection) for an SDK without the method, an erroring node and a malformed answer', async () => {
    expect(await mk(newState()).getFeeEstimate()).toBeNull();
    expect(await mk(newState({ feeEstimate: new Error('method not found') })).getFeeEstimate()).toBeNull();
    expect(await mk(newState({ feeEstimate: { estimate: { priorityBucket: { feerate: 0 }, normalBuckets: [], lowBuckets: [] } } })).getFeeEstimate()).toBeNull();
    expect(await mk(newState({ feeEstimate: null })).getFeeEstimate()).toBeNull();
  });
});

describe('describeNodeError', () => {
  const t = (s: string) => describeNodeError(new Error(s)).code;
  it('classifies known rejections', () => {
    expect(t('transaction is an orphan where orphan is disallowed')).toBe('orphan');
    expect(t('Rejected transaction ab: output already spent by transaction cd in the mempool')).toBe('double-spend');
    expect(t('transaction already in the mempool')).toBe('already-known');
    expect(t('Rejected transaction ab: transaction fee is too low')).toBe('fee');
    expect(t('storage mass exceeds the limit')).toBe('fee');
    expect(t('script ran, but verification failed')).toBe('script');
    expect(t('WebSocket is not connected')).toBe('unavailable');
    expect(t('something odd')).toBe('other');
  });
  it('is idempotent and unwraps the SDK wrapper', () => {
    const e = describeNodeError(new Error('RPC Server (remote error) -> code:0  message:`bad thing happened` data:None'));
    expect(e.message).toBe('bad thing happened');
    expect(describeNodeError(e)).toBe(e);
  });
});
