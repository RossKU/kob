// Standalone mode: the market from the node alone.
//
// The browser follows the node's new blocks (`NodeApi.subscribeBlocks`). It reads the KOB1
// placement records of every transaction with kob-wasm (kob/stream-orders.ts), the decoder the
// indexer uses, record by record. It also reads the spends of KOB orders, meaning fills, cancels
// and amends, and follows the orders it saw to their continuations. Every order is confirmed on
// the node before it is listed (kob/node-verify.ts `confirmSnapshotsOnNode`). The check is that
// the order UTXO, and an ask's custody, are unspent at the scripts its state derives under the
// templates this build pins. It is the same check the book gets before an order is taken
// (app/book-verify.ts). The wallet's own orders are added from its receipts once the node
// resolved them (`adopt`). Trades, candles, the 24 h figures and the depth are built from what was
// seen (data/stream-market.ts). A fill counts once the node shows an output of its transaction.
//
// The book does not start empty. Its first entries are the resting orders the page last read
// from the indexer (data/stream-carry.ts). Each one is asked on the node at once and again with
// every recheck. One the node no longer holds, or that a block shows spent, leaves the book. An
// order placed while the indexer could not be read and before the mode followed the blocks is not
// seen. No server and no other party is asked, and the node has no covenant-id lookup. Nothing is
// taken on this book's word, because the book check asks the node again before an order is
// planned (app/book-verify.ts).
//
// The user's three settings (`StandalonePrefs`) decide when the mode runs.
// - `manual` turns it on from the start, whatever the indexer does. The app then sends the indexer
//   no market request and does not poll its health.
// - `auto` (default on) starts it when the indexer in use stays bad. A bad health report (`report`,
//   from the status poll in ui/shell/status.ts) means down, stale, on another network or without
//   its node. That report, or the first market read that finds the indexer down (`readFailed`),
//   is checked once more after ENTER_CHECK_MS. If it is still bad, the mode starts and the failed
//   read is answered from it. With failover (data/indexer-failover.ts) the report is about the
//   active indexer, and the failover gets its time first (`fastFallback` off). A usable backup
//   keeps the app on the indexers. An indexer that answers but is behind the chain by more than
//   LAG_SWITCH_SECONDS counts the same way, with the same second look and the same kept data. The
//   lag is its health's `lag_seconds`, else `lag_daa` / 10. The mode then sets `behind` and says so
//   in its texts.
// - `autoReturn` (default on) ends an automatic mode when the indexer is caught up, meaning `ok`
//   and within LAG_RETURN_SECONDS of the chain. Such a report is checked once more after
//   RETURN_CHECK_MS, and the mode asks the indexer's health itself every few seconds. If it is
//   still caught up, the mode ends and the indexer's book is read again, and its book wins.
//   Between the two bounds the mode stays, so it does not flap. With `autoReturn` off the mode
//   stays, and `indexerBack` says it may end (`returnToIndexer`, the button of the bell's entry).
//
// What the page already read from the indexer in this session is kept (data/stream-carry.ts). That
// covers the candles, trades and 24 h figures of the markets it loaded, the token list with the
// facts of open-list tokens, the activity rows and the resting orders. The views merge what the
// node shows afterwards into it.
//
// While active, the app's indexer (`modeIndexer`) answers the book, the orders of the book, the
// token list and the market data from here. What needs the indexer's records fails as it does
// with the indexer down. That covers pair books, holders and token UTXOs, the order lists of My
// orders, and the notifications.
import type { IndexerApi } from './indexer';
import { IndexerError } from './indexer';
import type { BookView, CandleInterval, CandlesView, EventView, HealthView, IndexerTokenView, OrderView, StatsView, TradesView } from './indexer-types';
import type { BlockSubscription, NodeApi } from './node';
import type { StreamBlock } from './block-stream';
import { StreamBook, bookViewOf, listingOf, orderViewOf, tokensViewOf, type ListingContext, type RegistryFacts, type StreamOrigin } from './stream-book';
import { IndexerCarry, mergeCandles, mergeStats, mergeTokens, mergeTrades } from './stream-carry';
import { MAX_FILLS_PER_TOKEN, candlesView, depthView, statsView, tradesView, type SeenFill } from './stream-market';
import { SnapshotError, snapshotFromOrderView, type OrderSnapshot } from '../kob/cancel';
import { confirmSnapshotsOnNode, fetchNodeInputFacts, outpointKey, type OutpointRef } from '../kob/node-verify';
import { describeOrder } from '../kob/order-facts';
import { fillOf, followSpend, isStreamKind, revealSpend, snapshotOfPlacement, txFacts, type StreamFill } from '../kob/stream-orders';
import type { Hex, TxJson } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { assessIndexerHealth, type HealthAssessment } from '../ui/shell/health';

/**
 * The user's choices in Settings.
 * - `manual`: standalone mode always on.
 * - `auto`: fall back to it when the indexer is down.
 * - `autoReturn`: return to the indexer when it is back.
 */
export interface StandalonePrefs {
  manual: boolean;
  auto: boolean;
  autoReturn: boolean;
}
export const DEFAULT_STANDALONE: Readonly<StandalonePrefs> = Object.freeze({ manual: false, auto: true, autoReturn: true });

/**
 * A bad report or a failed read is checked once more after this long before the mode starts, so
 * a blip does not start it.
 */
export const ENTER_CHECK_MS = 1_000;
/**
 * A usable report is checked once more after this long before an automatic mode ends, so an
 * indexer that flickers does not end it.
 */
export const RETURN_CHECK_MS = 5_000;
/**
 * An automatic mode ends no sooner than this after it started, so an indexer that goes up and down
 * switches the page at most twice a minute each way (the up/down side of the lag rule's hysteresis).
 */
export const MIN_AUTO_MS = 30_000;
/**
 * An indexer that answers but is more than this many seconds behind the chain is not used. An
 * automatic mode starts, as for an indexer that is down.
 */
export const LAG_SWITCH_SECONDS = 120;
/**
 * An automatic mode ends only once the indexer is within this many seconds of the chain. It is
 * well under LAG_SWITCH_SECONDS, so the two do not flap.
 */
export const LAG_RETURN_SECONDS = 20;
/**
 * With further indexers the failover gets this long to find a usable one first. Its re-checks and
 * one round of probes take about 12 s.
 */
export const ENTER_CHECK_FAILOVER_MS = 20_000;
/** That check of the indexer is cut off after this long. */
export const PROBE_TIMEOUT_MS = 6_000;
/**
 * New or changed orders and fills are confirmed on the node this long after the block that showed
 * them, so that one read covers more of them.
 */
export const VERIFY_DELAY_MS = 1_500;
/**
 * An unconfirmed order or fill is asked again after these delays, then dropped. A block's
 * transactions reach the node's UTXO set only once the block is merged.
 */
export const VERIFY_RETRY_MS: readonly number[] = [2_000, 4_000, 8_000, 16_000];
/**
 * Confirmed orders are confirmed again this often, at most RECHECK_BATCH per round. This catches a
 * spend in a block the stream missed.
 */
export const RECHECK_MS = 30_000;
export const RECHECK_BATCH = 100;
/**
 * After this long without a block while following, the subscription is made again and every order
 * is confirmed again.
 */
export const STALL_MS = 30_000;
/** A failed subscription is tried again after this long, doubled up to SUBSCRIBE_RETRY_MAX_MS. */
export const SUBSCRIBE_RETRY_MS = 5_000;
export const SUBSCRIBE_RETRY_MAX_MS = 60_000;
/** A market a view read the book of within this many ms counts as on screen when a full book trims. */
const FOCUS_MS = 120_000;
/** The housekeeping tick of the stall watch and the rechecks, in ms. */
const TICK_MS = 5_000;
/** Most orders or fills confirmed in one node read. */
const VERIFY_CHUNK = 100;
/** Transactions remembered as read, because a transaction is in several parallel blocks. */
const SEEN_TXS = 20_000;
/** Blocks waiting to be read. Past it the oldest are dropped and every order is confirmed again. */
const MAX_QUEUE = 500;
/** Fills waiting for the node. Past it the oldest are dropped. */
const MAX_PENDING_FILLS = 2_000;

/** Whether standalone mode is off, subscribing to the node's blocks, or following them. */
export type StandalonePhase = 'off' | 'connecting' | 'live';

/** What the views read of standalone mode. */
export interface StandaloneState {
  active: boolean;
  /** `manual` when the setting turned it on, `auto` when the indexer could not be used. */
  reason: 'manual' | 'auto' | null;
  /** `connecting` while subscribing to the node's blocks (again), `live` while following them. */
  phase: StandalonePhase;
  /** When the mode started, in ms since the epoch. What happened before is not shown. */
  sinceMs: number | null;
  /** Counts every change of the book or the market data, so a view reads it again. */
  version: number;
  /** Orders listed now. */
  orders: number;
  /**
   * Set in an automatic mode with `autoReturn` off when the indexer answers again.
   * `returnToIndexer` ends the mode.
   */
  indexerBack: boolean;
  /**
   * Set in an automatic mode that started because the indexer is behind the chain, not because it
   * cannot be reached.
   */
  behind: boolean;
}

/** What `StandaloneController` needs. */
export interface StandaloneDeps {
  kob: KobWasm;
  node: Pick<NodeApi, 'getUtxosByAddresses' | 'subscribeBlocks'>;
  /** Script public key to address on the app's network, because the node is asked by address. */
  toAddress: (spk: string) => string;
  network: string;
  /** The indexer the app reads otherwise. It is asked once more before an automatic mode starts. */
  indexer: Pick<IndexerApi, 'health'>;
  /**
   * What the indexer in use answered before, as `modeIndexer` records it. A controller made
   * without one starts with nothing kept.
   */
  carry?: IndexerCarry;
  prefs?: Partial<StandalonePrefs>;
  /**
   * How long a bad report is waited on before the indexer is asked again. The default is
   * ENTER_CHECK_MS. It is longer with failover, which switches first.
   */
  enterCheckMs?: number;
  /** Seconds behind the chain from which an answering indexer is left (LAG_SWITCH_SECONDS). */
  lagSwitchSeconds?: number;
  /** Seconds behind the chain up to which an automatic mode may end. Default LAG_RETURN_SECONDS. */
  lagReturnSeconds?: number;
  /**
   * Whether a market read that finds the indexer down starts the check at once and waits for it.
   * It is on by default and off with failover, which switches first.
   */
  fastFallback?: boolean;
  /**
   * The registry's facts of a token. Its orders are listed only with them, and its scale is the
   * price basis.
   */
  registry?: (token: Hex) => RegistryFacts | null;
  now?: () => number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (h: unknown) => void;
}


/**
 * Tells whether an indexer read failed because the indexer is down. That means no answer, a
 * timeout or a server error, not a refusal, a cancel or a missing route.
 */
export function isIndexerDown(e: unknown): boolean {
  if (!(e instanceof IndexerError) || e.code === 'standalone_mode' || e.kind === 'aborted') return false;
  return e.kind === 'network' || e.kind === 'timeout' || (e.kind === 'http' && e.status >= 500);
}

/** The error every read that needs the indexer gets in standalone mode. */
export function standaloneUnavailable(what = 'this'): IndexerError {
  return new IndexerError('http', 503, 'standalone_mode', `Standalone mode: ${what} needs the indexer.`);
}

/** A fill waiting for the node to show one of its transaction's outputs. */
interface PendingFill { fill: StreamFill; ts: number; daa: bigint; probe: OutpointRef; due: number; misses: number }

/**
 * Runs standalone mode. It decides when the mode is on and builds the book and the market data
 * from the node's blocks.
 */
export class StandaloneController {
  readonly indexer: IndexerApi;
  /**
   * What the page read from the indexer before the mode. It is kept across the mode, while the
   * mode's own collection is dropped when it ends.
   */
  readonly carry: IndexerCarry;
  private readonly kob: KobWasm;
  private readonly node: StandaloneDeps['node'];
  private readonly toAddress: (spk: string) => string;
  private readonly network: string;
  private readonly real: Pick<IndexerApi, 'health'>;
  private readonly registry: StandaloneDeps['registry'];
  private readonly now: () => number;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private readonly clearTimer: (h: unknown) => void;
  private prefsNow: StandalonePrefs;
  private readonly enterCheckMs: number;
  private readonly lagSwitch: number;
  private readonly lagReturn: number;
  private readonly fastFallback: boolean;
  private readonly book = new StreamBook();
  /** When a view last read each market's book. A full book forgets the others' orders first. */
  private readonly looked = new Map<string, number>();
  private readonly listeners = new Set<(s: StandaloneState) => void>();
  private snap: StandaloneState = { active: false, reason: null, phase: 'off', sinceMs: null, version: 0, orders: 0, indexerBack: false, behind: false };
  private gen = 0;
  private daa = 0n;
  private sub: BlockSubscription | null = null;
  private subscribeFails = 0;
  private lastBlockMs = 0;
  private enterTimer: unknown = null;
  /** The second look before an automatic mode starts is under way. */
  private entering = false;
  /** Reads waiting for that look (`readFailed`). */
  private waiters: (() => void)[] = [];
  private returnTimer: unknown = null;
  /** Counts the return checks. A bad report in between makes a pending one void. */
  private returnSeq = 0;
  /** The mode's own look at the indexer's health is under way. */
  private watching = false;
  /** Entries started from the kept resting orders, not asked on the node yet. */
  private readonly carried = new Set<Hex>();
  private verifyTimer: unknown = null;
  private tickTimer: unknown = null;
  private retryTimer: unknown = null;
  private lastRecheckMs = 0;
  /** Next node check of each unconfirmed entry, in ms. */
  private readonly due = new Map<Hex, number>();
  private pendingFills: PendingFill[] = [];
  private readonly fills = new Map<Hex, SeenFill[]>();
  private fillSeq = 0;
  /**
   * The scale of the first order seen of each token. It is the price basis when the registry has
   * no decimals.
   */
  private readonly firstScale = new Map<Hex, bigint>();
  private seen = new Set<Hex>();
  private seenOld = new Set<Hex>();
  private queue: StreamBlock[] = [];
  private draining = false;
  private verifying = false;
  private disposed = false;

  constructor(d: StandaloneDeps) {
    this.kob = d.kob;
    this.node = d.node;
    this.toAddress = d.toAddress;
    this.network = d.network;
    this.real = d.indexer;
    this.registry = d.registry;
    this.carry = d.carry ?? new IndexerCarry(d.now);
    this.now = d.now ?? Date.now;
    this.setTimer = d.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = d.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
    this.prefsNow = { ...DEFAULT_STANDALONE, ...(d.prefs ?? {}) };
    this.enterCheckMs = d.enterCheckMs ?? ENTER_CHECK_MS;
    this.fastFallback = d.fastFallback ?? true;
    this.lagSwitch = d.lagSwitchSeconds ?? LAG_SWITCH_SECONDS;
    this.lagReturn = d.lagReturnSeconds ?? LAG_RETURN_SECONDS;
    this.indexer = standaloneIndexer(this);
    this.book.focus = (t) => {
      const at = this.looked.get(t);
      return at !== undefined && this.now() - at <= FOCUS_MS;
    };
    if (this.prefsNow.manual) this.enter('manual');
  }

  get state(): StandaloneState {
    return this.snap;
  }
  get prefs(): StandalonePrefs {
    return this.prefsNow;
  }
  /**
   * Whether the mode was switched on in Settings. The indexer is then not asked for anything of
   * the market, not even its health.
   */
  get manual(): boolean {
    return this.snap.reason === 'manual';
  }

  subscribe(cb: (s: StandaloneState) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  /**
   * Applies new settings at once. `manual` on starts the mode, and off ends a manual one. The
   * indexer is then used again, and a down one may start the mode again.
   */
  setPrefs(p: Partial<StandalonePrefs>): void {
    if (this.disposed) return;
    this.prefsNow = { ...this.prefsNow, ...p };
    if (this.prefsNow.manual) {
      if (!this.snap.active) this.enter('manual');
      else if (this.snap.reason !== 'manual') this.set({ reason: 'manual', indexerBack: false, behind: false });
      return;
    }
    if (this.snap.reason === 'manual') this.exit();
    else if (this.snap.indexerBack && this.prefsNow.autoReturn) this.exit();
  }

  /**
   * The indexer answers but is further behind the chain than the switch bound. A lag it does not
   * report is not held against it.
   */
  private isBehind(a: HealthAssessment): boolean {
    return (a.level === 'ok' || a.level === 'warn') && typeof a.lagEstimate === 'number' && a.lagEstimate > this.lagSwitch;
  }

  /** The indexer is to be left, because it is down or behind the chain. */
  private isUnfit(a: HealthAssessment): boolean {
    return a.level === 'bad' || this.isBehind(a);
  }

  /**
   * The indexer is caught up, meaning healthy and within the return bound. A lag it does not report
   * counts as caught up.
   */
  private isCaughtUp(a: HealthAssessment): boolean {
    return a.level === 'ok' && (typeof a.lagEstimate !== 'number' || a.lagEstimate <= this.lagReturn);
  }

  /**
   * Takes the health of the indexer in use from the status poll. It starts an automatic mode when
   * the health stays bad, and ends it, or offers to, when it stays usable.
   */
  report(a: HealthAssessment): void {
    if (this.disposed || this.manual) return;
    // The banner's reason follows the indexer while an automatic mode is on: behind the chain, or
    // not usable at all.
    if (this.snap.active && this.snap.reason === 'auto') {
      const answering = (a.level === 'ok' || a.level === 'warn') && !this.isCaughtUp(a) && typeof a.lagEstimate === 'number';
      const behind = this.isUnfit(a) ? this.isBehind(a) : answering ? true : this.snap.behind;
      if (behind !== this.snap.behind) this.set({ behind });
    }
    if (this.isUnfit(a)) {
      this.disarmReturn();
      if (this.snap.indexerBack) this.set({ indexerBack: false });
      this.armEnter();
      return;
    }
    if (a.level !== 'ok' && a.level !== 'warn') return;
    if (!this.entering) {
      this.cancel('enterTimer');
      this.release();
    }
    if (!this.snap.active) return;
    if (!this.isCaughtUp(a)) {
      // Answering, but not yet within the return bound. The two looks start over.
      this.disarmReturn();
      if (this.snap.indexerBack) this.set({ indexerBack: false });
      return;
    }
    if (this.prefsNow.autoReturn) this.armReturn();
    else if (!this.snap.indexerBack) this.set({ indexerBack: true });
  }

  /**
   * Takes a failed market read of the indexer (`modeIndexer`). When the indexer is down, the
   * second look starts now instead of after the next status poll. It resolves once that is
   * decided, to true when the mode is on and answers the read, else to false.
   */
  readFailed(e: unknown): Promise<boolean> {
    if (this.snap.active) return Promise.resolve(true);
    if (this.disposed || !this.fastFallback || !this.prefsNow.auto || !isIndexerDown(e)) return Promise.resolve(false);
    this.armEnter();
    return new Promise((resolve) => this.waiters.push(() => resolve(this.snap.active)));
  }

  /** The second look at a bad indexer. Still bad after ENTER_CHECK_MS, an automatic mode starts. */
  private armEnter(): void {
    if (this.snap.active || !this.prefsNow.auto || this.enterTimer !== null || this.entering) return;
    const gen = this.gen;
    this.enterTimer = this.setTimer(() => {
      this.enterTimer = null;
      this.entering = true;
      void this.probe().then((b) => {
        this.entering = false;
        if (this.isUnfit(b) && gen === this.gen && !this.snap.active && !this.disposed && this.prefsNow.auto) this.enter('auto', this.isBehind(b));
        this.release();
      });
    }, this.enterCheckMs);
  }

  /** Answers the reads waiting for the second look, whether the mode is on or not. */
  private release(): void {
    const w = this.waiters;
    this.waiters = [];
    for (const f of w) f();
  }

  /**
   * The second look at an indexer that is usable again. Still usable after RETURN_CHECK_MS, an
   * automatic mode ends, or offers to.
   */
  private armReturn(): void {
    if (this.returnTimer !== null) return;
    const seq = ++this.returnSeq;
    const gen = this.gen;
    this.returnTimer = this.setTimer(() => {
      void this.probe().then((b) => {
        if (seq !== this.returnSeq) return;
        this.returnTimer = null;
        if (gen !== this.gen || !this.snap.active || this.snap.reason !== 'auto' || this.disposed || !this.isCaughtUp(b)) return;
        // Too soon after the start: the next usable report looks again.
        if (this.prefsNow.autoReturn && this.now() - (this.snap.sinceMs ?? 0) < MIN_AUTO_MS) return;
        if (this.prefsNow.autoReturn) this.exit();
        else if (!this.snap.indexerBack) this.set({ indexerBack: true });
      });
    }, RETURN_CHECK_MS);
  }

  private disarmReturn(): void {
    this.returnSeq++;
    this.cancel('returnTimer');
  }

  /** Ends an automatic mode, when the user switches back once the indexer answers again. */
  returnToIndexer(): void {
    if (this.snap.reason === 'auto') this.exit();
  }

  /**
   * Lists the wallet's own orders, resolved on the node from its receipts (`resolveRecord`). They
   * are listed at once, because the resolution was the node check.
   */
  adopt(snaps: readonly OrderSnapshot[], daa?: bigint): void {
    if (!this.snap.active) return;
    if (daa !== undefined && daa > this.daa) this.daa = daa;
    let changed = false;
    for (const s of snaps) {
      if (!isStreamKind(s.order.state.kind)) continue;
      const cur = this.book.get(s.covenantId);
      if (cur && cur.confirmed && cur.snap.order.transactionId === s.order.transactionId && cur.snap.order.index === s.order.index) continue;
      this.book.put(s, { origin: 'receipt', nowMs: this.now(), confirmed: true });
      this.noteScale(s);
      this.due.delete(s.covenantId.toLowerCase());
      changed = true;
    }
    if (changed) this.publish(true);
  }

  /**
   * The orders of `maker` the book lists, optionally of one token, as indexer views. The ticket's
   * own-order check reads them.
   */
  ownOrders(maker: Hex, token?: Hex): OrderView[] {
    const ctx = this.ctx();
    const out: OrderView[] = [];
    for (const e of this.book.entries()) {
      const l = listingOf(e, ctx);
      if (!l || e.snap.order.state.state.maker.toLowerCase() !== maker.toLowerCase()) continue;
      if (token && l.token.toLowerCase() !== token.toLowerCase()) continue;
      out.push(orderViewOf(e, ctx));
    }
    return out;
  }

  dispose(): void {
    this.disposed = true;
    this.exit();
    this.cancel('enterTimer');
    this.release();
    this.listeners.clear();
  }

  // --- Views of the standalone indexer ---

  ctx(): ListingContext {
    // A token the registry does not name is listed against the facts the indexer gave for it
    // before the mode, meaning its program, commitment and scale.
    return { kob: this.kob, daa: this.daa, nowUnix: BigInt(Math.floor(this.now() / 1000)), registry: (t) => this.registry?.(t) ?? this.carry.factsOf(t) };
  }
  bookView(token: Hex, q: { depth?: number; aggregate?: boolean } = {}): BookView {
    this.looked.set(token.toLowerCase(), this.now());
    return bookViewOf(this.book.entries(), token, this.ctx(), q);
  }
  orderView(id: Hex): OrderView | null {
    const e = this.book.get(id);
    return e && e.confirmed ? orderViewOf(e, this.ctx()) : null;
  }
  /**
   * The tokens with orders the node showed, and the tokens the indexer listed before with the facts
   * it gave.
   */
  tokensView(): IndexerTokenView[] {
    return mergeTokens(this.carry.tokens(), tokensViewOf(this.book.entries(), this.ctx()));
  }
  /**
   * The price basis of `token`, the base units a sompi price is per. It is what the carried
   * figures are priced on, else the registry's scale, else the scale of the first order seen of
   * the token, else 1.
   */
  basisOf(token: Hex): bigint {
    const t = token.toLowerCase();
    return this.carry.basisOf(t) ?? this.registry?.(t)?.scale ?? this.firstScale.get(t) ?? 1n;
  }
  private fillsOf(token: Hex): SeenFill[] {
    return this.fills.get(token.toLowerCase()) ?? [];
  }
  /**
   * What the activity log (ui/activity) can show in this mode.
   * - The fills the node confirmed, of every token, as `/v1/fills` events, newest first.
   * - The placements this page saw in the node's blocks, as order views with their placement
   *   transaction.
   * It covers only what happened since the mode started. It has no cancels or refunds, and no pair
   * orders, which are not followed here.
   */
  seenActivity(): { fills: EventView[]; orders: OrderView[] } {
    const fills: EventView[] = [];
    for (const list of this.fills.values()) {
      for (const f of list) {
        fills.push({
          id: f.seq, covenant_id: f.covenantId ?? '', block_seq: 0, daa: Number(f.daa), ts: f.ts, txid: f.txid, tx_pos: 0, kind: 'fill', token: f.token,
          side: f.side === 'sell' ? 1 : 2, amount: f.amount.toString(), price: f.price.toString(), payout: null, closes: false, detail: null, confirmations: null, settled: false,
        });
      }
    }
    fills.sort((a, b) => b.id - a.id);
    const ctx = this.ctx();
    const since = this.snap.sinceMs ?? 0;
    const orders: OrderView[] = [];
    for (const e of this.book.entries()) {
      if (!e.confirmed || e.origin !== 'stream' || !e.genesis.txid || e.seenMs < since) continue;
      try {
        orders.push(orderViewOf(e, ctx));
      } catch {
        /* An order this build cannot describe is not listed. */
      }
    }
    return { fills, orders };
  }
  tradesView(token: Hex, q: { limit?: number; before?: number | string } = {}) {
    const seen = tradesView(token.toLowerCase(), this.fillsOf(token), this.basisOf(token), q);
    // The carried trades are the newest page's. A page read further back (`before`) is the node's
    // own rows only.
    return q.before === undefined ? mergeTrades(this.carry.trades(token), seen, Math.max(1, Math.min(q.limit ?? 50, 500))) : seen;
  }
  candlesView(token: Hex, q: { interval: CandleInterval; from?: number; to?: number; limit?: number }) {
    const t = token.toLowerCase();
    const basis = this.basisOf(t);
    const carried = this.carry.candles(t, q.interval);
    if (!carried.length) return candlesView(t, this.fillsOf(t), basis, q);
    // The node's bars over the same window, merged with the carried ones.
    const { from, to } = q;
    return mergeCandles(carried, candlesView(t, this.fillsOf(t), basis, { interval: q.interval, ...(from !== undefined ? { from } : {}), ...(to !== undefined ? { to } : {}), limit: 1500 }), q);
  }
  statsView(token: Hex) {
    const t = token.toLowerCase();
    const seen = statsView(t, this.fillsOf(t), this.basisOf(t), this.bookView(t, { depth: 500, aggregate: true }), this.now());
    const carried = this.carry.stats(t);
    return carried ? mergeStats(carried, seen, this.now()) : seen;
  }
  depthView(token: Hex, levels?: number) {
    return depthView(token.toLowerCase(), this.bookView(token, { depth: 500, aggregate: true }), this.basisOf(token), this.now(), this.daa, levels ?? 50);
  }

  // --- Mode ---

  private async probe(): Promise<HealthAssessment> {
    let health: HealthView | null = null;
    let error: Error | null = null;
    const ctl = new AbortController();
    const t = this.setTimer(() => ctl.abort(), PROBE_TIMEOUT_MS + 500);
    try {
      health = await this.real.health({ signal: ctl.signal, timeoutMs: PROBE_TIMEOUT_MS });
    } catch (e) {
      error = e instanceof Error ? e : new Error(String(e));
    } finally {
      this.clearTimer(t);
    }
    return assessIndexerHealth({ health, error, configured: true, network: this.network });
  }

  private reset(): void {
    this.book.clear();
    this.carried.clear();
    this.due.clear();
    this.daa = 0n;
    this.queue = [];
    this.pendingFills = [];
    this.fills.clear();
    this.firstScale.clear();
  }

  private enter(reason: 'manual' | 'auto', behind = false): void {
    this.gen++;
    this.reset();
    this.subscribeFails = 0;
    this.lastRecheckMs = this.now();
    this.cancel('enterTimer');
    this.disarmReturn();
    this.seed();
    this.snap = { active: true, reason, phase: 'connecting', sinceMs: this.now(), version: this.snap.version + 1, orders: 0, indexerBack: false, behind: reason === 'auto' && behind };
    this.publish(false);
    this.emit();
    void this.connect(this.gen);
    this.scheduleTick();
    this.release();
  }

  /**
   * Starts the book from the resting orders the page last read from the indexer. They are listed as
   * they were a moment ago and asked on the node at once. One the node no longer holds leaves the
   * book, and a block that spends one moves it as it moves any order. The book check asks the node
   * again before an order is planned, so an entry that is gone by then is never taken.
   */
  private seed(): void {
    const now = this.now();
    for (const v of this.carry.orders()) {
      let snap: OrderSnapshot;
      try {
        const { foreign: _foreign, foreignUnproven: _unproven, ...s } = snapshotFromOrderView(v);
        snap = { ...s, strays: [] };
      } catch {
        continue;
      }
      if (!isStreamKind(snap.order.state.kind) || this.book.get(snap.covenantId)) continue;
      const g = v.genesis;
      const e = this.book.put(snap, { origin: 'indexer', nowMs: now, confirmed: true, genesis: { txid: g?.txid ?? null, out: g?.out ?? null, daa: BigInt(g?.daa ?? 0) } });
      this.noteScale(snap);
      this.carried.add(e.snap.covenantId);
      const daa = typeof v.last_daa === 'number' && v.last_daa > 0 ? BigInt(v.last_daa) : 0n;
      if (daa > this.daa) this.daa = daa;
    }
    if (this.carried.size) this.scheduleVerify(0);
  }

  private exit(): void {
    const wasActive = this.snap.active;
    this.gen++;
    for (const k of ['verifyTimer', 'tickTimer', 'retryTimer'] as const) this.cancel(k);
    this.disarmReturn();
    const sub = this.sub;
    this.sub = null;
    void sub?.close().catch(() => undefined);
    this.reset();
    if (!wasActive) return;
    this.snap = { active: false, reason: null, phase: 'off', sinceMs: null, version: this.snap.version + 1, orders: 0, indexerBack: false, behind: false };
    this.emit();
    this.release();
  }

  private cancel(k: 'enterTimer' | 'verifyTimer' | 'tickTimer' | 'retryTimer' | 'returnTimer'): void {
    if (this[k] !== null) this.clearTimer(this[k]);
    this[k] = null;
  }

  private emit(): void {
    for (const cb of [...this.listeners]) {
      try {
        cb(this.snap);
      } catch {
        /* A subscriber's bug must not stop the mode. */
      }
    }
  }

  private set(p: Partial<StandaloneState>): void {
    this.snap = { ...this.snap, ...p };
    this.emit();
  }

  /**
   * Publishes a new state for the views. `changed` says the book or the market data changed, which
   * moves the version.
   */
  private publish(changed: boolean, phase: StandalonePhase = this.snap.phase): void {
    if (!this.snap.active) return;
    const ctx = this.ctx();
    let orders = 0;
    for (const e of this.book.entries()) if (listingOf(e, ctx)) orders++;
    if (!changed && phase === this.snap.phase && orders === this.snap.orders) return;
    this.set({ phase, orders, version: this.snap.version + (changed ? 1 : 0) });
  }

  private async connect(gen: number): Promise<void> {
    if (gen !== this.gen || !this.node.subscribeBlocks) return;
    try {
      const sub = await this.node.subscribeBlocks(
        (b) => gen === this.gen && this.onBlock(b),
        () => gen === this.gen && this.onGap(),
      );
      if (gen !== this.gen) return void sub.close().catch(() => undefined);
      this.sub = sub;
      this.subscribeFails = 0;
      this.lastBlockMs = this.now();
      this.publish(false, 'live');
    } catch {
      if (gen !== this.gen) return;
      const ms = Math.min(SUBSCRIBE_RETRY_MAX_MS, SUBSCRIBE_RETRY_MS * 2 ** Math.min(this.subscribeFails++, 6));
      this.cancel('retryTimer');
      this.retryTimer = this.setTimer(() => {
        this.retryTimer = null;
        void this.connect(gen);
      }, ms);
    }
  }

  /**
   * Blocks may have been missed, so every confirmed order is asked again. A spend the stream did
   * not see takes it out.
   */
  private onGap(): void {
    this.lastRecheckMs = 0;
    this.scheduleVerify(0);
  }

  private scheduleTick(): void {
    this.cancel('tickTimer');
    const gen = this.gen;
    this.tickTimer = this.setTimer(() => {
      this.tickTimer = null;
      if (gen !== this.gen) return;
      const now = this.now();
      if (this.snap.phase === 'live' && now - this.lastBlockMs > STALL_MS) {
        // A subscription went quiet, from a node that stopped notifying or a link the SDK did not
        // report. Subscribe again.
        const sub = this.sub;
        this.sub = null;
        void sub?.close().catch(() => undefined);
        this.publish(false, 'connecting');
        void this.connect(gen);
        this.onGap();
      }
      if (now - this.lastRecheckMs >= RECHECK_MS) this.scheduleVerify(0);
      // An automatic mode looks at the indexer itself, because the status poll is slower. Usable
      // again, the mode ends after one more look (`armReturn`).
      if (this.snap.reason === 'auto' && !this.watching && this.returnTimer === null) {
        this.watching = true;
        void this.probe().then((a) => {
          this.watching = false;
          if (gen === this.gen) this.report(a);
        });
      }
      // An order that expired or reached its deadline leaves the book on the clock alone.
      this.publish(false);
      this.scheduleTick();
    }, TICK_MS);
  }

  // --- Blocks ---

  private onBlock(b: StreamBlock): void {
    this.lastBlockMs = this.now();
    if (this.queue.length >= MAX_QUEUE) {
      this.queue.splice(0, this.queue.length - MAX_QUEUE + 1);
      this.onGap();
    }
    this.queue.push(b);
    if (!this.draining) void this.drain(this.gen);
  }

  private async drain(gen: number): Promise<void> {
    this.draining = true;
    try {
      while (this.queue.length && gen === this.gen) {
        const b = this.queue.shift()!;
        if (b.daaScore > this.daa) this.daa = b.daaScore;
        let changed = false;
        for (const tx of b.transactions) {
          if (this.seenTx(tx.id)) continue;
          if (await this.readTx(tx, b, gen)) changed = true;
          if (gen !== this.gen) return;
        }
        if (changed) {
          this.scheduleVerify(VERIFY_DELAY_MS);
          this.publish(true);
        }
      }
    } finally {
      if (gen === this.gen) this.draining = false;
    }
  }

  private seenTx(id: Hex): boolean {
    if (this.seen.has(id) || this.seenOld.has(id)) return true;
    this.seen.add(id);
    if (this.seen.size >= SEEN_TXS) {
      this.seenOld = this.seen;
      this.seen = new Set();
    }
    return false;
  }

  private noteScale(s: OrderSnapshot): void {
    try {
      const d = describeOrder(s.order.state, this.kob);
      const t = d.tokenCovId.toLowerCase();
      if (!this.firstScale.has(t) && d.scale > 0n) this.firstScale.set(t, d.scale);
    } catch {
      /* Not a readable order, so no scale. */
    }
  }

  /**
   * Reads one transaction. Its placements join the book, to be confirmed. Every input that spends a
   * KOB order moves that order to its continuation, or out of the book, and records its fill.
   * Returns true when anything changed.
   */
  private async readTx(tx: TxJson, b: StreamBlock, gen: number): Promise<boolean> {
    const daa = b.daaScore;
    const now = this.now();
    let changed = false;
    const put = (snap: OrderSnapshot, origin: StreamOrigin, genesis?: { txid: Hex | null; out: number | null; daa: bigint }) => {
      const e = this.book.put(snap, { origin, nowMs: now, ...(genesis ? { genesis } : {}) });
      this.noteScale(snap);
      this.due.set(e.snap.covenantId, now + VERIFY_DELAY_MS);
      changed = true;
    };
    if (tx.payload) {
      for (const r of txFacts(this.kob, tx).placements) {
        if (this.book.get(r.covenantId)) continue;
        put(snapshotOfPlacement(r, daa), 'stream', { txid: r.transactionId, out: r.output, daa });
      }
    }
    for (let i = 0; i < tx.inputs.length; i++) {
      const key = outpointKey(tx.inputs[i]!.transactionId, tx.inputs[i]!.index);
      const known = this.book.orderAt(key);
      const spend = revealSpend(this.kob, tx, i);
      if (!spend) {
        // The input spends a followed order but does not read as one. It is not believed to be the
        // same order any more.
        if (known) {
          this.book.remove(known.snap.covenantId);
          this.due.delete(known.snap.covenantId);
          changed = true;
        }
        continue;
      }
      const next = await followSpend(this.kob, spend, tx, daa, this.network, known?.snap ?? null);
      if (gen !== this.gen) return false;
      if (known && !next) {
        this.book.remove(known.snap.covenantId);
        this.due.delete(known.snap.covenantId);
        changed = true;
      } else if (next) {
        // An order placed before the mode started is learned from the transaction that continues
        // it.
        put(next, known?.origin ?? 'stream', known ? undefined : { txid: null, out: null, daa });
      }
      const fill = fillOf(this.kob, spend, tx.id, next?.covenantId ?? known?.snap.covenantId ?? null);
      if (fill) this.addPendingFill(fill, tx, b);
    }
    // A custody was spent by a transaction that does not spend its order. The order is asked again
    // before it is listed again.
    for (const e of this.book.touched(tx.inputs.map((x) => outpointKey(x.transactionId, x.index)))) {
      if (!e.confirmed || e.snap.order.transactionId === tx.id) continue;
      this.book.miss(e.snap.covenantId, VERIFY_RETRY_MS.length + 1);
      this.due.set(e.snap.covenantId, now);
      changed = true;
    }
    return changed;
  }

  private addPendingFill(fill: StreamFill, tx: TxJson, b: StreamBlock): void {
    // The transaction counts once the node shows one of its outputs. That is the continuation of
    // the filled order, else any covenant output, else the first.
    const cont = fill.covenantId ? tx.outputs.findIndex((o) => o.covenant?.covenantId === fill.covenantId) : -1;
    const any = tx.outputs.findIndex((o) => !!o.covenant);
    const index = cont >= 0 ? cont : any >= 0 ? any : 0;
    const out = tx.outputs[index];
    if (!out) return;
    if (this.pendingFills.length >= MAX_PENDING_FILLS) this.pendingFills.shift();
    this.pendingFills.push({ fill, ts: b.timestampMs || this.now(), daa: b.daaScore, probe: { transactionId: tx.id, index, scriptPublicKey: out.scriptPublicKey }, due: this.now() + VERIFY_DELAY_MS, misses: 0 });
  }

  // --- Node checks ---

  private scheduleVerify(ms: number): void {
    if (this.verifyTimer !== null) return;
    const gen = this.gen;
    this.verifyTimer = this.setTimer(() => {
      this.verifyTimer = null;
      if (gen === this.gen) void this.verify(gen);
    }, ms);
  }

  /**
   * Confirms the unconfirmed entries and fills that are due and, every RECHECK_MS, a batch of the
   * confirmed entries.
   */
  private async verify(gen: number): Promise<void> {
    if (this.verifying) return void this.scheduleVerify(VERIFY_DELAY_MS);
    this.verifying = true;
    let changed = false;
    try {
      const now = this.now();
      const pending = [...this.book.entries()].filter((e) => !e.confirmed && (this.due.get(e.snap.covenantId) ?? 0) <= now);
      const rechecking = now - this.lastRecheckMs >= RECHECK_MS;
      // The entries started from the kept orders are asked at once, all of them.
      const carried = [...this.carried].flatMap((id) => {
        const e = this.book.get(id);
        return e && e.confirmed ? [e] : [];
      });
      this.carried.clear();
      const recheck = rechecking ? [...this.book.entries()].filter((e) => e.confirmed && !carried.includes(e)).slice(0, RECHECK_BATCH) : [];
      if (rechecking) this.lastRecheckMs = now;
      const all = [...pending, ...carried, ...recheck];
      for (let i = 0; i < all.length && gen === this.gen; i += VERIFY_CHUNK) {
        const part = all.slice(i, i + VERIFY_CHUNK);
        let res: (OrderSnapshot | SnapshotError)[];
        try {
          res = await confirmSnapshotsOnNode(this.kob, this.node, part.map((e) => e.snap), this.toAddress);
        } catch {
          // The node could not be asked. Nothing is counted against the orders, and the pending and
          // the kept ones are asked again.
          for (const e of part) {
            if (!e.confirmed) this.due.set(e.snap.covenantId, this.now() + VERIFY_RETRY_MS[0]!);
            else if (carried.includes(e)) this.carried.add(e.snap.covenantId);
          }
          continue;
        }
        if (gen !== this.gen) return;
        part.forEach((e, k) => {
          const r = res[k]!;
          const id = e.snap.covenantId;
          if (this.book.get(id) !== e) return; // changed meanwhile: its new state is checked on its own
          if (!(r instanceof SnapshotError)) {
            if (!e.confirmed) changed = true;
            this.book.confirm(id, r);
            this.due.delete(id);
            return;
          }
          // An unconfirmed one is asked a few more times, because its block may not be merged yet.
          // A confirmed one the node no longer holds is out at once.
          const removed = this.book.miss(id, e.confirmed ? 1 : VERIFY_RETRY_MS.length + 1);
          if (removed) this.due.delete(id);
          else this.due.set(id, this.now() + VERIFY_RETRY_MS[Math.min(e.misses - 1, VERIFY_RETRY_MS.length - 1)]!);
          changed = true;
        });
      }
      if (gen === this.gen && (await this.verifyFills(gen))) changed = true;
      if (changed && gen === this.gen) this.publish(true);
    } finally {
      this.verifying = false;
    }
    if (gen !== this.gen) return;
    const next = [...this.due.values(), ...this.pendingFills.map((f) => f.due), ...(this.carried.size ? [this.now() + VERIFY_RETRY_MS[0]!] : [])];
    if (next.length) this.scheduleVerify(Math.max(200, Math.min(...next) - this.now()));
  }

  /**
   * Counts the fills whose transaction the node shows, meaning one of its outputs is in the UTXO
   * set. The others are asked a few more times.
   */
  private async verifyFills(gen: number): Promise<boolean> {
    const now = this.now();
    const due = this.pendingFills.filter((f) => f.due <= now);
    if (!due.length) return false;
    let changed = false;
    for (let i = 0; i < due.length; i += VERIFY_CHUNK) {
      const part = due.slice(i, i + VERIFY_CHUNK);
      let facts: Map<string, unknown>;
      try {
        facts = await fetchNodeInputFacts(this.node, part.map((f) => f.probe), this.toAddress);
      } catch {
        for (const f of part) f.due = this.now() + VERIFY_RETRY_MS[0]!;
        continue;
      }
      if (gen !== this.gen) return false;
      for (const f of part) {
        if (facts.has(outpointKey(f.probe.transactionId, f.probe.index))) {
          this.pendingFills = this.pendingFills.filter((x) => x !== f);
          const t = f.fill.token.toLowerCase();
          const list = this.fills.get(t) ?? [];
          list.push({ ...f.fill, seq: ++this.fillSeq, ts: f.ts, daa: f.daa });
          if (list.length > MAX_FILLS_PER_TOKEN) list.splice(0, list.length - MAX_FILLS_PER_TOKEN);
          this.fills.set(t, list);
          if (!this.firstScale.has(t)) this.firstScale.set(t, f.fill.scale);
          changed = true;
        } else if (++f.misses > VERIFY_RETRY_MS.length) {
          this.pendingFills = this.pendingFills.filter((x) => x !== f);
        } else f.due = this.now() + VERIFY_RETRY_MS[f.misses - 1]!;
      }
    }
    return changed;
  }
}

/**
 * The `IndexerApi` of standalone mode. It answers the book, its orders, the token list and the
 * market data from what was seen. Everything else is unavailable.
 */
function standaloneIndexer(c: StandaloneController): IndexerApi {
  const no = (what: string) => async (): Promise<never> => {
    throw standaloneUnavailable(what);
  };
  return {
    health: no('the indexer health'),
    tokens: async () => c.tokensView(),
    declaration: no('the token declaration'),
    book: async (token, q) => c.bookView(token, q ?? {}),
    order: async (id) => {
      const v = c.orderView(id);
      // An order the book does not hold is not known to be gone, so this is an error and never
      // "not found".
      if (!v) throw standaloneUnavailable('this order');
      return v;
    },
    orders: no('the order list'),
    allOrders: no('the order list'),
    orderEvents: no('the order history'),
    fills: no('the fill history'),
    strays: no('the stray list'),
    tokenEvents: no('the token history'),
    tokenUtxos: no('the token holdings'),
    trades: async (token, q) => c.tradesView(token, q ?? {}),
    candles: async (token, q) => c.candlesView(token, q),
    stats: async (token) => c.statsView(token),
    // There is no history of its own, so the Activity summary is not shown in standalone mode.
    activity: async () => null,
    depth: async (token, q) => c.depthView(token, q?.levels),
    pairBook: no('the pair book'),
    pairs: no('the pair list'),
    pairCandles: no('the pair chart'),
    pairFills: no('the pair fills'),
  };
}

/** How many of a market's best asks get their full view, with the custody, beside its orders. */
const CUSTODY_READS = 20;

/**
 * The reads standalone mode answers itself. One of them that finds the indexer down is answered
 * from the mode once it is on.
 */
const SERVED: ReadonlySet<string | symbol> = new Set(['tokens', 'book', 'order', 'trades', 'candles', 'stats', 'depth']);

/**
 * The app's indexer with standalone mode.
 *
 * It is the indexer (`real`, the failover proxy or the single indexer) while that is in use, and
 * the standalone views while the mode is active. `health` goes to the indexer unless the setting
 * turned the mode on, because the status poll must see the indexer come back. A read that finds
 * the indexer down starts the check of an automatic mode at once (`readFailed`). A read the mode
 * serves is then answered from it.
 */
export function modeIndexer(
  real: IndexerApi,
  c: Pick<StandaloneController, 'state' | 'indexer'> & { carry?: IndexerCarry; readFailed?: (e: unknown) => Promise<boolean> },
): IndexerApi {
  return new Proxy({} as IndexerApi, {
    get: (_t, prop) => {
      const useReal = (prop === 'health' && c.state.reason !== 'manual') || !c.state.active;
      const target = (useReal ? real : c.indexer) as unknown as Record<string | symbol, unknown>;
      const v = target[prop];
      if (typeof v !== 'function') return v;
      const call = (v as (...x: unknown[]) => unknown).bind(target);
      if (!useReal) return call;
      // What the indexer in use answers is remembered, to be kept when standalone mode starts
      // (data/stream-carry.ts).
      const rec = c.carry ? recording(c.carry, prop, call, real) : call;
      const readFailed = c.readFailed;
      if (prop === 'health' || !readFailed) return rec;
      const served = SERVED.has(prop);
      return async (...a: unknown[]) => {
        try {
          return await rec(...a);
        } catch (e) {
          const on = readFailed.call(c, e);
          if (!served || !(await on)) throw e;
          const alt = (c.indexer as unknown as Record<string | symbol, (...x: unknown[]) => unknown>)[prop]!;
          return alt.apply(c.indexer, a);
        }
      };
    },
    has: (_t, prop) => prop in real,
  });
}

/**
 * Wraps `call` of the indexer so that its answers to the market reads are remembered in `carry`.
 * The other reads pass through. A read of a book also has the market's resting orders read.
 */
function recording(carry: IndexerCarry, prop: string | symbol, call: (...x: unknown[]) => unknown, real?: IndexerApi): (...x: unknown[]) => unknown {
  const noted = (read: (r: never) => void) => async (...a: unknown[]) => {
    const r = await call(...a);
    try {
      read(r as never);
    } catch {
      /* Remembering must never fail a read. */
    }
    return r;
  };
  switch (prop) {
    case 'tokens':
      return noted((r: IndexerTokenView[]) => carry.recordTokens(r));
    case 'trades':
      return (...a) => noted((r: TradesView | null) => carry.recordTrades(a[0] as Hex, a[1] as { before?: unknown } | undefined, r))(...a);
    case 'candles':
      return (...a) => noted((r: CandlesView | null) => carry.recordCandles(a[0] as Hex, a[1] as { interval: CandleInterval; from?: number }, r))(...a);
    case 'stats':
      return (...a) => noted((r: StatsView | null) => carry.recordStats(a[0] as Hex, r))(...a);
    case 'order':
      return noted((r: OrderView | null) => carry.recordOrder(r));
    case 'allOrders':
      return (...a) =>
        noted((r: OrderView[]) => {
          const q = (a[0] ?? {}) as { token?: Hex; maker?: Hex; status?: string };
          if (q.token && !q.maker && q.status === 'active') carry.recordOrders(q.token, r);
          else for (const v of r) carry.recordOrder(v);
        })(...a);
    case 'book':
      return (...a) =>
        noted(() => {
          // The resting orders of the market are read with their states beside its book, at most
          // once per ORDERS_READ_MS. Standalone mode starts its book from them.
          const token = a[0] as Hex;
          if (!real || !carry.ordersDue(token)) return;
          void real.allOrders({ token, status: 'active', limit: 200 }, { maxPages: 3, priority: 'low' }).then(
            async (r) => {
              carry.recordOrders(token, r);
              // The list has no custody of an ask, so the best asks are read one by one. Only new
              // or moved ones are read, and a kept view at the same outpoint stays.
              const ids = carry.needsCustody(token, CUSTODY_READS);
              for (let i = 0; i < ids.length; i += 4) {
                const got = await Promise.all(ids.slice(i, i + 4).map((id) => real.order(id, { priority: 'low' }).catch(() => null)));
                for (const v of got) carry.recordOrder(v);
              }
            },
            () => carry.ordersFailed(token),
          ).catch(() => undefined);
        })(...a);
    default:
      return call;
  }
}
