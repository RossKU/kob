// The node check of the book binds what it confirms to THE MARKET (app/book-verify.ts, audit 2026-10-29 F2). A malicious indexer with no order of its own
// on the chain used to steer the start price of a market / close auction with REAL orders only: genuine asks listed dearest first, a real order of ANOTHER
// token, an expired one, or an inflated `amount_left` at the touch. Each of those is now refused or corrected; the touch is the best node-confirmed price.
import { describe, expect, it } from 'vitest';
import type { BookOrderView, BookView, OrderView } from '../data/indexer-types';
import { joinSpkString } from '../data/kaspa-sdk';
import { describeOrder } from '../kob/order-facts';
import type { OrderSnapshot } from '../kob/cancel';
import { planOrder } from '../kob/plan';
import type { OrderState } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { FakeChain, MAKER, orderViewOf, placeGolden, testAddress, TOKEN } from '../testing/chain-fixtures';
import { CLOCK, KAS, TOK, makeEnv } from '../testing/fixtures';
import { BOOK_CHECK_MIN_CONFIRMED, BOOK_CHECK_PER_SIDE } from './book-verify';
import { readMarketPart } from './env';
import type { Services } from './services';

const kob = loadKobNode();
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const priceOf = (s: OrderSnapshot): bigint => describeOrder(s.order.state, kob).price;

interface Over {
  price?: bigint;
  tokenCovId?: string;
  /** more fields of the order state (tokenTplHash, extensionCommitment, expiryDaa, activeFrom ...) */
  state?: Record<string, string>;
}

/** A REAL order on the chain (its order UTXO and custody exist unspent at the scripts their states derive), with another price / token / state. */
function realOrder(base: OrderSnapshot, n: number, over: Over, chain: FakeChain): OrderSnapshot {
  const txid = n.toString(16).padStart(2, '0').repeat(32);
  const st = clone(base.order.state) as OrderState & { state: Record<string, string> };
  if (over.price !== undefined) st.state.price = over.price.toString();
  if (over.tokenCovId !== undefined) st.state.tokenCovId = over.tokenCovId;
  Object.assign(st.state, over.state ?? {});
  const tokenCov = over.tokenCovId ?? TOKEN.covenantId;
  const snap: OrderSnapshot = {
    ...clone(base),
    covenantId: txid,
    order: { ...base.order, transactionId: txid, covenantId: txid, state: st },
    custody: { ...base.custody!, transactionId: txid, covenantId: tokenCov },
  };
  chain.add({ transactionId: txid, index: snap.order.index, amount: snap.order.amount, scriptPublicKey: kob.scriptPublicKey(st), covenantId: txid });
  chain.add({ transactionId: txid, index: snap.custody!.index, amount: snap.custody!.amount, scriptPublicKey: kob.tokenScriptPublicKey(TOKEN.program, snap.custody!.state), covenantId: tokenCov });
  return snap;
}

function rowOf(s: OrderSnapshot, over: Partial<BookOrderView> = {}): BookOrderView {
  const v = orderViewOf(kob, s);
  return {
    covenant_id: s.covenantId, contract: v.contract, maker: v.maker, price: priceOf(s).toString(), tip: v.tip, min_fill: v.min_fill, scale: v.scale,
    amount_left: v.amount_left, amount_estimated: false, status: 'open', cur_value: s.order.amount, expiry_daa: v.expiry_daa, expired: false, genesis_daa: 1000,
    confirmations: 100, settled: true, ...over,
  };
}

const sdk = {
  ScriptPublicKey: class { constructor(readonly version: number, readonly script: string) {} },
  addressFromScriptPublicKey: (s: { version: number; script: string }) => ({ toString: () => testAddress(joinSpkString(s.version, s.script)) }),
};

function world() {
  const placed = placeGolden(kob, 'create.ask', MAKER);
  const real = placed.snapshots[0]!;
  const chain = new FakeChain();
  chain.applyTx(placed.signed.tx);
  const views = new Map<string, OrderView>();
  const reg = (s: OrderSnapshot) => (views.set(s.covenantId, orderViewOf(kob, s)), s);
  reg(real);
  const market = { covenantId: TOKEN.covenantId, templateHash: TOKEN.templateHash, extensionCommitment: TOKEN.ext, scale: 1000n } as never;
  const services = (asks: BookOrderView[], bids: BookOrderView[] = []): Pick<Services, 'indexer' | 'kob' | 'node' | 'sdk' | 'config'> =>
    ({
      kob, sdk, node: chain, config: { network: 'testnet-10' },
      indexer: { book: async () => ({ token: '', daa_score: 1, aggregated: false, asks, bids }) as unknown as BookView, trades: async () => null, order: async (id: string) => views.get(id) ?? null },
    }) as never;
  return { real, chain, reg, market, services };
}

const buy = (book: { asks: { price: bigint; amount: bigint; orders: number }[]; bids: { price: bigint; amount: bigint; orders: number }[] }) =>
  planOrder({ ...makeEnv({ book, funding: [1_000_000n * KAS] }), clock: CLOCK }, { type: 'market', side: 'buy', amount: 5n * TOK });

describe('the node check binds the book to the market (audit 2026-10-29 F2)', () => {
  it('B1: asks listed dearest first: the book is sorted, the touch is the cheapest REAL ask, the auction starts there', async () => {
    const w = world();
    const p = priceOf(w.real);
    const mid = w.reg(realOrder(w.real, 0xa1, { price: p * 4n }, w.chain));
    const dear = w.reg(realOrder(w.real, 0xa2, { price: p * 10n }, w.chain));
    const part = await readMarketPart(w.services([rowOf(dear), rowOf(mid), rowOf(w.real)]), w.market);
    expect(part.dropped).toEqual([]);
    expect(part.book.asks.map((l) => l.price)).toEqual([p, p * 4n, p * 10n]);
    const plan = buy({ asks: part.book.asks, bids: [] });
    expect(plan.ok).toBe(true);
    expect(plan.disclosure!.expectedPrice).toBe(p);
  });

  it('B1b: orders at one price keep the indexer order', async () => {
    const w = world();
    const p = priceOf(w.real);
    // the golden ask stands in for a bid row only by side: use the ask side check of the wrong-side rule, and sort asks with equal prices stably
    const a = w.reg(realOrder(w.real, 0xa3, { price: p }, w.chain));
    const b = w.reg(realOrder(w.real, 0xa4, { price: p }, w.chain));
    const part = await readMarketPart(w.services([rowOf(b), rowOf(a), rowOf(w.real)]), w.market);
    expect(part.book.askOrders!.map((o) => o.covenantId)).toEqual([b.covenantId, a.covenantId, w.real.covenantId]);
  });

  it('B2: a real order of ANOTHER token is no order of this market', async () => {
    const w = world();
    const p = priceOf(w.real);
    const other = w.reg(realOrder(w.real, 0xb1, { price: p / 2n, tokenCovId: 'aa'.repeat(32) }, w.chain));
    const part = await readMarketPart(w.services([rowOf(other), rowOf(w.real)]), w.market);
    expect(part.dropped).toEqual([{ covenantId: other.covenantId, side: 'sell', price: priceOf(other), reason: 'other-market' }]);
    expect(part.book.asks.map((l) => l.price)).toEqual([p]);
  });

  it('B2b: the token covenant id of the market with another extension commitment is no order of this market; with another token template its custody does not fit it', async () => {
    const w = world();
    const p = priceOf(w.real);
    const tpl = w.reg(realOrder(w.real, 0xb2, { price: p / 2n, state: { tokenTplHash: 'cd'.repeat(32) } }, w.chain));
    const ext = w.reg(realOrder(w.real, 0xb3, { price: p / 3n, state: { extensionCommitment: '11'.repeat(32) } }, w.chain));
    const part = await readMarketPart(w.services([rowOf(ext), rowOf(tpl), rowOf(w.real)]), w.market);
    expect(part.dropped.map((d) => [d.covenantId, d.reason])).toEqual([[ext.covenantId, 'other-market'], [tpl.covenantId, 'not-on-node']]);
    expect(part.book.asks.map((l) => l.price)).toEqual([p]);
  });

  it('B3: an EXPIRED ask (a real UTXO nobody can fill) and one that is not yet active are no liquidity: the touch is the live ask', async () => {
    const w = world();
    const p = priceOf(w.real);
    const stale = w.reg(realOrder(w.real, 0xc1, { price: p / 2n, state: { expiryDaa: '1' } }, w.chain));
    const later = w.reg(realOrder(w.real, 0xc2, { price: p / 3n, state: { activeFrom: '9999999999' } }, w.chain));
    const part = await readMarketPart(w.services([rowOf(later), rowOf(stale, { expired: false }), rowOf(w.real)]), w.market);
    expect(part.book.asks.map((l) => l.price)).toEqual([p]);
    expect(part.dropped).toEqual([]); // not missing, just not liquidity: no note
  });

  it('B4: rows are checked best price first, so a phantom at half the price is looked at first and dropped, whatever its place in the list', async () => {
    const w = world();
    const p = priceOf(w.real);
    const r2 = w.reg(realOrder(w.real, 0xd1, { price: p + 100n }, w.chain));
    const r3 = w.reg(realOrder(w.real, 0xd2, { price: p + 200n }, w.chain));
    const ghost = realOrder(w.real, 0xd3, { price: p / 2n }, new FakeChain()); // exists on no chain
    const ghosts = [0xe1, 0xe2].map((n) => {
      const s = { ...clone(ghost), covenantId: n.toString(16).padStart(2, '0').repeat(32) };
      w.reg(s);
      return s;
    });
    const part = await readMarketPart(w.services([rowOf(w.real), rowOf(r2), rowOf(r3), ...ghosts.map((s) => rowOf(s))]), w.market);
    expect(part.dropped.map((d) => d.covenantId).sort()).toEqual(ghosts.map((s) => s.covenantId).sort());
    expect(part.book.askOrders!.map((o) => o.covenantId)).toEqual([w.real.covenantId, r2.covenantId, r3.covenantId]);
    expect(part.book.asks[0]!.price).toBe(p);
  });

  it('B4b: the depth checked is a minimum number of confirmed orders; a larger size asked for reads further', async () => {
    const w = world();
    const p = priceOf(w.real);
    const many = BOOK_CHECK_MIN_CONFIRMED + 3;
    const rows = Array.from({ length: many }, (_, i) => rowOf(w.reg(realOrder(w.real, 0x10 + i, { price: p + BigInt(i) + 1n }, w.chain))));
    expect(BOOK_CHECK_PER_SIDE).toBeLessThanOrEqual(BOOK_CHECK_MIN_CONFIRMED);
    const part = await readMarketPart(w.services([rowOf(w.real), ...rows]), w.market);
    // 1 + `many` rows listed; the verified depth is whole rounds of BOOK_CHECK_PER_SIDE rows until BOOK_CHECK_MIN_CONFIRMED are confirmed
    expect(part.book.askOrders!.length).toBe(Math.ceil(BOOK_CHECK_MIN_CONFIRMED / BOOK_CHECK_PER_SIDE) * BOOK_CHECK_PER_SIDE);
    expect(part.book.askOrders![0]!.covenantId).toBe(w.real.covenantId);
    // the size being planned asks for more than that: the check goes on until it is covered
    const deep = await readMarketPart(w.services([rowOf(w.real), ...rows]), w.market, { depth: 10n ** 30n });
    expect(deep.book.askOrders!.length).toBe(many + 1);
  });

  it('B5: a row marked amount_estimated does not carry the indexer\'s amount into the book: the custody the node confirms does', async () => {
    const w = world();
    const part = await readMarketPart(w.services([rowOf(w.real, { amount_estimated: true, amount_left: '1000000000000' })]), w.market);
    expect(part.dropped).toEqual([]);
    expect(part.book.asks[0]!.amount).toBe(BigInt(w.real.custody!.state.amount));
  });

  it('B6: the node answers with nothing (not synced / wrong network): fail closed, the side is empty', async () => {
    const w = world();
    const empty = new FakeChain();
    const svc = { ...w.services([rowOf(w.real)]), node: empty } as never;
    const part = await readMarketPart(svc, w.market);
    expect(part.book.asks).toEqual([]);
    expect(part.dropped.length).toBeGreaterThan(0);
  });
});
