// Checker state run/state/checker.json.
// It holds cursors (fills, new orders, txs.jsonl offset, agreement window), the per-order memo,
// tracked strays, persistence trackers and the check counters the report shows.
// It is written atomically after every round. No `@/` imports.
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import type { ScenarioRecord } from '../scenarios.ts';

export interface OrderMemo {
  /** contract */
  c: string;
  side: number;
  tif: number | null;
  status: string;
  lastDaa: number;
  genesisDaa: number;
  /** base units at creation (decimal string; null for a bid or while unknown) */
  initialAmount: string | null;
  /** immutable terms: the proven state without `exitState` (null while unknown) */
  terms: Record<string, string> | null;
  /** event keys already processed (`kind:txid`) */
  ev: string[];
  /** in the active set at the last round */
  active: boolean;
  /** follow its events (tif 2, conditional, if-done) */
  watch: boolean;
  /** copied from an older run's database (`isImportedHistory`): its fills are not checked */
  imported?: boolean;
  /**
   * Its token(s) as indexes into `CheckerState.tokenIds` (its token, and for a pair order also its
   * token B).
   * The report counts the orders of the soak tokens from the memo.
   * Absent on a memo that does not carry it.
   */
  tk?: number[];
}

/**
 * Fill sums of one token in one transaction: tokens (base units) and KAS (sompi) of the ask-side
 * (s, ks) and bid-side (b, kb) fills.
 */
export interface BookSums {
  s: string;
  b: string;
  ks: string;
  kb: string;
}

/**
 * Per-transaction fill sums: daa, the sums per token book (covenant id) and the pair order fills of
 * the transaction.
 */
export interface TxSums {
  d: number;
  k: Record<string, BookSums>;
  x?: number;
}

/** A recent-transaction entry in the single-token shape (the primary token's sums). */
export interface LegacyTxSums extends BookSums {
  d: number;
}

export interface TokenFills {
  count: number;
  sell: number;
  buy: number;
  trades: number;
  volumeTokens: string;
  volumeKas: string;
}

export interface CheckerState {
  version: 1;
  /**
   * Protocol generation of the memoized terms. A state written under another generation (such as
   * protocol v2.6, lots) is dropped, because its terms use other fields.
   */
  protocol?: number;
  startedAt: number;
  updatedAt: number;
  rounds: number;
  /** Version of the fill counters. A state with another value has its fill counters reset. */
  fillsVersion?: number;
  fills: {
    maxId: number;
    maxDaa: number;
    /** `order:txid` -> daa (pruned to the last ~2 h) */
    seen: Record<string, number>;
    /**
     * Recent fill transactions (trade count, volume = max of the ask-side and bid-side sums of a
     * transaction), pruned.
     */
    tx: Record<string, TxSums | LegacyTxSums>;
    count: number;
    sell: number;
    buy: number;
    trades: number;
    volumeTokens: string;
    volumeKas: string;
    /**
     * Per soak token (covenant id): its fills, trades (transactions touching its book) and volume.
     */
    byToken?: Record<string, TokenFills>;
    /**
     * Transactions filling more than one token book (`txMultiBook`), transactions with a pair order
     * fill (`txPair`) and with two or more pair order fills (`txMultiPair`).
     * Pair order fills are in `pairFills`.
     */
    multi?: { txMultiBook: number; txPair: number; pairFills: number; txMultiPair: number };
  };
  /**
   * Memos of the live orders and of those ended less than `ORDER_MEMO_WINDOW_DAA` ago. Older ended
   * ones are folded into `prunedOrders`.
   */
  orders: Record<string, OrderMemo>;
  /**
   * Counts of the ended orders whose memo was pruned, by `prunedKey` (contract, tif, status, side,
   * token indexes).
   * The report's order totals (memoOrderRows) add them to the memos.
   * Each order is added once, when its memo is dropped. A pruned order is never memoized again
   * (`memoFor` keeps a lookup of one out of `orders`).
   */
  prunedOrders?: Record<string, number>;
  /** the token covenant ids `OrderMemo.tk` indexes (append-only: an index never changes meaning) */
  tokenIds?: string[];
  /**
   * Full terms of orders seen by the fast sweep (every few seconds, newest orders) before any round
   * memoized them.
   * An order placed and filled between two rounds (such as a crossing IOC bid that is a stop's
   * trigger evidence) keeps its full state.
   * It is consumed by `memoize`.
   */
  early?: Record<string, { terms: Record<string, string>; daa: number }>;
  /** orders whose events must be re-read next round (a deferred check) */
  retry: string[];
  strays: Record<string, { order: string; maker: string | null; amount: string; firstSeen: number; token?: string }>;
  txsOffset: number;
  /** genesis DAA floor of the next new-order scan (0 = full history) */
  scanDaa: number;
  /**
   * The tokens the last scan covered. A token added later is first scanned over its full history.
   */
  scanDaaByToken?: Record<string, number>;
  /**
   * Supply check per token: the accepted constant offset (pre-run holdings no tracker knows), once
   * it is established.
   */
  supply?: Record<string, { baseline: string | null; last: string; live: string; untracked: string; ts: number }>;
  /** last DAA both indexers were compared up to */
  agreeDaa: number;
  persist: Record<string, { since: number; count: number; fired: boolean; sig?: string }>;
  stats: Record<string, number>;
  x402: Record<string, unknown> | null;
  /**
   * Byte cursor in run/state/scenarios.jsonl and the scenario records still waiting for the indexer
   * (scenario-rules.ts).
   */
  scenOffset?: number;
  scen?: ScenarioRecord[];
}

/**
 * How long (DAA, 72,000 = 2 h at 10 BPS) the memo of an ended order is kept after its last update.
 * Every check that reads an ended order's memo acts within minutes of the order's last event.
 * Those checks read a fill's terms, a stop's trigger evidence and the evidence orders' contracts,
 * the repeat cycle's entry and the retried events.
 * The memos `early`, `fills.seen` and `fills.tx` use the same window.
 */
export const ORDER_MEMO_WINDOW_DAA = 72_000;

/**
 * The key of `CheckerState.prunedOrders`: the fields the report groups orders by (`tk` absent means
 * empty).
 */
export const prunedKey = (m: Pick<OrderMemo, 'c' | 'tif' | 'status' | 'side' | 'tk'>): string => `${m.c}|${m.tif ?? '-'}|${m.status}|${m.side}|${(m.tk ?? []).join(',')}`;

/**
 * Drops the memos of orders that are not active and were last updated more than
 * `ORDER_MEMO_WINDOW_DAA` before `refDaa` (the indexer's cursor).
 * Those in `keep` (the orders of the round being checked, the deferred ones) stay.
 * It counts each dropped memo in `prunedOrders` and returns the number dropped.
 */
export function pruneOrders(st: CheckerState, refDaa: number, keep: { has(id: string): boolean }): number {
  let n = 0;
  for (const [id, m] of Object.entries(st.orders)) {
    if (m.active || refDaa - m.lastDaa <= ORDER_MEMO_WINDOW_DAA || keep.has(id)) continue;
    const k = prunedKey(m);
    const p = (st.prunedOrders ??= {});
    p[k] = (p[k] ?? 0) + 1;
    delete st.orders[id];
    n++;
  }
  return n;
}

/** protocol generation of the terms the memo holds (3 = amounts in base units, no lots) */
const PROTOCOL = 3;

export function emptyState(now = Date.now()): CheckerState {
  return {
    version: 1,
    protocol: PROTOCOL,
    startedAt: now,
    updatedAt: now,
    rounds: 0,
    fillsVersion: 2,
    fills: { maxId: 0, maxDaa: 0, seen: {}, tx: {}, count: 0, sell: 0, buy: 0, trades: 0, volumeTokens: '0', volumeKas: '0' },
    orders: {},
    retry: [],
    strays: {},
    txsOffset: 0,
    scanDaa: 0,
    agreeDaa: 0,
    persist: {},
    stats: {},
    x402: null,
  };
}

export function loadState(path: string): CheckerState {
  if (!existsSync(path)) return emptyState();
  try {
    const s = JSON.parse(readFileSync(path, 'utf8')) as CheckerState;
    if (s.version !== 1 || s.protocol !== PROTOCOL) return emptyState();
    const out: CheckerState =
      s.fillsVersion !== 2
        ? { ...emptyState(), ...s, fillsVersion: 2, fills: emptyState().fills }
        : { ...emptyState(), ...s, fills: { ...emptyState().fills, ...s.fills } };
    // A state that holds every order ever seen has its ended ones dropped now, with the cursor of
    // its last scan as the reference.
    if (out.scanDaa > 0) pruneOrders(out, out.scanDaa + 600, new Set(out.retry));
    return out;
  } catch {
    return emptyState();
  }
}

export function saveState(path: string, s: CheckerState): void {
  mkdirSync(dirname(path), { recursive: true });
  s.updatedAt = Date.now();
  writeFileSync(path + '.tmp', JSON.stringify(s));
  renameSync(path + '.tmp', path);
}
