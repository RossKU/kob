// What the page already read from the indexer in this session, kept for standalone mode (data/standalone.ts). When the page goes standalone (the
// switch in Settings, or the automatic fallback when the indexer cannot be used) it does not drop that: the candles, trades and 24 h figures of a
// market already loaded, the token facts of the list (an open-list token's decimals come from the indexer's market scale) and the activity rows
// stay, and what the node shows afterwards is merged into them. Nothing is asked of the indexer here: this only remembers answers it already gave
// (`modeIndexer` records them while the indexer is in use), so the trust is the one those answers had before the switch.
//
// The resting orders of a market are kept too (its `/v1/orders?token=&status=active` answer, read beside its book, and every `/v1/orders/{id}` answer):
// standalone mode starts its book from them instead of from nothing. That is never trust in the indexer's word: the mode lists such an order only while
// the node holds it (each one is checked on the node at once and again with every recheck, a spend in a block takes it out), and an order is taken only
// after the book check confirms it on the node again (app/book-verify.ts). The indexer's data wins again once it is back (the node's collected data is
// dropped, the carry goes on recording what the indexer says).
import type { CandleInterval, CandleView, CandlesView, IndexerTokenView, OrderView, StatsView, TradeView, TradesView } from './indexer-types';
import type { Hex } from '../kob/types';
import type { RegistryFacts } from './stream-book';

/** Most bars kept per token and interval (the chart's own limit). */
const MAX_BARS = 1500;
/** Most markets whose figures are kept (the least recently read go first). */
const MAX_MARKETS = 200;
/** Most resting orders kept per market. */
const MAX_ORDERS = 500;
/** The resting orders of a market are read again at most this often (a read of its book asks for them). */
export const ORDERS_READ_MS = 60_000;
/** Most parked stores of the activity log. */
const MAX_PARKED = 24;
const DAY_MS = 86_400_000;

interface MarketCarry {
  /** sompi per this many base units (every carried figure of the market is priced on it) */
  basis: bigint;
  /** when the last of these answers was read, ms */
  at: number;
  trades: { at: number; items: TradeView[] } | null;
  candles: Map<CandleInterval, Map<number, CandleView>>;
  stats: StatsView | null;
}

const hasCustody = (v: OrderView): boolean => !!v.custody?.utxo;
const sameOutpoint = (a: OrderView, b: OrderView): boolean => !!a.current && !!b.current && a.current.txid === b.current.txid && a.current.index === b.current.index;

/** An order of `token`'s KAS book that rests (open or partly filled, listed). */
const isResting = (v: OrderView, token: Hex): boolean =>
  typeof v.covenant_id === 'string' && typeof v.token === 'string' && v.token.toLowerCase() === token && (v.status === 'open' || v.status === 'partial') && v.listed !== false && v.in_book !== false && !v.expired;

const posBig = (s: string): bigint | null => (/^[1-9]\d*$/.test(s) ? BigInt(s) : null);
const isPow10 = (n: number): boolean => Number.isInteger(n) && n >= 1 && n <= 1_000_000_000 && /^10*$/.test(String(n));

export class IndexerCarry {
  private tokenRows = new Map<Hex, IndexerTokenView>();
  private tokensRead: number | null = null;
  private readonly markets = new Map<Hex, MarketCarry>();
  private readonly parkedStores = new Map<string, unknown>();
  private readonly orderViews = new Map<Hex, { at: number; views: Map<Hex, OrderView> }>();
  private readonly ordersAsked = new Map<Hex, number>();

  constructor(private readonly now: () => number = Date.now) {}

  // ------------------------------------------------------------------------------------------ recording (the indexer in use)

  /** The token list the indexer gave (all of it: a later read replaces it). */
  recordTokens(rows: readonly IndexerTokenView[]): void {
    this.tokenRows = new Map(rows.map((r) => [r.covenant_id.toLowerCase(), r]));
    this.tokensRead = this.now();
  }

  private market(token: Hex, basis: string): MarketCarry | null {
    const b = posBig(basis);
    if (b === null) return null;
    const id = token.toLowerCase();
    let m = this.markets.get(id);
    // another price basis: the carried figures are priced differently, so none of them mixes with the new ones
    if (m && m.basis !== b) m = undefined;
    if (!m) {
      m = { basis: b, at: 0, trades: null, candles: new Map(), stats: null };
      this.markets.delete(id);
      this.markets.set(id, m);
      while (this.markets.size > MAX_MARKETS) this.markets.delete(this.markets.keys().next().value!);
    }
    m.at = this.now();
    return m;
  }

  /** The newest page of trades (a page read further back is not kept). */
  recordTrades(token: Hex, q: { before?: unknown } | undefined, v: TradesView | null): void {
    if (!v || (q && q.before !== undefined && q.before !== null)) return;
    const m = this.market(token, v.price_basis);
    if (m) m.trades = { at: m.at, items: v.items };
  }

  /** Bars: a full load replaces the series, a load from a bucket replaces the bars from there on (the chart's own incremental rule). */
  recordCandles(token: Hex, q: { interval: CandleInterval; from?: number } | undefined, v: CandlesView | null): void {
    if (!v) return;
    const m = this.market(token, v.price_basis);
    if (!m) return;
    const interval = v.interval;
    const from = q?.from;
    let bars = m.candles.get(interval);
    if (!bars || from === undefined) bars = new Map();
    else for (const t of [...bars.keys()]) if (t >= from) bars.delete(t);
    for (const c of v.items) bars.set(c.t, c);
    if (bars.size > MAX_BARS) for (const t of [...bars.keys()].sort((a, b) => a - b).slice(0, bars.size - MAX_BARS)) bars.delete(t);
    m.candles.set(interval, bars);
  }

  recordStats(token: Hex, v: StatsView | null): void {
    if (!v) return;
    const m = this.market(token, v.price_basis);
    if (m) m.stats = v;
  }

  /**
   * The resting orders of a market the indexer gave (`/v1/orders?token=&status=active`): they replace what was kept of it. The list does not carry an
   * ask's custody (only `/v1/orders/{id}` does): a kept view of the same order at the same outpoint that has it stays (`needsCustody` names the others).
   */
  recordOrders(token: Hex, views: readonly OrderView[]): void {
    const id = token.toLowerCase() as Hex;
    const old = this.orderViews.get(id)?.views;
    const m = new Map<Hex, OrderView>();
    for (const v of views) {
      if (!isResting(v, id) || m.size >= MAX_ORDERS) continue;
      const key = v.covenant_id.toLowerCase() as Hex;
      const kept = old?.get(key);
      m.set(key, kept && hasCustody(kept) && !hasCustody(v) && sameOutpoint(kept, v) ? kept : v);
    }
    this.orderViews.delete(id);
    this.orderViews.set(id, { at: this.now(), views: m });
    while (this.orderViews.size > MAX_MARKETS) this.orderViews.delete(this.orderViews.keys().next().value!);
  }

  /** One order the indexer gave (`/v1/orders/{id}`): its newest view is kept while it rests, an order that no longer rests is forgotten. */
  recordOrder(v: OrderView | null): void {
    if (!v || typeof v.token !== 'string') return;
    const token = v.token.toLowerCase() as Hex;
    const id = v.covenant_id.toLowerCase() as Hex;
    const m = this.orderViews.get(token);
    if (!isResting(v, token)) return void m?.views.delete(id);
    if (m) {
      if (m.views.has(id) || m.views.size < MAX_ORDERS) m.views.set(id, v);
    } else this.orderViews.set(token, { at: this.now(), views: new Map([[id, v]]) });
  }

  /** The best (lowest priced) kept asks of `token` without their custody, at most `n`: their full views are read one by one. */
  needsCustody(token: Hex, n: number): Hex[] {
    const m = this.orderViews.get(token.toLowerCase() as Hex);
    if (!m) return [];
    const price = (v: OrderView): bigint => (/^\d+$/.test(v.price ?? '') ? BigInt(v.price!) : 0n);
    return [...m.views.values()]
      .filter((v) => v.side === 1 && !hasCustody(v))
      .sort((a, b) => (price(a) < price(b) ? -1 : price(a) > price(b) ? 1 : 0))
      .slice(0, n)
      .map((v) => v.covenant_id);
  }

  /** Whether the resting orders of `token` are to be read now (none were for ORDERS_READ_MS); true marks them asked. */
  ordersDue(token: Hex): boolean {
    const id = token.toLowerCase() as Hex;
    const at = this.ordersAsked.get(id);
    if (at !== undefined && this.now() - at < ORDERS_READ_MS) return false;
    this.ordersAsked.set(id, this.now());
    if (this.ordersAsked.size > MAX_MARKETS) this.ordersAsked.delete(this.ordersAsked.keys().next().value!);
    return true;
  }
  /** The read of `ordersDue` failed: it may be made again at the next read of the book. */
  ordersFailed(token: Hex): void {
    this.ordersAsked.delete(token.toLowerCase() as Hex);
  }

  // ------------------------------------------------------------------------------------------ reading (standalone mode)

  /** The resting orders kept, of every market (the indexer's views, as it gave them). */
  orders(): OrderView[] {
    return [...this.orderViews.values()].flatMap((m) => [...m.views.values()]);
  }
  /** When the resting orders of `id` were read from the indexer, ms; null: none were. */
  ordersAt(id: Hex): number | null {
    return this.orderViews.get(id.toLowerCase() as Hex)?.at ?? null;
  }

  /** The tokens the indexer listed (as it gave them). */
  tokens(): IndexerTokenView[] {
    return [...this.tokenRows.values()];
  }
  /** The indexer's row of a token, or null: it was never listed this session. */
  token(id: Hex): IndexerTokenView | null {
    return this.tokenRows.get(id.toLowerCase()) ?? null;
  }
  /** When the token list was read (ms), if it names `id` (else null): the facts of that token are as old as that. */
  tokenAt(id: Hex): number | null {
    return this.tokenRows.has(id.toLowerCase()) ? this.tokensRead : null;
  }
  /** When the token list was last read, whatever it names. */
  get tokensAt(): number | null {
    return this.tokensRead;
  }

  /** When market figures of `id` (trades, candles, 24 h figures) were last read from the indexer, ms; null: none were. */
  marketAt(id: Hex): number | null {
    const m = this.markets.get(id.toLowerCase());
    return m && (m.trades || m.stats || m.candles.size) ? m.at : null;
  }

  /** The price basis the indexer gave this token's figures on (they are merged on it), else its market scale or standard scale. */
  basisOf(id: Hex): bigint | null {
    const m = this.markets.get(id.toLowerCase());
    if (m) return m.basis;
    const v = this.tokenRows.get(id.toLowerCase());
    const s = v ? v.scale ?? v.market_scale : null;
    return typeof s === 'number' && isPow10(s) ? BigInt(s) : null;
  }

  /** The scale the indexer's row gives the market (the standard scale, else the market scale): the one the token was traded on before the mode. */
  scaleOf(id: Hex): number | null {
    const v = this.tokenRows.get(id.toLowerCase());
    const s = v ? v.scale ?? v.market_scale : null;
    return typeof s === 'number' && isPow10(s) ? s : null;
  }

  /**
   * The facts the indexer's row of a token gives a streamed order to be listed against: its program, extension commitment and scale (the same facts
   * the registry gives a registry token). Null without a listed program or when the row names more than one (an ambiguous token is not listed).
   */
  factsOf(id: Hex): RegistryFacts | null {
    const v = this.tokenRows.get(id.toLowerCase());
    if (!v || !v.template_hash || v.program_ambiguous) return null;
    const s = v.scale ?? v.market_scale;
    return { templateHash: v.template_hash, extensionCommitment: v.extension_commitment, scale: typeof s === 'number' && isPow10(s) ? BigInt(s) : null };
  }

  trades(id: Hex): TradeView[] {
    return this.markets.get(id.toLowerCase())?.trades?.items ?? [];
  }
  candles(id: Hex, interval: CandleInterval): CandleView[] {
    const bars = this.markets.get(id.toLowerCase())?.candles.get(interval);
    return bars ? [...bars.values()].sort((a, b) => a.t - b.t) : [];
  }
  stats(id: Hex): StatsView | null {
    return this.markets.get(id.toLowerCase())?.stats ?? null;
  }

  // ------------------------------------------------------------------------------------------ parked views (the activity log)

  /** Keeps a view's store under a key (the newest `MAX_PARKED` are kept): standalone mode seeds its own from it. */
  park(key: string, store: unknown): void {
    this.parkedStores.delete(key);
    this.parkedStores.set(key, store);
    while (this.parkedStores.size > MAX_PARKED) this.parkedStores.delete(this.parkedStores.keys().next().value!);
  }
  parked<T>(key: string): T | null {
    return (this.parkedStores.get(key) as T | undefined) ?? null;
  }
}

// ------------------------------------------------------------------------------------------ merging with what the node saw (pure)

const big = (s: string | null | undefined): bigint | null => (s !== null && s !== undefined && /^\d+$/.test(s) ? BigInt(s) : null);
const maxOf = (a: string | null, b: string | null): string | null => (a === null ? b : b === null ? a : (big(a) ?? 0n) >= (big(b) ?? 0n) ? a : b);
const minOf = (a: string | null, b: string | null): string | null => (a === null ? b : b === null ? a : (big(a) ?? 0n) <= (big(b) ?? 0n) ? a : b);
const sum = (a: string | null, b: string | null): string => ((big(a) ?? 0n) + (big(b) ?? 0n)).toString();

/**
 * The carried bars and the bars the node's fills made (`seen`, ascending): a bucket in both opens where the carried one did, closes where the node's did
 * and adds the volumes (the node saw only fills after the carried read). The window rules of `/v1/candles`: `from` / `to` bound the bucket start, the
 * first `limit` bars from `from`, else the newest `limit`.
 */
export function mergeCandles(carried: readonly CandleView[], seen: CandlesView, q: { from?: number; to?: number; limit?: number }): CandlesView {
  const limit = Math.max(1, Math.min(q.limit ?? 500, 1500));
  const by = new Map<number, CandleView>(carried.map((c) => [c.t, c]));
  for (const c of seen.items) {
    const o = by.get(c.t);
    by.set(c.t, o ? { t: c.t, o: o.o, h: maxOf(o.h, c.h)!, l: minOf(o.l, c.l)!, c: c.c, volume: sum(o.volume, c.volume), quote_volume: sum(o.quote_volume, c.quote_volume), trades: o.trades + c.trades } : c);
  }
  const all = [...by.values()].filter((c) => (q.from === undefined || c.t >= q.from) && (q.to === undefined || c.t < q.to)).sort((a, b) => a.t - b.t);
  return { ...seen, items: q.from !== undefined ? all.slice(0, limit) : all.slice(-limit) };
}

/** Trades, newest first: what the node saw, then the carried ones (a transaction in both is the node's). `before` pages only the node's own rows. */
export function mergeTrades(carried: readonly TradeView[], seen: TradesView, limit: number): TradesView {
  const have = new Set(seen.items.map((t) => t.txid));
  const older = carried.filter((t) => !have.has(t.txid));
  const items = [...seen.items, ...older].slice(0, limit);
  return { ...seen, items, next_cursor: seen.next_cursor };
}

/**
 * The 24 h figures: the carried ones (as the indexer gave them, `ts`) with what the node saw added (`seen`: the node's own figures; its book part is
 * the partial book). The last price is the node's newest fill, else the carried one; the window is the carried window plus what the node saw since,
 * so it is at most 24 h old at the carried read and grows by what has passed since: that is what "indexer data until HH:MM" says. Past 24 h from the
 * carried read none of its window figures is kept (only the last price).
 */
export function mergeStats(carried: StatsView, seen: StatsView, nowMs: number): StatsView {
  const live = nowMs - carried.ts < DAY_MS;
  const lastSeen = seen.last !== null;
  const last = lastSeen ? seen.last : carried.last;
  const open = live ? carried.open_24h ?? seen.open_24h : seen.open_24h;
  const b = (v: string | null): bigint | null => big(v);
  const ob = b(open);
  const lb = b(last);
  const change = ob !== null && ob > 0n && lb !== null ? Number(((lb - ob) * 10_000n) / ob) : null;
  return {
    ...seen,
    last, last_ts: lastSeen ? seen.last_ts : carried.last_ts, last_side: lastSeen ? seen.last_side : carried.last_side,
    open_24h: open,
    high_24h: live ? maxOf(carried.high_24h, seen.high_24h) : seen.high_24h,
    low_24h: live ? minOf(carried.low_24h, seen.low_24h) : seen.low_24h,
    change_24h_bps: change,
    volume_24h: live ? sum(carried.volume_24h, seen.volume_24h) : seen.volume_24h,
    quote_volume_24h: live ? sum(carried.quote_volume_24h, seen.quote_volume_24h) : seen.quote_volume_24h,
    trades_24h: live ? (carried.trades_24h ?? 0) + (seen.trades_24h ?? 0) : seen.trades_24h,
  };
}

/**
 * The token list: the node's rows (tokens with orders it saw) and the carried ones. A carried row keeps the indexer's facts (ticker, decimals, scales,
 * powers, warnings, declaration); the order counts are the node's partial book's (0 for a token it saw no order of), a fact the node proved is added to
 * a carried row that lacked it.
 */
export function mergeTokens(carried: readonly IndexerTokenView[], seen: readonly IndexerTokenView[]): IndexerTokenView[] {
  const by = new Map<Hex, IndexerTokenView>();
  for (const s of seen) by.set(s.covenant_id.toLowerCase(), s);
  const out: IndexerTokenView[] = [];
  for (const c of carried) {
    const id = c.covenant_id.toLowerCase();
    const s = by.get(id);
    by.delete(id);
    out.push(
      s
        ? { ...c, template_hash: c.template_hash ?? s.template_hash, extension_commitment: c.extension_commitment ?? s.extension_commitment, open_asks: s.open_asks, open_bids: s.open_bids }
        : { ...c, open_asks: 0, open_bids: 0 },
    );
  }
  return [...out, ...by.values()];
}
