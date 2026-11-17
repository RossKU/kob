// A curve sell order against the node (app/book-verify.ts): its rows (one per remaining level, one covenant id) are checked once; the levels its
// confirmed state has stay, each at its own price, in price order among the other asks; a level at another price or bigger than its state is not.
import { describe, expect, it } from 'vitest';
import type { BookOrderView, BookView, LevelView, OrderView } from '../data/indexer-types';
import type { OrderSnapshot } from '../kob/cancel';
import { curveFactsOf } from '../kob/order-facts';
import { loadKobNode } from '../kob/wasm.node';
import { FakeChain, MAKER, orderViewOf, placeGolden, testAddress } from '../testing/chain-fixtures';
import { verifyBook, type BookVerifyDeps } from './book-verify';

const kob = loadKobNode();

function world() {
  const placed = placeGolden(kob, 'create.curve', MAKER);
  const curve = placed.snapshots[0]!;
  const chain = new FakeChain();
  chain.applyTx(placed.signed.tx);
  const views = new Map<string, OrderView | null>([[curve.covenantId, orderViewOf(kob, curve, { expiry_daa: null })]]);
  const deps: BookVerifyDeps = { kob, node: chain, indexer: { order: async (id) => views.get(id) ?? null }, toAddress: testAddress };
  return { deps, curve, chain };
}

/** The curve's rows as the indexer lists them (`aggregate=false`): its first `n` levels. */
function rowsOf(s: OrderSnapshot, n: number, over: (l: number) => Partial<BookOrderView> = () => ({})): BookOrderView[] {
  const c = curveFactsOf(s.order.state)!;
  const v = orderViewOf(kob, s);
  return Array.from({ length: n }, (_, l) => ({
    covenant_id: s.covenantId, contract: 'KobCurveAsk', maker: v.maker, price: c.table[l]!.toString(), tip: v.tip, min_fill: v.min_fill, scale: v.scale,
    amount_left: c.levelSize.toString(), amount_estimated: false, status: 'open', cur_value: s.order.amount, expiry_daa: null, expired: false, genesis_daa: 1000,
    confirmations: 100, settled: true, curve_level: l, ...over(l),
  }));
}
const book = (asks: (BookOrderView | LevelView)[]): BookView => ({ token: '', daa_score: 1, aggregated: false, asks, bids: [] }) as unknown as BookView;

describe('verifyBook: a curve', () => {
  it('checks the order once and keeps every level its confirmed state has', async () => {
    const w = world();
    const rows = rowsOf(w.curve, 4);
    const out = await verifyBook(w.deps, book(rows));
    expect(out.dropped).toEqual([]);
    expect(out.view.asks.map((r) => (r as BookOrderView).curve_level)).toEqual([0, 1, 2, 3]);
    expect(w.chain.calls.length).toBe(1);
  });

  it('leaves out a level listed at another price or bigger than its state, and keeps price order', async () => {
    const w = world();
    const rows = rowsOf(w.curve, 4, (l) => (l === 1 ? { price: '1' } : l === 2 ? { amount_left: '999999999' } : {}));
    const out = await verifyBook(w.deps, book([rows[1]!, rows[0]!, rows[2]!, rows[3]!]));
    const kept = out.view.asks as BookOrderView[];
    expect(kept.map((r) => r.curve_level)).toEqual([0, 2, 3]);
    // the oversized level counts only its size in the state
    expect(kept[1]!.amount_left).toBe(curveFactsOf(w.curve.order.state)!.levelSize.toString());
  });
});
