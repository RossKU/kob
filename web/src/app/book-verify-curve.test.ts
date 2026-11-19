// A curve sell order against the node (app/book-verify.ts): its rows (one per remaining level, one
// covenant id) are checked once. The levels its confirmed state has stay, each at its own price, in
// price order among the other asks. A level at another price or bigger than its state is not.
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

describe('verifyBook: an unbound curve (made in its token genesis, its custody genesis output 0)', () => {
  it('is kept under its custody token, and is not this market\'s with a custody from another transaction', async () => {
    const { issueRaw } = await import('../kob/issue');
    const { curveTable } = await import('../kob/curve');
    const { snapshotOfRecovered, tokenUtxoView } = await import('../testing/chain-fixtures');
    const SCALE = 100_000_000n;
    const sell = 900n * SCALE;
    const t = curveTable(kob, { amount: sell, levels: 10, total: 100n * SCALE, multipleMilli: 5000n, scale: SCALE });
    const g = issueRaw(kob, {
      name: 'Curve Token', ticker: 'CRV', decimals: 8, supply: (1000n * SCALE).toString(),
      holders: [{ owner: '0'.repeat(64), ownerScheme: 4, amount: sell.toString() }, { owner: MAKER.pk, ownerScheme: 0, amount: (100n * SCALE).toString() }],
      funding: [{ transactionId: '07'.repeat(32), index: 1, amount: (500n * SCALE).toString(), blockDaaScore: '5000', pubkey: MAKER.pk }],
      network: 'testnet-10',
      curve: { custody: 0, levelSize: t.levelSize.toString(), table: t.table.map(String), minFill: '1' },
    } as never);
    const chain = new FakeChain();
    chain.applyTx(g.built.tx);
    const curve = snapshotOfRecovered(kob.recoverOrders(g.built.tx)[0]!);
    const token = g.token.covenantId;
    const st = curve.order.state.state as unknown as Record<string, string>;
    const market = { covenantId: token, templateHash: st.tokenTplHash!, extensionCommitment: st.extensionCommitment! };
    const viewWith = (custody: OrderSnapshot['custody']) =>
      orderViewOf(kob, curve, { side: 1, token, expiry_daa: null, custody: { expected_amount: curve.custody!.state.amount, utxo: tokenUtxoView(custody!, 'custody', { token: custody!.covenantId! }), ok: true } });
    const run = async (v: OrderView) => {
      const deps: BookVerifyDeps = { kob, node: chain, indexer: { order: async () => v }, toAddress: testAddress, market };
      return verifyBook(deps, book(rowsOf(curve, 2)));
    };
    const own = await run(viewWith({ ...curve.custody!, covenantId: token }));
    expect(own.dropped).toEqual([]);
    expect(own.view.asks).toHaveLength(2);
    // A look-alike at the custody's script in another transaction, of the market's token as the
    // indexer claims.
    const out0 = g.built.tx.outputs[0]!;
    chain.add({ transactionId: 'ab'.repeat(32), index: 0, amount: out0.value, scriptPublicKey: out0.scriptPublicKey, covenantId: token });
    const other = await run(viewWith({ ...curve.custody!, transactionId: 'ab'.repeat(32), index: 0, covenantId: token }));
    expect(other.view.asks).toHaveLength(0);
    expect(other.dropped.length).toBeGreaterThan(0);
  });
});
