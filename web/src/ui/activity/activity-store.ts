// The activity log's records (no Preact): what was read from the indexer (or, in standalone mode, from what the page saw on the node), the order facts
// the Wallet column needs, and the reads themselves. use-activity.ts drives it (live notices, polls, the market filter); activity-model.ts turns it
// into rows.
//
// Sources in phase 1 (no indexer change):
//   fills       all markets `/v1/fills`; a token `/v1/fills?token=`; a pair `/v1/pairs/{a}/{b}/fills` (or the pair page's own copy of it)
//   placements  all markets `/v1/orders`; a token `/v1/orders?token=` (its KAS-book orders); a pair `/v1/orders?token=<A>` (its pair orders)
// Paging back: `before=` (fills) and `cursor=` (orders). Each source is complete only down to the oldest record it read while it has more
// (`horizon`): older rows wait for "Load older", so the two sources never interleave with a gap.
import type { IndexerApi } from '../../data/indexer';
import type { EventView, OrderView, PairFillsView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import {
  fillRows, inMarket, mergeRows, MS_PER_DAA, orderMarket, pairFillEvent, placedRows, ROW_CAP,
  type ActivityKind, type ActivityRow, type MarketFilter, type OrderFacts, type RowContext,
} from './activity-model';

/** Records read at first and per "Load older". */
export const FIRST_LIMIT = 50;
/** Placements read per poll (about 10 s of a busy testnet). */
export const POLL_ORDERS_LIMIT = 20;
/** Pages read to close a gap after a pause (a poll whose newest page does not reach what is already known). */
export const GAP_PAGES = 3;
/** Order lookups (the Wallet column) at once: one, so the page's other reads (the order ticket's) never queue behind them. */
const LOOKUP_CONCURRENCY = 1;

export interface StoreSources {
  indexer: IndexerApi | null;
  /** standalone mode: what the page saw on the node (null when the mode is off) */
  standalone: { seenActivity(): { fills: EventView[]; orders: OrderView[] } } | null;
  /** the pair page's own copy of the pair's fills (so the pair's heavy route is not read twice) */
  pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null;
}

/** What a row shows: two rows with the same signature render the same (the list keeps the older object). */
export const rowSignature = (r: ActivityRow): string =>
  [r.kind, r.kinds.join('+'), r.side, r.daa, r.seq, r.orders.join(','), r.placed, r.amount ? `${r.amount.token}:${r.amount.units}` : '', r.value ? `${r.value.kind}:${'sompi' in r.value ? r.value.sompi : `${r.value.token}:${r.value.units}`}` : '',
    r.price ? `${r.price.low}-${r.price.high}/${r.price.scale}` : '', r.pairSide, r.counterparty, r.moreMarkets, r.timeMs, r.timeEstimated, r.confirmations, r.settled].join('|');

export class ActivityStore {
  readonly fills = new Map<string, EventView>();
  readonly orders = new Map<Hex, OrderView>();
  readonly facts = new Map<Hex, OrderFacts | null>();
  /** next_cursor of the oldest page read; undefined before the first read, null when there is nothing older */
  fillsCursor: string | null | undefined = undefined;
  ordersCursor: string | null | undefined = undefined;
  /** one fill's block time and DAA score (the clock of the placements, which have no time of their own) */
  timeRef: { daa: number; ms: number } | null = null;
  /** the chain's DAA score when it was last reported */
  chain: { daa: number; atMs: number } | null = null;
  version = 0;
  private lookups = new Set<Hex>();
  private queue: Hex[] = [];
  private gen = 0;
  private ctl = new AbortController();

  constructor(
    readonly market: MarketFilter,
    private readonly src: StoreSources,
    private readonly tokenScale: (token: Hex) => bigint | null,
    private readonly changed: () => void,
    private readonly now: () => number = Date.now,
  ) {}

  dispose(): void {
    this.gen++;
    this.ctl.abort();
  }

  setPairFills(p: StoreSources['pairFills']): void {
    this.src.pairFills = p;
    if (this.ownPairFills() && p?.view) {
      this.addPairFills(p.view);
      this.bump();
    }
  }

  /** The pair page supplies this pair's fills: they are not read again here. */
  private ownPairFills(): boolean {
    const p = this.src.pairFills;
    const m = this.market;
    return !!p && m.kind === 'pair' && p.base === m.base && p.quote === m.quote;
  }

  private bump(): void {
    this.version++;
    this.changed();
  }

  // ------------------------------------------------------------------------------------------ rows

  ctx(): RowContext {
    // the clock of the placements: a fill's block time, else the chain's DAA score when it was last reported (this page's clock)
    const ref = this.timeRef ?? (this.chain ? { daa: this.chain.daa, ms: this.chain.atMs } : null);
    return {
      tokenScale: this.tokenScale,
      order: (id) => (this.facts.has(id) ? this.facts.get(id)! : undefined),
      timeOfDaa: (daa) => (ref ? ref.ms + (daa - ref.daa) * MS_PER_DAA : null),
    };
  }

  /** Every row of the market filter, newest first, capped; rows below a source's horizon wait for "Load older". */
  rows(kinds: ReadonlySet<ActivityKind>): ActivityRow[] {
    // built again only when the records changed (`version`) or the shown kinds did; a row whose content is unchanged keeps its object, so the list's
    // memoized rows are not rendered again (ActivityRows.tsx)
    const memoKey = `${this.version}|${[...kinds].sort().join(',')}`;
    if (this.rowsMemo?.key === memoKey) return this.rowsMemo.rows;
    const ctx = this.ctx();
    const merged = mergeRows([fillRows(this.fills.values(), ctx), placedRows(this.orders.values(), ctx)], Number.MAX_SAFE_INTEGER).filter((r) => inMarket(r, this.market));
    const h = this.horizon(kinds);
    const fresh = (h === null ? merged : merged.filter((r) => r.daa >= h)).slice(0, ROW_CAP);
    const next = new Map<string, { sig: string; row: ActivityRow }>();
    const rows = fresh.map((r) => {
      const sig = rowSignature(r);
      const had = this.stable.get(r.key);
      const row = had && had.sig === sig ? had.row : r;
      next.set(r.key, { sig, row });
      return row;
    });
    this.stable = next;
    this.rowsMemo = { key: memoKey, rows };
    return rows;
  }
  private rowsMemo: { key: string; rows: ActivityRow[] } | null = null;
  private stable = new Map<string, { sig: string; row: ActivityRow }>();

  /** The DAA score down to which every shown source is complete (null: all of them are). */
  horizon(kinds: ReadonlySet<ActivityKind>): number | null {
    let h: number | null = null;
    const oldest = (xs: Iterable<{ daa: number }>) => {
      let m: number | null = null;
      for (const x of xs) m = m === null || x.daa < m ? x.daa : m;
      return m;
    };
    if (kinds.has('fill') && this.fillsCursor) {
      const o = oldest(this.fills.values());
      if (o !== null) h = Math.max(h ?? o, o);
    }
    if (kinds.has('placed') && this.ordersCursor) {
      const o = oldest([...this.orders.values()].map((x) => ({ daa: x.genesis.daa })));
      if (o !== null) h = Math.max(h ?? o, o);
    }
    return h;
  }

  hasOlder(kinds: ReadonlySet<ActivityKind>): boolean {
    return (kinds.has('fill') && !!this.fillsCursor) || (kinds.has('placed') && !!this.ordersCursor);
  }

  // ------------------------------------------------------------------------------------------ reads

  private signal(): AbortSignal {
    return this.ctl.signal;
  }

  private addFills(items: readonly EventView[]): number {
    let fresh = 0;
    for (const e of items) {
      if (e.kind !== 'fill') continue;
      const k = `e:${e.id}`;
      if (!this.fills.has(k)) fresh++;
      this.fills.set(k, e);
      if (typeof e.ts === 'number' && e.ts > 0 && (!this.timeRef || e.daa > this.timeRef.daa)) this.timeRef = { daa: e.daa, ms: e.ts };
      if (typeof e.confirmations === 'number') this.noteChain(e.daa + e.confirmations);
    }
    this.trim();
    return fresh;
  }

  private addPairFills(v: PairFillsView): number {
    const m = this.market;
    if (m.kind !== 'pair') return 0;
    let fresh = 0;
    for (const f of v.items) {
      const k = `p:${f.id}`;
      if (!this.fills.has(k)) fresh++;
      this.fills.set(k, pairFillEvent(f, v.base.toLowerCase(), v.quote.toLowerCase()));
      if (f.ts > 0 && (!this.timeRef || f.daa > this.timeRef.daa)) this.timeRef = { daa: f.daa, ms: f.ts };
      if (typeof f.confirmations === 'number') this.noteChain(f.daa + f.confirmations);
    }
    this.trim();
    return fresh;
  }

  private addOrders(items: readonly OrderView[]): number {
    let fresh = 0;
    for (const o of items) {
      const m = orderMarket(o);
      if (!m || !inMarket({ market: m }, this.market)) continue;
      const id = o.covenant_id.toLowerCase();
      if (!this.orders.has(id)) fresh++;
      this.orders.set(id, o);
      this.facts.set(id, { maker: o.maker, scale: o.scale !== null && o.scale > 0 ? BigInt(o.scale) : null, genesisDaa: o.genesis.daa });
      if (typeof o.genesis.confirmations === 'number') this.noteChain(o.genesis.daa + o.genesis.confirmations);
    }
    this.trim();
    return fresh;
  }

  /** At most a few times ROW_CAP records per source: the oldest go (and their "Load older" cursor with them). */
  private trim(): void {
    const max = ROW_CAP * 4;
    if (this.fills.size > max) {
      const keep = [...this.fills.entries()].sort((a, b) => b[1].daa - a[1].daa || b[1].id - a[1].id).slice(0, max);
      this.fills.clear();
      for (const [k, v] of keep) this.fills.set(k, v);
      this.fillsCursor = String(keep[keep.length - 1]![1].id);
    }
    if (this.orders.size > max) {
      const keep = [...this.orders.entries()].sort((a, b) => b[1].genesis.daa - a[1].genesis.daa).slice(0, max);
      this.orders.clear();
      for (const [k, v] of keep) this.orders.set(k, v);
      this.ordersCursor = null;
    }
  }

  /** The chain's DAA score as reported (the WebSocket's health frames, or a record's DAA score plus its confirmations). */
  noteChain(daa: number): void {
    if (!Number.isFinite(daa)) return;
    const now = this.now();
    if (!this.chain || daa >= this.chain.daa + Math.floor((now - this.chain.atMs) / MS_PER_DAA)) this.chain = { daa, atMs: now };
  }

  private async fillsPage(before?: string): Promise<{ items: EventView[]; next: string | null; pair?: PairFillsView }> {
    const m = this.market;
    const ix = this.src.indexer;
    if (!ix) return { items: [], next: null };
    const o = { signal: this.signal(), priority: 'low' as const };
    if (m.kind === 'pair') {
      const v = await ix.pairFills(m.base, m.quote, { limit: FIRST_LIMIT, ...(before ? { before } : {}) }, o);
      return v ? { items: [], next: v.next_cursor, pair: v } : { items: [], next: null };
    }
    const p = await ix.fills({ limit: FIRST_LIMIT, ...(m.kind === 'token' ? { token: m.token } : {}), ...(before ? { before } : {}) }, o);
    return { items: p.items, next: p.next_cursor };
  }

  private async ordersPage(limit: number, cursor?: string): Promise<{ items: OrderView[]; next: string | null }> {
    const m = this.market;
    const ix = this.src.indexer;
    if (!ix) return { items: [], next: null };
    const token = m.kind === 'token' ? m.token : m.kind === 'pair' ? m.base : undefined;
    const p = await ix.orders({ limit, ...(token ? { token } : {}), ...(cursor ? { cursor } : {}) }, { signal: this.signal(), priority: 'low' });
    return { items: p.items, next: p.next_cursor };
  }

  /** Standalone mode: everything the page saw (no paging: nothing before the mode started is known). */
  private readStandalone(): void {
    const seen = this.src.standalone!.seenActivity();
    this.addFills(seen.fills);
    this.addOrders(seen.orders);
    this.fillsCursor = null;
    this.ordersCursor = null;
  }

  /**
   * The newest records: fills (unless the pair page supplies them) and, with `placed`, placements; a newest page that does not reach what is already
   * known reads on (GAP_PAGES at most), so a pause leaves no hole. The first read also sets the cursors for "Load older".
   */
  async readNewest(o: { fills: boolean; placed: boolean; ordersLimit?: number; replace?: boolean }): Promise<void> {
    const gen = this.gen;
    // a reorg or a reconnect: the newest page is read again and what it no longer lists is dropped (the rows stay on screen meanwhile)
    if (o.replace) this.replacing = true;
    if (this.src.standalone) {
      this.readStandalone();
      this.bump();
      return;
    }
    const tasks: Promise<void>[] = [];
    if (o.fills && !this.ownPairFills()) tasks.push(this.readNewestFills(gen));
    else if (this.ownPairFills() && this.src.pairFills?.view) {
      this.addPairFills(this.src.pairFills.view);
      if (this.fillsCursor === undefined) this.fillsCursor = this.src.pairFills.view.next_cursor;
    }
    if (o.placed) tasks.push(this.readNewestOrders(gen, o.ordersLimit ?? (this.ordersCursor === undefined ? FIRST_LIMIT : POLL_ORDERS_LIMIT)));
    try {
      await Promise.all(tasks);
    } finally {
      this.replacing = false;
    }
    if (gen === this.gen) this.bump();
  }

  /** set by a replacing read: the next newest page of each source drops the records above its oldest one that it does not list (reverted) */
  private replacing = false;

  /** Drops the records newer than `fromDaa` that the page just read does not list (they were reverted). */
  private dropReverted<T>(map: Map<string, T>, listed: ReadonlySet<string>, fromDaa: number, daaOf: (v: T) => number): void {
    for (const [k, v] of [...map]) if (daaOf(v) >= fromDaa && !listed.has(k)) map.delete(k);
  }

  private async readNewestFills(gen: number): Promise<void> {
    const first = this.fillsCursor === undefined;
    const known = new Set(this.fills.keys());
    let before: string | undefined;
    for (let i = 0; i < (first ? 1 : GAP_PAGES); i++) {
      const page = await this.fillsPage(before);
      if (gen !== this.gen) return;
      const keys = page.pair ? page.pair.items.map((f) => `p:${f.id}`) : page.items.map((e) => `e:${e.id}`);
      if (this.replacing && i === 0) {
        const daas = page.pair ? page.pair.items.map((f) => f.daa) : page.items.map((e) => e.daa);
        if (daas.length) this.dropReverted(this.fills, new Set(keys), Math.min(...daas), (e) => e.daa);
      }
      if (page.pair) this.addPairFills(page.pair);
      else this.addFills(page.items);
      if (first) this.fillsCursor = page.next;
      if (first || !page.next || keys.some((k) => known.has(k)) || known.size === 0) return;
      before = page.next;
    }
  }

  private async readNewestOrders(gen: number, limit: number): Promise<void> {
    const first = this.ordersCursor === undefined;
    const known = new Set(this.orders.keys());
    let cursor: string | undefined;
    for (let i = 0; i < (first ? 1 : GAP_PAGES); i++) {
      const page = await this.ordersPage(limit, cursor);
      if (gen !== this.gen) return;
      if (this.replacing && i === 0 && page.items.length) {
        this.dropReverted(this.orders, new Set(page.items.map((x) => x.covenant_id.toLowerCase())), Math.min(...page.items.map((x) => x.genesis.daa)), (x) => x.genesis.daa);
      }
      this.addOrders(page.items);
      if (first) this.ordersCursor = page.next;
      if (first || !page.next || known.size === 0 || page.items.some((x) => known.has(x.covenant_id.toLowerCase()))) return;
      cursor = page.next;
    }
  }

  /** One more page of each shown source that has more. */
  async readOlder(kinds: ReadonlySet<ActivityKind>): Promise<void> {
    const gen = this.gen;
    if (this.src.standalone) return;
    const tasks: Promise<void>[] = [];
    if (kinds.has('fill') && this.fillsCursor) {
      const before = this.fillsCursor;
      tasks.push(
        this.fillsPage(before).then((p) => {
          if (gen !== this.gen) return;
          if (p.pair) this.addPairFills(p.pair);
          else this.addFills(p.items);
          this.fillsCursor = p.next;
        }),
      );
    }
    if (kinds.has('placed') && this.ordersCursor) {
      const cursor = this.ordersCursor;
      tasks.push(
        this.ordersPage(FIRST_LIMIT, cursor).then((p) => {
          if (gen !== this.gen) return;
          this.addOrders(p.items);
          this.ordersCursor = p.next;
        }),
      );
    }
    await Promise.all(tasks);
    if (gen === this.gen) this.bump();
  }

  // ------------------------------------------------------------------------------------------ order facts (the Wallet column)

  /** Looks up the orders these rows name that are not known yet (a few at a time; each order once). */
  want(ids: readonly Hex[]): void {
    for (const id of ids) {
      if (this.facts.has(id) || this.lookups.has(id)) continue;
      this.lookups.add(id);
      this.queue.push(id);
    }
    this.pump();
  }

  private active = 0;
  private pump(): void {
    const ix = this.src.indexer;
    while (ix && this.active < LOOKUP_CONCURRENCY && this.queue.length) {
      const id = this.queue.shift()!;
      const gen = this.gen;
      this.active++;
      ix.order(id, { signal: this.signal(), priority: 'low' })
        .then((o) => {
          if (gen !== this.gen) return;
          this.facts.set(id, o ? { maker: o.maker, scale: o.scale !== null && o.scale > 0 ? BigInt(o.scale) : null, genesisDaa: o.genesis?.daa ?? null } : null);
          this.bump();
        })
        .catch(() => {
          if (gen === this.gen) this.facts.set(id, null);
        })
        .finally(() => {
          this.active--;
          this.lookups.delete(id);
          if (gen === this.gen) this.pump();
        });
    }
  }
}
