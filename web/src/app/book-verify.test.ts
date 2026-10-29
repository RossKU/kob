// The order book against the node (app/book-verify.ts): a phantom ask, a spent order, a changed amount or price, all present, the node down.
import { describe, expect, it } from 'vitest';
import type { BookOrderView, BookView, LevelView, OrderView } from '../data/indexer-types';
import { joinSpkString } from '../data/kaspa-sdk';
import { NodeError } from '../data/node-error';
import { describeOrder } from '../kob/order-facts';
import type { OrderSnapshot } from '../kob/cancel';
import { planOrder } from '../kob/plan';
import type { OrderState, TxJson } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { FakeChain, MAKER, orderViewOf, placeGolden, testAddress, TOKEN } from '../testing/chain-fixtures';
import { CLOCK, KAS, TOK, makeEnv } from '../testing/fixtures';
import { BOOK_CHECK_MAX_ROWS, BOOK_CHECK_PER_SIDE, BOOK_CHECK_ROUNDS, verifyBook, type BookVerifyDeps } from './book-verify';
import { readMarketPart } from './env';
import type { Services } from './services';

const kob = loadKobNode();
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

/** a snapshot of the golden ask with another covenant id, outpoint and (optionally) price: an order only an indexer could invent */
function forge(base: OrderSnapshot, n: number, price: bigint): OrderSnapshot {
  const txid = n.toString(16).padStart(2, '0').repeat(32);
  const st = clone(base.order.state) as OrderState & { state: Record<string, string> };
  st.state.price = price.toString();
  return {
    ...clone(base),
    covenantId: txid,
    order: { ...base.order, transactionId: txid, covenantId: txid, state: st },
    custody: base.custody ? { ...base.custody, transactionId: txid } : null,
  };
}

const priceOf = (s: OrderSnapshot): bigint => describeOrder(s.order.state, kob).price;

function rowOf(s: OrderSnapshot, over: Partial<BookOrderView> = {}): BookOrderView {
  const v = orderViewOf(kob, s);
  return {
    covenant_id: s.covenantId, contract: v.contract, maker: v.maker, price: priceOf(s).toString(), tip: v.tip, min_fill: v.min_fill, scale: v.scale,
    amount_left: v.amount_left, amount_estimated: false, status: 'open', cur_value: s.order.amount, expiry_daa: v.expiry_daa, expired: false, genesis_daa: 1000,
    confirmations: 100, settled: true, ...over,
  };
}

interface World {
  deps: BookVerifyDeps;
  chain: FakeChain;
  views: Map<string, OrderView | null>;
  real: OrderSnapshot;
  nodeCalls: () => number;
}

/** the golden ask is really on the chain; the indexer knows every snapshot it is given (`known`) */
function world(known: OrderSnapshot[] = []): World {
  const placed = placeGolden(kob, 'create.ask', MAKER);
  const real = placed.snapshots[0]!;
  const chain = new FakeChain();
  chain.applyTx(placed.signed.tx);
  const views = new Map<string, OrderView | null>([real, ...known].map((s) => [s.covenantId, orderViewOf(kob, s)]));
  const deps: BookVerifyDeps = { kob, node: chain, indexer: { order: async (id) => views.get(id) ?? null }, toAddress: testAddress };
  return { deps, chain, views, real, nodeCalls: () => chain.calls.length };
}

const book = (asks: (BookOrderView | LevelView)[], bids: (BookOrderView | LevelView)[] = []): BookView => ({ token: '', daa_score: 1, aggregated: false, asks, bids }) as unknown as BookView;
const ids = (rows: (BookOrderView | LevelView)[]): string[] => rows.map((r) => ('covenant_id' in r ? r.covenant_id : '?'));

describe('verifyBook: the orders of the book must exist on the node as the indexer lists them', () => {
  it('all present: the book is returned as it is, from ONE node read', async () => {
    const w = world();
    const v = book([rowOf(w.real)]);
    const out = await verifyBook(w.deps, v);
    expect(out.view).toBe(v);
    expect(out.dropped).toEqual([]);
    expect(w.nodeCalls()).toBe(1);
  });

  it('a phantom cheap ask (the node holds nothing at its script) is left out; the real ask behind it is the touch', async () => {
    const real0 = world().real;
    const phantom = forge(real0, 0xa1, priceOf(real0) / 2n);
    const w = world([phantom]);
    const out = await verifyBook(w.deps, book([rowOf(phantom), rowOf(w.real)]));
    expect(ids(out.view.asks)).toEqual([w.real.covenantId]);
    expect(out.dropped).toEqual([{ covenantId: phantom.covenantId, side: 'sell', price: priceOf(phantom), reason: 'not-on-node' }]);
    expect(w.nodeCalls()).toBe(1);
  });

  it('a spent order (its UTXO gone from the node) is left out', async () => {
    const w = world();
    // somebody spent it: the node no longer lists the order UTXO nor its custody
    const spend = { id: 'ee'.repeat(32), inputs: [{ transactionId: w.real.order.transactionId, index: w.real.order.index }, { transactionId: w.real.custody!.transactionId, index: w.real.custody!.index }], outputs: [] } as unknown as TxJson;
    w.chain.applyTx(spend);
    const out = await verifyBook(w.deps, book([rowOf(w.real)]));
    expect(out.view.asks).toEqual([]);
    expect(out.dropped.map((d) => d.reason)).toEqual(['not-on-node']);
  });

  it('a different amount: the node holds another value than the book row says', async () => {
    const w = world();
    // the order UTXO is there at its script, but holds less KAS than the row claims
    w.chain.add({ transactionId: w.real.order.transactionId, index: w.real.order.index, amount: '5', scriptPublicKey: kob.scriptPublicKey(w.real.order.state), covenantId: w.real.covenantId });
    const out = await verifyBook(w.deps, book([rowOf(w.real)]));
    expect(out.dropped.map((d) => d.reason)).toEqual(['changed']);
    expect(out.view.asks).toEqual([]);
  });

  it('a different token amount: the book row lists more than the custody holds', async () => {
    const w = world();
    const out = await verifyBook(w.deps, book([rowOf(w.real, { amount_left: (BigInt(w.real.custody!.state.amount) * 10n).toString() })]));
    expect(out.dropped.map((d) => d.reason)).toEqual(['changed']);
  });

  it('a different price: the row lists another price than the state the node confirms commits', async () => {
    const w = world();
    const out = await verifyBook(w.deps, book([rowOf(w.real, { price: (priceOf(w.real) / 4n).toString() })]));
    expect(out.dropped).toMatchObject([{ reason: 'changed', side: 'sell' }]);
    expect(out.view.asks).toEqual([]);
  });

  it('an order of the wrong side is refused (an ask listed among the bids)', async () => {
    const w = world();
    const out = await verifyBook(w.deps, book([], [rowOf(w.real)]));
    expect(out.dropped).toMatchObject([{ reason: 'changed', side: 'buy' }]);
  });

  it('an order the indexer cannot give a state for is refused (unproven); with nothing to check, the node is not asked', async () => {
    const w = world();
    w.views.set(w.real.covenantId, null);
    const out = await verifyBook(w.deps, book([rowOf(w.real)]));
    expect(out.dropped.map((d) => d.reason)).toEqual(['unproven']);
    expect(w.nodeCalls()).toBe(0);
  });

  it('a side whose checked orders are all refused goes on to the next ones, then is left out entirely (fail closed)', async () => {
    const real0 = world().real;
    const n = BOOK_CHECK_MAX_ROWS + 3;
    const phantoms = Array.from({ length: n }, (_, i) => forge(real0, 0x40 + i, priceOf(real0) / 2n + BigInt(i)));
    const w = world(phantoms);
    const out = await verifyBook(w.deps, book([...phantoms.map((p) => rowOf(p)), rowOf(w.real)]));
    expect(out.view.asks).toEqual([]);
    expect(out.dropped).toHaveLength(n + 1);
    expect(out.dropped.filter((d) => d.reason === 'unchecked')).toHaveLength(n + 1 - BOOK_CHECK_MAX_ROWS);
    expect(w.nodeCalls()).toBe(BOOK_CHECK_ROUNDS);
  });

  it('the real order within the first rounds is found behind several phantoms', async () => {
    const real0 = world().real;
    const phantoms = Array.from({ length: BOOK_CHECK_PER_SIDE }, (_, i) => forge(real0, 0xc0 + i, priceOf(real0) / 2n + BigInt(i)));
    const w = world(phantoms);
    const out = await verifyBook(w.deps, book([...phantoms.map((p) => rowOf(p)), rowOf(w.real)]));
    expect(ids(out.view.asks)).toEqual([w.real.covenantId]);
    expect(out.dropped.every((d) => d.reason === 'not-on-node')).toBe(true);
    expect(w.nodeCalls()).toBe(2);
  });

  it('a book listed as aggregated levels cannot be checked: that side is left out', async () => {
    const w = world();
    const level = { price: '100', amount: '5', amount_estimated: false, orders: 1, scale: 100_000_000 } satisfies LevelView;
    const out = await verifyBook(w.deps, book([level], [rowOf(w.real)]));
    expect(out.view.asks).toEqual([]);
    expect(out.dropped).toMatchObject([{ reason: 'unchecked', side: 'sell' }, { reason: 'changed', side: 'buy' }]);
  });

  it('an empty book asks nothing of the node', async () => {
    const w = world();
    const v = book([], []);
    expect((await verifyBook(w.deps, v)).view).toBe(v);
    expect(w.nodeCalls()).toBe(0);
  });

  it('the node down: rejects with a NodeError (unavailable), so the ticket shows its node banner and plans nothing', async () => {
    const w = world();
    const deps: BookVerifyDeps = { ...w.deps, node: { getUtxosByAddresses: async () => { throw new Error('WebSocket is not connected'); } } };
    const err = await verifyBook(deps, book([rowOf(w.real)])).catch((e) => e);
    expect(err).toBeInstanceOf(NodeError);
    expect(err.code).toBe('unavailable');
  });

  it('a node that does not answer in time is "unavailable" after the short timeout', async () => {
    const w = world();
    const deps: BookVerifyDeps = { ...w.deps, timeoutMs: 30, node: { getUtxosByAddresses: () => new Promise(() => undefined) } };
    const t0 = Date.now();
    const err = await verifyBook(deps, book([rowOf(w.real)])).catch((e) => e);
    expect(err).toBeInstanceOf(NodeError);
    expect(err.code).toBe('unavailable');
    expect(Date.now() - t0).toBeLessThan(2_000);
  });
});

describe('readMarketPart hands the planners the book the node confirms', () => {
  const sdk = {
    ScriptPublicKey: class { constructor(readonly version: number, readonly script: string) {} },
    addressFromScriptPublicKey: (s: { version: number; script: string }) => ({ toString: () => testAddress(joinSpkString(s.version, s.script)) }),
  };
  const market = { covenantId: TOKEN.covenantId, templateHash: TOKEN.templateHash, extensionCommitment: TOKEN.ext, scale: 1000n } as never;
  const services = (w: World, node: { getClock: () => Promise<unknown>; getUtxosByAddresses: (a: string[]) => Promise<never[]> } | FakeChain = w.chain, rows: BookOrderView[] = []): Pick<Services, 'indexer' | 'kob' | 'node' | 'sdk' | 'config'> =>
    ({
      kob, sdk, node, config: { network: 'testnet-10' },
      indexer: { book: async () => book(rows), trades: async () => null, order: async (id: string) => w.views.get(id) ?? null },
    }) as never;

  it('a phantom ask is not in the planners book; the real one is, and the part says what was left out', async () => {
    const real0 = world().real;
    const phantom = forge(real0, 0xd1, priceOf(real0) / 2n);
    const w = world([phantom]);
    const part = await readMarketPart(services(w, w.chain, [rowOf(phantom), rowOf(w.real)]), market);
    expect(part.book.asks.map((l) => l.price)).toEqual([priceOf(w.real)]);
    expect(part.dropped).toMatchObject([{ covenantId: phantom.covenantId, reason: 'not-on-node' }]);
    expect(part.bookUnavailable).toBe(false);
  });

  it('the node down rejects the read (nothing is planned on an unchecked book)', async () => {
    const w = world();
    const down = { getClock: async () => w.chain.getClock(), getUtxosByAddresses: async () => { throw new Error('connection refused'); } };
    await expect(readMarketPart(services(w, down, [rowOf(w.real)]), market)).rejects.toMatchObject({ name: 'NodeError', code: 'unavailable' });
  });
});

describe('a book that was cut tells the planners nothing to show', () => {
  const book2 = { asks: [{ price: 250_000_000n, amount: 50n * TOK, orders: 1 }], bids: [{ price: 245_000_000n, amount: 50n * TOK, orders: 1 }] };

  it('a market buy against a book the node check cut carries no note about it (the unconfirmed orders are simply not in the book)', () => {
    const p = planOrder({ ...makeEnv({ book: book2, funding: [100_000n * KAS] }), clock: CLOCK }, { type: 'market', side: 'buy', amount: 5n * TOK });
    expect(p.ok).toBe(true);
    expect(p.issues.map((i) => i.code).filter((c) => /BOOK/.test(c))).toEqual([]);
  });
});
