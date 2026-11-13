// The order book checked against the node. A market or close order, or any order that takes
// liquidity now, is an auction that starts at the best price of the indexer's book. One indexer is
// not an authority for that price. A dishonest or compromised one can list an ask that does not
// exist, a phantom cheap ask the order would start from, or hide the real ones.
//
// The P2SH of an order commits its whole state: price, amount, owner and so on. An order UTXO that
// exists unspent on the node at the script derived from the reported state therefore proves that
// state, and the touch of the book is real.
//
// `verifyBook` sorts each side by price, best first, because the indexer's order is nobody's word.
// It checks the best orders in batched node reads (kob/node-verify.ts `confirmSnapshotsOnNode`,
// the reconciliation that cancel and amend use too).
// - Each round reads BOOK_CHECK_PER_SIDE rows per side.
// - Rounds go on until BOOK_CHECK_MIN_CONFIRMED orders of the side are confirmed and the size the
//   caller asks for is covered, at most BOOK_CHECK_MAX_ROWS rows.
// - The result is the book without the orders the node does not confirm, and a list of what was
//   left out and why.
//
// Beyond "the order exists", the confirmed state must belong to the market. Its token covenant id,
// token template hash and extension commitment are the market's. An order that has expired or is
// not yet active is no liquidity. A bid's amount is bounded by what its node-confirmed KAS can buy
// at its price.
//
// The touch of a side is the best price the node confirms among the rows the indexer lists. An
// order the indexer does not list at all is not seen. The optional second indexer
// (config `extraIndexerUrls`) cross-checks that start.
//
// The check fails closed. When no row of a side is confirmed, the whole side is left out, so the
// planner finds no liquidity rather than a price nobody can verify. Rows behind the verified depth
// are the indexer's word and are not part of the book the planners get, because what the planners
// derive from the whole book must not follow it. That covers the signed minimum fill, the fills
// budget, the depth warning and the cost preview. A caller that knows the size it plans passes
// `depth` to have more rows verified.
//
// The user acknowledges nothing and sees nothing when every order is confirmed. A node that cannot
// answer in time rejects with a `NodeError` (`unavailable`). The ticket then shows its node banner
// and plans nothing.
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
/**
 * A side is verified once this many of its orders are confirmed in the first round. More rounds
 * run while the size asked for in `BookVerifyDeps.depth` is not covered. Each round is one more
 * answer of the indexer in a row, so raising this costs ticket latency with a slow indexer
 * (e2e ticket-latency).
 */
export const BOOK_CHECK_MIN_CONFIRMED = 3;
/** Most rows looked at per side, refused ones included. A side with none confirmed is left out. */
export const BOOK_CHECK_MAX_ROWS = 18;
/** Rounds: BOOK_CHECK_MAX_ROWS / BOOK_CHECK_PER_SIDE. */
export const BOOK_CHECK_ROUNDS = BOOK_CHECK_MAX_ROWS / BOOK_CHECK_PER_SIDE;
/** The check of one round gets this long. A node that has not answered by then is `unavailable`. */
export const BOOK_CHECK_TIMEOUT_MS = 5_000;
/**
 * An order read from the indexer is reused this long while its book row (price, amount, value) is
 * unchanged. A stale one only fails the node check below, so the order leaves the book and never
 * enters it.
 */
const ORDER_VIEW_TTL_MS = 120_000;

/** What `verifyBook` and `confirmOrders` need. */
export interface BookVerifyDeps {
  kob: KobWasm;
  node: Pick<NodeApi, 'getUtxosByAddresses'>;
  indexer: Pick<IndexerApi, 'order'>;
  /** Script public key to address of the app's network. The node is asked by address. */
  toAddress: (spk: string) => string;
  /** Whether a row has the market's scale. Other rows are not part of the book, so not checked. */
  inScale?: (row: BookOrderView) => boolean;
  /** The market the book belongs to. A confirmed order must be of this token. Unset in tests. */
  market?: BookMarket;
  /** The node's DAA now. An order past its expiry or before its activeFrom is no liquidity. */
  daa?: bigint;
  /** Token base units a side must offer in confirmed orders before it counts as verified. */
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

/** The result of `verifyBook`. */
export interface VerifiedBook {
  /**
   * The book without the unconfirmed orders and the rows behind the verified depth. It is the same
   * object when nothing was left out.
   */
  view: BookView;
  dropped: DroppedBookOrder[];
  /** Rows behind the verified depth, left out of `view` and never looked at. */
  beyond: number;
}

const isOrderRow = (r: LevelView | BookOrderView): r is BookOrderView => 'covenant_id' in r;

/** Rejects with `onTimeout()` when `p` has not settled within `ms`. */
export const settle = <T,>(p: Promise<T>, ms: number, onTimeout: () => Error): Promise<T> => {
  let h: ReturnType<typeof setTimeout>;
  const limit = new Promise<never>((_, rej) => {
    h = setTimeout(() => rej(onTimeout()), ms);
  });
  return Promise.race([p, limit]).finally(() => clearTimeout(h));
};

// Order reads.

const viewCache = new WeakMap<object, Map<string, { at: number; p: Promise<OrderView | null> }>>();

/**
 * The indexer's full view of a book row's order (its decoded state and current outpoint), or null
 * when it cannot be read. Reused while the row is unchanged.
 */
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

// One batch.

/** Why the node, or the order itself, does not stand behind a book row. Pure given the snapshot. */
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
      // The state the node confirms commits the token. A real, unspent order of another token, or
      // of a look-alike program or commitment of this covenant id, is not an order of this market,
      // whatever the indexer lists it under.
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
      // The confirmed state's own clock decides, not the row's `expired` flag.
      if (d.expiryDaa > 0n && deps.daa >= d.expiryDaa) return 'expired';
      if (d.activeFrom > deps.daa) return 'inactive';
    }
    if (row.scale !== null && row.scale !== undefined && BigInt(row.scale) !== d.scale) return 'changed';
    // The KAS value of the order UTXO is the node's, and so are the tokens of an ask's custody,
    // because its script commits the state that holds the amount. The book row's claim must match.
    const value = row.cur_value ?? claimed.order.amount;
    if (BigInt(value) !== BigInt(confirmed.order.amount)) return 'changed';
    // An ask is liquidity only with the custody the node holds. Its amount is that, and an ask with
    // none has nothing to sell.
    if (side === 'sell' && !confirmed.custody) return 'unproven';
    if (confirmed.custody) {
      const left = !row.amount_estimated && row.amount_left != null ? row.amount_left : claimed.custody?.state.amount;
      if (left !== undefined && BigInt(left) !== BigInt(confirmed.custody.state.amount)) return 'changed';
    }
    return {
      amount: confirmed.custody ? BigInt(confirmed.custody.state.amount) : null,
      cap: d.price > 0n ? (BigInt(confirmed.order.amount) * d.scale) / d.price : 0n,
      // The tip and the smallest fill shape the minimum fill a planner signs. They are the
      // node-confirmed state's, not the row's.
      tip: d.tip,
      minFill: d.minFill,
    };
  } catch {
    return 'unproven';
  }
}

/**
 * The verdict on one row: why it is not part of the book, or what the node confirmed of it.
 * For an ask that is the custody amount, the node's and not the indexer's. A bid has no custody,
 * so its `cap` is the most its node-confirmed KAS buys at its price, and the row's own amount is
 * believed only up to it.
 */
type Verdict = DroppedBookOrder['reason'] | 'expired' | 'inactive' | { amount: bigint | null; cap: bigint; tip: bigint; minFill: bigint };

/** A book row's identity for the order-view cache: its covenant id and the facts the key uses. */
export type OrderKey = Pick<BookOrderView, 'covenant_id' | 'price' | 'amount_left' | 'cur_value'>;

/**
 * The indexer's view of each order, a snapshot of the state it claims, and the node's answer for
 * it. The node read is one call for all of them. `confirmed` is a `SnapshotError` for an order the
 * node does not confirm. A null entry means no usable view of the order. Rejects only when the
 * node cannot be asked.
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
      // Only the order and its custody count. Strays do not change what a taker meets.
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

// The book.

const dropOf = (row: LevelView | BookOrderView, side: 'sell' | 'buy', reason: DroppedBookOrder['reason']): DroppedBookOrder => {
  let price = 0n;
  try {
    price = BigInt(row.price);
  } catch {
    /* An unreadable price stays 0. */
  }
  return { covenantId: isOrderRow(row) ? row.covenant_id : ('' as Hex), side, price, reason };
};

/** A price as a bigint for sorting. An unreadable one sorts last. */
const priceKey = (row: BookOrderView): bigint | null => {
  try {
    return BigInt(row.price);
  } catch {
    return null;
  }
};

/**
 * The book (`aggregate=false`, one row per order) with the orders the node does not confirm left
 * out. Each side is sorted best price first: asks ascending, bids descending, the indexer's order
 * within a price. The best BOOK_CHECK_PER_SIDE rows of each side are checked together in one node
 * read. A side goes on with the next rows until BOOK_CHECK_MIN_CONFIRMED orders and `deps.depth`
 * token units are confirmed or BOOK_CHECK_MAX_ROWS rows were looked at. A side with none confirmed
 * is left out entirely. Rows behind the verified depth are left out of the view (`beyond`).
 * A side listed as aggregated levels cannot be checked and is left out too. Rejects with a
 * `NodeError` when the node cannot be asked in time.
 */
export async function verifyBook(deps: BookVerifyDeps, view: BookView): Promise<VerifiedBook> {
  const inScale = deps.inScale ?? (() => true);
  const sides = (['sell', 'buy'] as const).map((side) => {
    const rows = side === 'sell' ? view.asks : view.bids;
    // One row per order. The indexer listing an order N times does not count it N times.
    const seenIds = new Set<string>();
    const cands = rows
      .filter(isOrderRow)
      .filter(inScale)
      .filter((row) => !seenIds.has(row.covenant_id) && !!seenIds.add(row.covenant_id))
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
        // An ask's amount is the custody the node holds. The row's own word counts only where the
        // order has no custody to read, which is a bid, and then only up to what the node-confirmed
        // KAS of the bid can buy at its price.
        const claimed = b.row.amount_left != null ? BigInt(b.row.amount_left) : 1n;
        const amount = v.amount ?? (claimed < v.cap ? claimed : v.cap);
        b.s.confirmed++;
        b.s.depth += amount;
        const row0 = v.amount !== null ? (b.row.amount_estimated ? { ...b.row, amount_left: v.amount.toString(), amount_estimated: false } : b.row) : amount !== claimed ? { ...b.row, amount_left: amount.toString() } : b.row;
        // The row's tip and minimum fill are the state's. It is the same object when they already
        // are, so an unchanged book stays the same book.
        const [tip, minFill] = [v.tip.toString(), v.minFill.toString()];
        b.s.kept.push(row0.tip === tip && row0.min_fill === minFill ? row0 : { ...row0, tip, min_fill: minFill });
        return;
      }
      // An order that is over or not yet live is no liquidity, but it is not a missing one either.
      // It leaves the book without a note.
      if (v !== 'expired' && v !== 'inactive') dropped.push(dropOf(b.row, b.s.side, v));
    });
  }
  // A side with nothing confirmed after the last round: its remaining orders are not trusted.
  for (const s of sides) {
    if (s.levels || s.confirmed > 0) continue;
    for (const row of s.cands.slice(s.next)) dropped.push(dropOf(row, s.side, 'unchecked'));
  }
  // The checked book is the confirmed rows in price order. Rows never looked at are the indexer's
  // word and stay out. A side with nothing confirmed has none, because all of them were dropped.
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
