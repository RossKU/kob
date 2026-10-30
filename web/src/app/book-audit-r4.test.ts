// Audit 2026-10-30 round 4 regressions for the book against the node (W1, W2, W3): a decaying pair ask does not set the market start by its far end,
// duplicates and claimed amounts count only as far as the node confirms them, and a kept row's tip and minimum fill are the state's.
import { describe, expect, it } from 'vitest';
import type { OrderView, BookOrderView, BookView } from '../data/indexer-types';
import { describeOrder, pairFactsOf } from '../kob/order-facts';
import type { OrderSnapshot } from '../kob/cancel';
import { planOrder } from '../kob/plan';
import { makePairEnv } from '../kob/orders/pair-fixtures';
import type { OrderState } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { FakeChain, MAKER, orderViewOf, placeGolden, tokenUtxoView, testAddress, TOKEN } from '../testing/chain-fixtures';
import { verifyBook } from './book-verify';
import { verifyPairBook, type PairBookDeps, type PairToken } from './pair-book';

const kob = loadKobNode();
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const hex = (n: number): string => n.toString(16).padStart(4, '0').repeat(16);
const f = (v: unknown) => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? `${x}n` : x));

function viewOf(snap: OrderSnapshot): OrderView {
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
  } as unknown as OrderView;
}
const tokenOf = (t: NonNullable<ReturnType<typeof pairFactsOf>>['a']): PairToken => ({ covenantId: t.covId, templateHash: t.tplHash, extensionCommitment: t.ext ?? '00'.repeat(32), scale: t.scale });

function pairWorld() {
  const ask = placeGolden(kob, 'pair.create.ask', MAKER);
  const chain = new FakeChain();
  chain.applyTx(ask.signed.tx);
  const askSnap = ask.snapshots[0]!;
  const pf = pairFactsOf(askSnap.order.state)!;
  const views = new Map<string, OrderView>();
  const reg = (v: OrderView) => (views.set(v.covenant_id, v), v);
  const deps = (over: Partial<PairBookDeps> = {}): PairBookDeps => ({
    kob, node: chain, indexer: { order: async (id: string) => views.get(id) ?? null }, toAddress: testAddress, base: tokenOf(pf.a), quote: tokenOf(pf.b), daa: 5_000n, ...over,
  });
  return { ask, chain, askSnap, askView: reg(viewOf(askSnap)), pf, reg, deps };
}

/** A real pair ask moved to another covenant id with its state changed (the P2SH commits the state, so the node holds the changed one). */
function variant(w: ReturnType<typeof pairWorld>, id: number, patch: Record<string, string>): OrderView {
  const txid = hex(id);
  const st = clone(w.askSnap.order.state) as OrderState & { state: Record<string, string> };
  Object.assign(st.state, patch);
  const snap: OrderSnapshot = {
    ...clone(w.askSnap), covenantId: txid,
    order: { ...w.askSnap.order, transactionId: txid, covenantId: txid, state: st },
    custody: { ...w.askSnap.custody!, transactionId: txid },
  } as OrderSnapshot;
  w.chain.add({ transactionId: txid, index: snap.order.index, amount: snap.order.amount, scriptPublicKey: kob.scriptPublicKey(st), covenantId: txid });
  const c = snap.custody!;
  const cOut = (w.ask as any).signed.tx.outputs[c.index];
  w.chain.add({ transactionId: txid, index: c.index, amount: c.amount, scriptPublicKey: cOut.scriptPublicKey, covenantId: cOut.covenant?.covenantId ?? (c.covenantId as string) });
  return w.reg(viewOf(snap));
}

/** A real KAS ask on a fake chain, its order view and a book row with the given figures. */
function kasAsk(row: (v: ReturnType<typeof orderViewOf>, d: ReturnType<typeof describeOrder>, amountOnChain: string) => Record<string, unknown>, mutateView: (v: ReturnType<typeof orderViewOf>) => ReturnType<typeof orderViewOf> = (v) => v) {
  const ask = placeGolden(kob, 'create.ask', MAKER);
  const chain = new FakeChain();
  chain.applyTx(ask.signed.tx);
  const real = ask.snapshots[0]!;
  const v0 = orderViewOf(kob, real);
  const v = mutateView(clone(v0));
  const d = describeOrder(real.order.state, kob);
  const base = {
    covenant_id: real.covenantId, contract: v.contract, maker: v.maker, price: d.price.toString(), tip: v.tip, min_fill: v.min_fill, scale: v.scale,
    amount_left: v.amount_left, amount_estimated: false, status: 'open', cur_value: real.order.amount, expiry_daa: v.expiry_daa, expired: false, genesis_daa: 1000,
    confirmations: 100, settled: true,
  };
  const r = { ...base, ...row(v0, d, v0.amount_left ?? '0') } as unknown as BookOrderView;
  const verify = (rows: BookOrderView[], depth?: bigint) =>
    verifyBook(
      {
        kob, node: chain, indexer: { order: async (id: string) => (id === real.covenantId ? v : null) }, toAddress: testAddress,
        market: { covenantId: TOKEN.covenantId, templateHash: TOKEN.templateHash, extensionCommitment: TOKEN.ext }, daa: 5_000n, ...(depth !== undefined ? { depth } : {}),
      },
      { token: '', daa_score: 1, aggregated: false, asks: rows, bids: [] } as unknown as BookView,
    );
  return { r, d, v0, verify };
}

describe('W1: a decaying pair ask is ranked by its price now', () => {
  it('an ask that starts at 3x and decays to 1/100 slowly is not the touch of the book while it is still near its start', async () => {
    const w = pairWorld();
    const real = describeOrder(w.askSnap.order.state, kob).pair!;
    const decaying = variant(w, 0x78, {
      price: (real.price * 3n).toString(), priceEnd: (real.price / 100n).toString(), slope: '1', decayStep: '1000000', interval: '0',
    });
    const out = await verifyPairBook(w.deps(), [decaying, w.askView]);
    expect(out.dropped).toEqual([]);
    // the real ask is the best price; the decaying one is at about 3x now (its far end, 1/100, nobody can reach for millions of DAA)
    expect(out.book.asks.map((a) => a.price), f(out.book.asks)).toEqual([real.price, real.price * 3n]);
    const e = makePairEnv({ independentReference: false });
    const plan = planOrder({ ...e, book: out.book } as never, { type: 'market', side: 'buy', amount: 1000n });
    expect(plan.disclosure?.expectedPrice, f(plan.disclosure)).not.toBe(real.price / 100n);
  });

  it('an ask that has already decayed is at its price of now, clamped at its end; without a clock it is at its start', async () => {
    const w = pairWorld();
    const real = describeOrder(w.askSnap.order.state, kob).pair!;
    const fast = variant(w, 0x79, { price: (real.price * 2n).toString(), priceEnd: (real.price / 2n).toString(), slope: '100', decayStep: '1', interval: '0' });
    const out = await verifyPairBook(w.deps({ daa: 10_000_000n }), [fast]);
    expect(out.book.asks.map((a) => a.price)).toEqual([real.price / 2n]);
    const none = await verifyPairBook(w.deps({ daa: undefined }), [fast]);
    expect(none.book.asks.map((a) => a.price)).toEqual([real.price * 2n]);
  });
});

describe('W2: duplicates and claimed amounts count only as far as the node confirms them', () => {
  it('the same pair ask listed three times is one order', async () => {
    const w = pairWorld();
    const real = describeOrder(w.askSnap.order.state, kob).pair!;
    const out = await verifyPairBook(w.deps({ depth: real.escrowA * 3n }), [w.askView, w.askView, w.askView]);
    expect(out.book.asks).toEqual([{ price: real.price, amount: real.escrowA, orders: 1 }]);
  });

  it('a pair ask whose state names more A than its custody UTXO holds is worth the custody', async () => {
    const w = pairWorld();
    const real = describeOrder(w.askSnap.order.state, kob).pair!;
    const lying = variant(w, 0x77, { custody: (real.escrowA * 1000n).toString() });
    const out = await verifyPairBook(w.deps(), [lying]);
    expect(out.book.asks.every((a) => a.amount <= real.escrowA), f(out.book.asks)).toBe(true);
  });

  it('the same real KAS ask listed three times is one order', async () => {
    const k = kasAsk(() => ({}));
    const out = await k.verify([k.r, k.r, k.r], 3n * BigInt(k.v0.amount_left ?? '0'));
    expect(out.view.asks).toHaveLength(1);
  });

  it('a KAS ask the indexer lists without custody is not liquidity, whatever amount its row claims', async () => {
    const k = kasAsk(
      (v0) => ({ amount_left: (BigInt(v0.amount_left ?? '0') * 1000n).toString() }),
      (v) => ({ ...v, custody: null }) as unknown as typeof v,
    );
    const out = await k.verify([k.r]);
    expect(out.view.asks).toEqual([]);
    expect(out.dropped.map((x) => x.reason)).toEqual(['unproven']);
  });
});

describe('W3: a kept row carries the tip and the minimum fill of the confirmed state', () => {
  it("a real ask listed with another tip and minimum fill is kept with the state's", async () => {
    const k = kasAsk(() => ({ tip: '123456', min_fill: '1' }));
    const out = await k.verify([k.r]);
    const kept = out.view.asks[0] as BookOrderView;
    expect([kept.tip, kept.min_fill]).toEqual([k.d.tip.toString(), k.d.minFill.toString()]);
    expect(out.dropped).toEqual([]);
  });

  it('a book whose rows already match is returned as it came', async () => {
    const k = kasAsk((_v, d) => ({ tip: d.tip.toString(), min_fill: d.minFill.toString() }));
    const out = await k.verify([k.r]);
    expect(out.view.asks[0]).toBe(k.r);
  });
});
