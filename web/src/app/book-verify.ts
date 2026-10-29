// The order book against the NODE. A market or close order (and any order that takes liquidity now) is an auction that starts at the best price of
// the indexer's book. One indexer is not an authority for that price: a dishonest or compromised one can list an ask that does not exist (a "phantom"
// cheap ask the order would start from) or hide the real ones. The P2SH of an order commits its whole state (price, amount, owner ...), so an order
// UTXO that EXISTS UNSPENT on the node at the script derived from the state the indexer reported proves that state: the touch of the book is real.
//
// `verifyBook` sorts each side by price (best first: the indexer's order is nobody's word) and checks the best orders in batched node reads
// (kob/node-verify.ts `confirmSnapshotsOnNode`, the same reconciliation cancel and amend use), BOOK_CHECK_PER_SIDE per side and round, until
// BOOK_CHECK_MIN_CONFIRMED orders of the side are confirmed (and the size the caller asks for is covered), at most BOOK_CHECK_MAX_ROWS rows. It
// returns the book without the orders the node does not confirm, with a list of what was left out and why. Beside "the order exists" it binds the
// confirmed state to THE MARKET: the order's token covenant id, token template hash and extension commitment are the market's, and an order that has
// expired or is not yet active is no liquidity. The touch of a side is therefore the best price the node confirms among the rows the indexer lists
// (an order the indexer does not list at all cannot be missed by a node: an optional second indexer, config extraIndexerUrls, cross-checks that start).
// When none of the rows of a side is confirmed, the whole side is left out (fail closed): the planner then finds no liquidity rather than a price
// nobody can verify. Rows behind the verified depth are NOT part of the book the planners get: they are the indexer's word, and what the planners derive
// from the whole book (the signed minimum fill, the fills budget, the depth warning, the cost preview) must not follow it. A caller that knows the size it
// plans passes `depth` to have more rows verified. A bid's amount is bounded by what its node-confirmed KAS can buy at its price. Nothing is acknowledged by the user and nothing is shown
// when every order is confirmed. A node that cannot answer in time rejects with a `NodeError` (`unavailable`): the ticket shows its node banner and
// plans nothing.
import type { IndexerApi } from '../data/indexer';
import type { BookOrderView, BookView, LevelView, OrderView } from '../data/indexer-types';
import type { NodeApi } from '../data/node';
import { NodeError, describeNodeError } from '../data/node-error';
import { SnapshotError, snapshotFromOrderView, type OrderSnapshot } from '../kob/cancel';
import { confirmSnapshotsOnNode } from '../kob/node-verify';
import { describeOrder } from '../kob/order-facts';
import type { DroppedBookOrder } from '../kob/plan-types';
import type { Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';

const ZERO32 = '0'.repeat(64);

/** Rows read per side and round (best price first). */
export const BOOK_CHECK_PER_SIDE = 3;
/** A side is verified once this many of its orders are confirmed in the first round (more rounds while the size asked for in `BookVerifyDeps.depth` is not covered). Each round is one more answer of the indexer in a row: raising this costs ticket latency with a slow indexer (e2e ticket-latency). */
export const BOOK_CHECK_MIN_CONFIRMED = 3;
/** Most rows looked at per side, refused ones included: a side with none confirmed among them is left out. */
export const BOOK_CHECK_MAX_ROWS = 18;
/** Rounds: BOOK_CHECK_MAX_ROWS / BOOK_CHECK_PER_SIDE. */
export const BOOK_CHECK_ROUNDS = BOOK_CHECK_MAX_ROWS / BOOK_CHECK_PER_SIDE;
/** The whole check of one round (the order reads and the node read) is given this long; the node not answering by then is `unavailable`. */
export const BOOK_CHECK_TIMEOUT_MS = 5_000;
/** An order read from the indexer is reused this long while the book row it belongs to (price, amount, value) is unchanged (a stale one only fails the node check below: the order leaves the book, never enters it). */
const ORDER_VIEW_TTL_MS = 120_000;

export interface BookVerifyDeps {
  kob: KobWasm;
  node: Pick<NodeApi, 'getUtxosByAddresses'>;
  indexer: Pick<IndexerApi, 'order'>;
  /** script public key -> address of the app's network (the node is asked by address) */
  toAddress: (spk: string) => string;
  /** a row of another scale than the market's is not part of the book the planners use: not checked (default: all rows count) */
  inScale?: (row: BookOrderView) => boolean;
  /** the market the book belongs to: a confirmed order must be an order of THIS token (default: not bound, tests only) */
  market?: BookMarket;
  /** the node's DAA now: an order whose expiry has passed or whose activeFrom is still ahead is no liquidity (default: not applied) */
  daa?: bigint;
  /** token base units a side must offer in confirmed orders (the size of the order being planned) before it counts as verified */
  depth?: bigint;
  timeoutMs?: number;
}

/** What an order of the market commits: the token it sells or buys. */
export interface BookMarket {
  covenantId: Hex;
  templateHash: Hex;
  /** KCC-20 extension commitment; all zeros for a KRON token */
  extensionCommitment: Hex;
}

export interface VerifiedBook {
  /** the book without the orders the node does not confirm and without the rows behind the verified depth (the same object when nothing was left out) */
  view: BookView;
  dropped: DroppedBookOrder[];
  /** rows behind the verified depth, left out of `view` (never looked at) */
  beyond: number;
}

const isOrderRow = (r: LevelView | BookOrderView): r is BookOrderView => 'covenant_id' in r;

export const settle = <T,>(p: Promise<T>, ms: number, onTimeout: () => Error): Promise<T> => {
  let h: ReturnType<typeof setTimeout>;
  const limit = new Promise<never>((_, rej) => {
    h = setTimeout(() => rej(onTimeout()), ms);
  });
  return Promise.race([p, limit]).finally(() => clearTimeout(h));
};

// ------------------------------------------------------------------------------------------------ order reads

const viewCache = new WeakMap<object, Map<string, { at: number; p: Promise<OrderView | null> }>>();

/** The indexer's full view of a book row's order (its decoded state and current outpoint), `null` when it cannot be read; reused while the row is unchanged. */
function readOrder(ix: Pick<IndexerApi, 'order'>, row: OrderKey, ms: number): Promise<OrderView | null> {
  const cache = viewCache.get(ix) ?? new Map<string, { at: number; p: Promise<OrderView | null> }>();
  viewCache.set(ix, cache);
  const key = `${row.covenant_id}|${row.price}|${row.amount_left}|${row.cur_value}`;
  const hit = cache.get(key);
  if (hit && Date.now() - hit.at < ORDER_VIEW_TTL_MS) return hit.p;
  const p = settle(ix.order(row.covenant_id), ms, () => new Error('order read timed out')).catch(() => null);
  cache.set(key, { at: Date.now(), p });
  void p.then((v) => {
    if (v === null && cache.get(key)?.p === p) cache.delete(key);
  });
  if (cache.size > 64) for (const k of [...cache.keys()].slice(0, cache.size - 64)) cache.delete(k);
  return p;
}

// ------------------------------------------------------------------------------------------------ one batch

/** Why the node (or the order itself) does not stand behind a book row, `null` when it does. Pure given the node-confirmed snapshot. */
function judge(
  deps: Pick<BookVerifyDeps, 'kob' | 'market' | 'daa'>,
  row: BookOrderView,
  side: 'sell' | 'buy',
  claimed: OrderSnapshot,
  confirmed: OrderSnapshot | SnapshotError,
): Verdict {
  if (confirmed instanceof SnapshotError) return confirmed.code === 'node-mismatch' ? 'not-on-node' : 'unproven';
  try {
    const d = describeOrder(confirmed.order.state, deps.kob);
    if (confirmed.covenantId !== row.covenant_id || d.side !== side || d.price !== BigInt(row.price)) return 'changed';
    const m = deps.market;
    if (m) {
      // the state the node confirms commits the token: a real, unspent order of ANOTHER token (or of a look-alike program / commitment of this
      // covenant id) is not an order of this market, whatever the indexer lists it under
      const st = confirmed.order.state.state as unknown as Record<string, string | undefined>;
      if (
        d.tokenCovId?.toLowerCase() !== m.covenantId.toLowerCase() ||
        (st.tokenTplHash ?? '').toLowerCase() !== m.templateHash.toLowerCase() ||
        (st.extensionCommitment ?? ZERO32).toLowerCase() !== m.extensionCommitment.toLowerCase() ||
        (confirmed.custody && confirmed.custody.covenantId?.toLowerCase() !== m.covenantId.toLowerCase())
      ) {
        return 'other-market';
      }
    }
    if (deps.daa !== undefined) {
      // the confirmed state's own clock, not the row's `expired` flag
      if (d.expiryDaa > 0n && deps.daa >= d.expiryDaa) return 'expired';
      if (d.activeFrom > deps.daa) return 'inactive';
    }
    if (row.scale !== null && row.scale !== undefined && BigInt(row.scale) !== d.scale) return 'changed';
    // the KAS value of the order UTXO is the node's; so are the tokens of an ask's custody (its script commits the state that holds the amount): the
    // book row's claim must be the same
    const value = row.cur_value ?? claimed.order.amount;
    if (BigInt(value) !== BigInt(confirmed.order.amount)) return 'changed';
    if (confirmed.custody) {
      const left = !row.amount_estimated && row.amount_left != null ? row.amount_left : claimed.custody?.state.amount;
      if (left !== undefined && BigInt(left) !== BigInt(confirmed.custody.state.amount)) return 'changed';
    }
    return { amount: confirmed.custody ? BigInt(confirmed.custody.state.amount) : null, cap: d.price > 0n ? (BigInt(confirmed.order.amount) * d.scale) / d.price : 0n };
  } catch {
    return 'unproven';
  }
}

/**
 * The verdict on one row: why it is not part of the book, or what the node confirmed of it: an ask's custody amount (the node's, not the indexer's);
 * a bid has no custody, its `cap` is what its node-confirmed KAS buys at its price at most (the row's own amount is only believed up to it).
 */
type Verdict = DroppedBookOrder['reason'] | 'expired' | 'inactive' | { amount: bigint | null; cap: bigint };

/** A book row's identity for the order-view cache and the read: the covenant id and the row facts the cache key follows. */
export type OrderKey = Pick<BookOrderView, 'covenant_id' | 'price' | 'amount_left' | 'cur_value'>;

/**
 * The indexer's view of each order (a snapshot of the state it claims) and the node's answer for it, the node read being ONE call for all of them
 * (`confirmed` is a `SnapshotError` for an order the node does not confirm). `null`: no usable view of the order. Rejects only when the node cannot be asked.
 */
export async function confirmOrders(
  deps: Pick<BookVerifyDeps, 'kob' | 'node' | 'indexer' | 'toAddress' | 'timeoutMs'>,
  keys: readonly OrderKey[],
): Promise<({ claimed: OrderSnapshot; confirmed: OrderSnapshot | SnapshotError } | null)[]> {
  const ms = deps.timeoutMs ?? BOOK_CHECK_TIMEOUT_MS;
  const views = await Promise.all(keys.map((k) => readOrder(deps.indexer, k, ms)));
  const claimed = views.map((v, i): OrderSnapshot | null => {
    if (!v || v.covenant_id !== keys[i]!.covenant_id) return null;
    try {
      // only the order and its custody count: strays do not change what a taker meets
      const { foreign: _foreign, foreignUnproven: _unproven, ...snap } = snapshotFromOrderView(v);
      return { ...snap, strays: [] };
    } catch {
      return null;
    }
  });
  const toCheck = claimed.flatMap((s, i) => (s ? [{ s, i }] : []));
  let confirmed: (OrderSnapshot | SnapshotError)[];
  try {
    confirmed = await settle(
      confirmSnapshotsOnNode(deps.kob, deps.node, toCheck.map((c) => c.s), deps.toAddress),
      ms,
      () => new NodeError('unavailable', `The node did not answer the order check within ${Math.round(ms / 1000)}s.`),
    );
  } catch (e) {
    throw describeNodeError(e, 'read');
  }
  const out: ({ claimed: OrderSnapshot; confirmed: OrderSnapshot | SnapshotError } | null)[] = keys.map(() => null);
  toCheck.forEach((c, k) => {
    out[c.i] = { claimed: c.s, confirmed: confirmed[k]! };
  });
  return out;
}

/** The verdict of each row. Rejects only when the node cannot be asked. */
async function checkRows(deps: BookVerifyDeps, rows: readonly { row: BookOrderView; side: 'sell' | 'buy' }[]): Promise<Verdict[]> {
  const got = await confirmOrders(deps, rows.map((r) => r.row));
  return got.map((g, i) => (g ? judge(deps, rows[i]!.row, rows[i]!.side, g.claimed, g.confirmed) : 'unproven'));
}

// ------------------------------------------------------------------------------------------------ the book

const dropOf = (row: LevelView | BookOrderView, side: 'sell' | 'buy', reason: DroppedBookOrder['reason']): DroppedBookOrder => {
  let price = 0n;
  try {
    price = BigInt(row.price);
  } catch {
    /* an unreadable price stays 0 */
  }
  return { covenantId: isOrderRow(row) ? row.covenant_id : ('' as Hex), side, price, reason };
};

/** A price as a number for sorting; an unreadable one sorts last. */
const priceKey = (row: BookOrderView): bigint | null => {
  try {
    return BigInt(row.price);
  } catch {
    return null;
  }
};

/**
 * The book (`aggregate=false`: one row per order) with the orders the node does not confirm left out, each side sorted best price first (asks
 * ascending, bids descending, the indexer's order within a price). The best BOOK_CHECK_PER_SIDE rows of each side are checked together in one node
 * read; a side goes on with the next ones until BOOK_CHECK_MIN_CONFIRMED orders (and `deps.depth` token units) are confirmed or BOOK_CHECK_MAX_ROWS
 * rows were looked at, and is left out entirely when none is confirmed. Rows behind the verified depth are left out of the view (`beyond`). A side listed as aggregated levels
 * cannot be checked and is left out too. Rejects with a `NodeError` when the node cannot be asked in time.
 */
export async function verifyBook(deps: BookVerifyDeps, view: BookView): Promise<VerifiedBook> {
  const inScale = deps.inScale ?? (() => true);
  const sides = (['sell', 'buy'] as const).map((side) => {
    const rows = side === 'sell' ? view.asks : view.bids;
    const cands = rows
      .filter(isOrderRow)
      .filter(inScale)
      .map((row, at) => ({ row, at, key: priceKey(row) }))
      .sort((a, b) => {
        if (a.key === b.key) return a.at - b.at;
        if (a.key === null) return 1;
        if (b.key === null) return -1;
        return (side === 'sell' ? a.key < b.key : a.key > b.key) ? -1 : 1;
      })
      .map((c) => c.row);
    return { side, rows, levels: rows.length > 0 && !rows.every(isOrderRow), cands, next: 0, confirmed: 0, depth: 0n, kept: [] as BookOrderView[] };
  });
  const dropped: DroppedBookOrder[] = [];
  for (const s of sides) {
    if (!s.levels) continue;
    for (const r of s.rows) dropped.push(dropOf(r, s.side, 'unchecked'));
  }
  const want = deps.depth ?? 0n;
  const satisfied = (s: (typeof sides)[number]): boolean =>
    s.levels || s.next >= Math.min(s.cands.length, BOOK_CHECK_MAX_ROWS) || (s.confirmed >= BOOK_CHECK_MIN_CONFIRMED && s.depth >= want);
  for (let round = 0; round < BOOK_CHECK_ROUNDS; round++) {
    const batch = sides.flatMap((s) => (satisfied(s) ? [] : s.cands.slice(s.next, Math.min(s.next + BOOK_CHECK_PER_SIDE, BOOK_CHECK_MAX_ROWS)).map((row) => ({ s, row }))));
    if (!batch.length) break;
    for (const s of sides) if (!satisfied(s)) s.next = Math.min(s.next + BOOK_CHECK_PER_SIDE, BOOK_CHECK_MAX_ROWS);
    const verdicts = await checkRows(deps, batch.map((b) => ({ row: b.row, side: b.s.side })));
    batch.forEach((b, i) => {
      const v = verdicts[i]!;
      if (typeof v === 'object') {
        // an ask's amount is the custody the node holds; the row's own word counts only where the order has no custody to read (a bid), and then
        // only up to what the node-confirmed KAS of the bid can buy at its price
        const claimed = b.row.amount_left != null ? BigInt(b.row.amount_left) : 1n;
        const amount = v.amount ?? (claimed < v.cap ? claimed : v.cap);
        b.s.confirmed++;
        b.s.depth += amount;
        b.s.kept.push(
          v.amount !== null ? (b.row.amount_estimated ? { ...b.row, amount_left: v.amount.toString(), amount_estimated: false } : b.row) : amount !== claimed ? { ...b.row, amount_left: amount.toString() } : b.row,
        );
        return;
      }
      // an order that is over or not yet live is no liquidity but is not a missing one either: it leaves the book without a note
      if (v !== 'expired' && v !== 'inactive') dropped.push(dropOf(b.row, b.s.side, v));
    });
  }
  // a side with nothing confirmed after the last round: its remaining orders are not trusted either
  for (const s of sides) {
    if (s.levels || s.confirmed > 0) continue;
    for (const row of s.cands.slice(s.next)) dropped.push(dropOf(row, s.side, 'unchecked'));
  }
  // the checked book: the confirmed rows in price order; the rows never looked at are the indexer's word and stay out (a side with nothing confirmed has none: all of them were dropped)
  const same = (rows: readonly (LevelView | BookOrderView)[], kept: readonly BookOrderView[]) => rows.length === kept.length && rows.every((r, i) => r === kept[i]);
  const [a, b] = sides as [(typeof sides)[number], (typeof sides)[number]];
  const rowsOf = (x: typeof a): BookOrderView[] => (x.levels || x.confirmed === 0 ? [] : x.kept);
  const beyondOf = (x: typeof a): number => (x.levels || x.confirmed === 0 ? 0 : Math.max(0, x.cands.length - x.next));
  const asks = rowsOf(a);
  const bids = rowsOf(b);
  const beyond = beyondOf(a) + beyondOf(b);
  if (!dropped.length && !beyond && same(view.asks, asks) && same(view.bids, bids)) return { view, dropped, beyond };
  return { view: { ...view, asks, bids }, dropped, beyond };
}
