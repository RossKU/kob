// The pair book checked against the node. A pair market or close order is an auction that starts
// at the best price of the pair book, as a KAS-quoted one starts at the best price of the KAS book
// (app/book-verify.ts). One indexer is not an authority for that price. The indexer's pair book
// (`/v1/pairs/{a}/{b}/book`) lists levels with no order behind them to check, so the book the
// planners get is built here from what the node confirms.
//
// - Resting pair orders are `KobPair` and `KobIfdPair` entries with no stop. The indexer lists
//   them under token A. Each is read one by one and must meet all of these.
//   - It exists unspent on the node at the script its state derives. This is the same
//     reconciliation as the KAS book (kob/node-verify.ts).
//   - It is an order of this pair: both tokens' covenant ids, template hashes and extension
//     commitments are the pair's.
//   - It is live now: expiry and activeFrom are checked against the node's DAA.
//   - It sits on the side it is listed on.
//   Its price and amount come from the confirmed state. An ask's amount is the A it holds, and a
//   bid's is what its confirmed B buys at its price. An auction's price is its value at the node's
//   DAA now (`priceNow`).
// - The route through the two KAS books is derived here from the two KAS books the node
//   confirmed (`readMarketPart`), never from the indexer's route levels.
//
// Rows are looked at best price first, BOOK_CHECK_PER_SIDE per side and round, as in the KAS
// book. Rows behind the verified depth are not part of the book. A node that cannot answer
// rejects with a `NodeError`, and the ticket then shows its node banner and plans nothing. An
// order the indexer does not list at all is not seen. The optional second indexer
// (`extraIndexerUrls`) cross-checks that start (plan.ts `marketStartIssues`).
import type { IndexerApi } from '../data/indexer';
import type { OrderView } from '../data/indexer-types';
import type { NodeApi } from '../data/node';
import { SnapshotError, type OrderSnapshot } from '../kob/cancel';
import { describeOrder, pairExtFor } from '../kob/order-facts';
import type { BookLevel, BookView, DroppedBookOrder } from '../kob/plan-types';
import type { Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { BOOK_CHECK_MAX_ROWS, BOOK_CHECK_MIN_CONFIRMED, BOOK_CHECK_PER_SIDE, BOOK_CHECK_ROUNDS, confirmOrders } from './book-verify';

const ZERO32 = '0'.repeat(64);
/** The contract kinds the pair book lists as resting liquidity: direct and entry orders. */
const BOOK_CONTRACTS = new Set(['KobPair', 'KobIfdPair']);

/** One token of the pair, as the registry (the app's pinned entry) defines it. */
export interface PairToken {
  covenantId: Hex;
  templateHash: Hex;
  /** KCC-20 extension commitment. All zeros for a KRON token. */
  extensionCommitment: Hex;
  /** Base units per whole token. */
  scale: bigint;
}

/** What `verifyPairBook` needs. */
export interface PairBookDeps {
  kob: KobWasm;
  node: Pick<NodeApi, 'getUtxosByAddresses'>;
  indexer: Pick<IndexerApi, 'order'>;
  toAddress: (spk: string) => string;
  /** A (base) and B (quote). */
  base: PairToken;
  quote: PairToken;
  /** The node's DAA now. An order past its expiry or before its activeFrom is no liquidity. */
  daa?: bigint;
  /**
   * Base units of A a side must offer in confirmed orders, which is the size being planned, before
   * it counts as verified. More rounds run while it is not covered.
   */
  depth?: bigint;
  timeoutMs?: number;
}

/** The result of `verifyPairBook`. */
export interface VerifiedPairBook {
  /**
   * B base units per whole A, asks ascending, bids descending. It holds the node-confirmed direct
   * orders, merged with the route of the two KAS books.
   */
  book: BookView;
  dropped: DroppedBookOrder[];
  /** Rows behind the verified depth, left out. */
  beyond: number;
}

interface Row {
  id: Hex;
  side: 'sell' | 'buy';
  price: bigint;
  key: { covenant_id: Hex; price: string; amount_left: string | null; cur_value: string | null };
}

const lower = (s: string | null | undefined): string => (s ?? '').toLowerCase();

/** The listed orders of the pair A/B that count as liquidity, as rows. Bad ones are skipped. */
function rowsOf(orders: readonly OrderView[], base: PairToken, quote: PairToken): Row[] {
  const out: Row[] = [];
  // One row per order. The indexer listing an order N times does not count it N times.
  const seen = new Set<string>();
  for (const o of orders) {
    const p = o.pair;
    if (!p || !BOOK_CONTRACTS.has(o.contract) || (o.status !== 'open' && o.status !== 'partial')) continue;
    if (seen.has(o.covenant_id)) continue;
    seen.add(o.covenant_id);
    if (lower(p.base) !== lower(base.covenantId) || lower(p.quote) !== lower(quote.covenantId)) continue;
    let price: bigint;
    try {
      price = BigInt(p.quote_now ?? p.price ?? '');
    } catch {
      continue;
    }
    out.push({
      id: o.covenant_id, side: p.side === 'ask' ? 'sell' : 'buy', price,
      key: { covenant_id: o.covenant_id, price: price.toString(), amount_left: p.amount_left ?? o.amount_left ?? null, cur_value: o.current?.value ?? null },
    });
  }
  return out;
}

type Verdict = DroppedBookOrder['reason'] | 'expired' | 'inactive' | { price: bigint; amount: bigint };

/**
 * Why the node, or the order itself, does not stand behind a pair order, or else the price and
 * amount it confirms. Pure given the node-confirmed snapshot.
 */
function judge(deps: PairBookDeps, row: Row, confirmed: OrderSnapshot | SnapshotError): Verdict {
  if (confirmed instanceof SnapshotError) return confirmed.code === 'node-mismatch' ? 'not-on-node' : 'unproven';
  try {
    const st = confirmed.order.state;
    const d = describeOrder(st, deps.kob);
    const p = d.pair;
    if (confirmed.covenantId !== row.id || !p) return 'changed';
    if (!(p.kind === 'KobPair' || (p.kind === 'KobIfdPair' && p.stop === 0n)) || p.side !== row.side) return 'changed';
    // The state the node confirms commits both tokens. A real, unspent order of another pair, or a
    // look-alike program or commitment, is not an order of this pair.
    for (const [f, t] of [[p.base, deps.base], [p.quote, deps.quote]] as const) {
      const ext = pairExtFor(st, f.covId);
      if (lower(f.covId) !== lower(t.covenantId) || lower(f.tplHash) !== lower(t.templateHash) || f.scale !== t.scale || (ext !== null && lower(ext) !== lower(t.extensionCommitment || ZERO32))) return 'other-market';
    }
    // What it holds is the token it sells: A for an ask, B for a bid.
    const held = row.side === 'sell' ? deps.base.covenantId : deps.quote.covenantId;
    if (confirmed.custody && lower(confirmed.custody.covenantId) !== lower(held)) return 'other-market';
    if (deps.daa !== undefined) {
      if (d.expiryDaa > 0n && deps.daa >= d.expiryDaa) return 'expired';
      if (d.activeFrom > deps.daa) return 'inactive';
    }
    // An auction's price is what it is at the node's DAA now, between its start and its end (the
    // covenant's `price(t)`). Ranking it by its far end would put a tiny ask at the head of the
    // book and start every market order there.
    const price = priceNow(d, p, row.side, deps.daa);
    if (price <= 0n) return 'changed';
    // What it holds is the node's: the state's escrow, no more than the custody UTXO the node
    // confirms. An order with no custody holds nothing.
    if (!confirmed.custody) return 'unproven';
    const onNode = BigInt(confirmed.custody.state.amount);
    const escrow = row.side === 'sell' ? p.escrowA : p.escrowB;
    const had = escrow < onNode ? escrow : onNode;
    const amount = row.side === 'sell' ? had : (had * deps.base.scale) / price;
    return amount > 0n ? { price, amount } : 'changed';
  } catch {
    return 'unproven';
  }
}

/**
 * The price of a pair order at `daa`. A decaying ask is
 * `max(priceEnd, price - slope * floor((daa - activeFrom) / decayStep))` and a rising bid is
 * `min(priceEnd, price + ...)` (KobPair.sil). An order that does not decay, or a call without a
 * clock, gets the stated price, which is the start and never a far end.
 */
function priceNow(d: ReturnType<typeof describeOrder>, p: NonNullable<ReturnType<typeof describeOrder>['pair']>, side: 'sell' | 'buy', daa: bigint | undefined): bigint {
  const a = d.auction;
  if (p.priceEnd === null || !a || a.slope <= 0n || a.decayStep <= 0n || a.interval > 0n || daa === undefined) return p.price;
  const moved = a.slope * ((daa > d.activeFrom ? daa - d.activeFrom : 0n) / a.decayStep);
  if (side === 'sell') {
    const now = p.price - moved;
    return now < p.priceEnd ? p.priceEnd : now;
  }
  const now = p.price + moved;
  return now > p.priceEnd ? p.priceEnd : now;
}

const dropOf = (row: Row, reason: DroppedBookOrder['reason']): DroppedBookOrder => ({ covenantId: row.id, side: row.side, price: row.price, reason });

/**
 * The route levels through the two KAS books, each the planners' view of a node-confirmed book.
 * Selling B into the bids of B and buying A at the asks of A is an ask of the pair. Selling A into
 * its bids and buying B at the asks of B is a bid. There is one level per side at the two touches,
 * indicative like the indexer's, with no fees and no minimum fills. Its amount is what the thinner
 * of the two touches allows. A side is empty when a touch is missing.
 */
export function routeLevels(a: BookView, b: BookView, scaleA: bigint, scaleB: bigint): { asks: BookLevel[]; bids: BookLevel[] } {
  const level = (price: bigint, amount: bigint): BookLevel[] => (price > 0n && amount > 0n ? [{ price, amount, orders: 2, routeOrders: 2, routeRows: 1 }] : []);
  const min = (x: bigint, y: bigint): bigint => (x < y ? x : y);
  const aAsk = a.asks[0];
  const bBid = b.bids[0];
  const aBid = a.bids[0];
  const bAsk = b.asks[0];
  let asks: BookLevel[] = [];
  let bids: BookLevel[] = [];
  if (aAsk && bBid && aAsk.price > 0n && bBid.price > 0n) {
    const price = (aAsk.price * scaleB + bBid.price - 1n) / bBid.price;
    asks = level(price, min(aAsk.amount, (((bBid.amount * bBid.price) / scaleB) * scaleA) / aAsk.price));
  }
  if (aBid && bAsk && aBid.price > 0n && bAsk.price > 0n) {
    const price = (aBid.price * scaleB) / bAsk.price;
    bids = level(price, min(aBid.amount, (((bAsk.amount * bAsk.price) / scaleB) * scaleA) / aBid.price));
  }
  return { asks, bids };
}

const mergeLevels = (levels: BookLevel[], side: 'sell' | 'buy'): BookLevel[] => {
  const out: BookLevel[] = [];
  for (const l of [...levels].sort((x, y) => (x.price === y.price ? 0 : (side === 'sell' ? x.price < y.price : x.price > y.price) ? -1 : 1))) {
    const last = out[out.length - 1];
    if (last && last.price === l.price) {
      last.amount += l.amount;
      last.orders += l.orders;
      if (l.routeOrders) {
        last.routeOrders = (last.routeOrders ?? 0) + l.routeOrders;
        last.routeRows = (last.routeRows ?? 0) + (l.routeRows ?? 1);
      }
    } else out.push({ ...l });
  }
  return out;
};

/**
 * The pair book the node confirms. `orders` are the pair orders the indexer lists under token A,
 * and rows of other pairs and kinds are ignored. `route` holds the two KAS books' route levels
 * (`routeLevels`). Rejects with a `NodeError` when the node cannot be asked in time.
 */
export async function verifyPairBook(deps: PairBookDeps, orders: readonly OrderView[], route: { asks: BookLevel[]; bids: BookLevel[] } = { asks: [], bids: [] }): Promise<VerifiedPairBook> {
  const all = rowsOf(orders, deps.base, deps.quote);
  const sides = (['sell', 'buy'] as const).map((side) => ({
    side,
    cands: all.filter((r) => r.side === side).sort((x, y) => (x.price === y.price ? 0 : (side === 'sell' ? x.price < y.price : x.price > y.price) ? -1 : 1)),
    next: 0,
    confirmed: 0,
    depth: 0n,
    levels: [] as BookLevel[],
  }));
  const dropped: DroppedBookOrder[] = [];
  const want = deps.depth ?? 0n;
  const satisfied = (s: (typeof sides)[number]): boolean =>
    s.next >= Math.min(s.cands.length, BOOK_CHECK_MAX_ROWS) || (s.confirmed >= BOOK_CHECK_MIN_CONFIRMED && s.depth >= want);
  for (let round = 0; round < BOOK_CHECK_ROUNDS; round++) {
    const batch = sides.flatMap((s) => (satisfied(s) ? [] : s.cands.slice(s.next, Math.min(s.next + BOOK_CHECK_PER_SIDE, BOOK_CHECK_MAX_ROWS)).map((row) => ({ s, row }))));
    if (!batch.length) break;
    for (const s of sides) if (!satisfied(s)) s.next = Math.min(s.next + BOOK_CHECK_PER_SIDE, BOOK_CHECK_MAX_ROWS);
    const got = await confirmOrders({ ...deps }, batch.map((b) => b.row.key));
    batch.forEach((b, i) => {
      const g = got[i];
      const v: Verdict = g ? judge(deps, b.row, g.confirmed) : 'unproven';
      if (typeof v === 'object') {
        b.s.confirmed++;
        b.s.depth += v.amount;
        b.s.levels.push({ price: v.price, amount: v.amount, orders: 1 });
      } else if (v !== 'expired' && v !== 'inactive') dropped.push(dropOf(b.row, v));
    });
  }
  const beyond = sides.reduce((n, s) => n + Math.max(0, s.cands.length - s.next), 0);
  const [a, b] = sides as [(typeof sides)[number], (typeof sides)[number]];
  // A side whose rows are all refused has only the route. The planners then find what the route
  // offers and no more.
  return { book: { asks: mergeLevels([...a.levels, ...route.asks], 'sell'), bids: mergeLevels([...b.levels, ...route.bids], 'buy') }, dropped, beyond };
}
