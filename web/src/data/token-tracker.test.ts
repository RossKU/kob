import { beforeEach, describe, expect, it } from 'vitest';
import { TokenTracker, sanitizeTracked, type TokenRef, type TrackedToken } from './token-tracker';
import { loadKaspaSdkNode } from './kaspa-sdk.node';
import { spkStringToAddress } from './kaspa-sdk';
import { loadKobNode } from '../kob/wasm.node';
import { MockNode } from '../testing/mock-node';
import { signBuilt } from '../testing/local-signer';
import {
  CARRIER, MAKER_PK, MAKER_SK, OTHER_PK, TOKEN_COV_ID, TOKEN_EXT, TOKEN_PROGRAM, sendTokensRequest, tokenState, tokenUtxo,
} from '../testing/token-fixtures';
import type { StorageLike } from '../config';
import type { BuiltTx, SignedTx } from '../kob/types';
import type { TokenUtxoView } from './indexer-types';

const sdk = loadKaspaSdkNode();
const kob = loadKobNode();
const NET = 'testnet-10';
const TOKEN: TokenRef = { covenantId: TOKEN_COV_ID, program: TOKEN_PROGRAM };

class MemStorage implements StorageLike {
  m = new Map<string, string>();
  getItem(k: string) { return this.m.get(k) ?? null; }
  setItem(k: string, v: string) { this.m.set(k, v); }
  removeItem(k: string) { this.m.delete(k); }
}

interface World { node: MockNode; storage: MemStorage; now: { t: number }; tracker: TokenTracker }
function world(extra: Partial<ConstructorParameters<typeof TokenTracker>[0]> = {}): World {
  const node = new MockNode({ addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });
  const storage = new MemStorage();
  const now = { t: 1_000_000 };
  const tracker = new TokenTracker({ kob, node, sdk, network: NET, storage, now: () => now.t, ...extra });
  return { node, storage, now, tracker };
}

/** builds + signs + finalizes a sendTokens (default: 12000 tokens in, 9000 to OTHER, 3000 change to MAKER) */
function makeSend(opts: Parameters<typeof sendTokensRequest>[0] = {}): { built: BuiltTx; signed: SignedTx } {
  const built = kob.build(sendTokensRequest(opts));
  const signed = kob.finalize(built, signBuilt(built, [MAKER_SK]), { tightenBudgets: true });
  return { built, signed };
}

describe('TokenTracker.trackFromBuilt', () => {
  it('tracks only the maker-owned token outputs (the change), paired by derived script', () => {
    const { tracker } = world();
    const { built } = makeSend();
    const found = tracker.trackFromBuilt(kob, built, MAKER_PK);
    expect(found).toHaveLength(1);
    const t = found[0]!;
    expect(t.transactionId).toBe(built.tx.id);
    expect(t.tokenCovId).toBe(TOKEN_COV_ID);
    expect(t.program).toBe(TOKEN_PROGRAM);
    expect(t.state).toEqual(tokenState(3000n));
    expect(t.carrier).toBe(CARRIER);
    // the tracked output really is the one whose script commits to the state
    expect(kob.tokenScriptPublicKey(TOKEN_PROGRAM, t.state)).toBe(built.tx.outputs[t.index]!.scriptPublicKey);
    expect(built.tx.outputs[t.index]!.covenant?.covenantId).toBe(TOKEN_COV_ID);
    expect(tracker.list(MAKER_PK)).toHaveLength(1);
    // the recipient's output is not ours
    expect(tracker.list(OTHER_PK)).toEqual([]);
  });

  it('is idempotent and also learns the recipient side when asked for that key', () => {
    const { tracker } = world();
    const { built } = makeSend();
    tracker.trackFromBuilt(kob, built, MAKER_PK);
    tracker.trackFromBuilt(kob, built, MAKER_PK);
    expect(tracker.list(MAKER_PK)).toHaveLength(1);
    const theirs = tracker.trackFromBuilt(kob, built, OTHER_PK);
    expect(theirs.map((t) => t.state.amount)).toEqual(['9000']);
  });

  it('a send that consumes all tokens leaves nothing to track for the maker', () => {
    const { tracker } = world();
    const { built } = makeSend({ amount: 12000n });
    expect(tracker.trackFromBuilt(kob, built, MAKER_PK)).toEqual([]);
  });

  it('ignores a transaction without a token leader (plain KAS)', () => {
    const { tracker } = world();
    const built = { ...makeSend().built, plans: [{ kind: 'p2pk' as const, pubkey: MAKER_PK }] };
    expect(tracker.trackFromBuilt(kob, built, MAKER_PK)).toEqual([]);
  });
});

describe('TokenTracker.tokenUtxosFor (local candidates verified on the node)', () => {
  let w: World;
  beforeEach(() => { w = world(); });

  it('a candidate is offered only once the node shows it, with the exact state and the node\'s carrier', async () => {
    const { built, signed } = makeSend();
    w.node.seedFromInputs(built.tx);
    w.tracker.trackFromBuilt(kob, built, MAKER_PK);
    // submitted to nobody yet: tracked but not live
    expect(await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).toEqual([]);
    expect(w.tracker.list(MAKER_PK)).toHaveLength(1); // kept: within the grace period

    await w.node.submitTransaction(signed.tx);
    const utxos = await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN);
    expect(utxos).toHaveLength(1);
    const u = utxos[0]!;
    expect(u).toMatchObject({ transactionId: built.tx.id, amount: CARRIER, covenantId: TOKEN_COV_ID, state: tokenState(3000n) });
    expect(await w.tracker.balanceFor(MAKER_PK, TOKEN)).toBe(3000n);
  });

  it('the resolved UTXO is directly spendable: a second send built from it is consensus-valid', async () => {
    const first = makeSend();
    w.node.seedFromInputs(first.built.tx);
    w.tracker.trackFromBuilt(kob, first.built, MAKER_PK);
    await w.node.submitTransaction(first.signed.tx);
    const [change] = await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN);
    // KAS change of the first tx as funding
    const kasChange = first.built.tx.outputs.findIndex((o, i) => !o.covenant && i > 0);
    const funding = { transactionId: first.built.tx.id, index: kasChange, amount: first.built.tx.outputs[kasChange]!.value, blockDaaScore: '600', covenantId: null, pubkey: MAKER_PK };
    const built = kob.build(sendTokensRequest({ tokens: [change!], amount: 1000n, funding: [funding] }));
    const signed = kob.finalize(built, signBuilt(built, [MAKER_SK]), { tightenBudgets: true });
    expect(() => kob.validate(signed)).not.toThrow();
    w.node.seedFromInputs(built.tx);
    w.tracker.trackFromBuilt(kob, built, MAKER_PK);
    await w.node.submitTransaction(signed.tx);
    const after = await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN);
    expect(after.map((x) => x.state.amount)).toEqual(['2000']);
  });

  it('a spent candidate disappears from the result at once and is pruned only once the misses are confirmed', async () => {
    const { built, signed } = makeSend();
    w.node.seedFromInputs(built.tx);
    w.tracker.trackFromBuilt(kob, built, MAKER_PK);
    await w.node.submitTransaction(signed.tx);
    expect(await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).toHaveLength(1);
    // a second, live candidate keeps the node's answer non-empty (an empty answer is never evidence)
    const other = tokenUtxo(0x0f, 77n);
    const spk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, other.state);
    w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: other.transactionId, index: other.index, amount: CARRIER, scriptPublicKey: spk, covenantId: TOKEN_COV_ID });
    w.tracker.add(MAKER_PK, { transactionId: other.transactionId, index: other.index, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: other.state, carrier: CARRIER });
    const tracked = w.tracker.list(MAKER_PK).find((t) => t.transactionId === built.tx.id)!;
    w.node.removeUtxo(tracked.transactionId, tracked.index); // spent elsewhere
    expect((await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['77']);
    expect(w.tracker.list(MAKER_PK)).toHaveLength(2); // still within the grace period
    w.now.t += 16 * 60_000;
    expect((await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['77']);
    expect(w.tracker.list(MAKER_PK)).toHaveLength(2); // past the grace period, but a miss is not yet confirmed
    for (let i = 0; i < 3; i++) {
      w.now.t += 16 * 60_000;
      await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN);
    }
    expect(w.tracker.list(MAKER_PK).map((t) => t.state.amount)).toEqual(['77']); // pruned after confirmed misses
  });

  it('a candidate whose outpoint the node holds under another covenant id is dropped immediately', async () => {
    const { built, signed } = makeSend();
    w.tracker.trackFromBuilt(kob, built, MAKER_PK);
    w.node.seedFromInputs(built.tx);
    await w.node.submitTransaction(signed.tx);
    const t = w.tracker.list(MAKER_PK)[0]!;
    w.node.removeUtxo(t.transactionId, t.index);
    w.node.addUtxo({
      address: spkStringToAddress(sdk, kob.tokenScriptPublicKey(TOKEN_PROGRAM, t.state), NET),
      transactionId: t.transactionId, index: t.index, amount: CARRIER,
      scriptPublicKey: kob.tokenScriptPublicKey(TOKEN_PROGRAM, t.state), covenantId: '99'.repeat(32),
    });
    expect(await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).toEqual([]);
    expect(w.tracker.list(MAKER_PK)).toEqual([]);
  });

  it('a claimed state that does not match the on-chain script is never returned', async () => {
    const { built, signed } = makeSend();
    w.node.seedFromInputs(built.tx);
    await w.node.submitTransaction(signed.tx);
    const change = built.tx.outputs.findIndex((o) => o.covenant);
    const idx = built.tx.outputs.findIndex((o, i) => o.covenant && kob.tokenScriptPublicKey(TOKEN_PROGRAM, tokenState(3000n)) === o.scriptPublicKey && i >= 0);
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(change).toBeGreaterThanOrEqual(0);
    // claim a bigger amount for the same outpoint (a lying indexer / corrupted storage)
    w.tracker.add(MAKER_PK, { transactionId: built.tx.id, index: idx, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: tokenState(999_999n), carrier: CARRIER });
    expect(await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).toEqual([]);
  });

  it('keeps tokens of different covenants apart and sorts the result largest first', async () => {
    const outs = [tokenUtxo(0x11, 500n), tokenUtxo(0x12, 900n), tokenUtxo(0x13, 900n, 2)];
    for (const o of outs) {
      const spk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, o.state);
      w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: o.transactionId, index: o.index, amount: CARRIER, scriptPublicKey: spk, covenantId: TOKEN_COV_ID });
      w.tracker.add(MAKER_PK, { transactionId: o.transactionId, index: o.index, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: o.state, carrier: CARRIER });
    }
    const other = '55'.repeat(32);
    w.tracker.add(MAKER_PK, { transactionId: '14'.repeat(32), index: 0, tokenCovId: other, program: TOKEN_PROGRAM, state: tokenState(1n), carrier: CARRIER });
    const res = await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN);
    expect(res.map((r) => [r.transactionId.slice(0, 2), r.state.amount])).toEqual([['12', '900'], ['13', '900'], ['11', '500']]);
    expect(w.tracker.list(MAKER_PK)).toHaveLength(4); // the other token's candidate is untouched
    expect(await w.tracker.tokenUtxosFor(MAKER_PK, { covenantId: other, program: TOKEN_PROGRAM })).toEqual([]);
  });

  it('asks the node for all candidate addresses in ONE call and not at all without candidates', async () => {
    expect(await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).toEqual([]);
    expect(w.node.utxoQueries).toHaveLength(0);
    for (const n of [0x21, 0x22, 0x23]) {
      w.tracker.add(MAKER_PK, { transactionId: n.toString(16).repeat(32).slice(0, 64), index: 1, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: tokenState(BigInt(n)), carrier: CARRIER });
    }
    await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN);
    expect(w.node.utxoQueries).toHaveLength(1);
    expect(w.node.utxoQueries[0]).toHaveLength(3);
  });
});

describe('TokenTracker with the indexer as first source', () => {
  const liveToken = (w: World, n: number, amount: bigint) => {
    const o = tokenUtxo(n, amount);
    const spk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, o.state);
    w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: o.transactionId, index: o.index, amount: CARRIER, scriptPublicKey: spk, covenantId: TOKEN_COV_ID });
    return o;
  };
  const view = (o: ReturnType<typeof tokenUtxo>, withState: boolean): TokenUtxoView => ({
    txid: o.transactionId, index: o.index, token: TOKEN_COV_ID, owner: MAKER_PK, amount: o.state.amount, value: CARRIER, role: 'owned',
    created_daa: 1, spent: false, spent_txid: null, confirmations: 10, settled: true, ...(withState ? { state: o.state } : {}),
  });

  it('uses indexer candidates (with or without a state), verified on the node, unioned with local ones', async () => {
    const calls: unknown[] = [];
    const bag: { views: TokenUtxoView[] | null } = { views: [] };
    const w = world({ indexer: { tokenUtxos: async (q) => { calls.push(q); return bag.views; } } });
    const a = liveToken(w, 0x31, 100n);
    const b = liveToken(w, 0x32, 200n);
    const c = liveToken(w, 0x33, 300n);
    bag.views = [view(a, true), view(b, false)];
    w.tracker.add(MAKER_PK, { transactionId: c.transactionId, index: c.index, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: c.state, carrier: CARRIER });
    // no extension commitment given: the state-less indexer view cannot be rebuilt
    expect((await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['300', '100']);
    // with the token's commitment the view is rebuilt into a full state
    const res = await w.tracker.tokenUtxosFor(MAKER_PK, { ...TOKEN, extensionCommitment: TOKEN_EXT });
    expect(res.map((x) => x.state.amount)).toEqual(['300', '200', '100']);
    expect(res.find((x) => x.state.amount === '200')!.state).toEqual(tokenState(200n));
    expect(calls[0]).toEqual({ owner: MAKER_PK, token: TOKEN_COV_ID, spent: false });
  });

  it('indexer claims the node contradicts are not returned (spent, wrong owner, spent flag, other token)', async () => {
    const w = world();
    const live = liveToken(w, 0x41, 50n);
    const ghost = tokenUtxo(0x42, 60n); // not on the node
    const foreign: TokenUtxoView = { ...view(tokenUtxo(0x43, 70n, 1, OTHER_PK), true), owner: OTHER_PK };
    const spentView: TokenUtxoView = { ...view(liveToken(w, 0x44, 80n), true), spent: true };
    const otherToken: TokenUtxoView = { ...view(liveToken(w, 0x45, 90n), true), token: '55'.repeat(32) };
    const t = new TokenTracker({ kob, node: w.node, sdk, network: NET, storage: w.storage, indexer: { tokenUtxos: async () => [view(live, true), view(ghost, true), foreign, spentView, otherToken] } });
    expect((await t.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['50']);
  });

  it('degrades to the local tracker when the indexer is unsupported (null) or failing', async () => {
    for (const impl of [async () => null, async () => { throw new Error('indexer down'); }]) {
      const w = world({ indexer: { tokenUtxos: impl as never } });
      const o = liveToken(w, 0x51, 10n);
      w.tracker.add(MAKER_PK, { transactionId: o.transactionId, index: o.index, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: o.state, carrier: CARRIER });
      expect((await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['10']);
    }
  });

  it('the same outpoint from both sources is returned once', async () => {
    const w = world();
    const o = liveToken(w, 0x61, 5n);
    const t = new TokenTracker({ kob, node: w.node, sdk, network: NET, storage: w.storage, indexer: { tokenUtxos: async () => [view(o, true)] } });
    t.add(MAKER_PK, { transactionId: o.transactionId, index: o.index, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: o.state, carrier: CARRIER });
    expect(await t.tokenUtxosFor(MAKER_PK, TOKEN)).toHaveLength(1);
    // the local copy stays: after an indexer reset (fresh database) it is the only record of the holding
    expect(t.list(MAKER_PK)).toHaveLength(1);
  });

  it('an outpoint only the local list knows (issuance output, not yet indexed) stays', async () => {
    const w = world();
    const known = liveToken(w, 0x71, 5n);
    const genesis = liveToken(w, 0x72, 7n);
    const t = new TokenTracker({ kob, node: w.node, sdk, network: NET, storage: w.storage, indexer: { tokenUtxos: async () => [view(known, true)] } });
    t.add(MAKER_PK, { transactionId: genesis.transactionId, index: genesis.index, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: genesis.state, carrier: CARRIER });
    expect((await t.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['7', '5']);
    expect(t.list(MAKER_PK).map((x) => x.transactionId)).toEqual([genesis.transactionId]);
  });

  it('KRON: an address-presence holding from the indexer is rebuilt without an extension commitment and verified on the node', async () => {
    const w = world();
    const program = 'KronToken2433' as const;
    const kronCov = '9a'.repeat(32);
    const state = { amount: '4000', owner: MAKER_PK, id_type: 3, is_minter: 0 };
    const spk = kob.tokenScriptPublicKey(program, state);
    const txid = '81'.repeat(32);
    w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: txid, index: 1, amount: CARRIER, scriptPublicKey: spk, covenantId: kronCov });
    const kview = (over: Partial<TokenUtxoView>): TokenUtxoView => ({
      txid, index: 1, token: kronCov, owner: MAKER_PK, amount: '4000', value: CARRIER, role: 'owned', created_daa: 1, spent: false, spent_txid: null, confirmations: 10, settled: true, family: 'kron', program, ...over,
    });
    const ref: TokenRef = { covenantId: kronCov, program };
    const t = new TokenTracker({ kob, node: w.node, sdk, network: NET, storage: w.storage, indexer: { tokenUtxos: async () => [kview({}), kview({ index: 9 }), kview({ state: { amount: '4000', owner: MAKER_PK, id_type: 0, is_minter: 0 }, index: 1 })] } });
    const res = await t.tokenUtxosFor(MAKER_PK, ref);
    expect(res).toHaveLength(1);
    expect(res[0]!.state).toEqual(state);
    // a KCC-20 state under a KRON token is refused before any script is derived
    const bad = new TokenTracker({ kob, node: w.node, sdk, network: NET, storage: new MemStorage(), indexer: { tokenUtxos: async () => [kview({ state: tokenState(4000n) })] } });
    expect(await bad.tokenUtxosFor(MAKER_PK, ref)).toEqual([]);
  });

  it('KRON candidates survive the local store round trip and reject a mixed layout', () => {
    const st = world().tracker;
    const kron = { transactionId: '82'.repeat(32), index: 0, tokenCovId: '9a'.repeat(32), program: 'KronToken2433' as const, state: { amount: '5', owner: MAKER_PK, id_type: 3, is_minter: 0 }, carrier: CARRIER };
    expect(st.add(MAKER_PK, kron)).toBe(true);
    expect(st.list(MAKER_PK)[0]!.state).toEqual(kron.state);
    expect(sanitizeTracked({ ...kron, state: tokenState(5n), addedAt: 1 })).toBeNull();
    expect(sanitizeTracked({ ...kron, program: 'KCC20Ref_8x8', addedAt: 1 })).toBeNull();
  });
});

describe('TokenTracker fails safe: holdings are never dropped on weak evidence', () => {
  const GRACE = 10 * 60_000; // miss spacing = 1 min
  let w: World;
  beforeEach(() => { w = world({ pendingGraceMs: GRACE }); });

  /** a token UTXO on the node (optional) and in the local list, added long enough ago to be past the pending grace */
  const holding = (n: number, amount: bigint, o: { onNode?: boolean; ageMs?: number } = {}) => {
    const u = tokenUtxo(n, amount);
    const spk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, u.state);
    if (o.onNode !== false) w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: u.transactionId, index: u.index, amount: CARRIER, scriptPublicKey: spk, covenantId: TOKEN_COV_ID });
    w.tracker.add(MAKER_PK, { transactionId: u.transactionId, index: u.index, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: u.state, carrier: CARRIER, addedAt: w.now.t - (o.ageMs ?? 3 * GRACE) });
    return u;
  };
  const amounts = () => w.tracker.list(MAKER_PK).map((t) => t.state.amount).sort();
  const check = async () => (await w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount);

  it('an EMPTY node answer neither drops nor counts against any candidate, however often it repeats', async () => {
    const a = holding(0x91, 10n, { onNode: false });
    holding(0x92, 20n, { onNode: false });
    for (let i = 0; i < 10; i++) {
      expect(await check()).toEqual([]);
      w.now.t += 2 * GRACE;
    }
    expect(amounts()).toEqual(['10', '20']);
    expect(w.tracker.list(MAKER_PK).find((t) => t.transactionId === a.transactionId)!.missCount).toBeUndefined();
    // the node comes back: both verify at once
    for (const n of [0x91, 0x92]) {
      const u = tokenUtxo(n, n === 0x91 ? 10n : 20n);
      const spk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, u.state);
      w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: u.transactionId, index: u.index, amount: CARRIER, scriptPublicKey: spk, covenantId: TOKEN_COV_ID });
    }
    expect(await check()).toEqual(['20', '10']);
  });

  it('a failed node call rejects and leaves the store untouched', async () => {
    holding(0x93, 10n);
    const before = [...w.storage.m.values()][0];
    w.node.getUtxosByAddresses = async () => { throw new Error('node down'); };
    await expect(w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).rejects.toThrow('node down');
    w.now.t += 5 * GRACE;
    await expect(w.tracker.tokenUtxosFor(MAKER_PK, TOKEN)).rejects.toThrow('node down');
    expect([...w.storage.m.values()][0]).toBe(before);
  });

  it('a candidate missing from ONE partial answer survives; the miss is cleared when it verifies again', async () => {
    holding(0x94, 10n);
    const b = holding(0x95, 20n);
    w.node.removeUtxo(b.transactionId, b.index); // the answer lists only the first candidate
    expect(await check()).toEqual(['10']);
    expect(amounts()).toEqual(['10', '20']);
    expect(w.tracker.list(MAKER_PK).find((t) => t.transactionId === b.transactionId)).toMatchObject({ missCount: 1, firstMissAt: w.now.t, lastMissAt: w.now.t });
    const spk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, b.state);
    w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: b.transactionId, index: b.index, amount: CARRIER, scriptPublicKey: spk, covenantId: TOKEN_COV_ID });
    w.now.t += 2 * 60_000;
    expect(await check()).toEqual(['20', '10']);
    const back = w.tracker.list(MAKER_PK).find((t) => t.transactionId === b.transactionId)!;
    expect(back.missCount).toBeUndefined();
    expect(back.firstMissAt).toBeUndefined();
    expect(back.lastMissAt).toBeUndefined();
  });

  it('repeated misses are pruned only when there are >= 3 of them on separate checks that span the grace period', async () => {
    holding(0x96, 10n);
    const gone = holding(0x97, 20n);
    w.node.removeUtxo(gone.transactionId, gone.index);
    // a burst of checks within the spacing is ONE miss; many checks inside the grace period are not enough either
    for (let i = 0; i < 20; i++) await check();
    expect(w.tracker.list(MAKER_PK).find((t) => t.transactionId === gone.transactionId)!.missCount).toBe(1);
    for (let i = 0; i < 8; i++) {
      w.now.t += 60_000;
      await check();
    }
    expect(amounts()).toEqual(['10', '20']); // 3+ misses, but they span 8 min < the 10 min grace
    w.now.t += 2 * 60_000;
    await check();
    expect(amounts()).toEqual(['10']); // confirmed gone
  });

  it('a candidate still within the pending grace is never pruned, whatever the node says', async () => {
    holding(0x98, 10n);
    const fresh = holding(0x99, 20n, { onNode: false, ageMs: 0 });
    for (let i = 0; i < 5; i++) {
      w.now.t += 60_000;
      await check();
    }
    expect(amounts()).toEqual(['10', '20']);
    w.now.t += 2 * GRACE;
    for (let i = 0; i < 5; i++) {
      w.now.t += 60_000;
      await check();
    }
    expect(w.tracker.list(MAKER_PK).some((t) => t.transactionId === fresh.transactionId)).toBe(false);
  });

  it('an outpoint the node holds under another covenant id is still dropped at once (node confirmation)', async () => {
    const bad = holding(0x9a, 10n, { onNode: false });
    holding(0x9b, 20n);
    const spk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, bad.state);
    w.node.addUtxo({ address: spkStringToAddress(sdk, spk, NET), transactionId: bad.transactionId, index: bad.index, amount: CARRIER, scriptPublicKey: spk, covenantId: '99'.repeat(32) });
    expect(await check()).toEqual(['20']);
    expect(amounts()).toEqual(['20']);
  });

  it('indexer-listed candidates stay in the local list: after an indexer reset they are the only record', async () => {
    const u = holding(0x9c, 30n);
    const view: TokenUtxoView = { txid: u.transactionId, index: u.index, token: TOKEN_COV_ID, owner: MAKER_PK, amount: '30', value: CARRIER, role: 'owned', created_daa: 1, spent: false, spent_txid: null, confirmations: 10, settled: true, state: u.state };
    const bag: { views: TokenUtxoView[] } = { views: [view] };
    const t = new TokenTracker({ kob, node: w.node, sdk, network: NET, storage: w.storage, now: () => w.now.t, pendingGraceMs: GRACE, indexer: { tokenUtxos: async () => bag.views } });
    expect((await t.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['30']);
    expect(t.list(MAKER_PK)).toHaveLength(1);
    // the indexer database is reset (it lists nothing): the holding is still found and spendable
    bag.views = [];
    expect((await t.tokenUtxosFor(MAKER_PK, TOKEN)).map((x) => x.state.amount)).toEqual(['30']);
    // spent on the node: pruned by the confirmed-miss rule like any other candidate (another live one keeps the answer non-empty)
    holding(0x9d, 5n);
    w.node.removeUtxo(u.transactionId, u.index);
    for (let i = 0; i < 14; i++) {
      w.now.t += 60_000;
      await t.tokenUtxosFor(MAKER_PK, TOKEN);
    }
    expect(t.list(MAKER_PK).map((x) => x.state.amount)).toEqual(['5']);
  });

  it('an add() that lands while a node check is in flight is not reverted by that check', async () => {
    holding(0xa1, 10n);
    // a contradicted candidate makes this check write the store (a stale snapshot written back would lose the add below)
    const bad = holding(0xa3, 5n, { onNode: false });
    const badSpk = kob.tokenScriptPublicKey(TOKEN_PROGRAM, bad.state);
    w.node.addUtxo({ address: spkStringToAddress(sdk, badSpk, NET), transactionId: bad.transactionId, index: bad.index, amount: CARRIER, scriptPublicKey: badSpk, covenantId: '99'.repeat(32) });
    const orig = w.node.getUtxosByAddresses.bind(w.node);
    w.node.getUtxosByAddresses = async (addrs: string[]) => {
      const r = await orig(addrs);
      holding(0xa2, 20n, { onNode: false, ageMs: 0 }); // learned from a just-submitted transaction while the node was answering
      return r;
    };
    expect(await check()).toEqual(['10']);
    expect(amounts()).toEqual(['10', '20']);
  });

  it('miss fields round-trip through the store; stores without them still load; malformed ones are ignored', () => {
    const base = { transactionId: '0a'.repeat(32), index: 1, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: tokenState(1n), carrier: CARRIER, addedAt: 5 };
    const full = { ...base, missCount: 2, firstMissAt: 100, lastMissAt: 200 };
    expect(sanitizeTracked(full)).toEqual(full);
    expect(sanitizeTracked(base)).toEqual(base);
    expect(sanitizeTracked({ ...base, missCount: -1, firstMissAt: 'x', lastMissAt: NaN })).toEqual(base);
    w.storage.setItem(`kob.tokens.v1.testnet-10.${MAKER_PK}`, JSON.stringify({ v: 1, items: [base, full] }));
    expect(w.tracker.list(MAKER_PK).map((t) => t.missCount)).toEqual([undefined, 2]);
  });

  describe('a store that cannot be read is never overwritten', () => {
    const key = `kob.tokens.v1.testnet-10.${MAKER_PK}`;
    const fresh = (n: number): Omit<TrackedToken, 'addedAt'> => ({ transactionId: n.toString(16).padStart(2, '0').repeat(32), index: 0, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: tokenState(1n), carrier: CARRIER });
    const sideKeys = () => [...w.storage.m.keys()].filter((k) => k.startsWith(key + '.unreadable.'));

    it('corrupt JSON: the raw value is kept under a side key before the first write replaces it', () => {
      w.storage.setItem(key, '{"v":1,"items":[ torn');
      expect(w.tracker.list(MAKER_PK)).toEqual([]);
      expect(w.tracker.add(MAKER_PK, fresh(1))).toBe(true);
      expect(sideKeys()).toHaveLength(1);
      expect(w.storage.m.get(sideKeys()[0]!)).toBe('{"v":1,"items":[ torn');
      expect(w.tracker.list(MAKER_PK)).toHaveLength(1);
      // the value is readable now: later writes keep no further copies
      w.tracker.add(MAKER_PK, fresh(2));
      expect(sideKeys()).toHaveLength(1);
    });

    it('a foreign version, and a store with entries this code rejects, are backed up the same way', () => {
      const foreign = JSON.stringify({ v: 2, items: [{ anything: 1 }] });
      w.storage.setItem(key, foreign);
      w.tracker.add(MAKER_PK, fresh(1));
      expect(w.storage.m.get(sideKeys()[0]!)).toBe(foreign);
      w.now.t += 1000;
      const partial = JSON.stringify({ v: 1, items: [{ ...fresh(3), addedAt: 1 }, { future: 'entry' }] });
      w.storage.setItem(key, partial);
      w.tracker.trackFromBuilt(kob, makeSend().built, MAKER_PK);
      expect(sideKeys().map((k) => w.storage.m.get(k))).toContain(partial);
    });

    it('when the backup cannot be written the original value is left alone', () => {
      w.storage.setItem(key, '{oops');
      const realSet = w.storage.setItem.bind(w.storage);
      w.storage.setItem = (k: string, v: string) => { if (k.includes('.unreadable.')) throw new Error('quota'); realSet(k, v); };
      expect(w.tracker.add(MAKER_PK, fresh(1))).toBe(true); // best effort, no throw
      expect(w.storage.m.get(key)).toBe('{oops');
    });

    it('an unreadable store is not wiped by a node check either', async () => {
      w.storage.setItem(key, '{oops');
      expect(await check()).toEqual([]);
      expect(w.storage.m.get(key)).toBe('{oops');
    });
  });
});

describe('TokenTracker storage', () => {
  const cand = (n: number): Omit<TrackedToken, 'addedAt'> => ({
    transactionId: n.toString(16).padStart(2, '0').repeat(32), index: 0, tokenCovId: TOKEN_COV_ID, program: TOKEN_PROGRAM, state: tokenState(1n), carrier: CARRIER,
  });

  it('is namespaced by network and public key', () => {
    const storage = new MemStorage();
    const node = new MockNode();
    const main = new TokenTracker({ kob, node, sdk, network: 'mainnet', storage });
    const test = new TokenTracker({ kob, node, sdk, network: NET, storage });
    expect(test.add(MAKER_PK, cand(1))).toBe(true);
    expect(test.add(MAKER_PK, cand(1))).toBe(false); // duplicate outpoint
    expect(main.list(MAKER_PK)).toEqual([]);
    expect(test.list(OTHER_PK)).toEqual([]);
    expect([...storage.m.keys()]).toEqual([`kob.tokens.v1.testnet-10.${MAKER_PK}`]);
    // a new tracker instance over the same storage sees the data
    expect(new TokenTracker({ kob, node, sdk, network: NET, storage }).list(MAKER_PK)).toHaveLength(1);
  });

  it('survives corrupt, foreign-version and partially invalid storage, and blocked storage', () => {
    const node = new MockNode();
    const storage = new MemStorage();
    const t = new TokenTracker({ kob, node, sdk, network: NET, storage });
    const key = `kob.tokens.v1.testnet-10.${MAKER_PK}`;
    storage.setItem(key, '{oops');
    expect(t.list(MAKER_PK)).toEqual([]);
    storage.setItem(key, JSON.stringify({ v: 99, items: [{}] }));
    expect(t.list(MAKER_PK)).toEqual([]);
    storage.setItem(key, JSON.stringify({ v: 1, items: [{ ...cand(1), addedAt: 5 }, { transactionId: 'zz' }, null, 7] }));
    expect(t.list(MAKER_PK)).toHaveLength(1);
    const blocked: StorageLike = { getItem() { throw new Error('blocked'); }, setItem() { throw new Error('blocked'); } };
    const b = new TokenTracker({ kob, node, sdk, network: NET, storage: blocked });
    expect(b.list(MAKER_PK)).toEqual([]);
    expect(() => b.add(MAKER_PK, cand(2))).not.toThrow();
  });

  it('falls back to memory when no storage exists, remove/clear work', () => {
    const t = new TokenTracker({ kob, node: new MockNode(), sdk, network: NET, storage: null });
    t.add(MAKER_PK, cand(1));
    t.add(MAKER_PK, cand(2));
    expect(t.list(MAKER_PK)).toHaveLength(2);
    expect(t.remove(MAKER_PK, cand(1).transactionId, 0)).toBe(true);
    expect(t.remove(MAKER_PK, cand(1).transactionId, 0)).toBe(false);
    t.clear(MAKER_PK);
    expect(t.list(MAKER_PK)).toEqual([]);
  });

  it('manual import accepts one or many valid entries and refuses malformed input as a whole', () => {
    const t = new TokenTracker({ kob, node: new MockNode(), sdk, network: NET, storage: new MemStorage() });
    expect(t.importJson(MAKER_PK, JSON.stringify(cand(1)))).toBe(1);
    expect(t.importJson(MAKER_PK, JSON.stringify([cand(1), cand(2), cand(3)]))).toBe(2);
    expect(() => t.importJson(MAKER_PK, 'nope')).toThrow(/valid JSON/);
    expect(() => t.importJson(MAKER_PK, JSON.stringify([cand(4), { ...cand(5), index: -1 }]))).toThrow(/not a valid/);
    expect(t.list(MAKER_PK)).toHaveLength(3); // nothing from the refused batch
    expect(() => t.importJson(MAKER_PK, '[]')).toThrow();
  });

  it('sanitizeTracked validates every field', () => {
    const ok = { ...cand(1), addedAt: 1 };
    expect(sanitizeTracked(ok)).toEqual(ok);
    const bads: unknown[] = [
      { ...ok, transactionId: 'ab' }, { ...ok, index: 1.5 }, { ...ok, index: 70000 }, { ...ok, tokenCovId: 'x' }, { ...ok, program: 'Nope' },
      { ...ok, carrier: '-1' }, { ...ok, state: { ...ok.state, amount: '1.5' } }, { ...ok, state: { ...ok.state, owner: 'ab' } },
      { ...ok, state: undefined }, 'str', null,
    ];
    for (const b of bads) expect(sanitizeTracked(b), JSON.stringify(b)?.slice(0, 60)).toBeNull();
  });
});
