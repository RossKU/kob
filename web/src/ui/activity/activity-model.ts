// The activity log's view model, pure. It turns indexer records into one row per transaction. It
// also holds the filters and their stored form, the hold that keeps rows still while the viewer
// points at the list, and the confirmation clock. Rendering is in ActivityLog.tsx and fetching in
// use-activity.ts.
//
// A row is a FILL or a PLACEMENT.
// - A fill comes from `/v1/fills`, from the WebSocket `fills` channels, or for a pair from
//   `/v1/pairs/{a}/{b}/fills`.
// - A placement comes from `/v1/orders`, newest first by genesis, and is polled.
//
// Cancels, amends, refunds, kills, expiries and stop arms are recorded per order only
// (`/v1/orders/{id}/events`). The log needs a global event list for them, so the Columns menu does
// not offer them (`AVAILABLE_KINDS`). Their icons and filter slots exist so the stored filters
// keep their meaning.
//
// A fill names the order that was filled, and its maker is looked up once per order
// (`/v1/orders/{id}`). The indexer does not record which executor sent the transaction. The log
// therefore shows ROLES (maker-signed orders and executions), never an executor's identity.
import { explorerTxUrl, type AppConfig } from '../../config';
import type { EventView, OrderView, PairFillsView, PairFillView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { pow10 } from '../../kob/units';
import { columnPriceDp, fitAmountColumn, unitsFraction } from '../kit/format';
import { pageWindow, type ListPageSize, type PageWindow } from '../market/market-list';

// ---- kinds

/**
 * What a row is about. `placed` includes amends (maker-signed). `refund` includes kills and expiry
 * returns. `stop` covers arms, triggers and trails.
 */
export type ActivityKind = 'placed' | 'fill' | 'cancel' | 'refund' | 'stop';
export const ALL_KINDS: readonly ActivityKind[] = ['placed', 'fill', 'cancel', 'refund', 'stop'];
/** The kinds the current API lists across orders. The others wait for a global event list. */
export const AVAILABLE_KINDS: readonly ActivityKind[] = ['placed', 'fill'];
/** Signed by the order's owner. */
export const ORDER_KINDS: readonly ActivityKind[] = ['placed', 'cancel'];
/** Sent by an executor or by anyone: fills, refunds, kills, expiries and stop triggers. */
export const EXECUTION_KINDS: readonly ActivityKind[] = ['fill', 'refund', 'stop'];
/** A transaction with several kinds shows the strongest. */
const STRENGTH: Record<ActivityKind, number> = { fill: 5, cancel: 4, refund: 3, stop: 2, placed: 1 };

/** DAA scores after which a row counts as settled (the indexer's `settled`: 100, about 10 s). */
export const SETTLE_DAA = 100;
/** Rows kept in memory; older ones are dropped. */
export const ROW_CAP = 500;
/** The rows of a page on the Activity page until the viewer picks another size. */
export const PAGE_ROWS: ListPageSize = 50;
/** How many rows the log reaches back, whatever the page size. New rows push the oldest off. */
export const ACTIVITY_DEPTH = 100;
/** Milliseconds per DAA score (10 blocks per second on mainnet and testnet-10). */
export const MS_PER_DAA = 100;

// ---- markets

/**
 * A KAS book (`token`) or a token/token pair (`pair`). A pair is in the orientation its order was
 * placed in.
 */
export type MarketRef = { kind: 'token'; token: Hex } | { kind: 'pair'; base: Hex; quote: Hex };

/** The log's market filter: every market, one token's KAS book, or one pair (both orientations). */
export type MarketFilter = { kind: 'all' } | MarketRef;

export const marketKeyOf = (m: MarketFilter): string => (m.kind === 'all' ? 'all' : m.kind === 'token' ? m.token : `${m.base}/${m.quote}`);

/**
 * Whether a row's market is the filter's. A pair matches in both orientations. A pair row never
 * matches a token's KAS book.
 */
export function inMarket(row: Pick<ActivityRow, 'market'>, f: MarketFilter): boolean {
  if (f.kind === 'all') return true;
  const m = row.market;
  if (f.kind === 'token') return m.kind === 'token' && m.token === f.token;
  return m.kind === 'pair' && ((m.base === f.base && m.quote === f.quote) || (m.base === f.quote && m.quote === f.base));
}

// ---- rows

export interface ActivityRow {
  /** The transaction id. There is one row per transaction. */
  key: string;
  txid: Hex;
  /** The strongest kind of the transaction. It sets the icon. */
  kind: ActivityKind;
  /** Every kind the transaction has, shown in the tooltip. */
  kinds: ActivityKind[];
  /** The colour: a fill's TAKER side (`buy` means an ask was taken), else a placement's side. */
  side: 'buy' | 'sell' | null;
  market: MarketRef;
  /** Further markets the transaction touched, not counting the KAS legs of a pair route. */
  moreMarkets: number;
  /** The orders the row is about (covenant ids), resting side first. */
  orders: Hex[];
  /** Placements in this transaction. A ladder places several. */
  placed: number;
  /** The main amount, in base units of `token` (for a pair, of A). */
  amount: { token: Hex; units: bigint } | null;
  /** The second line: the KAS value in sompi, or a pair's B amount. */
  value: { kind: 'kas'; sompi: bigint } | { kind: 'token'; token: Hex; units: bigint } | null;
  /**
   * KAS per whole token as a state price, in sompi per `scale` base units. A ladder shows its
   * range. It is null for a pair, because a pair fill never makes a price.
   */
  price: { low: bigint; high: bigint; scale: bigint } | null;
  /** A pair placement's order side (`ask` sells A). */
  pairSide: 'ask' | 'bid' | null;
  /** A pair fill's route: route, netting or inventory. */
  counterparty: string | null;
  daa: number;
  /** Block time in ms, or null when unknown (a placement without a time reference). */
  timeMs: number | null;
  /**
   * Whether `timeMs` is estimated from the DAA score. A placement is, because the indexer reports
   * its DAA score and not its block time.
   */
  timeEstimated: boolean;
  /** Confirmations when the indexer reported the row. The live count is `confirmationsAt`. */
  confirmations: number | null;
  settled: boolean;
  /** Sort key after `daa`, newest first: an event id or a placement's block sequence. */
  seq: number;
}

/**
 * What the rows need to know about the orders they name. It comes from `/v1/orders/{id}` lookups
 * and the placements already listed.
 */
export interface OrderFacts {
  maker: Hex | null;
  /** The order's `scale`: base units per whole token, the denominator of its prices. */
  scale: bigint | null;
  genesisDaa: number | null;
}

export interface RowContext {
  /** The market's price scale of a token (its standard scale), null when unknown. */
  tokenScale(token: Hex): bigint | null;
  /** What is known about an order, or undefined when it has not been looked up yet. */
  order(covenantId: Hex): OrderFacts | null | undefined;
  /** A time for a DAA score (a placement), or null. */
  timeOfDaa(daa: number): number | null;
}

const big = (v: unknown): bigint | null => (typeof v === 'string' && /^\d+$/.test(v) ? BigInt(v) : typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : null);
const hex = (v: unknown): v is Hex => typeof v === 'string' && /^[0-9a-f]{64}$/i.test(v);
const strongest = (kinds: Iterable<ActivityKind>): ActivityKind => [...kinds].reduce<ActivityKind>((a, b) => (STRENGTH[b] > STRENGTH[a] ? b : a), 'placed');

/** `detail.pair` of a fill event, when it is a pair fill. */
function pairDetail(e: EventView): { base: Hex; quote: Hex; side: 'ask' | 'bid'; amountA: bigint | null; amountB: bigint | null; counterparty: string | null } | null {
  const d = (e.detail as { pair?: Record<string, unknown> } | null | undefined)?.pair;
  if (!d || typeof d !== 'object' || !hex(d.base) || !hex(d.quote)) return null;
  return {
    base: (d.base as string).toLowerCase(), quote: (d.quote as string).toLowerCase(), side: d.side === 'bid' ? 'bid' : 'ask',
    amountA: big(d.amount_a) ?? big(e.amount), amountB: big(d.amount_b), counterparty: typeof d.counterparty === 'string' ? d.counterparty : null,
  };
}

/**
 * Converts a pair fill of `/v1/pairs/{a}/{b}/fills` to the fill event the feed would list, so
 * both build the same rows.
 */
export function pairFillEvent(f: PairFillView, viewBase: Hex, viewQuote: Hex): EventView {
  const base = (f.base ?? viewBase).toLowerCase();
  const quote = (f.quote ?? viewQuote).toLowerCase();
  return {
    id: f.id, covenant_id: f.order, block_seq: 0, daa: f.daa, ts: f.ts, txid: f.txid, tx_pos: 0, kind: 'fill', token: base, side: f.side === 'ask' ? 1 : 2, amount: f.amount_a,
    price: null, payout: null, closes: false, confirmations: f.confirmations, settled: f.settled,
    detail: { pair: { side: f.side, base, quote, amount_a: f.amount_a, amount_b: f.amount_b, counterparty: f.counterparty } },
  };
}

/** Turns fill events into one row per transaction. The caller sorts them, newest first. */
export function fillRows(events: Iterable<EventView>, ctx: RowContext): ActivityRow[] {
  const byTx = new Map<string, EventView[]>();
  for (const e of events) {
    if (e.kind !== 'fill' || !hex(e.txid)) continue;
    const k = e.txid.toLowerCase();
    const list = byTx.get(k);
    if (list) list.push(e);
    else byTx.set(k, [e]);
  }
  const out: ActivityRow[] = [];
  for (const [txid, evs] of byTx) {
    const row = fillRowOf(txid, evs, ctx);
    if (row) out.push(row);
  }
  return out;
}

function fillRowOf(txid: Hex, evs: EventView[], ctx: RowContext): ActivityRow | null {
  evs.sort((a, b) => a.id - b.id);
  const daa = Math.max(...evs.map((e) => e.daa));
  const seq = Math.max(...evs.map((e) => e.id));
  const ts = evs.reduce((m, e) => (typeof e.ts === 'number' && e.ts > m ? e.ts : m), 0);
  const confs = evs.map((e) => e.confirmations).filter((c): c is number => typeof c === 'number');
  const common = {
    key: txid, txid, kind: 'fill' as const, kinds: ['fill' as const], daa, seq, timeMs: ts > 0 ? ts : null, timeEstimated: false,
    confirmations: confs.length ? Math.min(...confs) : null, settled: evs.every((e) => e.settled), placed: 0,
  };
  const pairs = evs.map((e) => ({ e, p: pairDetail(e) })).filter((x): x is { e: EventView; p: NonNullable<ReturnType<typeof pairDetail>> } => x.p !== null);
  if (pairs.length) {
    // A pair fill, with the KAS legs of a route if any. The row is the pair's.
    const first = pairs[0]!.p;
    const same = pairs.filter((x) => x.p.base === first.base && x.p.quote === first.quote && x.p.side === first.side);
    const otherPairs = new Set(pairs.filter((x) => x.p.base !== first.base || x.p.quote !== first.quote).map((x) => `${x.p.base}/${x.p.quote}`));
    const sumA = same.reduce<bigint | null>((s, x) => (s === null || x.p.amountA === null ? null : s + x.p.amountA), 0n);
    const sumB = same.reduce<bigint | null>((s, x) => (s === null || x.p.amountB === null ? null : s + x.p.amountB), 0n);
    const orders = uniq([...same.map((x) => x.e.covenant_id), ...pairs.map((x) => x.e.covenant_id), ...evs.map((e) => e.covenant_id)]);
    return {
      ...common, side: first.side === 'ask' ? 'buy' : 'sell', market: { kind: 'pair', base: first.base, quote: first.quote }, moreMarkets: otherPairs.size, orders,
      amount: sumA === null ? null : { token: first.base, units: sumA }, value: sumB === null ? null : { kind: 'token', token: first.quote, units: sumB },
      price: null, pairSide: null, counterparty: first.counterparty,
    };
  }
  // KAS-book fills. The token with the most fills is the row's market.
  const byToken = new Map<Hex, EventView[]>();
  for (const e of evs) {
    if (!hex(e.token)) continue;
    const t = e.token.toLowerCase();
    byToken.set(t, [...(byToken.get(t) ?? []), e]);
  }
  if (!byToken.size) return null;
  const [token, fills] = [...byToken.entries()].sort((a, b) => b[1].length - a[1].length)[0]!;
  const asks = fills.filter((e) => e.side === 1);
  const bids = fills.filter((e) => e.side === 2);
  // The RESTING side sets the price and the amount, as in /v1/trades. That is one side only, or the
  // side of the older order, with asks winning a tie or an unknown age.
  let resting = asks.length ? asks : bids;
  if (asks.length && bids.length) {
    const oldest = (xs: EventView[]) => Math.min(...xs.map((e) => ctx.order(e.covenant_id)?.genesisDaa ?? Number.POSITIVE_INFINITY));
    if (oldest(bids) < oldest(asks)) resting = bids;
  }
  const tokenScale = ctx.tokenScale(token);
  let units = 0n;
  let sompi: bigint | null = 0n;
  let low: bigint | null = null;
  let high: bigint | null = null;
  let rowScale: bigint | null = tokenScale;
  for (const e of resting) {
    const n = big(e.amount);
    const p = big(e.price);
    const sc = ctx.order(e.covenant_id)?.scale ?? tokenScale;
    if (n !== null) units += n;
    if (n === null || p === null || sc === null || sc <= 0n) {
      sompi = null;
      continue;
    }
    if (sompi !== null) sompi += (n * p) / sc;
    if (rowScale === null) rowScale = sc;
    // Every price on the row's scale.
    const q = rowScale === sc ? p : (p * rowScale) / sc;
    low = low === null || q < low ? q : low;
    high = high === null || q > high ? q : high;
  }
  const orders = uniq([...resting.map((e) => e.covenant_id), ...fills.map((e) => e.covenant_id), ...evs.map((e) => e.covenant_id)]);
  return {
    ...common, side: resting === asks ? 'buy' : 'sell', market: { kind: 'token', token }, moreMarkets: byToken.size - 1, orders,
    amount: { token, units }, value: sompi === null ? null : { kind: 'kas', sompi },
    price: low !== null && high !== null && rowScale !== null ? { low, high, scale: rowScale } : null, pairSide: null, counterparty: null,
  };
}

/**
 * What an order placed. That is `initial_amount`, or else what was filled plus what is left, for
 * an index that has none or 0. Every bid has 0, because its quantity is its escrow.
 * An ended bid has nothing left to read. Only a bid that ended by a fill (less than one minimum
 * fill of buying power left) placed about what it filled. For any other end (closed partly
 * filled, cancelled, refunded) the placed amount is unknown. It is never the filled amount,
 * which would read as 100 % filled.
 */
export function placedAmount(o: OrderView): bigint | null {
  const init = big(o.initial_amount);
  if (init !== null && init > 0n) return init;
  if (o.contract.startsWith('KobBid') && o.status !== 'open' && o.status !== 'partial' && o.status !== 'filled') return null;
  const filled = big(o.filled_amount) ?? 0n;
  const left = big(o.pair ? o.pair.amount_left : o.amount_left) ?? 0n;
  return filled + left > 0n ? filled + left : init;
}

/**
 * The market an order trades in: its pair in the order's orientation, or its token's KAS book.
 * It is null for neither.
 */
export function orderMarket(o: Pick<OrderView, 'pair' | 'token'>): MarketRef | null {
  if (o.pair && hex(o.pair.base) && hex(o.pair.quote)) return { kind: 'pair', base: o.pair.base.toLowerCase(), quote: o.pair.quote.toLowerCase() };
  return hex(o.token) ? { kind: 'token', token: o.token.toLowerCase() } : null;
}

/**
 * Turns placements (`/v1/orders` views) into one row per placement transaction. A ladder places
 * several orders in one.
 */
export function placedRows(orders: Iterable<OrderView>, ctx: RowContext): ActivityRow[] {
  const byTx = new Map<string, OrderView[]>();
  for (const o of orders) {
    const txid = o.genesis?.txid;
    if (!hex(txid)) continue;
    const k = txid.toLowerCase();
    byTx.set(k, [...(byTx.get(k) ?? []), o]);
  }
  const out: ActivityRow[] = [];
  for (const [txid, os] of byTx) {
    os.sort((a, b) => (a.covenant_id < b.covenant_id ? -1 : 1));
    const o = os[0]!;
    const daa = Math.max(...os.map((x) => x.genesis.daa));
    const seq = Math.max(...os.map((x) => x.genesis.block_seq ?? 0));
    const confs = os.map((x) => x.genesis.confirmations).filter((c): c is number => typeof c === 'number');
    const time = ctx.timeOfDaa(daa);
    const pair = o.pair && hex(o.pair.base) && hex(o.pair.quote) ? o.pair : null;
    const market = orderMarket(o);
    if (!market) continue;
    const mk = marketKeyOf(market);
    const same = os.filter((x) => {
      const m = orderMarket(x);
      return m !== null && marketKeyOf(m) === mk;
    });
    const amountToken = market.kind === 'pair' ? market.base : market.token;
    let units: bigint | null = 0n;
    let sompi: bigint | null = market.kind === 'token' ? 0n : null;
    // A pair placement's second line is the quote it asks or offers. In base units of B it is
    // units x price_num / price_den, with the price in B per base unit of A.
    let quoteUnits: bigint | null = market.kind === 'pair' ? 0n : null;
    let low: bigint | null = null;
    let high: bigint | null = null;
    const scale = market.kind === 'token' ? big(o.scale) ?? ctx.tokenScale(market.token) : null;
    for (const x of same) {
      const n = placedAmount(x);
      units = units === null || n === null ? null : units + n;
      if (market.kind === 'pair') {
        const num = big(x.pair?.price_num);
        const den = big(x.pair?.price_den);
        quoteUnits = quoteUnits === null || n === null || num === null || den === null || den <= 0n ? null : quoteUnits + (n * num) / den;
        continue;
      }
      if (market.kind !== 'token') continue;
      const p = big(x.price);
      const sc = big(x.scale) ?? scale;
      if (n === null || p === null || sc === null || sc <= 0n || scale === null) {
        sompi = null;
        continue;
      }
      if (sompi !== null) sompi += (n * p) / sc;
      const q = sc === scale ? p : (p * scale) / sc;
      low = low === null || q < low ? q : low;
      high = high === null || q > high ? q : high;
    }
    const pairSide = pair ? (pair.side === 'bid' ? 'bid' : 'ask') : null;
    out.push({
      key: txid, txid, kind: 'placed', kinds: ['placed'], side: pair ? (pairSide === 'ask' ? 'sell' : 'buy') : o.side === 1 ? 'sell' : 'buy', market,
      moreMarkets: new Set(os.map((x) => { const m = orderMarket(x); return m ? marketKeyOf(m) : ''; })).size - 1, orders: os.map((x) => x.covenant_id.toLowerCase()), placed: os.length,
      amount: units === null ? null : { token: amountToken, units },
      value: sompi !== null ? { kind: 'kas', sompi } : quoteUnits !== null && market.kind === 'pair' ? { kind: 'token', token: market.quote, units: quoteUnits } : null,
      price: low !== null && high !== null && scale !== null ? { low, high, scale } : null, pairSide, counterparty: null,
      daa, timeMs: time, timeEstimated: time !== null, confirmations: confs.length ? Math.min(...confs) : null, settled: os.every((x) => x.genesis.settled), seq,
    });
  }
  return out;
}

const uniq = (xs: readonly (string | null | undefined)[]): Hex[] => {
  const out: Hex[] = [];
  for (const x of xs) if (hex(x) && !out.includes(x.toLowerCase())) out.push(x.toLowerCase());
  return out;
};

/**
 * Merges the rows of every source into one row per transaction, newest first, capped. A
 * transaction in several lists keeps the strongest kind and lists them all.
 */
export function mergeRows(lists: readonly (readonly ActivityRow[])[], cap = ROW_CAP): ActivityRow[] {
  const byKey = new Map<string, ActivityRow>();
  for (const list of lists) {
    for (const r of list) {
      const had = byKey.get(r.key);
      if (!had) {
        byKey.set(r.key, r);
        continue;
      }
      const kinds = [...new Set([...had.kinds, ...r.kinds])];
      const main = STRENGTH[r.kind] > STRENGTH[had.kind] ? r : had;
      byKey.set(r.key, { ...main, kinds, kind: strongest(kinds), orders: uniq([...main.orders, ...had.orders, ...r.orders]), placed: had.placed + r.placed, timeMs: main.timeMs ?? had.timeMs ?? r.timeMs });
    }
  }
  return [...byKey.values()].sort(newestFirst).slice(0, cap);
}

export const newestFirst = (a: Pick<ActivityRow, 'daa' | 'seq' | 'key'>, b: Pick<ActivityRow, 'daa' | 'seq' | 'key'>): number => b.daa - a.daa || b.seq - a.seq || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0);

/**
 * The row's link: its transaction on the network's explorer (`config.explorerTxUrl`, or the
 * configured one).
 */
export const rowHref = (c: Pick<AppConfig, 'network' | 'explorerUrl'>, r: Pick<ActivityRow, 'txid'>): string | null => explorerTxUrl(c, r.txid);

/** The makers a row names, in its order list's order, once looked up. Unknown ones are left out. */
export function rowMakers(row: Pick<ActivityRow, 'orders'>, ctx: Pick<RowContext, 'order'>): Hex[] {
  const out: Hex[] = [];
  for (const id of row.orders) {
    const m = ctx.order(id)?.maker;
    if (m && hex(m) && !out.includes(m.toLowerCase())) out.push(m.toLowerCase());
  }
  return out;
}

// ---- time and confirmations

/**
 * A clock that turns DAA scores into times. It uses one reference point, a fill's block time and
 * DAA score or the node's DAA score when reported, and MS_PER_DAA between them. It is null without
 * a reference.
 */
export function daaClock(ref: { daa: number; ms: number } | null): (daa: number) => number | null {
  return (daa) => (ref ? ref.ms + (daa - ref.daa) * MS_PER_DAA : null);
}

/**
 * The chain's DAA score now, from the last report (`daa` reported at `atMs`). It advances
 * MS_PER_DAA per DAA score, for at most a minute past the report, so a stalled report cannot make
 * every row settle.
 */
export function daaNow(report: { daa: number; atMs: number } | null, nowMs: number): number | null {
  if (!report) return null;
  const ahead = Math.max(0, Math.min(600, Math.floor((nowMs - report.atMs) / MS_PER_DAA)));
  return report.daa + ahead;
}

/**
 * A row's confirmations now. They come from the chain's DAA score when known, else from what the
 * indexer reported.
 */
export function confirmationsAt(row: Pick<ActivityRow, 'daa' | 'confirmations' | 'settled'>, chainDaa: number | null): { n: number | null; settled: boolean } {
  const n = chainDaa !== null ? Math.max(0, chainDaa - row.daa, row.confirmations ?? 0) : row.confirmations;
  return { n, settled: row.settled || (n !== null && n >= SETTLE_DAA) };
}

/**
 * An address for the Wallet column. The network prefix is dropped and the rest is whole. The cell
 * ends it with an ellipsis where the column is too narrow, and its title has the whole address.
 * Ids and wallets are never shortened in the middle. The time column is the kit's `formatRowTime`,
 * one 24-hour format on every table and log: today HH:MM:SS, else YYYY-MM-DD HH:MM.
 */
export function walletBody(address: string): string {
  return address.includes(':') ? address.slice(address.indexOf(':') + 1) : address;
}

// ---- number formats

/**
 * Most fraction digits an amount shows in the log (the Portfolio's table rule). The exact amount
 * is in the cell's title.
 */
export const AMOUNT_FRACTION_MAX = 4;

/**
 * One amount cell of the log.
 * - It shows at most AMOUNT_FRACTION_MAX fraction digits, fewer when the amount needs fewer.
 * - It uses plain digits, never a subscript-zero form, because a column of amounts is read by eye.
 * - It shows `<0.0001` for an amount too small to show, never a zero for something that is there.
 *
 * `exact` is the whole amount for the title, null when the text is already it. A column too
 * narrow for the whole digits falls back to K / M / B (`fitAmountColumn`), so the cell edge never
 * cuts a digit.
 */
export function amountCell(units: bigint, decimals: number, width = 12): { text: string; exact: string | null } {
  const fraction = Math.min(unitsFraction(units, decimals), AMOUNT_FRACTION_MAX);
  const cell = fitAmountColumn([units], decimals, { fraction, width })[0]!;
  if (units > 0n && /^[0.,]+$/.test(cell.text)) return { text: `<0.${'0'.repeat(Math.max(0, fraction - 1))}1`, exact: cell.exact };
  return cell;
}

/**
 * A row's price at the high end of its range, as KAS (sompi) per whole token. The display rule
 * (`displayPriceDp`) reads it. It is null for a pair row and for a row without a price.
 */
export function priceRefOf(row: Pick<ActivityRow, 'price' | 'market'>, decimalsOf: (token: Hex) => number): bigint | null {
  if (!row.price || row.market.kind !== 'token' || row.price.scale <= 0n) return null;
  return (row.price.high * pow10(decimalsOf(row.market.token))) / row.price.scale;
}

/**
 * The fraction digits of the Price column. It is ONE number for the rows on the page: the finest
 * any of their markets needs, with 5 significant digits and at most 6. Every decimal point then
 * lines up whatever markets the log mixes. A log of one market has that market's number.
 */
export const logPriceDp = (rows: readonly Pick<ActivityRow, 'price' | 'market'>[], decimalsOf: (token: Hex) => number): number => columnPriceDp(rows.map((r) => priceRefOf(r, decimalsOf)));

// ---- filters

/** All / Orders (signed by the order's owner) / Executions (sent by an executor or anyone). */
export type ActorFilter = 'all' | 'orders' | 'executions';

export interface ActivityPrefs {
  kinds: Record<ActivityKind, boolean>;
  actor: ActorFilter;
  /** only rows about the connected wallet's orders */
  mine: boolean;
}

export const DEFAULT_PREFS: Readonly<ActivityPrefs> = Object.freeze({
  kinds: Object.freeze({ placed: true, fill: true, cancel: true, refund: false, stop: false }),
  actor: 'all',
  mine: false,
});

/** The kinds a row may have to be shown: the toggles, the quick switch, and what the API offers. */
export function shownKinds(p: ActivityPrefs, available: readonly ActivityKind[] = AVAILABLE_KINDS): Set<ActivityKind> {
  const actor = p.actor === 'orders' ? ORDER_KINDS : p.actor === 'executions' ? EXECUTION_KINDS : ALL_KINDS;
  return new Set(available.filter((k) => p.kinds[k] && actor.includes(k)));
}

/**
 * The rows the filters let through. `mine` is the connected wallet's x-only key. Without a wallet
 * "mine only" has no effect, and its toggle is disabled.
 */
export function filterRows(rows: readonly ActivityRow[], o: { prefs: ActivityPrefs; market: MarketFilter; mine: Hex | null; makersOf: (r: ActivityRow) => Hex[]; available?: readonly ActivityKind[] }): ActivityRow[] {
  const kinds = shownKinds(o.prefs, o.available);
  const me = o.mine?.toLowerCase() ?? null;
  return rows.filter((r) => kinds.has(r.kind) && inMarket(r, o.market) && (!o.prefs.mine || me === null || o.makersOf(r).includes(me)));
}

// ---- stored filters

/**
 * The localStorage key of the filters. One record is for the Activity page and one is shared by
 * the logs on the token and pair pages.
 */
export const PREFS_KEY = 'kob.activity.v1';
export type PrefsScope = 'page' | 'market';

type Store = Pick<Storage, 'getItem' | 'setItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/**
 * Validates a stored record. Unknown fields are dropped, and a missing or malformed one takes its
 * default.
 */
export function parsePrefs(v: unknown): ActivityPrefs {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  const k = (o.kinds && typeof o.kinds === 'object' ? o.kinds : {}) as Record<string, unknown>;
  const kinds = { ...DEFAULT_PREFS.kinds };
  for (const name of ALL_KINDS) if (typeof k[name] === 'boolean') kinds[name] = k[name] as boolean;
  const actor = o.actor === 'orders' || o.actor === 'executions' || o.actor === 'all' ? o.actor : DEFAULT_PREFS.actor;
  return { kinds, actor, mine: typeof o.mine === 'boolean' ? o.mine : DEFAULT_PREFS.mine };
}

/**
 * The viewer's filters of `scope`, or the defaults without storage or with a damaged record. It
 * never throws.
 */
export function readPrefs(scope: PrefsScope, store: Store | null = browserStore()): ActivityPrefs {
  try {
    const raw = store?.getItem(PREFS_KEY);
    if (!raw) return parsePrefs(null);
    return parsePrefs((JSON.parse(raw) as Record<string, unknown>)[scope]);
  } catch {
    return parsePrefs(null);
  }
}

/**
 * Stores the filters of `scope` and keeps the other scope's record. It returns false when the
 * browser keeps nothing, so the choice lasts for the visit.
 */
export function writePrefs(scope: PrefsScope, prefs: ActivityPrefs, store: Store | null = browserStore()): boolean {
  try {
    if (!store) return false;
    let all: Record<string, unknown> = {};
    try {
      const raw = store.getItem(PREFS_KEY);
      const parsed: unknown = raw ? JSON.parse(raw) : null;
      if (parsed && typeof parsed === 'object') all = parsed as Record<string, unknown>;
    } catch {
      /* A damaged record is replaced. */
    }
    all[scope] = parsePrefs(prefs);
    store.setItem(PREFS_KEY, JSON.stringify(all));
    return true;
  } catch {
    return false;
  }
}

// ---- pages

/**
 * One page of the log: the rows within reach (`kept`, at most ACTIVITY_DEPTH) and those on the
 * page (`shown`, at most `size`).
 */
export function logPage<R>(rows: readonly R[], size: number, page: number): PageWindow & { kept: R[]; shown: R[] } {
  const kept = rows.slice(0, ACTIVITY_DEPTH);
  const w = pageWindow(kept.length, size, page);
  return { ...w, kept, shown: kept.slice(w.from, w.to) };
}

/**
 * Whether the log can turn on from `w`. It can when the rows are there, or when older ones may be
 * there and the depth is not reached. `asked` is the page the viewer asked for. A page asked for
 * and not there yet waits for its rows, so a second press is not offered.
 */
export function logHasNext(w: PageWindow, size: number, asked: number, hasOlder: boolean): boolean {
  return w.page + 1 < w.pages || (hasOlder && asked === w.page && (w.page + 1) * size < ACTIVITY_DEPTH);
}

// ---- hold

/**
 * What the list shows while it is HELD, meaning paused or with the pointer or focus in it.
 * - The rows shown when the hold began stay where they are and keep updating (confirmations).
 * - A row that would be inserted among or above them is held back and counted (`+N new`).
 * - Rows older than every shown one, from a read further back, are appended. That moves nothing.
 *
 * Without a hold (`snapshot` null) every row shows.
 */
export function heldView<R extends { key: string }>(rows: readonly R[], snapshot: ReadonlySet<string> | null): { rows: R[]; held: number } {
  if (!snapshot) return { rows: [...rows], held: 0 };
  let last = -1;
  rows.forEach((r, i) => {
    if (snapshot.has(r.key)) last = i;
  });
  const out: R[] = [];
  let held = 0;
  rows.forEach((r, i) => {
    if (snapshot.has(r.key) || i > last) out.push(r);
    else held++;
  });
  return { rows: out, held };
}

/**
 * The fills of the two orientations of a pair as one view of BASE/QUOTE (`ab`).
 * - The fills of QUOTE/BASE orders keep their own pair (`base`, `quote`), so their rows say what
 *   those orders did.
 * - The 24 h volume adds QUOTE/BASE's with its two amounts swapped.
 * - The page goes on below the newer of the two cursors.
 *
 * A fill in both lists, a netting, is one row, because rows are per transaction. The view is
 * newest first.
 */
export function mergePairFills(ab: PairFillsView, ba: PairFillsView | null | undefined): PairFillsView {
  if (!ba || ba.base !== ab.quote || ba.quote !== ab.base) return ab;
  const n = (s: string) => (/^\d+$/.test(s) ? BigInt(s) : 0n);
  // A record is one id. One that BASE/QUOTE already lists is not taken again from the other view.
  const seen = new Set(ab.items.map((f) => f.id));
  const turned = ba.items.filter((f) => !seen.has(f.id)).map((f) => ({ ...f, base: ba.base, quote: ba.quote }));
  const cursor = (a: string | null, b: string | null) => (a === null ? b : b === null ? a : /^\d+$/.test(a) && /^\d+$/.test(b) ? (BigInt(a) > BigInt(b) ? a : b) : a);
  return {
    ...ab,
    volume_24h: {
      amount_a: (n(ab.volume_24h.amount_a) + n(ba.volume_24h.amount_b)).toString(),
      amount_b: (n(ab.volume_24h.amount_b) + n(ba.volume_24h.amount_a)).toString(),
      fills: ab.volume_24h.fills + ba.volume_24h.fills,
    },
    items: [...ab.items, ...turned].sort((x, y) => y.id - x.id),
    next_cursor: cursor(ab.next_cursor, ba.next_cursor),
  };
}

/**
 * A row of a pair market as the page shows the pair. A row of the other orientation (the order's
 * pair is QUOTE/BASE) is turned round, so a buy of the page's base is a buy whatever way its
 * order was placed. The side and the pair side swap, and a fill's two amounts swap. The amount is
 * then in the page's base and the second line in its quote. A placement's amounts swap too, when
 * its price gave its quote amount. Any other row is unchanged.
 */
export function orientRow(row: ActivityRow, f: MarketFilter): ActivityRow {
  if (f.kind !== 'pair' || row.market.kind !== 'pair' || row.market.base !== f.quote || row.market.quote !== f.base) return row;
  const side = row.side === 'buy' ? 'sell' : row.side === 'sell' ? 'buy' : row.side;
  const pairSide = row.pairSide === 'ask' ? 'bid' : row.pairSide === 'bid' ? 'ask' : row.pairSide;
  // A fill's two amounts, or a placement's amount and the quote it asks or offers.
  const swap = !!row.amount && row.value?.kind === 'token';
  return {
    ...row,
    side,
    pairSide,
    market: { kind: 'pair', base: f.base, quote: f.quote },
    ...(swap ? { amount: { token: (row.value as { token: Hex }).token, units: (row.value as { units: bigint }).units }, value: { kind: 'token' as const, token: row.amount!.token, units: row.amount!.units } } : {}),
  };
}
