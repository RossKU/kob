// The activity log's view model (pure): indexer records -> one row per transaction, the filters and their stored form, the hold that keeps rows
// still while the viewer points at the list, and the confirmation clock. Rendering is ActivityLog.tsx, fetching use-activity.ts.
//
// PHASE 1 (no indexer change): a row is a FILL (`/v1/fills`, the WebSocket `fills` channels; a pair's fills from `/v1/pairs/{a}/{b}/fills`) or a
// PLACEMENT (`/v1/orders`, newest first by genesis, polled). Cancels, amends, refunds, kills, expiries and stop arms are recorded per order only
// (`/v1/orders/{id}/events`): they need the global event list of phase 2 (`GET /v1/events`, an `events` WebSocket channel) and are not offered in
// Vis until then (`AVAILABLE_KINDS`). Their icons and filter slots exist already so the stored filters keep their meaning.
//
// Who: a fill names the order that was filled (its maker is looked up once per order, `/v1/orders/{id}`); the executor that sent the transaction is
// not recorded by the indexer, so the log shows ROLES (maker-signed orders vs executions), never an executor's identity.
import { explorerTxUrl, type AppConfig } from '../../config';
import type { EventView, OrderView, PairFillView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';

// ------------------------------------------------------------------------------------------------ kinds

/** What a row is about. `placed` includes amends (maker-signed), `refund` includes kills and expiry returns, `stop` arms / triggers / trails. */
export type ActivityKind = 'placed' | 'fill' | 'cancel' | 'refund' | 'stop';
export const ALL_KINDS: readonly ActivityKind[] = ['placed', 'fill', 'cancel', 'refund', 'stop'];
/** Phase 1: the kinds the current API lists across orders (see the head of this file). The others wait for the phase-2 event list. */
export const AVAILABLE_KINDS: readonly ActivityKind[] = ['placed', 'fill'];
/** Signed by the order's owner. */
export const ORDER_KINDS: readonly ActivityKind[] = ['placed', 'cancel'];
/** Sent by an executor (or anyone): fills, refunds / kills / expiries, stop triggers. */
export const EXECUTION_KINDS: readonly ActivityKind[] = ['fill', 'refund', 'stop'];
/** A transaction with several kinds shows the strongest. */
const STRENGTH: Record<ActivityKind, number> = { fill: 5, cancel: 4, refund: 3, stop: 2, placed: 1 };

/** DAA scores after which a row counts as settled (the indexer's `settled`: 100 DAA, about 10 s). */
export const SETTLE_DAA = 100;
/** Rows kept in memory; older ones are dropped. */
export const ROW_CAP = 500;
/** Rows shown at first and added by each "Load older". */
export const PAGE_ROWS = 50;
/** Milliseconds per DAA score (10 blocks per second on mainnet and testnet-10). */
export const MS_PER_DAA = 100;

// ------------------------------------------------------------------------------------------------ markets

/** A KAS book (`token`) or a token/token pair (`pair`, in the orientation its order was placed in). */
export type MarketRef = { kind: 'token'; token: Hex } | { kind: 'pair'; base: Hex; quote: Hex };

/** The log's market filter: every market, one token's KAS book, or one pair (both orientations). */
export type MarketFilter = { kind: 'all' } | MarketRef;

export const marketKeyOf = (m: MarketFilter): string => (m.kind === 'all' ? 'all' : m.kind === 'token' ? m.token : `${m.base}/${m.quote}`);
export const sameMarket = (a: MarketFilter, b: MarketFilter): boolean => marketKeyOf(a) === marketKeyOf(b);

/** Whether a row's market is the filter's (a pair matches in both orientations; a pair row never matches a token's KAS book). */
export function inMarket(row: Pick<ActivityRow, 'market'>, f: MarketFilter): boolean {
  if (f.kind === 'all') return true;
  const m = row.market;
  if (f.kind === 'token') return m.kind === 'token' && m.token === f.token;
  return m.kind === 'pair' && ((m.base === f.base && m.quote === f.quote) || (m.base === f.quote && m.quote === f.base));
}

// ------------------------------------------------------------------------------------------------ rows

export interface ActivityRow {
  /** the transaction id: one row per transaction */
  key: string;
  txid: Hex;
  /** the strongest kind of the transaction (its icon) */
  kind: ActivityKind;
  /** every kind the transaction has (the tooltip) */
  kinds: ActivityKind[];
  /** the colour: a fill's TAKER side (`buy` = an ask was taken); a placement's order side */
  side: 'buy' | 'sell' | null;
  market: MarketRef;
  /** further markets the transaction touched (not the KAS legs of a pair route) */
  moreMarkets: number;
  /** the orders the row is about (covenant ids), resting side first */
  orders: Hex[];
  /** placements in this transaction (a ladder places several) */
  placed: number;
  /** the main amount: base units of `token` (a pair: of A) */
  amount: { token: Hex; units: bigint } | null;
  /** the second line: the KAS value (sompi), or a pair's B amount */
  value: { kind: 'kas'; sompi: bigint } | { kind: 'token'; token: Hex; units: bigint } | null;
  /** KAS per whole token as a state price (sompi per `scale` base units); a ladder: its range; null for a pair (a pair fill never makes a price) */
  price: { low: bigint; high: bigint; scale: bigint } | null;
  /** a pair placement: the pair order's side (`ask` sells A) */
  pairSide: 'ask' | 'bid' | null;
  /** a pair fill: how it was filled (route / netting / inventory) */
  counterparty: string | null;
  daa: number;
  /** block time, ms; null when unknown (a placement without a time reference) */
  timeMs: number | null;
  /** `timeMs` is estimated from the DAA score (a placement: the indexer reports its DAA score, not its block time) */
  timeEstimated: boolean;
  /** confirmations when the indexer reported the row (the live count is `confirmationsAt`) */
  confirmations: number | null;
  settled: boolean;
  /** newest first: DAA score, then this (an event id or a placement's block sequence) */
  seq: number;
}

/** What the rows need to know about the orders they name (from `/v1/orders/{id}` lookups and the placements already listed). */
export interface OrderFacts {
  maker: Hex | null;
  /** the order's `scale` (base units per whole token: its prices' denominator) */
  scale: bigint | null;
  genesisDaa: number | null;
}

export interface RowContext {
  /** the market's price scale of a token (its standard scale), null when unknown */
  tokenScale(token: Hex): bigint | null;
  /** what is known about an order (undefined: not looked up yet) */
  order(covenantId: Hex): OrderFacts | null | undefined;
  /** a time for a DAA score (a placement), or null */
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

/** A pair fill of `/v1/pairs/{a}/{b}/fills` as the fill event the feed would list (so both build the same rows). */
export function pairFillEvent(f: PairFillView, base: Hex, quote: Hex): EventView {
  return {
    id: f.id, covenant_id: f.order, block_seq: 0, daa: f.daa, ts: f.ts, txid: f.txid, tx_pos: 0, kind: 'fill', token: base, side: f.side === 'ask' ? 1 : 2, amount: f.amount_a,
    price: null, payout: null, closes: false, confirmations: f.confirmations, settled: f.settled,
    detail: { pair: { side: f.side, base, quote, amount_a: f.amount_a, amount_b: f.amount_b, counterparty: f.counterparty } },
  };
}

/** Fill events -> one row per transaction (newest first is up to the caller). */
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
    // a pair fill (with the KAS legs of a route, if any): the row is the pair's
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
  // KAS-book fills: the token with the most fills is the row's market
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
  // the RESTING side sets the price and the amount (as /v1/trades): one side only, or the side of the older order (asks on a tie or when unknown)
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
    // every price on the row's scale
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

/** What an order placed: `initial_amount`, else (an index that has none, or 0) what was filled plus what is left. */
function placedAmount(o: OrderView): bigint | null {
  const init = big(o.initial_amount);
  if (init !== null && init > 0n) return init;
  const filled = big(o.filled_amount) ?? 0n;
  const left = big(o.pair ? o.pair.amount_left : o.amount_left) ?? 0n;
  return filled + left > 0n ? filled + left : init;
}

/** The market an order trades in: its pair (in the order's orientation) or its token's KAS book; null without either. */
export function orderMarket(o: Pick<OrderView, 'pair' | 'token'>): MarketRef | null {
  if (o.pair && hex(o.pair.base) && hex(o.pair.quote)) return { kind: 'pair', base: o.pair.base.toLowerCase(), quote: o.pair.quote.toLowerCase() };
  return hex(o.token) ? { kind: 'token', token: o.token.toLowerCase() } : null;
}

/** Placements (`/v1/orders` views) -> one row per placement transaction (a ladder places several orders in one). */
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
    let low: bigint | null = null;
    let high: bigint | null = null;
    const scale = market.kind === 'token' ? big(o.scale) ?? ctx.tokenScale(market.token) : null;
    for (const x of same) {
      const n = placedAmount(x);
      units = units === null || n === null ? null : units + n;
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
      amount: units === null ? null : { token: amountToken, units }, value: sompi === null ? null : { kind: 'kas', sompi },
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

/** Rows of every source, one per transaction (a transaction in several lists keeps the strongest kind and lists them all), newest first, capped. */
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

/** The row's link: its transaction on the network's explorer (config.explorerTxUrl: kaspa.stream, tn10.kaspa.stream on testnet-10, or the configured one). */
export const rowHref = (c: Pick<AppConfig, 'network' | 'explorerUrl'>, r: Pick<ActivityRow, 'txid'>): string | null => explorerTxUrl(c, r.txid);

/** The makers a row names, in its order list's order, once looked up (unknown ones are left out). */
export function rowMakers(row: Pick<ActivityRow, 'orders'>, ctx: Pick<RowContext, 'order'>): Hex[] {
  const out: Hex[] = [];
  for (const id of row.orders) {
    const m = ctx.order(id)?.maker;
    if (m && hex(m) && !out.includes(m.toLowerCase())) out.push(m.toLowerCase());
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ time and confirmations

/**
 * A clock that turns DAA scores into times: one reference point (a fill's block time and DAA score, or the node's DAA score when it was reported) and
 * MS_PER_DAA between them. Null without a reference.
 */
export function daaClock(ref: { daa: number; ms: number } | null): (daa: number) => number | null {
  return (daa) => (ref ? ref.ms + (daa - ref.daa) * MS_PER_DAA : null);
}

/**
 * The chain's DAA score now, from the last report (`daa` reported at `atMs`): it advances MS_PER_DAA per DAA score, for at most a minute past the
 * report (a stalled report must not make every row settle).
 */
export function daaNow(report: { daa: number; atMs: number } | null, nowMs: number): number | null {
  if (!report) return null;
  const ahead = Math.max(0, Math.min(600, Math.floor((nowMs - report.atMs) / MS_PER_DAA)));
  return report.daa + ahead;
}

/** A row's confirmations now: from the chain's DAA score when known, else what the indexer reported. */
export function confirmationsAt(row: Pick<ActivityRow, 'daa' | 'confirmations' | 'settled'>, chainDaa: number | null): { n: number | null; settled: boolean } {
  const n = chainDaa !== null ? Math.max(0, chainDaa - row.daa, row.confirmations ?? 0) : row.confirmations;
  return { n, settled: row.settled || (n !== null && n >= SETTLE_DAA) };
}

/** `HH:MM:SS` today, `MM-DD HH:MM` on an earlier day (local time). */
export function rowTime(ms: number | null, nowMs: number): string {
  if (ms === null) return '--:--:--';
  const d = new Date(ms);
  const n = new Date(nowMs);
  const p = (x: number) => String(x).padStart(2, '0');
  const today = d.getFullYear() === n.getFullYear() && d.getMonth() === n.getMonth() && d.getDate() === n.getDate();
  return today ? `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}` : `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** A short form of an address for the Wallet column: the network prefix dropped, the first and last four characters of the rest. */
export function shortWallet(address: string): string {
  const body = address.includes(':') ? address.slice(address.indexOf(':') + 1) : address;
  return body.length <= 10 ? body : `${body.slice(0, 4)}…${body.slice(-4)}`;
}

// ------------------------------------------------------------------------------------------------ filters

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

/** The kinds a row may have to be shown: the toggles, the quick switch, and what the API offers (phase 1). */
export function shownKinds(p: ActivityPrefs, available: readonly ActivityKind[] = AVAILABLE_KINDS): Set<ActivityKind> {
  const actor = p.actor === 'orders' ? ORDER_KINDS : p.actor === 'executions' ? EXECUTION_KINDS : ALL_KINDS;
  return new Set(available.filter((k) => p.kinds[k] && actor.includes(k)));
}

/** The rows the filters let through. `mine`: the connected wallet's key (x-only); without a wallet "mine only" has no effect (its toggle is disabled). */
export function filterRows(rows: readonly ActivityRow[], o: { prefs: ActivityPrefs; market: MarketFilter; mine: Hex | null; makersOf: (r: ActivityRow) => Hex[]; available?: readonly ActivityKind[] }): ActivityRow[] {
  const kinds = shownKinds(o.prefs, o.available);
  const me = o.mine?.toLowerCase() ?? null;
  return rows.filter((r) => kinds.has(r.kind) && inMarket(r, o.market) && (!o.prefs.mine || me === null || o.makersOf(r).includes(me)));
}

// ------------------------------------------------------------------------------------------------ stored filters

/** localStorage key of the filters: one record for the Activity page, one shared by the logs on the token and pair pages. */
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

/** Validates a stored record: unknown fields are dropped, a missing or malformed one takes its default. */
export function parsePrefs(v: unknown): ActivityPrefs {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  const k = (o.kinds && typeof o.kinds === 'object' ? o.kinds : {}) as Record<string, unknown>;
  const kinds = { ...DEFAULT_PREFS.kinds };
  for (const name of ALL_KINDS) if (typeof k[name] === 'boolean') kinds[name] = k[name] as boolean;
  const actor = o.actor === 'orders' || o.actor === 'executions' || o.actor === 'all' ? o.actor : DEFAULT_PREFS.actor;
  return { kinds, actor, mine: typeof o.mine === 'boolean' ? o.mine : DEFAULT_PREFS.mine };
}

/** The viewer's filters of `scope` (defaults without storage or with a damaged record). Never throws. */
export function readPrefs(scope: PrefsScope, store: Store | null = browserStore()): ActivityPrefs {
  try {
    const raw = store?.getItem(PREFS_KEY);
    if (!raw) return parsePrefs(null);
    return parsePrefs((JSON.parse(raw) as Record<string, unknown>)[scope]);
  } catch {
    return parsePrefs(null);
  }
}

/** Stores the filters of `scope` (the other scope's record is kept). False when the browser keeps nothing (the choice lasts for the visit). */
export function writePrefs(scope: PrefsScope, prefs: ActivityPrefs, store: Store | null = browserStore()): boolean {
  try {
    if (!store) return false;
    let all: Record<string, unknown> = {};
    try {
      const raw = store.getItem(PREFS_KEY);
      const parsed: unknown = raw ? JSON.parse(raw) : null;
      if (parsed && typeof parsed === 'object') all = parsed as Record<string, unknown>;
    } catch {
      /* a damaged record is replaced */
    }
    all[scope] = parsePrefs(prefs);
    store.setItem(PREFS_KEY, JSON.stringify(all));
    return true;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------------------------------------------ hold

/**
 * What the list shows while it is HELD (paused, or the pointer or the focus is in it): the rows it showed when the hold began stay where they are and
 * keep updating (confirmations); a row that would be inserted among or above them is held back and counted (`+N new`). Rows older than every shown
 * one (Load older) are appended, which moves nothing. Without a hold (`snapshot` null) every row shows.
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
