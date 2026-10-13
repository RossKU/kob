// View model of the market list (`#/market`): one row per tradable token's KAS market (TOKEN/KAS) and one per token pair with live pair orders
// (BASE/QUOTE), with the 24 h figures the indexer already serves:
//
//   * a token's KAS market: `GET /v1/stats/<token>` (one call per token): last price, 24 h change, 24 h KAS volume and trade count, best bid / ask
//     and spread; the open order counts of `GET /v1/tokens`.
//   * a token pair (`GET /v1/pairs`, both tokens tradable and in the registry, as the pair page needs): last price and change implied through the two
//     KAS markets (the same `usdStats` the pair page uses: prices come only from KAS-book fills), the 24 h pair volume of
//     `GET /v1/pairs/<base>/<quote>/fills?limit=1` (`volume_24h`, in QUOTE), the live pair order counts of the summary. No bid / ask here (the pair
//     book is on the pair page).
//
// Each figure is a cell (text + tooltip): prices with about five significant digits (the market's tick when the registry has one), compact K / M / B
// when a cell would be too wide, never a cut digit (the exact figure is in the tooltip). A row whose figures are still loading or unavailable shows
// skeletons / `—`. Sorting by any column, missing figures always last; search over pairs by either token (or `BASE/QUOTE`). Pure.
import type { PairSummaryView, StatsView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { cmpRational, formatRational, significantDp, type Rational } from '../../kob/pair';
import { fitAmountColumn, fitDecimalTexts, tickPriceFraction } from '../kit/format';
import { basisOf, basisToTokenSompi, bigOrNull, bpsPercentText, priceDecimals, toneOf, unitsToNumber, type Tone } from './market-model';
import { kasPerToken } from './orientation';
import type { PairVolume } from './pair-market';
import { filterTokens, tokenLabel, type TokenRow } from './token-model';
import { usdStats } from './usd-model';

// ------------------------------------------------------------------------------------------------ async slots

/** One fetched figure: still loading, there (`value`), or unavailable (no indexer, a route it does not serve, an error). */
export type Slot<T> = { state: 'loading' } | { state: 'ok'; value: T } | { state: 'none' };

export const LOADING: Slot<never> = { state: 'loading' };
export const NONE: Slot<never> = { state: 'none' };

/**
 * Runs `fn` over `items` with at most `limit` calls in flight (the list asks the indexer for one token at a time per worker: its heavy routes are
 * rate limited). Stops taking new items once `signal` aborts; a rejected call does not stop the others (`fn` handles its own errors).
 */
export async function runPool<T>(items: readonly T[], limit: number, fn: (item: T) => Promise<void>, signal?: AbortSignal): Promise<void> {
  let next = 0;
  const worker = async (): Promise<void> => {
    while (next < items.length && !signal?.aborted) {
      const item = items[next++]!;
      try {
        await fn(item);
      } catch {
        /* the item's own failure: the others go on */
      }
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(limit, items.length)) }, worker));
}

// ------------------------------------------------------------------------------------------------ cells

/** A figure as a cell shows it: `text` null = no figure (a skeleton while the row loads, else `—`). */
export interface Cell {
  text: string | null;
  title: string | null;
  tone?: Tone | null;
}

const EMPTY: Cell = { text: null, title: null };

/** Characters of a price or volume cell before it turns compact (K / M / B; the exact figure in the tooltip). */
export const LIST_CELL_CHARS = 12;

/** Exact-enough text of a rational for a tooltip: up to 18 fraction digits, trailing zeros trimmed, thousands grouped. */
const exactText = (r: Rational): string => formatRational(r, 18, 'nearest', 0, ',');

/** A price cell: `dp` fraction digits (compact when too wide); the tooltip holds the exact price and its unit (`BASE / QUOTE`). */
export function priceCell(r: Rational | null, dp: number, unit: string): Cell {
  if (!r || r.num <= 0n) return EMPTY;
  const fit = fitDecimalTexts([formatRational(r, dp, 'nearest', dp, ',')], { width: LIST_CELL_CHARS })[0]!;
  return { text: fit.text, title: t('market.list.priceTitle', { price: exactText(r), unit }) };
}

/** A 24 h change cell (`+2.87%`) coloured by its sign. */
export function changeCell(bps: number | null): Cell {
  return bps === null ? EMPTY : { text: bpsPercentText(bps), title: null, tone: toneOf(bps) };
}

/**
 * A volume cell (base units of a `decimals` asset): two decimals, compact when too wide, `<0.01` for a positive amount that would read zero; the
 * tooltip holds the exact amount with its unit and `note` (the trade count).
 */
export function volumeCell(v: bigint | null, decimals: number, unit: string, note: string | null): Cell {
  if (v === null) return EMPTY;
  const fraction = Math.min(2, decimals);
  const fit = fitAmountColumn([v], decimals, { fraction, width: LIST_CELL_CHARS, unit })[0]!;
  const below = v > 0n && /^[0.,]+$/.test(fit.text);
  const text = below ? (fraction > 0 ? `<0.${'0'.repeat(fraction - 1)}1` : '<1') : fit.text;
  const exact = fit.exact ?? `${fit.text} ${unit}`;
  return { text, title: note ? `${exact} · ${note}` : exact };
}

// ------------------------------------------------------------------------------------------------ entries

export type SortKey = 'market' | 'last' | 'change' | 'volume' | 'spread' | 'orders';
export type SortDir = 'asc' | 'desc';
export interface SortState { key: SortKey; dir: SortDir }

/** The list opens by 24 h volume, the most traded market first. */
export const DEFAULT_SORT: SortState = { key: 'volume', dir: 'desc' };

/** One row of either table. */
export interface ListEntry {
  /** the covenant id, or `<base>/<quote>` */
  key: string;
  kind: 'token' | 'pair';
  /** the shown market name (the token's label, or `BASE (id) / QUOTE (id)`): the `market` sort, lower case */
  name: string;
  /** a figure is still being fetched */
  loading: boolean;
  /** the 24 h figures could not be read (no indexer, a route it does not serve, an error) */
  unavailable: boolean;
  /** the values the columns sort by (null: none, always last) */
  sort: { last: Rational | null; change: number | null; volume: number | null; spread: number | null; orders: number | null };
  cells: { last: Cell; change: Cell; volume: Cell; spread: Cell; orders: Cell };
}

const finiteInt = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? Math.trunc(v) : null);

/** Fraction digits of a token's KAS price: the registry tick's (every tick-aligned price exact), else about five significant digits of `ref`. */
export function tokenPriceDp(row: Pick<TokenRow, 'tick' | 'scale'>, decimals: number, ref: bigint | null, basis: bigint | null): number {
  return tickPriceFraction(row.tick, decimals, row.scale) ?? priceDecimals(ref !== null && basis ? basisToTokenSompi(ref, decimals, basis) : null);
}

const ordersCell = (asks: number | null, bids: number | null): Cell =>
  asks === null && bids === null ? EMPTY : { text: String((asks ?? 0) + (bids ?? 0)), title: t('market.list.ordersTitle', { asks: asks ?? 0, bids: bids ?? 0 }) };

/** A token's KAS market (TOKEN/KAS) from its `/v1/stats` slot; the open order counts of `/v1/tokens` are there before the stats. */
export function tokenEntry(row: TokenRow, slot: Slot<StatsView>): ListEntry {
  const label = tokenLabel(row);
  const v = slot.state === 'ok' ? slot.value : null;
  const asks = row.openAsks ?? finiteInt(v?.open_asks);
  const bids = row.openBids ?? finiteInt(v?.open_bids);
  const base: ListEntry = {
    key: row.covenantId, kind: 'token', name: label.toLowerCase(), loading: slot.state === 'loading', unavailable: slot.state === 'none',
    sort: { last: null, change: null, volume: null, spread: null, orders: asks === null && bids === null ? null : (asks ?? 0) + (bids ?? 0) },
    cells: { last: EMPTY, change: EMPTY, volume: EMPTY, spread: EMPTY, orders: ordersCell(asks, bids) },
  };
  if (!v) return base;
  const basis = basisOf(v.price_basis);
  const decimals = row.decimals ?? finiteInt(v.decimals) ?? 0;
  const unit = t('market.stats.unitOf', { base: row.ticker || label, quote: 'KAS' });
  const last = bigOrNull(v.last);
  const bid = bigOrNull(v.best_bid);
  const ask = bigOrNull(v.best_ask);
  const dp = tokenPriceDp(row, decimals, last ?? bigOrNull(v.mid) ?? ask ?? bid, basis);
  const kas = (p: bigint | null): Rational | null => (p !== null && p > 0n && basis ? kasPerToken(p, decimals, basis) : null);
  const lastR = kas(last);
  const bps = finiteInt(v.change_24h_bps);
  const qv = bigOrNull(v.quote_volume_24h);
  const trades = finiteInt(v.trades_24h);
  const spreadBps = finiteInt(v.spread_bps);
  const side = (p: bigint | null): string => {
    const r = kas(p);
    return r ? formatRational(r, dp, 'nearest', dp, ',') : '—';
  };
  const quotes = bid !== null || ask !== null ? t('market.list.spreadTitle', { bid: side(bid), ask: side(ask), unit }) : null;
  return {
    ...base,
    sort: { ...base.sort, last: lastR, change: bps, volume: qv === null ? null : unitsToNumber(qv, 8), spread: spreadBps },
    cells: {
      ...base.cells,
      last: priceCell(lastR, dp, unit),
      change: changeCell(bps),
      volume: volumeCell(qv, 8, 'KAS', trades === null ? null : t('market.list.trades', { n: trades })),
      spread: spreadBps === null ? { text: null, title: quotes } : { text: bpsPercentText(spreadBps).replace('+', ''), title: quotes },
    },
  };
}

/** A token pair with live pair orders whose two tokens the list offers. */
export interface ListedPair {
  key: string;
  base: TokenRow;
  quote: TokenRow;
  summary: PairSummaryView;
}

/**
 * The pairs of `/v1/pairs` the list shows: both tokens among the tradable rows AND in the registry (a pair page needs both registry entries);
 * one entry per oriented pair.
 */
export function listedPairs(summaries: readonly PairSummaryView[] | null | undefined, rows: readonly TokenRow[]): ListedPair[] {
  const byId = new Map(rows.filter((r) => r.info !== null).map((r) => [r.covenantId, r]));
  const out = new Map<string, ListedPair>();
  for (const s of summaries ?? []) {
    const base = byId.get(s.base);
    const quote = byId.get(s.quote);
    const key = `${s.base}/${s.quote}`;
    if (!base || !quote || base === quote || out.has(key)) continue;
    out.set(key, { key, base, quote, summary: s });
  }
  return [...out.values()];
}

/** `BASE (id) / QUOTE (id)`: both tokens with their id fragments. */
export const pairLabelOf = (p: Pick<ListedPair, 'base' | 'quote'>): string => `${tokenLabel(p.base)} / ${tokenLabel(p.quote)}`;

/**
 * A token pair: last price and change implied through the two KAS markets (the pair page's figures: no price ever comes from a pair fill), the 24 h
 * pair volume in QUOTE, the live pair order counts. The spread cell says the best prices are on the pair page.
 */
export function pairEntry(p: ListedPair, baseStats: Slot<StatsView>, quoteStats: Slot<StatsView>, volume: Slot<PairVolume | null>): ListEntry {
  const s = p.summary;
  const asks = s.direct_asks + s.entry_asks;
  const bids = s.direct_bids + s.entry_bids;
  const orders = asks + bids + s.conditionals;
  const bd = p.base.decimals ?? 0;
  const qd = p.quote.decimals ?? 0;
  const stats =
    baseStats.state === 'ok' && quoteStats.state === 'ok' ? usdStats({ stats: baseStats.value, decimals: bd }, { stats: quoteStats.value, decimals: qd }) : null;
  const last = stats?.last ?? null;
  const unit = t('market.stats.unitOf', { base: p.base.ticker, quote: p.quote.ticker });
  const vol = volume.state === 'ok' ? volume.value : null;
  const loading = baseStats.state === 'loading' || quoteStats.state === 'loading' || volume.state === 'loading';
  const unavailable = !loading && (baseStats.state === 'none' || quoteStats.state === 'none') && volume.state === 'none';
  return {
    key: p.key, kind: 'pair', name: pairLabelOf(p).toLowerCase(), loading, unavailable,
    sort: { last, change: stats?.changeBps ?? null, volume: vol ? unitsToNumber(vol.b, qd) : null, spread: null, orders },
    cells: {
      last: priceCell(last, last ? significantDp(last, 5, 2, 10) : 4, unit),
      change: changeCell(stats?.changeBps ?? null),
      volume: vol ? volumeCell(vol.b, qd, p.quote.ticker, t('market.list.pairFills', { n: vol.fills })) : EMPTY,
      spread: { text: null, title: t('market.list.pairSpread') },
      orders: { text: String(orders), title: t('market.list.pairOrdersTitle', { asks, bids, conditionals: s.conditionals }) },
    },
  };
}

// ------------------------------------------------------------------------------------------------ search and sort

/**
 * Pairs matching the list's search: `BASE/QUOTE` (either part may be empty) matches each side against its token, anything else matches a pair when
 * it matches either token (ticker, name, covenant id, look-alike letters: `filterTokens`). Empty query = every pair.
 */
export function filterPairs<P extends Pick<ListedPair, 'base' | 'quote'>>(pairs: readonly P[], query: string): P[] {
  const q = query.trim();
  if (!q) return [...pairs];
  const hit = (r: TokenRow, s: string): boolean => !s.trim() || filterTokens([r], s).length > 0;
  const slash = q.indexOf('/');
  if (slash >= 0) {
    const a = q.slice(0, slash);
    const b = q.slice(slash + 1);
    return pairs.filter((p) => hit(p.base, a) && hit(p.quote, b));
  }
  return pairs.filter((p) => hit(p.base, q) || hit(p.quote, q));
}

/** The direction a column sorts in when its header is first chosen: names and spreads ascending, every other figure descending. */
export const firstDir = (key: SortKey): SortDir => (key === 'market' || key === 'spread' ? 'asc' : 'desc');

/** A header was chosen: the same column turns its direction over, another column starts in its own first direction. */
export const nextSort = (cur: SortState, key: SortKey): SortState => (cur.key === key ? { key, dir: cur.dir === 'asc' ? 'desc' : 'asc' } : { key, dir: firstDir(key) });

/** `aria-sort` of a column header. */
export const ariaSort = (cur: SortState, key: SortKey): 'ascending' | 'descending' | 'none' => (cur.key !== key ? 'none' : cur.dir === 'asc' ? 'ascending' : 'descending');

const cmpValue = (a: Rational | number, b: Rational | number): number =>
  typeof a === 'number' && typeof b === 'number' ? a - b : typeof a === 'object' && typeof b === 'object' ? cmpRational(a, b) : 0;

/** The entries in the chosen order. Missing figures go last in both directions; ties keep the name order (then the key). */
export function sortEntries<E extends Pick<ListEntry, 'key' | 'name' | 'sort'>>(entries: readonly E[], s: SortState): E[] {
  const sign = s.dir === 'asc' ? 1 : -1;
  const byName = (a: E, b: E): number => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
  return [...entries].sort((a, b) => {
    if (s.key === 'market') return sign * byName(a, b);
    const x = a.sort[s.key];
    const y = b.sort[s.key];
    if (x === null || y === null) return x === null && y === null ? byName(a, b) : x === null ? 1 : -1;
    const c = cmpValue(x, y);
    return c !== 0 ? sign * Math.sign(c) : byName(a, b);
  });
}
