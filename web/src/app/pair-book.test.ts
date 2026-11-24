// Tests the pair book against the node (app/pair-book.ts).
//
// A pair market or close order starts at the touch of the pair book. The indexer's pair levels
// have no order behind them. So the planners get a book built from the orders the node confirms
// and the route of the two KAS books. A phantom pair ask must not set the start of a pair market
// buy (3x the real price) without a node read.
import { describe, expect, it } from 'vitest';
import type { OrderView } from '../data/indexer-types';
import { joinSpkString } from '../data/kaspa-sdk';
import { describeOrder, pairFactsOf } from '../kob/order-facts';
import type { OrderSnapshot } from '../kob/cancel';
import { planOrder } from '../kob/plan';
import { isPairEnv, type BookView } from '../kob/plan-types';
import { parseRegistry } from '../kob/registry';
import { makePairEnv } from '../kob/orders/pair-fixtures';
import { loadKobNode } from '../kob/wasm.node';
import { FakeChain, MAKER, orderViewOf, placeGolden, tokenUtxoView, testAddress, tradableRegistryJson } from '../testing/chain-fixtures';
import { CLOCK } from '../testing/fixtures';
import { buildPairPlanEnv } from './env';
import { routeLevels, verifyPairBook, type PairBookDeps, type PairToken } from './pair-book';
import type { Services } from './services';

const kob = loadKobNode();
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

/** A pair order's indexer view, listed as the executor lists it under token A. */
function viewOf(snap: OrderSnapshot, over: Partial<OrderView> = {}): OrderView {
  const st = snap.order.state;
  const pf = pairFactsOf(st)!;
  const d = describeOrder(st, kob);
  const custody = snap.custody!;
  return {
    ...orderViewOf(kob, snap),
    contract: st.kind, status: 'open', token: pf.a.covId, price: null, quote: null,
    pair: {
      base: pf.a.covId, quote: pf.b.covId, base_family: 'kcc20', quote_family: 'kcc20', base_template_hash: pf.a.tplHash, quote_template_hash: pf.b.tplHash,
      base_scale: Number(pf.a.scale), quote_scale: Number(pf.b.scale), side: pf.side === 'sell' ? 'ask' : 'bid', price: d.pair!.price.toString(), price_num: null, price_den: null,
      quote_now: d.pair!.price.toString(), amount_left: d.pair!.escrowA.toString(), delivery_carrier: '0',
      custodies: [{ token: custody.covenantId!, role: pf.side === 'sell' ? 'base' : 'quote', expected_amount: custody.state.amount, utxo: tokenUtxoView(custody, 'custody'), ok: true }],
    },
    ...over,
  } as unknown as OrderView;
}

const tokenOf = (f: NonNullable<ReturnType<typeof pairFactsOf>>['a']): PairToken => ({ covenantId: f.covId, templateHash: f.tplHash, extensionCommitment: f.ext ?? '00'.repeat(32), scale: f.scale });

function world() {
  const ask = placeGolden(kob, 'pair.create.ask', MAKER);
  const bid = placeGolden(kob, 'pair.create.bid', MAKER);
  const chain = new FakeChain();
  chain.applyTx(ask.signed.tx);
  chain.applyTx(bid.signed.tx);
  const askSnap = ask.snapshots[0]!;
  const bidSnap = bid.snapshots[0]!;
  const pf = pairFactsOf(askSnap.order.state)!;
  const views = new Map<string, OrderView>();
  const reg = (v: OrderView) => (views.set(v.covenant_id, v), v);
  const deps = (over: Partial<PairBookDeps> = {}): PairBookDeps => ({
    kob, node: chain, indexer: { order: async (id: string) => views.get(id) ?? null }, toAddress: testAddress, base: tokenOf(pf.a), quote: tokenOf(pf.b), daa: 5_000n, ...over,
  });
  return { chain, askSnap, bidSnap, askView: reg(viewOf(askSnap)), bidView: reg(viewOf(bidSnap)), pf, reg, deps };
}

/** an order the indexer invents: a view with a state and custody, but no UTXO on any chain */
function phantomOf(w: ReturnType<typeof world>, n: number, price: bigint): OrderView {
  const id = n.toString(16).padStart(2, '0').repeat(32);
  const v = clone(w.askView) as OrderView & { pair: { price: string; quote_now: string } };
  v.covenant_id = id;
  v.pair.price = price.toString();
  v.pair.quote_now = price.toString();
  return w.reg(v);
}

describe('verifyPairBook: the orders of the pair book must exist on the node as the indexer lists them', () => {
  it('a real ask and a real bid are the book: prices from the confirmed state, amounts from what the orders hold', async () => {
    const w = world();
    const out = await verifyPairBook(w.deps(), [w.askView, w.bidView]);
    const ask = describeOrder(w.askSnap.order.state, kob).pair!;
    const bid = describeOrder(w.bidSnap.order.state, kob).pair!;
    expect(out.dropped).toEqual([]);
    expect(out.book.asks).toEqual([{ price: ask.price, amount: ask.escrowA, orders: 1 }]);
    expect(out.book.bids).toEqual([{ price: bid.price, amount: (bid.escrowB * w.pf.a.scale) / bid.price, orders: 1 }]);
  });

  it('a phantom ask at a better price is dropped (not on the node): the touch stays the real ask', async () => {
    const w = world();
    const real = describeOrder(w.askSnap.order.state, kob).pair!.price;
    const ghost = phantomOf(w, 0xc1, real / 2n);
    w.chain.calls.length = 0;
    const out = await verifyPairBook(w.deps(), [ghost, w.askView]);
    expect(out.dropped.map((d) => [d.covenantId, d.reason])).toEqual([[ghost.covenant_id, 'not-on-node']]);
    expect(out.book.asks.map((l) => l.price)).toEqual([real]);
    expect(w.chain.calls.length).toBe(1); // one node read for the batch
  });

  it('a phantom ask at a worse price (3x) is dropped too: it cannot lift the start of a market buy', async () => {
    const w = world();
    const real = describeOrder(w.askSnap.order.state, kob).pair!.price;
    const out = await verifyPairBook(w.deps(), [phantomOf(w, 0xc2, real * 3n)]);
    expect(out.book.asks).toEqual([]);
    expect(out.dropped.map((d) => d.reason)).toEqual(['not-on-node']);
  });

  it('a REAL order of another pair (the pair names other tokens than the market) is refused', async () => {
    const w = world();
    const other = tokenOf(w.pf.b);
    const out = await verifyPairBook(w.deps({ quote: { ...other, templateHash: 'ab'.repeat(32) } }), [w.askView]);
    expect(out.dropped.map((d) => d.reason)).toEqual(['other-market']);
    expect(out.book.asks).toEqual([]);
    const ext = await verifyPairBook(w.deps({ base: { ...tokenOf(w.pf.a), extensionCommitment: 'cd'.repeat(32) } }), [w.askView]);
    // An order whose state names no extension for the sold token is bound by the tokens' ids and
    // templates. One that names it must match.
    expect(ext.dropped.length === 0 || ext.dropped[0]!.reason === 'other-market').toBe(true);
  });

  it('an order of the pair listed on the other side than its state says is refused', async () => {
    const w = world();
    const flipped = clone(w.askView) as OrderView & { pair: { side: string } };
    flipped.pair.side = 'bid';
    const out = await verifyPairBook(w.deps(), [flipped]);
    expect(out.dropped.map((d) => d.reason)).toEqual(['changed']);
    expect(out.book).toEqual({ asks: [], bids: [] });
  });

  it('an expired order is no liquidity (left out without a note); the node clock decides, not the row', async () => {
    const w = world();
    const expiry = describeOrder(w.askSnap.order.state, kob).expiryDaa;
    if (expiry === 0n) return; // the golden ask never expires: nothing to test
    const out = await verifyPairBook(w.deps({ daa: expiry + 1n }), [w.askView]);
    expect(out.dropped).toEqual([]);
    expect(out.book.asks).toEqual([]);
  });

  it('a node that cannot answer rejects (the ticket then shows its node banner and plans nothing)', async () => {
    const w = world();
    const down = { getUtxosByAddresses: async () => { throw new Error('node down'); } };
    await expect(verifyPairBook(w.deps({ node: down }), [w.askView])).rejects.toMatchObject({ name: 'NodeError' });
  });

  it('rows of other pairs and of kinds that are not resting liquidity are ignored', async () => {
    const w = world();
    const cond = { ...clone(w.askView), contract: 'KobCondPair' } as OrderView;
    const swapped = clone(w.askView) as OrderView & { pair: { base: string; quote: string } };
    [swapped.pair.base, swapped.pair.quote] = [swapped.pair.quote, swapped.pair.base];
    w.chain.calls.length = 0;
    const out = await verifyPairBook(w.deps(), [cond, swapped]);
    expect(out.book).toEqual({ asks: [], bids: [] });
    expect(w.chain.calls.length).toBe(0);
  });

  it('an order read that fails or times out ahead of the confirmed touch marks the side unread, and is cancelled (R7B-L2)', async () => {
    const w = world();
    const asked: { signal?: AbortSignal; priority?: string }[] = [];
    const slow = w.deps({ orderTimeoutMs: 10, indexer: { order: (id: string, o?: { signal?: AbortSignal; priority?: string }) => (id === w.askView.covenant_id ? (asked.push(o ?? {}), new Promise(() => undefined)) : Promise.resolve(w.bidView)) } as never });
    const out = await verifyPairBook(slow, [w.askView, w.bidView]);
    expect(out.unread).toEqual(['sell']);
    expect(out.book.asks).toEqual([]);
    expect(asked[0]!.signal?.aborted).toBe(true);
    expect(asked[0]!.priority).toBe('high');
    // all read: nothing unread
    expect((await verifyPairBook(w.deps(), [w.askView, w.bidView])).unread).toEqual([]);
  });

  it('the route levels are merged with the direct ones at an equal price', async () => {
    const w = world();
    const price = describeOrder(w.askSnap.order.state, kob).pair!.price;
    const out = await verifyPairBook(w.deps(), [w.askView], { asks: [{ price, amount: 5n, orders: 2, routeOrders: 2, routeRows: 1 }], bids: [] });
    expect(out.book.asks).toHaveLength(1);
    expect(out.book.asks[0]).toMatchObject({ price, orders: 3, routeOrders: 2, routeRows: 1 });
  });
});

describe('routeLevels: the route of the pair is derived from the two node-confirmed KAS books', () => {
  const kasA: BookView = { asks: [{ price: 3_000_000_000n, amount: 400_000n, orders: 1 }], bids: [{ price: 2_900_000_000n, amount: 100_000n, orders: 1 }] };
  const kasB: BookView = { asks: [{ price: 210_000_000n, amount: 9_000_000_000n, orders: 1 }], bids: [{ price: 200_000_000n, amount: 50_000_000_000n, orders: 1 }] };
  it('an ask buys A at the asks of A with the KAS of B sold into the bids of B; a bid sells A into its bids and buys B at its asks', () => {
    // Scale A = 1000 (3 decimals), scale B = 10^8.
    // B per whole A is ceil(3e9 x 1e8 / 2e8) = 1.5e9 for the ask and floor(2.9e9 x 1e8 / 2.1e8)
    // for the bid.
    const r = routeLevels(kasA, kasB, 1000n, 100_000_000n);
    // The bids of B hold 1000 KAS of B. That is 33,333 base units of A at 3 KAS a whole A, less
    // than the 400,000 the asks of A offer.
    expect(r.asks).toEqual([{ price: 1_500_000_000n, amount: 33_333n, orders: 2, routeOrders: 2, routeRows: 1 }]);
    expect(r.bids[0]!.price).toBe((2_900_000_000n * 100_000_000n) / 210_000_000n);
  });
  it('its amount is what the thinner touch allows; a missing touch gives no level', () => {
    const thin: BookView = { asks: kasB.asks, bids: [{ price: 200_000_000n, amount: 1_000_000_000n, orders: 1 }] };
    // 10 B (1e9 base units) sold at 0.2 KAS = 20 KAS buys 666 base units of A at 3 KAS a whole A
    expect(routeLevels(kasA, thin, 1000n, 100_000_000n).asks[0]!.amount).toBe(666n);
    expect(routeLevels({ asks: [], bids: [] }, kasB, 1000n, 100_000_000n)).toEqual({ asks: [], bids: [] });
  });
});

describe('buildPairPlanEnv hands the planners the node-confirmed pair book (audit round 2, R1: PoC H8)', () => {
  const registry = parseRegistry(tradableRegistryJson(), { kob });
  const a = registry.tokens[0]!;
  const b = registry.tokens[1]!;
  const sdk = {
    ScriptPublicKey: class { constructor(readonly version: number, readonly script: string) {} },
    addressFromScriptPublicKey: (s: { version: number; script: string }) => ({ toString: () => testAddress(joinSpkString(s.version, s.script)) }),
  };
  const phantomAsk = { source: 'direct', price_num: '3000', price_den: '1', amount: '1000000', orders: 1 };
  const services = (getUtxosByAddresses: (a: string[]) => Promise<unknown>, listed: OrderView[]) =>
    ({
      kob, registry, sdk, node: { getClock: async () => ({ daa: CLOCK.daa, unixSeconds: CLOCK.unixSeconds, rateMilli: 10_000 }), getUtxosByAddresses },
      utxos: { fundingFor: async () => [] }, tracker: { tokenUtxosFor: async () => [] },
      indexer: {
        pairBook: async () => ({ base: a.covenantId, quote: b.covenantId, daa_score: 1, asks: [phantomAsk], bids: [] }),
        allOrders: async () => listed, book: async () => ({ asks: [], bids: [] }), trades: async () => null, order: async (id: string) => listed.find((o) => o.covenant_id === id) ?? null,
      },
      config: { features: {}, network: 'testnet-10' },
    }) as unknown as Services;

  it('a phantom pair ask no order of which exists is not the touch: a pair market buy finds no liquidity instead of starting at 3,000,000', async () => {
    const w = world();
    const ghost = clone(w.askView) as OrderView & { pair: { base: string; quote: string } };
    ghost.pair.base = a.covenantId;
    ghost.pair.quote = b.covenantId;
    const env = await buildPairPlanEnv(services((x) => new FakeChain().getUtxosByAddresses(x), [ghost]), { pubkey: MAKER.pk }, a, b);
    expect(isPairEnv(env)).toBe(true);
    expect(env.book.asks).toEqual([]);
    const real = makePairEnv({ independentReference: false });
    const plan = planOrder({ ...real, book: env.book } as never, { type: 'market', side: 'buy', amount: 5n * real.token.scale });
    expect(plan.ok).toBe(false);
    expect(plan.issues.map((i) => i.code)).toContain('NO_LIQUIDITY');
  });

  it('a pair ask whose order read failed, or a listing cut at its page bound, makes the book unavailable: a crossing pair limit is refused, not planned as resting (R7B-L2, R7B-L3)', async () => {
    const w = world();
    const ask = clone(w.askView) as OrderView & { pair: { base: string; quote: string } };
    ask.pair.base = a.covenantId;
    ask.pair.quote = b.covenantId;
    const chain = (x: string[]) => new FakeChain().getUtxosByAddresses(x);
    const failing = services(chain, [ask]);
    (failing.indexer as unknown as { order: () => Promise<never> }).order = async () => { throw new Error('The indexer did not answer within 10s.'); };
    const env1 = await buildPairPlanEnv(failing, { pubkey: MAKER.pk }, a, b);
    expect(env1.guardsUnavailable).toContain('book');
    // both token lists cut and neither holds the pair order the pair book counts: the book is unknown
    const kasBid = { ...clone(w.bidView), covenant_id: 'cc'.repeat(32), pair: null, contract: 'KobBid' } as unknown as OrderView;
    const cut = services(chain, Object.assign([kasBid], { truncated: true }) as OrderView[]);
    const env2 = await buildPairPlanEnv(cut, { pubkey: MAKER.pk }, a, b);
    expect(env2.guardsUnavailable).toContain('book');
    // the plan at the click carries the existing "could not read" error
    const real = makePairEnv({ independentReference: false });
    const plan = planOrder({ ...real, book: env1.book, guardsUnavailable: env1.guardsUnavailable } as never, { type: 'limit', side: 'buy', amount: 5n * real.token.scale, price: 1_000_000n });
    expect(plan.issues.filter((i) => i.severity === 'error').map((i) => i.code)).toContain('GUARDS_UNAVAILABLE');
    // read in full, nothing is unavailable for the book (the phantom is only left out)
    const env3 = await buildPairPlanEnv(services(chain, [ask]), { pubkey: MAKER.pk }, a, b);
    expect(env3.guardsUnavailable ?? []).not.toContain('book');
  });

  it('thousands of active orders of A do not shut the pair ticket: a complete list of B, or cut lists that hold the pair orders the book counts, are read (R8B-M2)', async () => {
    const w = world();
    const ask = clone(w.askView) as OrderView & { pair: { base: string; quote: string } };
    ask.pair.base = a.covenantId;
    ask.pair.quote = b.covenantId;
    const chain = (x: string[]) => new FakeChain().getUtxosByAddresses(x);
    const kasBids = Array.from({ length: 4_000 }, (_, i) => ({ ...clone(w.bidView), covenant_id: i.toString(16).padStart(64, '0'), pair: null, contract: 'KobBid' }) as unknown as OrderView);
    const svc = services(chain, []);
    // A: 4,000 KAS bids, cut; B: complete, with the pair ask
    (svc.indexer as unknown as { allOrders: (q: { token?: string }) => Promise<unknown> }).allOrders = async (q) =>
      q.token === a.covenantId ? Object.assign([...kasBids], { truncated: true }) : [ask];
    const env = await buildPairPlanEnv(svc, { pubkey: MAKER.pk }, a, b);
    expect(env.guardsUnavailable ?? []).not.toContain('book');
    // both cut, but what was read holds the one direct ask the pair book counts
    (svc.indexer as unknown as { allOrders: () => Promise<unknown> }).allOrders = async () => Object.assign([...kasBids.slice(0, 10), ask], { truncated: true });
    expect((await buildPairPlanEnv(svc, { pubkey: MAKER.pk }, a, b)).guardsUnavailable ?? []).not.toContain('book');
  });

  it('the pair route lists the pair orders whatever either token holds; more than it reads is said as too many, not "try again" (R9B-L4)', async () => {
    const w = world();
    const ask = clone(w.askView) as OrderView & { pair: { base: string; quote: string } };
    ask.pair.base = a.covenantId;
    ask.pair.quote = b.covenantId;
    const chain = (x: string[]) => new FakeChain().getUtxosByAddresses(x);
    const svc = services(chain, []);
    // both tokens cut by thousands of other orders: the token lists are not read at all
    (svc.indexer as unknown as { allOrders: () => Promise<unknown> }).allOrders = async () => { throw new Error('not asked'); };
    (svc.indexer as unknown as { allPairOrders: (x: string, y: string) => Promise<unknown> }).allPairOrders = async (x) => (x === a.covenantId ? [ask] : []);
    const env = await buildPairPlanEnv(svc, { pubkey: MAKER.pk }, a, b);
    expect(env.guardsUnavailable ?? []).not.toContain('book');
    (svc.indexer as unknown as { allPairOrders: () => Promise<unknown> }).allPairOrders = async () => Object.assign([ask], { truncated: true });
    const cut = await buildPairPlanEnv(svc, { pubkey: MAKER.pk }, a, b);
    expect(cut.guardsUnavailable).toEqual(expect.arrayContaining(['book', 'list-too-long']));
    const { chainTimeIssues: _c, ...plan } = await import('../kob/plan');
    const p = plan.planOrder(cut, { type: 'limit', side: 'sell', amount: 1n, price: 1n } as never);
    expect(p.issues.map((i) => i.code)).toContain('GUARDS_LIST_TOO_LONG');
  });

  it('a node that cannot be asked for the order check fails the build (the node banner), it is not skipped', async () => {
    const w = world();
    const ghost = clone(w.askView) as OrderView & { pair: { base: string; quote: string } };
    ghost.pair.base = a.covenantId;
    ghost.pair.quote = b.covenantId;
    await expect(buildPairPlanEnv(services(async () => { throw new Error('node down'); }, [ghost]), { pubkey: MAKER.pk }, a, b)).rejects.toMatchObject({ name: 'NodeError' });
  });
});
