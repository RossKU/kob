// Checker state run/state/checker.json: cursors (fills, new orders, txs.jsonl offset, agreement window), per-order memo, tracked strays,
// persistence trackers and the check counters the report shows. Written atomically after every round. No `@/` imports.
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';

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
}

/** fill sums of one token in one transaction: tokens (base units) and KAS (sompi) of the ask-side (s, ks) and bid-side (b, kb) fills */
export interface BookSums {
  s: string;
  b: string;
  ks: string;
  kb: string;
}

/** per-transaction fill sums: daa, the sums per token book (covenant id), the pair order fills of the transaction */
export interface TxSums {
  d: number;
  k: Record<string, BookSums>;
  x?: number;
}

/** a recent-transaction entry of a state written before the second token (the primary token's sums) */
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
  /** protocol generation of the memoized terms: a state written under protocol v2.6 (lots) is dropped, its terms use other fields */
  protocol?: number;
  startedAt: number;
  updatedAt: number;
  rounds: number;
  /** 2: fill counters of a state from before the id cursor fix are reset (they were inflated by re-counting pruned fills) */
  fillsVersion?: number;
  fills: {
    maxId: number;
    maxDaa: number;
    /** `order:txid` -> daa (pruned to the last ~2 h) */
    seen: Record<string, number>;
    /** recent fill transactions (trade count, volume = max of the ask-side and bid-side sums of a transaction), pruned */
    tx: Record<string, TxSums | LegacyTxSums>;
    count: number;
    sell: number;
    buy: number;
    trades: number;
    volumeTokens: string;
    volumeKas: string;
    /** per soak token (covenant id): its fills, trades (transactions touching its book) and volume */
    byToken?: Record<string, TokenFills>;
    /**
     * transactions filling more than one token book (`txMultiBook`), with a pair order fill (`txPair`), with two or more pair order
     * fills (`txMultiPair`); pair order fills (`pairFills`)
     */
    multi?: { txMultiBook: number; txPair: number; pairFills: number; txMultiPair: number };
  };
  orders: Record<string, OrderMemo>;
  /**
   * full terms of orders seen by the fast sweep (every few seconds, newest orders) before any round memoized them: an order placed and
   * filled between two rounds (a crossing IOC bid that is a stop's trigger evidence) keeps its full state; consumed by `memoize`
   */
  early?: Record<string, { terms: Record<string, string>; daa: number }>;
  /** orders whose events must be re-read next round (a deferred check) */
  retry: string[];
  strays: Record<string, { order: string; maker: string | null; amount: string; firstSeen: number; token?: string }>;
  txsOffset: number;
  /** genesis DAA floor of the next new-order scan (0 = full history) */
  scanDaa: number;
  /** the tokens the last scan covered (a token added later is first scanned over its full history) */
  scanDaaByToken?: Record<string, number>;
  /** supply check per token: the accepted constant offset (pre-run holdings no tracker knows) once it is established */
  supply?: Record<string, { baseline: string | null; last: string; live: string; untracked: string; ts: number }>;
  /** last DAA both indexers were compared up to */
  agreeDaa: number;
  persist: Record<string, { since: number; count: number; fired: boolean; sig?: string }>;
  stats: Record<string, number>;
  x402: Record<string, unknown> | null;
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
    if (s.fillsVersion !== 2) return { ...emptyState(), ...s, fillsVersion: 2, fills: emptyState().fills };
    return { ...emptyState(), ...s, fills: { ...emptyState().fills, ...s.fills } };
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
