// Standalone mode: the market from the node alone. The browser follows the node's new blocks (`NodeApi.subscribeBlocks`), reads the KOB1 placement
// records of every transaction with kob-wasm (kob/stream-orders.ts: the decoder the indexer uses, record by record) and the spends of KOB orders
// (fills, cancels, amends), follows the orders it saw to their continuations, and confirms every one on the node before it is listed
// (kob/node-verify.ts `confirmSnapshotsOnNode`: the order UTXO, and an ask's custody, unspent at the scripts its state derives under the templates
// this build pins; the same check the book gets before an order is taken, app/book-verify.ts). The wallet's own orders are added from its receipts
// once the node resolved them (`adopt`). Trades, candles, the 24 h figures and the depth are built from what was seen (data/stream-market.ts); a fill
// counts once the node shows an output of its transaction. Everything is PARTIAL: an order placed before the mode started, and not touched since, is
// not seen (no server and no other party is asked; covenant-id lookups on the node do not exist yet): the views say "since HH:MM".
//
// When (the user's three settings, `StandalonePrefs`):
//   * `manual`: on from the start, whatever the indexer does; the app then sends the indexer no market request and does not poll its health;
//   * `auto` (default on): the health of the indexer in use (`report`, from the status poll, ui/shell/status.ts) that is `bad` (down, stale, on another
//     network, without its node) is asked once more after ENTER_CHECK_MS; still bad, standalone mode starts. With failover (data/indexer-failover.ts)
//     the report is about the active indexer: a usable backup keeps the app on the indexers;
//   * `autoReturn` (default on): the first report that the indexer is usable again (`ok`, or `warn`: reachable, minutes behind) ends an automatic
//     standalone mode and the indexer's book is read again (its book wins). Off: the mode stays and `indexerBack` says it may end
//     (`returnToIndexer`, the button of the bell's entry).
//
// While active, the app's indexer (`modeIndexer`) answers the book, the orders of the book, the token list and the market data from here; what
// needs the indexer's records (pair books, holders and token UTXOs, the order lists of My orders and the notifications) fails as it does with the
// indexer down.
import type { IndexerApi } from './indexer';
import { IndexerError } from './indexer';
import type { BookView, CandleInterval, EventView, HealthView, IndexerTokenView, OrderView } from './indexer-types';
import type { BlockSubscription, NodeApi } from './node';
import type { StreamBlock } from './block-stream';
import { StreamBook, bookViewOf, listingOf, orderViewOf, tokensViewOf, type ListingContext, type RegistryFacts } from './stream-book';
import { MAX_FILLS_PER_TOKEN, candlesView, depthView, statsView, tradesView, type SeenFill } from './stream-market';
import { SnapshotError, type OrderSnapshot } from '../kob/cancel';
import { confirmSnapshotsOnNode, fetchNodeInputFacts, outpointKey, type OutpointRef } from '../kob/node-verify';
import { describeOrder } from '../kob/order-facts';
import { fillOf, followSpend, isStreamKind, revealSpend, snapshotOfPlacement, txFacts, type StreamFill } from '../kob/stream-orders';
import type { Hex, TxJson } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { assessIndexerHealth, type HealthAssessment } from '../ui/shell/health';

/** The user's choices (Settings): standalone always on; fall back to it when the indexer is down; return to the indexer when it is back. */
export interface StandalonePrefs {
  manual: boolean;
  auto: boolean;
  autoReturn: boolean;
}
export const DEFAULT_STANDALONE: Readonly<StandalonePrefs> = Object.freeze({ manual: false, auto: true, autoReturn: true });

/** A bad report is checked once more after this long before the mode starts (a blip does not start it). */
export const ENTER_CHECK_MS = 5_000;
/** With further indexers the failover gets this long to find a usable one first (its re-checks and one round of probes take about 12 s). */
export const ENTER_CHECK_FAILOVER_MS = 20_000;
/** That check of the indexer is cut off after this long. */
export const PROBE_TIMEOUT_MS = 6_000;
/** New or changed orders (and fills) are confirmed on the node this long after the block that showed them (more of them in one read). */
export const VERIFY_DELAY_MS = 1_500;
/** An unconfirmed order or fill is asked again after these delays (a block's transactions reach the node's UTXO set once it is merged), then dropped. */
export const VERIFY_RETRY_MS: readonly number[] = [2_000, 4_000, 8_000, 16_000];
/** Confirmed orders are confirmed again this often (a spend in a block the stream missed), at most RECHECK_BATCH per round. */
export const RECHECK_MS = 30_000;
export const RECHECK_BATCH = 100;
/** No block for this long while following: the subscription is made again and every order confirmed again. */
export const STALL_MS = 30_000;
/** A failed subscription is tried again after this long, doubled up to SUBSCRIBE_RETRY_MAX_MS. */
export const SUBSCRIBE_RETRY_MS = 5_000;
export const SUBSCRIBE_RETRY_MAX_MS = 60_000;
/** The housekeeping tick (stall watch, rechecks), ms. */
const TICK_MS = 5_000;
/** Most orders or fills confirmed in one node read. */
const VERIFY_CHUNK = 100;
/** Transactions remembered as read (a transaction is in several parallel blocks). */
const SEEN_TXS = 20_000;
/** Blocks waiting to be read; past it the oldest are dropped and every order is confirmed again. */
const MAX_QUEUE = 500;
/** Fills waiting for the node; past it the oldest are dropped. */
const MAX_PENDING_FILLS = 2_000;

export type StandalonePhase = 'off' | 'connecting' | 'live';

export interface StandaloneState {
  active: boolean;
  /** `manual`: the setting; `auto`: the indexer could not be used */
  reason: 'manual' | 'auto' | null;
  /** `connecting`: subscribing to the node's blocks (again); `live`: following them */
  phase: StandalonePhase;
  /** when the mode started (ms since the epoch): what happened before is not shown */
  sinceMs: number | null;
  /** counts every change of the book or the market data (a view reads it again) */
  version: number;
  /** orders listed now */
  orders: number;
  /** an automatic mode with `autoReturn` off: the indexer answers again (`returnToIndexer` ends the mode) */
  indexerBack: boolean;
}

export interface StandaloneDeps {
  kob: KobWasm;
  node: Pick<NodeApi, 'getUtxosByAddresses' | 'subscribeBlocks'>;
  /** script public key -> address of the app's network (the node is asked by address) */
  toAddress: (spk: string) => string;
  network: string;
  /** the indexer the app reads otherwise: asked once more before an automatic mode starts */
  indexer: Pick<IndexerApi, 'health'>;
  prefs?: Partial<StandalonePrefs>;
  /** how long a bad report is waited on before the indexer is asked again (default ENTER_CHECK_MS; longer with failover, which switches first) */
  enterCheckMs?: number;
  /** the registry's facts of a token: its orders are listed only with them, and its scale is the price basis */
  registry?: (token: Hex) => RegistryFacts | null;
  now?: () => number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (h: unknown) => void;
}

const isUsable = (a: HealthAssessment): boolean => a.level === 'ok' || a.level === 'warn';

/** The error every read that needs the indexer gets in standalone mode. */
export function standaloneUnavailable(what = 'this'): IndexerError {
  return new IndexerError('http', 503, 'standalone_mode', `Standalone mode: ${what} needs the indexer.`);
}

/** A fill waiting for the node to show its transaction (one of its outputs). */
interface PendingFill { fill: StreamFill; ts: number; daa: bigint; probe: OutpointRef; due: number; misses: number }

export class StandaloneController {
  readonly indexer: IndexerApi;
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
  private readonly book = new StreamBook();
  private readonly listeners = new Set<(s: StandaloneState) => void>();
  private snap: StandaloneState = { active: false, reason: null, phase: 'off', sinceMs: null, version: 0, orders: 0, indexerBack: false };
  private gen = 0;
  private daa = 0n;
  private sub: BlockSubscription | null = null;
  private subscribeFails = 0;
  private lastBlockMs = 0;
  private enterTimer: unknown = null;
  private verifyTimer: unknown = null;
  private tickTimer: unknown = null;
  private retryTimer: unknown = null;
  private lastRecheckMs = 0;
  /** next node check of each unconfirmed entry, ms */
  private readonly due = new Map<Hex, number>();
  private pendingFills: PendingFill[] = [];
  private readonly fills = new Map<Hex, SeenFill[]>();
  private fillSeq = 0;
  /** the scale of the first order seen of each token (its price basis when the registry has no decimals) */
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
    this.now = d.now ?? Date.now;
    this.setTimer = d.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = d.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
    this.prefsNow = { ...DEFAULT_STANDALONE, ...(d.prefs ?? {}) };
    this.enterCheckMs = d.enterCheckMs ?? ENTER_CHECK_MS;
    this.indexer = standaloneIndexer(this);
    if (this.prefsNow.manual) this.enter('manual');
  }

  get state(): StandaloneState {
    return this.snap;
  }
  get prefs(): StandalonePrefs {
    return this.prefsNow;
  }
  /** the mode was switched on in Settings: the indexer is not asked for anything of the market, not even its health */
  get manual(): boolean {
    return this.snap.reason === 'manual';
  }

  subscribe(cb: (s: StandaloneState) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  /** New settings, applied at once: `manual` on starts the mode, off ends a manual one (the indexer is used again; a down one may start it again). */
  setPrefs(p: Partial<StandalonePrefs>): void {
    if (this.disposed) return;
    this.prefsNow = { ...this.prefsNow, ...p };
    if (this.prefsNow.manual) {
      if (!this.snap.active) this.enter('manual');
      else if (this.snap.reason !== 'manual') this.set({ reason: 'manual', indexerBack: false });
      return;
    }
    if (this.snap.reason === 'manual') this.exit();
    else if (this.snap.indexerBack && this.prefsNow.autoReturn) this.exit();
  }

  /** The health of the indexer in use (the status poll): starts an automatic mode when it stays bad, ends it (or offers to) when it is usable again. */
  report(a: HealthAssessment): void {
    if (this.disposed || this.manual) return;
    if (isUsable(a)) {
      this.cancel('enterTimer');
      if (!this.snap.active) return;
      if (this.prefsNow.autoReturn) this.exit();
      else if (!this.snap.indexerBack) this.set({ indexerBack: true });
      return;
    }
    if (a.level !== 'bad') return;
    if (this.snap.indexerBack) this.set({ indexerBack: false });
    if (this.snap.active || !this.prefsNow.auto || this.enterTimer !== null) return;
    const gen = this.gen;
    this.enterTimer = this.setTimer(() => {
      this.enterTimer = null;
      void this.probe().then((b) => {
        if (b.level === 'bad' && gen === this.gen && !this.snap.active && !this.disposed && this.prefsNow.auto) this.enter('auto');
      });
    }, this.enterCheckMs);
  }

  /** Ends an automatic mode (the user switches back once the indexer answers again). */
  returnToIndexer(): void {
    if (this.snap.reason === 'auto') this.exit();
  }

  /** The wallet's own orders, resolved on the node from its receipts (`resolveRecord`): listed at once (the resolution was the node check). */
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

  /** The orders of `maker` the book lists (optionally of one token), as indexer views: the ticket's own-order check reads them. */
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
    this.listeners.clear();
  }

  // ------------------------------------------------------------------------------------------ views (the standalone indexer)

  ctx(): ListingContext {
    return { kob: this.kob, daa: this.daa, nowUnix: BigInt(Math.floor(this.now() / 1000)), ...(this.registry ? { registry: this.registry } : {}) };
  }
  bookView(token: Hex, q: { depth?: number; aggregate?: boolean } = {}): BookView {
    return bookViewOf(this.book.entries(), token, this.ctx(), q);
  }
  orderView(id: Hex): OrderView | null {
    const e = this.book.get(id);
    return e && e.confirmed ? orderViewOf(e, this.ctx()) : null;
  }
  tokensView(): IndexerTokenView[] {
    return tokensViewOf(this.book.entries(), this.ctx());
  }
  /** Sompi per this many base units: the registry's scale, else the scale of the first order seen of the token, else 1. */
  basisOf(token: Hex): bigint {
    const t = token.toLowerCase();
    return this.registry?.(t)?.scale ?? this.firstScale.get(t) ?? 1n;
  }
  private fillsOf(token: Hex): SeenFill[] {
    return this.fills.get(token.toLowerCase()) ?? [];
  }
  /**
   * What the activity log can show in this mode (ui/activity): the fills the node confirmed (every token, as `/v1/fills` events, newest first) and the
   * placements this page saw in the node's blocks (as order views, with their placement transaction). Only what happened since the mode started; no
   * cancels, refunds or pair orders (phase 1 of the log, and pair orders are not followed here).
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
        /* an order this build cannot describe is not listed */
      }
    }
    return { fills, orders };
  }
  tradesView(token: Hex, q: { limit?: number; before?: number | string } = {}) {
    return tradesView(token.toLowerCase(), this.fillsOf(token), this.basisOf(token), q);
  }
  candlesView(token: Hex, q: { interval: CandleInterval; from?: number; to?: number; limit?: number }) {
    return candlesView(token.toLowerCase(), this.fillsOf(token), this.basisOf(token), q);
  }
  statsView(token: Hex) {
    return statsView(token.toLowerCase(), this.fillsOf(token), this.basisOf(token), this.bookView(token, { depth: 500, aggregate: true }), this.now());
  }
  depthView(token: Hex, levels?: number) {
    return depthView(token.toLowerCase(), this.bookView(token, { depth: 500, aggregate: true }), this.basisOf(token), this.now(), this.daa, levels ?? 50);
  }

  // ------------------------------------------------------------------------------------------ mode

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
    this.due.clear();
    this.daa = 0n;
    this.queue = [];
    this.pendingFills = [];
    this.fills.clear();
    this.firstScale.clear();
  }

  private enter(reason: 'manual' | 'auto'): void {
    this.gen++;
    this.reset();
    this.subscribeFails = 0;
    this.lastRecheckMs = this.now();
    this.snap = { active: true, reason, phase: 'connecting', sinceMs: this.now(), version: this.snap.version + 1, orders: 0, indexerBack: false };
    this.emit();
    void this.connect(this.gen);
    this.scheduleTick();
  }

  private exit(): void {
    const wasActive = this.snap.active;
    this.gen++;
    for (const k of ['verifyTimer', 'tickTimer', 'retryTimer'] as const) this.cancel(k);
    const sub = this.sub;
    this.sub = null;
    void sub?.close().catch(() => undefined);
    this.reset();
    if (!wasActive) return;
    this.snap = { active: false, reason: null, phase: 'off', sinceMs: null, version: this.snap.version + 1, orders: 0, indexerBack: false };
    this.emit();
  }

  private cancel(k: 'enterTimer' | 'verifyTimer' | 'tickTimer' | 'retryTimer'): void {
    if (this[k] !== null) this.clearTimer(this[k]);
    this[k] = null;
  }

  private emit(): void {
    for (const cb of [...this.listeners]) {
      try {
        cb(this.snap);
      } catch {
        /* a subscriber's bug must not stop the mode */
      }
    }
  }

  private set(p: Partial<StandaloneState>): void {
    this.snap = { ...this.snap, ...p };
    this.emit();
  }

  /** A new state for the views; `changed`: the book or the market data changed (the version moves). */
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

  /** Blocks may have been missed: every confirmed order is asked again (a spend the stream did not see takes it out). */
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
        // a subscription that went quiet (a node that stopped notifying, a link the SDK did not report): subscribe again
        const sub = this.sub;
        this.sub = null;
        void sub?.close().catch(() => undefined);
        this.publish(false, 'connecting');
        void this.connect(gen);
        this.onGap();
      }
      if (now - this.lastRecheckMs >= RECHECK_MS) this.scheduleVerify(0);
      // an order that expired or reached its deadline leaves the book on the clock alone
      this.publish(false);
      this.scheduleTick();
    }, TICK_MS);
  }

  // ------------------------------------------------------------------------------------------ blocks

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
      /* not a readable order: no scale */
    }
  }

  /**
   * One transaction: its placements join the book (to be confirmed), every input that spends a KOB order moves that order to its continuation
   * (or out of the book) and records its fill. True when anything changed.
   */
  private async readTx(tx: TxJson, b: StreamBlock, gen: number): Promise<boolean> {
    const daa = b.daaScore;
    const now = this.now();
    let changed = false;
    const put = (snap: OrderSnapshot, origin: 'stream' | 'receipt', genesis?: { txid: Hex | null; out: number | null; daa: bigint }) => {
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
        // the input spends a followed order but does not read as one: it is not believed to be the same order any more
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
        // an order placed before the mode started is learned from the transaction that continues it
        put(next, known?.origin ?? 'stream', known ? undefined : { txid: null, out: null, daa });
      }
      const fill = fillOf(this.kob, spend, tx.id, next?.covenantId ?? known?.snap.covenantId ?? null);
      if (fill) this.addPendingFill(fill, tx, b);
    }
    // a custody spent by a transaction that does not spend its order: the order is asked again before it is listed again
    for (const e of this.book.touched(tx.inputs.map((x) => outpointKey(x.transactionId, x.index)))) {
      if (!e.confirmed || e.snap.order.transactionId === tx.id) continue;
      this.book.miss(e.snap.covenantId, VERIFY_RETRY_MS.length + 1);
      this.due.set(e.snap.covenantId, now);
      changed = true;
    }
    return changed;
  }

  private addPendingFill(fill: StreamFill, tx: TxJson, b: StreamBlock): void {
    // the transaction counts once the node shows one of its outputs: the continuation of the filled order, else any covenant output, else the first
    const cont = fill.covenantId ? tx.outputs.findIndex((o) => o.covenant?.covenantId === fill.covenantId) : -1;
    const any = tx.outputs.findIndex((o) => !!o.covenant);
    const index = cont >= 0 ? cont : any >= 0 ? any : 0;
    const out = tx.outputs[index];
    if (!out) return;
    if (this.pendingFills.length >= MAX_PENDING_FILLS) this.pendingFills.shift();
    this.pendingFills.push({ fill, ts: b.timestampMs || this.now(), daa: b.daaScore, probe: { transactionId: tx.id, index, scriptPublicKey: out.scriptPublicKey }, due: this.now() + VERIFY_DELAY_MS, misses: 0 });
  }

  // ------------------------------------------------------------------------------------------ node checks

  private scheduleVerify(ms: number): void {
    if (this.verifyTimer !== null) return;
    const gen = this.gen;
    this.verifyTimer = this.setTimer(() => {
      this.verifyTimer = null;
      if (gen === this.gen) void this.verify(gen);
    }, ms);
  }

  /** Confirms the unconfirmed entries and fills that are due and, every RECHECK_MS, a batch of the confirmed entries. */
  private async verify(gen: number): Promise<void> {
    if (this.verifying) return void this.scheduleVerify(VERIFY_DELAY_MS);
    this.verifying = true;
    let changed = false;
    try {
      const now = this.now();
      const pending = [...this.book.entries()].filter((e) => !e.confirmed && (this.due.get(e.snap.covenantId) ?? 0) <= now);
      const rechecking = now - this.lastRecheckMs >= RECHECK_MS;
      const recheck = rechecking ? [...this.book.entries()].filter((e) => e.confirmed).slice(0, RECHECK_BATCH) : [];
      if (rechecking) this.lastRecheckMs = now;
      const all = [...pending, ...recheck];
      for (let i = 0; i < all.length && gen === this.gen; i += VERIFY_CHUNK) {
        const part = all.slice(i, i + VERIFY_CHUNK);
        let res: (OrderSnapshot | SnapshotError)[];
        try {
          res = await confirmSnapshotsOnNode(this.kob, this.node, part.map((e) => e.snap), this.toAddress);
        } catch {
          // the node could not be asked: nothing is counted against the orders, the pending ones are asked again
          for (const e of part) if (!e.confirmed) this.due.set(e.snap.covenantId, this.now() + VERIFY_RETRY_MS[0]!);
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
          // an unconfirmed one is asked a few more times (its block may not be merged yet); a confirmed one the node no longer holds is out at once
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
    const next = [...this.due.values(), ...this.pendingFills.map((f) => f.due)];
    if (next.length) this.scheduleVerify(Math.max(200, Math.min(...next) - this.now()));
  }

  /** Counts the fills whose transaction the node shows (one of its outputs is in the UTXO set); the others are asked a few more times. */
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

/** The `IndexerApi` of standalone mode: the book, its orders, the token list and the market data from what was seen; everything else is unavailable. */
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
      // an order the book does not hold is not known to be gone: an error, never "not found"
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
    depth: async (token, q) => c.depthView(token, q?.levels),
    pairBook: no('the pair book'),
    pairs: no('the pair list'),
    pairCandles: no('the pair chart'),
    pairFills: no('the pair fills'),
  };
}

/**
 * The app's indexer with standalone mode: the indexer (`real`, the failover proxy or the single indexer) while it is in use, the standalone views
 * while the mode is active. `health` goes to the indexer unless the mode is on by the setting (the status poll must see it come back).
 */
export function modeIndexer(real: IndexerApi, c: Pick<StandaloneController, 'state' | 'indexer'>): IndexerApi {
  return new Proxy({} as IndexerApi, {
    get: (_t, prop) => {
      const target = ((prop === 'health' && c.state.reason !== 'manual') || !c.state.active ? real : c.indexer) as unknown as Record<string | symbol, unknown>;
      const v = target[prop];
      return typeof v === 'function' ? (v as (...x: unknown[]) => unknown).bind(target) : v;
    },
    has: (_t, prop) => prop in real,
  });
}
