// The order book of standalone mode (data/standalone.ts). It is pure, with no timers and no node.
//
// It holds three kinds of entries:
// - the resting orders the indexer gave before the mode, asked on the node at once,
// - the orders the browser saw placed or followed in the node's new blocks since the stream
//   started,
// - the wallet's own orders, resolved from its receipts.
// A placement is confirmed on the node before it is listed.
//
// Per token it lists what the indexer's `/v1/books` rules would, as far as the browser can apply
// them (crates/kobd/src/indexer/reads.rs and tokens.rs `ListingRules`). An order is listed only
// when it is:
// - a plain ask or bid (kob/stream-orders.ts `STREAM_KINDS`) that the node confirmed,
// - good till cancelled, because IOC and FOK orders do not rest, or a curve sell order,
// - without an auction, so its price is its limit,
// - active and not expired at the newest block's DAA score, and before its day-order deadline,
// - worth at least MIN_ORDER_VALUE_SOMPI, the executor's default `min_order_value_sompi`. An
//   order the indexer listed before the mode keeps the verdict of the executor it reads.
// - for an ask, held with its custody,
// - for a registry token, under the registry's program, extension commitment and scale.
// Everything else stays out, because a smaller book is the safe side.
//
// The views have the indexer's response shapes (`BookView`, `OrderView`, `IndexerTokenView`). So
// the market page, the order ticket and the book check read them unchanged. The book check
// (app/book-verify.ts) asks the node again before an order is taken.
import type { BookOrderView, BookView, IndexerTokenView, LevelView, OrderView, TokenUtxoView } from './indexer-types';
import type { OrderSnapshot } from '../kob/cancel';
import { describeOrder, extensionOf } from '../kob/order-facts';
import { outpointsOf } from '../kob/stream-orders';
import { extensionOfState } from '../kob/token-state';
import type { AnyState, Hex, TokenState } from '../kob/types';
import { quoteOf } from '../kob/units';
import { curveLevelsLeft } from '../kob/curve';
import type { KobWasm } from '../kob/wasm';

/**
 * The least an order is worth to be listed, in sompi: the executor's default of 1 KAS. An ask is
 * worth its amount at its price, a bid its escrow.
 */
export const MIN_ORDER_VALUE_SOMPI = 100_000_000n;
/**
 * The most orders a streamed book holds. Past it entries are forgotten in this order (`trim`):
 * - the entry just put never, so a new placement or a continuation of a kept entry stays;
 * - the wallet's own orders last;
 * - first those of markets no view looked at lately (`focus`), then those farthest from their
 *   market's best price on their side, then the oldest.
 * Each trim sorts the book once: at most 2,001 entries.
 */
export const MAX_STREAM_ORDERS = 2_000;

/**
 * Where an entry came from.
 * - `stream`: seen placed or followed in a block.
 * - `receipt`: the wallet's own order, resolved on the node from its receipt.
 * - `indexer`: a resting order the indexer gave before the mode (data/stream-carry.ts), asked on
 *   the node at once.
 */
export type StreamOrigin = 'stream' | 'receipt' | 'indexer';

/** One order of the streamed book. */
export interface StreamEntry {
  snap: OrderSnapshot;
  /**
   * Whether the node confirmed `snap`, meaning the order UTXO, and an ask's custody, exist unspent
   * at the scripts its state derives.
   */
  confirmed: boolean;
  origin: StreamOrigin;
  /** Where the order was placed. Null parts are unknown, as for a receipt without them. */
  genesis: { txid: Hex | null; out: number | null; daa: bigint };
  /** amountLeft when first seen. The filled amount shown is measured from there. */
  firstAmount: bigint | null;
  /** When it was first seen, in ms. */
  seenMs: number;
  /** Node checks in a row that did not confirm it. An unconfirmed entry is dropped after a few. */
  misses: number;
}

/** The registry's facts of a token. An order of a registry token is listed only with them. */
export interface RegistryFacts { templateHash: Hex; extensionCommitment: Hex | null; scale: bigint | null }

/** What a listing decision needs besides the entry. */
export interface ListingContext {
  kob: KobWasm;
  /** The newest block's DAA score. */
  daa: bigint;
  /** The wall clock in unix seconds, for day-order deadlines. */
  nowUnix: bigint;
  registry?: (token: Hex) => RegistryFacts | null;
}

const ZERO32 = '0'.repeat(64);
const TRIM_RELIST_DAA = 600n;
const s64 = (v: bigint | null | undefined): number | null => (v === null || v === undefined ? null : Number(v));

/**
 * One listable order with its side, price and amount. An ask's amount is its custody. A bid's is
 * what its escrow buys, an estimate.
 */
export interface Listing {
  entry: StreamEntry;
  token: Hex;
  side: 'sell' | 'buy';
  price: bigint;
  scale: bigint;
  amount: bigint;
  estimated: boolean;
  /** The remaining levels of a curve sell order, each a row of its own, the current first. */
  levels?: { level: number; price: bigint; amount: bigint }[];
}

/** Whether a confirmed entry is part of the book now, and as what. It is pure. */
export function listingOf(e: StreamEntry, ctx: ListingContext): Listing | null {
  if (!e.confirmed) return null;
  const st = e.snap.order.state;
  let d;
  try {
    d = describeOrder(st, ctx.kob);
  } catch {
    return null;
  }
  // A curve has no time in force. It rests until sold out or cancelled.
  if (d.pair || d.trigger || d.entry || (d.tif !== 'gtc' && !d.curve) || d.auction) return null;
  if (d.activeFrom > ctx.daa || (d.expiryDaa > 0n && ctx.daa >= d.expiryDaa)) return null;
  if (e.snap.deadline !== null && e.snap.deadline <= ctx.nowUnix) return null;
  if (d.price <= 0n || d.scale <= 0n) return null;
  const s = st.state as unknown as Record<string, string | undefined>;
  const reg = ctx.registry?.(d.tokenCovId) ?? null;
  if (reg) {
    const ext = (e.snap.custody ? extensionOfState(e.snap.custody.state) : null) ?? extensionOf(st) ?? null;
    if ((s.tokenTplHash ?? '').toLowerCase() !== reg.templateHash.toLowerCase()) return null;
    if (reg.extensionCommitment && ext && ext !== ZERO32 && ext.toLowerCase() !== reg.extensionCommitment.toLowerCase()) return null;
    if (reg.scale !== null && reg.scale !== d.scale) return null;
  }
  const value = BigInt(e.snap.order.amount);
  // An order the indexer listed, kept from before the mode, passed the listing rule of the executor
  // it reads. That rule has its own minimum, not the default one.
  const min = e.origin === 'indexer' ? 1n : MIN_ORDER_VALUE_SOMPI;
  if (d.side === 'sell') {
    if (!e.snap.custody) return null;
    const amount = BigInt(e.snap.custody.state.amount);
    if (amount <= 0n || quoteOf(amount, d.price, d.scale, 'down') < min) return null;
    // An unbound curve, made in its token's genesis, names no token. Its custody's token is it.
    const c = d.curve;
    const token = c?.unbound ? (e.snap.custody.covenantId ?? d.tokenCovId) : d.tokenCovId;
    return { entry: e, token, side: 'sell', price: d.price, scale: d.scale, amount, estimated: false, ...(c ? { levels: curveLevelsLeft(c.levelSize, c.table, c.total, amount) } : {}) };
  }
  if (value < min) return null;
  let amount: bigint;
  try {
    amount = ctx.kob.bidBuyingPower(st, value);
  } catch {
    return null;
  }
  if (amount <= 0n) return null;
  return { entry: e, token: d.tokenCovId, side: 'buy', price: d.price, scale: d.scale, amount, estimated: true };
}

/** Compares the price per base unit exactly, `a.price / a.scale` against `b.price / b.scale`. */
const cmpPrice = (a: Listing, b: Listing): number => {
  const l = a.price * b.scale;
  const r = b.price * a.scale;
  return l < r ? -1 : l > r ? 1 : 0;
};

/** Asks ascending, bids descending. Within a price, the order seen first, then the covenant id. */
function sorted(rows: Listing[], side: 'sell' | 'buy'): Listing[] {
  return rows.sort((a, b) => {
    const p = cmpPrice(a, b);
    if (p !== 0) return side === 'sell' ? p : -p;
    if (a.entry.seenMs !== b.entry.seenMs) return a.entry.seenMs - b.entry.seenMs;
    return a.entry.snap.covenantId < b.entry.snap.covenantId ? -1 : 1;
  });
}

const statusOf = (e: StreamEntry, left: bigint | null): 'open' | 'partial' => (e.firstAmount !== null && left !== null && left < e.firstAmount ? 'partial' : 'open');

function rowOf(l: Listing, ctx: ListingContext): BookOrderView {
  const d = describeOrder(l.entry.snap.order.state, ctx.kob);
  const lv = l.levels?.[0];
  return {
    ...(lv ? { curve_level: lv.level } : {}),
    covenant_id: l.entry.snap.covenantId, contract: l.entry.snap.order.state.kind, maker: d.maker, price: l.price.toString(), tip: d.tip.toString(), min_fill: d.minFill.toString(),
    scale: Number(l.scale), amount_left: l.amount.toString(), amount_estimated: l.estimated, status: statusOf(l.entry, d.amountLeft), cur_value: l.entry.snap.order.amount,
    expiry_daa: d.expiryDaa > 0n ? Number(d.expiryDaa) : null, expired: false, genesis_daa: Number(l.entry.genesis.daa), confirmations: null, settled: false, quote: l.price.toString(),
  };
}

/** Rows of one price and scale as one level, as the indexer's `aggregate=true` gives them. */
function levelsOf(rows: BookOrderView[]): LevelView[] {
  const out: LevelView[] = [];
  for (const r of rows) {
    const last = out[out.length - 1];
    if (last && last.price === r.price && last.scale === r.scale) {
      last.amount = (BigInt(last.amount) + BigInt(r.amount_left ?? '0')).toString();
      last.orders += 1;
      last.amount_estimated ||= r.amount_estimated;
    } else out.push({ price: r.price, amount: r.amount_left ?? '0', amount_estimated: r.amount_estimated, orders: 1, scale: r.scale ?? 0 });
  }
  return out;
}

/**
 * The streamed book of `token` as `/v1/books/{token}` gives it, one row per order or levels with
 * `aggregate`. It has at most `depth` rows or levels per side.
 */
export function bookViewOf(entries: Iterable<StreamEntry>, token: Hex, ctx: ListingContext, q: { depth?: number; aggregate?: boolean } = {}): BookView {
  const t = token.toLowerCase();
  const asks: Listing[] = [];
  const bids: Listing[] = [];
  for (const e of entries) {
    const l = listingOf(e, ctx);
    if (!l || l.token.toLowerCase() !== t) continue;
    // A curve gives one listing per remaining level, each at its price with its size.
    const ls = l.levels ? l.levels.map((lv) => ({ ...l, price: lv.price, amount: lv.amount, levels: [lv] })) : [l];
    (l.side === 'sell' ? asks : bids).push(...ls);
  }
  const depth = Math.max(1, q.depth ?? 50);
  const side = (rows: Listing[], s: 'sell' | 'buy'): (LevelView | BookOrderView)[] => {
    const r = sorted(rows, s).map((l) => rowOf(l, ctx));
    return q.aggregate ? levelsOf(r).slice(0, depth) : r.slice(0, depth);
  };
  return { token: t, aggregated: !!q.aggregate, node_daa: Number(ctx.daa), asks: side(asks, 'sell'), bids: side(bids, 'buy') };
}

function tokenUtxoView(u: NonNullable<OrderSnapshot['custody']>, owner: Hex, role: string): TokenUtxoView {
  const st = u.state as TokenState;
  return {
    txid: u.transactionId, index: u.index, token: (u.covenantId ?? '') as Hex, owner, amount: st.amount, value: u.amount, role, created_daa: Number(u.blockDaaScore ?? 0), spent: false,
    spent_txid: null, state: st, confirmations: null, settled: false,
  };
}

/**
 * The `/v1/orders/{id}` view of an entry, whose current state the node proved. The book check and
 * the ticket read it.
 */
export function orderViewOf(e: StreamEntry, ctx: ListingContext): OrderView {
  const snap = e.snap;
  const st = snap.order.state;
  const d = describeOrder(st, ctx.kob);
  const s = st.state as unknown as Record<string, string | undefined>;
  const tpl = ctx.kob.templates().find((t) => t.name === st.kind);
  const ext = (snap.custody ? extensionOfState(snap.custody.state) : null) ?? extensionOf(st);
  const left = d.side === 'sell' ? (snap.custody ? BigInt(snap.custody.state.amount) : d.amountLeft) : (() => {
    try {
      return ctx.kob.bidBuyingPower(st, snap.order.amount);
    } catch {
      return null;
    }
  })();
  const filled = e.firstAmount !== null && d.amountLeft !== null && d.amountLeft < e.firstAmount ? e.firstAmount - d.amountLeft : 0n;
  const daa = Number(snap.order.blockDaaScore ?? 0);
  // What the book shows of it now, by the listing rule (R4B-I5).
  const listed = listingOf(e, ctx) !== null;
  const expired = (d.expiryDaa > 0n && ctx.daa >= d.expiryDaa) || (snap.deadline !== null && snap.deadline <= ctx.nowUnix);
  return {
    covenant_id: snap.covenantId, contract: st.kind, template_hash: tpl?.hash ?? '', family: st.kind.endsWith('Kron') ? 2 : 1, side: d.side === 'sell' ? 1 : 2, maker: d.maker,
    token: d.tokenCovId, token_template_hash: s.tokenTplHash ?? null, extension_commitment: ext ?? null, scale: Number(d.scale), min_fill: d.minFill.toString(),
    price: d.price.toString(), tip: d.tip.toString(), tif: s.tif !== undefined ? Number(s.tif) : null, expiry_daa: d.expiryDaa > 0n ? Number(d.expiryDaa) : null,
    active_from: Number(d.activeFrom), in_book: listed, budget_rate: null, reserve: s.reserve ?? null, initial_amount: e.firstAmount?.toString() ?? null, listed,
    unlisted_reason: null, origin: e.origin, parent: null,
    genesis: { txid: e.genesis.txid, out: e.genesis.out, block_seq: 0, daa: Number(e.genesis.daa), confirmations: null, settled: false },
    status: statusOf(e, d.amountLeft), filled_amount: filled.toString(), amount_left: left?.toString() ?? null, amount_estimated: d.side === 'buy',
    current: { txid: snap.order.transactionId, index: snap.order.index, value: snap.order.amount }, state_known: true, state: st as AnyState, current_daa: daa,
    deadline: s64(snap.deadline), refund_due_daa: null, quote: d.price.toString(),
    custody: snap.custody ? { expected_amount: snap.custody.state.amount, utxo: tokenUtxoView(snap.custody, snap.covenantId, 'custody'), ok: true } : { expected_amount: null, utxo: null, ok: d.side === 'buy' },
    strays: [], last_block: 0, last_daa: daa, confirmations: null, settled: false, expired,
    // A Collected bid that has bought: its bought-custody, which a taker's fill spends.
    ...(snap.boughtCustody ? { bought_custody: { expected_amount: snap.boughtCustody.state.amount, utxo: tokenUtxoView(snap.boughtCustody, snap.covenantId, 'bought'), ok: true } } : {}),
  };
}

/**
 * The tokens with listed orders, as `/v1/tokens` gives them. They have no ticker, no decimals and
 * no scale of their own, because a registry entry or a verified declaration names those. A token
 * whose orders pin different programs or extension commitments is listed without one. Different
 * programs also set `program_ambiguous`.
 */
export function tokensViewOf(entries: Iterable<StreamEntry>, ctx: ListingContext): IndexerTokenView[] {
  const by = new Map<Hex, { tpls: Set<Hex>; exts: Set<Hex>; asks: number; bids: number; family: 'kcc20' | 'kron' }>();
  for (const e of entries) {
    const l = listingOf(e, ctx);
    if (!l) continue;
    const st = e.snap.order.state;
    const s = st.state as unknown as Record<string, string | undefined>;
    const t = l.token.toLowerCase();
    const cur = by.get(t) ?? { tpls: new Set<Hex>(), exts: new Set<Hex>(), asks: 0, bids: 0, family: st.kind.endsWith('Kron') ? ('kron' as const) : ('kcc20' as const) };
    if (s.tokenTplHash) cur.tpls.add(s.tokenTplHash.toLowerCase());
    const ext = (e.snap.custody ? extensionOfState(e.snap.custody.state) : null) ?? extensionOf(st);
    if (ext && ext !== ZERO32) cur.exts.add(ext.toLowerCase());
    if (l.side === 'sell') cur.asks++;
    else cur.bids++;
    by.set(t, cur);
  }
  return [...by.entries()].map(([id, v]) => {
    const one = v.tpls.size === 1 ? [...v.tpls][0]! : null;
    return {
      ticker: '', covenant_id: id, template_hash: one, extension_commitment: v.exts.size === 1 ? [...v.exts][0]! : null,
      ...(v.tpls.size > 1 ? { program_ambiguous: true, candidate_templates: [...v.tpls] } : {}),
      scale: null, market_scale: null, decimals: null, open_asks: v.asks, open_bids: v.bids, family: v.family,
    };
  });
}

/**
 * The book's entries and an index of the outpoints they hold. It is pure.
 * - `put` adds or replaces an entry. A replacement waits for the node again unless it is
 *   confirmed.
 * - `confirm` and `miss` record the node's answer.
 * - `touched` names the entries a transaction's inputs spend.
 */
export class StreamBook {
  private readonly items = new Map<Hex, StreamEntry>();
  private readonly byOutpoint = new Map<string, Hex>();
  /** Whether a view looked at this token's market lately. Absent: every market counts as looked at. */
  focus: ((token: Hex) => boolean) | null = null;
  /**
   * The listing context the trim judges entries by. Absent: every entry counts as listable, at its
   * state's price.
   */
  listing: (() => ListingContext) | null = null;
  /**
   * The trim's listing of each entry, kept while its snapshot is the same, for up to TRIM_RELIST_DAA
   * (about a minute). Only the order of eviction uses it, and a full book trims on every put.
   */
  private readonly listed = new WeakMap<StreamEntry, { snap: OrderSnapshot; daa: bigint; l: Listing | null }>();
  constructor(private readonly max = MAX_STREAM_ORDERS) {}

  get size(): number {
    return this.items.size;
  }
  entries(): IterableIterator<StreamEntry> {
    return this.items.values();
  }
  get(covenantId: Hex): StreamEntry | undefined {
    return this.items.get(covenantId.toLowerCase());
  }

  private unindex(e: StreamEntry): void {
    for (const k of outpointsOf(e.snap)) if (this.byOutpoint.get(k) === e.snap.covenantId) this.byOutpoint.delete(k);
  }
  private index(e: StreamEntry): void {
    for (const k of outpointsOf(e.snap)) this.byOutpoint.set(k, e.snap.covenantId);
  }

  /**
   * Adds or replaces the entry of `snap.covenantId`. A new state is unconfirmed until the node
   * answers, and the first sighting is kept.
   */
  put(snap: OrderSnapshot, o: { origin: StreamOrigin; genesis?: StreamEntry['genesis']; nowMs: number; confirmed?: boolean }): StreamEntry {
    const id = snap.covenantId.toLowerCase() as Hex;
    const old = this.items.get(id);
    if (old) this.unindex(old);
    const left = describeLeft(snap);
    const e: StreamEntry = {
      snap: { ...snap, covenantId: id },
      confirmed: o.confirmed ?? false,
      // An own order learned from its receipt stays one when the stream follows it.
      origin: old?.origin === 'receipt' ? 'receipt' : o.origin,
      genesis: old?.genesis ?? o.genesis ?? { txid: null, out: null, daa: BigInt(snap.order.blockDaaScore ?? 0) },
      firstAmount: old ? old.firstAmount : left,
      seenMs: old?.seenMs ?? o.nowMs,
      misses: 0,
    };
    this.items.set(id, e);
    this.index(e);
    this.trim(id);
    return e;
  }

  /**
   * Records that the node confirmed the entry's snapshot. `snap` is as confirmed, so its amounts
   * are the node's. False when the entry changed meanwhile.
   */
  confirm(covenantId: Hex, snap: OrderSnapshot): boolean {
    const e = this.items.get(covenantId.toLowerCase());
    if (!e || e.snap.order.transactionId !== snap.order.transactionId || e.snap.order.index !== snap.order.index) return false;
    this.unindex(e);
    e.snap = { ...snap, covenantId: e.snap.covenantId };
    e.confirmed = true;
    e.misses = 0;
    this.index(e);
    return true;
  }

  /**
   * Records that the node did not confirm the entry. It is removed after `limit` misses in a row,
   * and then this returns true.
   */
  miss(covenantId: Hex, limit: number): boolean {
    const e = this.items.get(covenantId.toLowerCase());
    if (!e) return false;
    e.confirmed = false;
    e.misses++;
    if (e.misses < limit) return false;
    this.remove(covenantId);
    return true;
  }

  remove(covenantId: Hex): void {
    const id = covenantId.toLowerCase();
    const e = this.items.get(id);
    if (!e) return;
    this.unindex(e);
    this.items.delete(id);
  }

  /** The entry whose order UTXO is `outpoint` (`txid:index`). */
  orderAt(outpoint: string): StreamEntry | undefined {
    const id = this.byOutpoint.get(outpoint);
    const e = id ? this.items.get(id) : undefined;
    return e && `${e.snap.order.transactionId}:${e.snap.order.index}` === outpoint ? e : undefined;
  }

  /** The entries whose order or custody UTXO one of `outpoints` is. */
  touched(outpoints: readonly string[]): StreamEntry[] {
    const ids = new Set<Hex>();
    for (const k of outpoints) {
      const id = this.byOutpoint.get(k);
      if (id) ids.add(id);
    }
    return [...ids].map((id) => this.items.get(id)!).filter(Boolean);
  }

  clear(): void {
    this.items.clear();
    this.byOutpoint.clear();
  }

  private trim(keep: Hex): void {
    if (this.items.size <= this.max) return;
    // Each entry's price per base unit, and the best of its market's side among the entries the
    // book would list. An entry it would never list (expired, dust, another template...) goes
    // first and sets no best, so junk at an absurd price cannot push a market's honest orders out
    // (R4B-L1). An entry still waiting for the node is judged by its terms.
    const ctx = this.listing?.() ?? null;
    const facts = new Map<StreamEntry, { token: Hex; sell: boolean; p: number; listable: boolean }>();
    const best = new Map<string, number>();
    for (const e of this.items.values()) {
      const s = e.snap.order.state.state as unknown as Record<string, string | undefined>;
      let l: Listing | null = null;
      if (ctx) {
        const c = this.listed.get(e);
        if (c && c.snap === e.snap && ctx.daa - c.daa < TRIM_RELIST_DAA && ctx.daa >= c.daa) l = c.l;
        else this.listed.set(e, { snap: e.snap, daa: ctx.daa, l: (l = listingOf({ ...e, confirmed: true }, ctx)) });
      }
      const token = (l ? l.token : String(s.tokenCovId ?? '')).toLowerCase() as Hex;
      const sell = e.snap.order.state.kind === 'KobAsk' || e.snap.order.state.kind === 'KobAskKron';
      const scale = Number(s.scale ?? 0);
      const p = scale > 0 ? Number(s.price ?? 0) / scale : 0;
      const listable = !ctx || l !== null;
      facts.set(e, { token, sell, p, listable });
      if (!(p > 0) || !listable) continue;
      const k = `${token}:${sell ? 1 : 0}`;
      const b = best.get(k);
      if (b === undefined || (sell ? p < b : p > b)) best.set(k, p);
    }
    // How far from the touch: 1 at the best price, more further out, Infinity without a price.
    const away = (e: StreamEntry): number => {
      const f = facts.get(e)!;
      const b = best.get(`${f.token}:${f.sell ? 1 : 0}`);
      return f.p > 0 && b !== undefined ? (f.sell ? f.p / b : b / f.p) : Infinity;
    };
    const focus = this.focus;
    const listed = (e: StreamEntry): number => (facts.get(e)!.listable ? 1 : 0);
    const seen = (e: StreamEntry): number => (focus === null || focus(facts.get(e)!.token) ? 1 : 0);
    const order = [...this.items.values()]
      .filter((e) => e.snap.covenantId !== keep)
      .sort((a, b) => (a.origin === 'receipt' ? 1 : 0) - (b.origin === 'receipt' ? 1 : 0) || listed(a) - listed(b) || seen(a) - seen(b) || away(b) - away(a) || a.seenMs - b.seenMs);
    for (const e of order.slice(0, this.items.size - this.max)) this.remove(e.snap.covenantId);
  }
}

const describeLeft = (snap: OrderSnapshot): bigint | null => {
  const s = snap.order.state.state as unknown as Record<string, string | undefined>;
  return s.amountLeft !== undefined ? BigInt(s.amountLeft) : null;
};
