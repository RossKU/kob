// The pair book against the NODE. A pair market or close order is an auction that starts at the best price of the pair book, as a KAS-quoted one starts
// at the best price of the KAS book (app/book-verify.ts): one indexer is not an authority for that price. The indexer's pair book lists levels
// (`/v1/pairs/{a}/{b}/book`) with no order behind them to check, so the book the planners get is built here from what the node confirms:
//
//   * resting pair orders (`KobPair`, and `KobIfdPair` entries with no stop): listed by the indexer under token A, read one by one, and each one must
//     exist unspent on the node at the script its state derives (the same reconciliation as the KAS book, kob/node-verify.ts), be an order of THIS pair
//     (both tokens' covenant ids, template hashes and extension commitments are the pair's), be live now (expiry and activeFrom against the node's DAA)
//     and sit on the side it is listed on. Its price and its amount come from the confirmed state (an ask: the A it holds; a bid: what its confirmed B
//     buys at its price); an auction's price is its most favourable end for the taker, so the start is never worse than the order can be.
//   * the route through the two KAS books: derived here from the two KAS books the node confirmed (`readMarketPart`), never from the indexer's route levels.
//
// Rows are looked at best price first, BOOK_CHECK_PER_SIDE per side and round, as in the KAS book; rows behind the verified depth are not part of the
// book. A node that cannot answer rejects with a `NodeError` (the ticket shows its node banner and plans nothing). An order the indexer does not list at
// all cannot be missed by a node: an optional second indexer (extraIndexerUrls) cross-checks that start, plan.ts marketStartIssues.
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
/** The kinds the pair book lists as resting liquidity (matcher.md / executor.md 5.4: `direct` and `entry`). */
const BOOK_CONTRACTS = new Set(['KobPair', 'KobIfdPair']);

/** One token of the pair, as the registry (the app's pinned entry) defines it. */
export interface PairToken {
  covenantId: Hex;
  templateHash: Hex;
  /** KCC-20 extension commitment; all zeros for a KRON token */
  extensionCommitment: Hex;
  /** base units per whole token */
  scale: bigint;
}

export interface PairBookDeps {
  kob: KobWasm;
  node: Pick<NodeApi, 'getUtxosByAddresses'>;
  indexer: Pick<IndexerApi, 'order'>;
  toAddress: (spk: string) => string;
  /** A (base) and B (quote) */
  base: PairToken;
  quote: PairToken;
  /** the node's DAA now: an order whose expiry has passed or whose activeFrom is still ahead is no liquidity (default: not applied) */
  daa?: bigint;
  /** base units of A a side must offer in confirmed orders (the size being planned) before it counts as verified; more rounds while it is not covered */
  depth?: bigint;
  timeoutMs?: number;
}

export interface VerifiedPairBook {
  /** B base units per whole A, asks ascending, bids descending: the node-confirmed direct orders, merged with the route of the two KAS books */
  book: BookView;
  dropped: DroppedBookOrder[];
  /** rows behind the verified depth, left out */
  beyond: number;
}

interface Row {
  id: Hex;
  side: 'sell' | 'buy';
  price: bigint;
  key: { covenant_id: Hex; price: string; amount_left: string | null; cur_value: string | null };
}

const lower = (s: string | null | undefined): string => (s ?? '').toLowerCase();

/** The listed pair orders of the pair A/B that the pair book counts as liquidity, as rows (a malformed one is skipped). */
function rowsOf(orders: readonly OrderView[], base: PairToken, quote: PairToken): Row[] {
  const out: Row[] = [];
  // one row per order: the indexer listing an order N times does not count it N times (audit 2026-10-30 W2)
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

/** Why the node (or the order itself) does not stand behind a pair order, or the price and amount it confirms. Pure given the node-confirmed snapshot. */
function judge(deps: PairBookDeps, row: Row, confirmed: OrderSnapshot | SnapshotError): Verdict {
  if (confirmed instanceof SnapshotError) return confirmed.code === 'node-mismatch' ? 'not-on-node' : 'unproven';
  try {
    const st = confirmed.order.state;
    const d = describeOrder(st, deps.kob);
    const p = d.pair;
    if (confirmed.covenantId !== row.id || !p) return 'changed';
    if (!(p.kind === 'KobPair' || (p.kind === 'KobIfdPair' && p.stop === 0n)) || p.side !== row.side) return 'changed';
    // the state the node confirms commits both tokens: a real, unspent order of ANOTHER pair (or a look-alike program / commitment) is not an order of this pair
    for (const [f, t] of [[p.base, deps.base], [p.quote, deps.quote]] as const) {
      const ext = pairExtFor(st, f.covId);
      if (lower(f.covId) !== lower(t.covenantId) || lower(f.tplHash) !== lower(t.templateHash) || f.scale !== t.scale || (ext !== null && lower(ext) !== lower(t.extensionCommitment || ZERO32))) return 'other-market';
    }
    // what it holds is the token it sells (an ask: A, a bid: B)
    const held = row.side === 'sell' ? deps.base.covenantId : deps.quote.covenantId;
    if (confirmed.custody && lower(confirmed.custody.covenantId) !== lower(held)) return 'other-market';
    if (deps.daa !== undefined) {
      if (d.expiryDaa > 0n && deps.daa >= d.expiryDaa) return 'expired';
      if (d.activeFrom > deps.daa) return 'inactive';
    }
    // an auction's price is what it is at the node's DAA now, between its start and its end (the covenant's `price(t)`): ranking it by its far end
    // (a decay to 1 sompi that nobody can reach for hours) put a tiny ask at the head of the book and started every market order there (W1)
    const price = priceNow(d, p, row.side, deps.daa);
    if (price <= 0n) return 'changed';
    // what it holds is the node's: the state's escrow, no more than the custody UTXO the node confirms, and an order with no custody holds nothing (W2)
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
 * The price of a pair order at `daa`: a decaying ask is `max(priceEnd, price - slope * floor((daa - activeFrom) / decayStep))`, a rising bid
 * `min(priceEnd, price + ...)` (KobPair.sil); the stated price when it does not decay, or without a clock (the start, never a far end).
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
 * The route levels through the two KAS books (each the planners' view of a node-confirmed book): selling B into the bids of B and buying A at the asks
 * of A is an ask of the pair, selling A into its bids and buying B at the asks of B a bid. One level per side at the two touches, indicative like the
 * indexer's (no fees, no minimum fills); its amount is what the thinner of the two touches allows. Empty when a touch is missing.
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
 * The pair book the node confirms. `orders` are the pair orders the indexer lists under token A (rows of other pairs and kinds are ignored), `route`
 * the two KAS books' route levels (`routeLevels`). Rejects with a `NodeError` when the node cannot be asked in time.
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
  // a side whose rows are all refused has only the route (and the planners then find what the route offers, no more)
  return { book: { asks: mergeLevels([...a.levels, ...route.asks], 'sell'), bids: mergeLevels([...b.levels, ...route.bids], 'buy') }, dropped, beyond };
}
