// The order book against the NODE. A market or close order (and any order that takes liquidity now) is an auction that starts at the best price of
// the indexer's book. One indexer is not an authority for that price: a dishonest or compromised one can list an ask that does not exist (a "phantom"
// cheap ask the order would start from) or hide the real ones. The P2SH of an order commits its whole state (price, amount, owner ...), so an order
// UTXO that EXISTS UNSPENT on the node at the script derived from the state the indexer reported proves that state: the touch of the book is real.
//
// `verifyBook` checks the first orders of each side (the touch and the few behind it) in ONE batched node read (kob/node-verify.ts
// `confirmSnapshotsOnNode`, the same reconciliation cancel and amend use) and returns the book without the orders the node does not confirm,
// with a list of what was left out and why. When none of the first orders of a side is confirmed, the whole side is left out (fail closed): the
// planner then finds no liquidity rather than a price nobody can verify. Nothing is acknowledged by the user and nothing is shown when every order
// is confirmed. A node that cannot answer in time rejects with a `NodeError` (`unavailable`): the ticket shows its node banner and plans nothing.
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

/** Orders checked per side and round: the touch and the few behind it (the planner prices from the first, the rest are its depth). */
export const BOOK_CHECK_PER_SIDE = 3;
/** Rounds for a side whose first orders are all refused: at most this many times BOOK_CHECK_PER_SIDE orders are looked at, then the side is left out. */
export const BOOK_CHECK_ROUNDS = 3;
/** The whole check of one round (the order reads and the node read) is given this long; the node not answering by then is `unavailable`. */
export const BOOK_CHECK_TIMEOUT_MS = 5_000;
/** An order read from the indexer is reused this long while the book row it belongs to (price, amount, value) is unchanged. */
const ORDER_VIEW_TTL_MS = 30_000;

export interface BookVerifyDeps {
  kob: KobWasm;
  node: Pick<NodeApi, 'getUtxosByAddresses'>;
  indexer: Pick<IndexerApi, 'order'>;
  /** script public key -> address of the app's network (the node is asked by address) */
  toAddress: (spk: string) => string;
  /** a row of another scale than the market's is not part of the book the planners use: not checked (default: all rows count) */
  inScale?: (row: BookOrderView) => boolean;
  timeoutMs?: number;
}

export interface VerifiedBook {
  /** the book without the orders the node does not confirm (the same object when nothing was left out) */
  view: BookView;
  dropped: DroppedBookOrder[];
}

const isOrderRow = (r: LevelView | BookOrderView): r is BookOrderView => 'covenant_id' in r;

const settle = <T,>(p: Promise<T>, ms: number, onTimeout: () => Error): Promise<T> => {
  let h: ReturnType<typeof setTimeout>;
  const limit = new Promise<never>((_, rej) => {
    h = setTimeout(() => rej(onTimeout()), ms);
  });
  return Promise.race([p, limit]).finally(() => clearTimeout(h));
};

// ------------------------------------------------------------------------------------------------ order reads

const viewCache = new WeakMap<object, Map<string, { at: number; p: Promise<OrderView | null> }>>();

/** The indexer's full view of a book row's order (its decoded state and current outpoint), `null` when it cannot be read; reused while the row is unchanged. */
function readOrder(ix: Pick<IndexerApi, 'order'>, row: BookOrderView, ms: number): Promise<OrderView | null> {
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
function judge(kob: KobWasm, row: BookOrderView, side: 'sell' | 'buy', claimed: OrderSnapshot, confirmed: OrderSnapshot | SnapshotError): DroppedBookOrder['reason'] | null {
  if (confirmed instanceof SnapshotError) return confirmed.code === 'node-mismatch' ? 'not-on-node' : 'unproven';
  try {
    const d = describeOrder(confirmed.order.state, kob);
    if (confirmed.covenantId !== row.covenant_id || d.side !== side || d.price !== BigInt(row.price)) return 'changed';
    if (row.scale !== null && row.scale !== undefined && BigInt(row.scale) !== d.scale) return 'changed';
    // the KAS value of the order UTXO is the node's; so are the tokens of an ask's custody (its script commits the state that holds the amount): the
    // book row's claim must be the same
    const value = row.cur_value ?? claimed.order.amount;
    if (BigInt(value) !== BigInt(confirmed.order.amount)) return 'changed';
    if (confirmed.custody) {
      const left = !row.amount_estimated && row.amount_left != null ? row.amount_left : claimed.custody?.state.amount;
      if (left !== undefined && BigInt(left) !== BigInt(confirmed.custody.state.amount)) return 'changed';
    }
    return null;
  } catch {
    return 'unproven';
  }
}

/** The verdict of each row (`null`: confirmed), the node read being ONE call for all of them. Rejects only when the node cannot be asked. */
async function checkRows(deps: BookVerifyDeps, rows: readonly { row: BookOrderView; side: 'sell' | 'buy' }[]): Promise<(DroppedBookOrder['reason'] | null)[]> {
  const ms = deps.timeoutMs ?? BOOK_CHECK_TIMEOUT_MS;
  const views = await Promise.all(rows.map((r) => readOrder(deps.indexer, r.row, ms)));
  const claimed = views.map((v, i): OrderSnapshot | null => {
    if (!v || v.covenant_id !== rows[i]!.row.covenant_id) return null;
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
  const out: (DroppedBookOrder['reason'] | null)[] = rows.map(() => 'unproven');
  toCheck.forEach((c, k) => {
    out[c.i] = judge(deps.kob, rows[c.i]!.row, rows[c.i]!.side, c.s, confirmed[k]!);
  });
  return out;
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

/**
 * The book (`aggregate=false`: one row per order) with the orders the node does not confirm left out. The first BOOK_CHECK_PER_SIDE orders of each
 * side are checked together in one node read; a side whose checked orders are all refused goes on with the next ones (BOOK_CHECK_ROUNDS times at
 * most) and is left out entirely when none is confirmed. A side listed as aggregated levels cannot be checked and is left out too. Rejects with a
 * `NodeError` when the node cannot be asked in time.
 */
export async function verifyBook(deps: BookVerifyDeps, view: BookView): Promise<VerifiedBook> {
  const inScale = deps.inScale ?? (() => true);
  const sides = (['sell', 'buy'] as const).map((side) => {
    const rows = side === 'sell' ? view.asks : view.bids;
    return { side, rows, levels: rows.length > 0 && !rows.every(isOrderRow), cands: rows.filter(isOrderRow).filter(inScale), next: 0, confirmed: 0 };
  });
  const dropped: DroppedBookOrder[] = [];
  const out = new Set<string>();
  for (const s of sides) {
    if (!s.levels) continue;
    for (const r of s.rows) dropped.push(dropOf(r, s.side, 'unchecked'));
  }
  for (let round = 0; round < BOOK_CHECK_ROUNDS; round++) {
    const batch = sides.flatMap((s) => (s.levels || s.confirmed > 0 ? [] : s.cands.slice(s.next, s.next + BOOK_CHECK_PER_SIDE).map((row) => ({ s, row }))));
    if (!batch.length) break;
    for (const s of sides) if (!s.levels && s.confirmed === 0) s.next += BOOK_CHECK_PER_SIDE;
    const verdicts = await checkRows(deps, batch.map((b) => ({ row: b.row, side: b.s.side })));
    batch.forEach((b, i) => {
      const reason = verdicts[i]!;
      if (reason === null) b.s.confirmed++;
      else {
        out.add(b.row.covenant_id);
        dropped.push(dropOf(b.row, b.s.side, reason));
      }
    });
  }
  // a side with nothing confirmed after the last round: its remaining orders are not trusted either
  for (const s of sides) {
    if (s.levels || s.confirmed > 0) continue;
    for (const row of s.cands.slice(s.next)) {
      out.add(row.covenant_id);
      dropped.push(dropOf(row, s.side, 'unchecked'));
    }
  }
  if (!dropped.length) return { view, dropped };
  const keep = (rows: (LevelView | BookOrderView)[], levels: boolean) => (levels ? [] : rows.filter((r) => !(isOrderRow(r) && out.has(r.covenant_id))));
  return { view: { ...view, asks: keep(view.asks, sides[0]!.levels), bids: keep(view.bids, sides[1]!.levels) }, dropped };
}
