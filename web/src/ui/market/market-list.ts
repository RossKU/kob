// View model of the market list (`#/market`): one row per tradable token's KAS market (TOKEN/KAS) and one per token pair with live pair orders
// (BASE/QUOTE), with the 24 h figures the indexer already serves:
//
//   * a token's KAS market: `GET /v1/stats/<token>` (one call per token): last price, 24 h change, 24 h KAS volume and trade count, best bid / ask
//     and spread, the KAS the book offers within 2 % of its mid (`depth_2pct_quote`; an older indexer sends none: "—"); the open order counts of
//     `GET /v1/tokens` (the depth tooltip). The MARKET CAP is the last trade price x the token's total supply (`supplyOf`: the registry's verified genesis record
//     of a token with no way to mint more, else the supply kascov.io reports for a token it marks verified, labelled as kascov's); none without both.
//   * a token pair (`GET /v1/pairs`, both tokens tradable and in the registry, as the pair page needs): last price and change implied through the two
//     KAS markets (the same `usdStats` the pair page uses: prices come only from KAS-book fills), the 24 h pair volume of
//     `GET /v1/pairs/<base>/<quote>/fills?limit=1` (`volume_24h`, in QUOTE), the depth within 2 % of the mid of `GET /v1/pairs/<base>/<quote>/book`
//     (QUOTE, `pairDepthOf`), the live pair order counts of the summary (the depth tooltip). No bid / ask here (the pair book is on the pair page).
//
// Each figure is a cell (text + tooltip): prices with about five significant digits (the market's tick when the registry has one), compact K / M / B
// when a cell would be too wide, never a cut digit (the exact figure is in the tooltip). A row whose figures are still loading or unavailable shows
// skeletons / `—`. Sorting by any column, missing figures always last; search over pairs by either token (or `BASE/QUOTE`). Pure.
import type { PairSummaryView, StatsView } from '../../data/indexer-types';
import { ASSUMED_TOKEN_DECIMALS, t } from '../../i18n';
import { cmpRational, formatRational, significantDp, type Rational } from '../../kob/pair';
import { fitAmountColumn, fitDecimalTexts, tickPriceFraction } from '../kit/format';
import { basisOf, basisToTokenSompi, bigOrNull, bpsPercentText, priceDecimals, toneOf, unitsToNumber, type Tone } from './market-model';
import { kasPerToken } from './orientation';
import type { PairDepth, PairVolume } from './pair-market';
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

/** A price cell: `dp` fraction digits (compact when too wide); the tooltip holds the exact price and its unit ("QUOTE per BASE"). */
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

export type SortKey = 'market' | 'last' | 'change' | 'volume' | 'mcap' | 'spread' | 'depth';
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
  /** a token listed from kascov.io that no order of the KOB indexer names: the depth cell says "no orders" and the row sorts after every other row without a figure */
  noOrders?: boolean;
  /** the values the columns sort by (null: none, always last) */
  sort: { last: Rational | null; change: number | null; volume: number | null; mcap: number | null; spread: number | null; depth: number | null };
  cells: { last: Cell; change: Cell; volume: Cell; mcap: Cell; spread: Cell; depth: Cell };
}

const finiteInt = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? Math.trunc(v) : null);

/** Fraction digits of a token's KAS price: the registry tick's (every tick-aligned price exact), else about five significant digits of `ref`. */
export function tokenPriceDp(row: Pick<TokenRow, 'tick' | 'scale'>, decimals: number, ref: bigint | null, basis: bigint | null): number {
  return tickPriceFraction(row.tick, decimals, row.scale) ?? priceDecimals(ref !== null && basis ? basisToTokenSompi(ref, decimals, basis) : null);
}

/** The open order counts as a tooltip note (`3 asks · 4 bids`); null when neither is known. */
const ordersNote = (asks: number | null, bids: number | null): string | null =>
  asks === null && bids === null ? null : t('market.list.ordersTitle', { asks: asks ?? 0, bids: bids ?? 0 });

/** The total supply a market cap divides by, in base units, and where it comes from. */
export interface Supply { units: bigint; source: 'registry' | 'kascov' }

/**
 * A token's fixed total supply, or null when KOB has no sound figure:
 *   1. the registry's genesis record when it is verified and the token has no way to mint more (no minter output, no public-mint allowance, no live minter): a fact
 *      re-derivable from the chain;
 *   2. else the supply kascov.io reports (data/kascov.ts), only for a token it marks `verified` (it checked every event of the token's history, supply conserved),
 *      only when its declared decimals agree with the ones the market uses, and not for a token it says can still be issued. Third party and unchecked by KOB: the
 *      cell says so.
 * (The indexer holds no token output, so it cannot know a supply.)
 */
export function supplyOf(row: Pick<TokenRow, 'info' | 'kascov'>): Supply | null {
  const j = row.info?.json;
  const g = j?.genesis;
  if (j?.genesis_verified === true && g && (g.minter_outputs?.length ?? 0) === 0 && (g.live_minters?.length ?? 0) === 0 && !g.mint_allowance && Number.isSafeInteger(g.supply) && g.supply > 0) {
    return { units: BigInt(g.supply), source: 'registry' };
  }
  const k = row.kascov;
  if (k?.verified && k.supply != null && k.supply > 0n && !k.decimalsDiffers) return { units: k.supply, source: 'kascov' };
  return null;
}

/**
 * The market cap cell in KAS: the last trade price (`last`, KAS per whole token) x the supply (`decimals`: the token's decimals, which the supply is in base units of);
 * the tooltip holds the exact figure and the supply it used with its source. Nothing without a price or a supply. The sort value is whole KAS.
 */
export function marketCapCell(last: Rational | null, supply: Supply | null, decimals: number, ticker: string): { cell: Cell; value: number | null } {
  if (!last || last.num <= 0n || !supply) return { cell: EMPTY, value: null };
  const sompi = (last.num * supply.units * 100_000_000n) / (last.den * 10n ** BigInt(decimals));
  const whole = exactText({ num: supply.units, den: 10n ** BigInt(decimals) });
  const note = t(supply.source === 'registry' ? 'market.list.mcapSupplyRegistry' : 'market.list.mcapSupplyKascov', { supply: whole, unit: ticker });
  return { cell: volumeCell(sompi, 8, 'KAS', note), value: unitsToNumber(sompi, 8) };
}

/** The depth cell of a token in KAS (`depth_2pct_quote`, sompi); empty for an empty book or an indexer that does not send it. */
export function depthCell(sompi: bigint | null, note: string | null): { cell: Cell; value: number | null } {
  return sompi === null ? { cell: EMPTY, value: null } : { cell: volumeCell(sompi, 8, 'KAS', note), value: unitsToNumber(sompi, 8) };
}

/** A token only kascov.io names (`source: 'kascov'`): nothing is fetched for it, every figure is empty and the depth cell says "no orders". */
export function kascovEntry(row: TokenRow): ListEntry {
  return {
    key: row.covenantId, kind: 'token', name: tokenLabel(row).toLowerCase(), loading: false, unavailable: false, noOrders: true,
    sort: { last: null, change: null, volume: null, mcap: null, spread: null, depth: null },
    cells: { last: EMPTY, change: EMPTY, volume: EMPTY, mcap: EMPTY, spread: EMPTY, depth: { text: t('market.list.noOrders'), title: t('market.list.noOrdersTitle') } },
  };
}

/** A token's KAS market (TOKEN/KAS) from its `/v1/stats` slot; the open order counts of `/v1/tokens` (the depth tooltip) are there before the stats. */
export function tokenEntry(row: TokenRow, slot: Slot<StatsView>): ListEntry {
  const label = tokenLabel(row);
  const v = slot.state === 'ok' ? slot.value : null;
  const asks = row.openAsks ?? finiteInt(v?.open_asks);
  const bids = row.openBids ?? finiteInt(v?.open_bids);
  const base: ListEntry = {
    key: row.covenantId, kind: 'token', name: label.toLowerCase(), loading: slot.state === 'loading', unavailable: slot.state === 'none',
    sort: { last: null, change: null, volume: null, mcap: null, spread: null, depth: null },
    cells: { last: EMPTY, change: EMPTY, volume: EMPTY, mcap: EMPTY, spread: EMPTY, depth: EMPTY },
  };
  if (!v) return base;
  const basis = basisOf(v.price_basis);
  // a token without registry entry or declaration: the indexer reports 0 decimals for "unknown", which would show its price per base unit (1e8 too low
  // against its page); the page's rule applies, 8 assumed, never base units (token page `decimals`)
  const reported = finiteInt(v.decimals);
  const decimals = row.decimals ?? (reported !== null && reported > 0 ? reported : ASSUMED_TOKEN_DECIMALS);
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
  const mcap = marketCapCell(lastR, supplyOf(row), decimals, row.ticker || label);
  const depth = depthCell(bigOrNull(v.depth_2pct_quote), ordersNote(asks, bids));
  const side = (p: bigint | null): string => {
    const r = kas(p);
    return r ? formatRational(r, dp, 'nearest', dp, ',') : '—';
  };
  const quotes = bid !== null || ask !== null ? t('market.list.spreadTitle', { bid: side(bid), ask: side(ask), unit }) : null;
  return {
    ...base,
    sort: { ...base.sort, last: lastR, change: bps, volume: qv === null ? null : unitsToNumber(qv, 8), mcap: mcap.value, spread: spreadBps, depth: depth.value },
    cells: {
      ...base.cells,
      last: priceCell(lastR, dp, unit),
      change: changeCell(bps),
      volume: volumeCell(qv, 8, 'KAS', trades === null ? null : t('market.list.trades', { n: trades })),
      mcap: mcap.cell,
      depth: depth.cell,
      spread: spreadBps === null ? { text: null, title: quotes } : { text: bpsPercentText(spreadBps).replace('+', ''), title: quotes },
    },
  };
}

/** The depth cell of a pair (QUOTE base units within 2 % of the mid); a lower bound is marked with a leading "≥" and says so. */
export function pairDepthCell(d: PairDepth, decimals: number, unit: string, note: string | null): Cell {
  const c = volumeCell(d.quote, decimals, unit, note);
  return d.atLeast ? { text: c.text === null ? null : `≥${c.text}`, title: `${c.title ?? ''} · ${t('market.list.depthAtLeast')}` } : c;
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
 * pair volume in QUOTE, the depth within 2 % of the mid in QUOTE (the live pair order counts in its tooltip). The spread cell says the best prices are on the pair page.
 */
export function pairEntry(p: ListedPair, baseStats: Slot<StatsView>, quoteStats: Slot<StatsView>, volume: Slot<PairVolume | null>, depth: Slot<PairDepth | null> = NONE): ListEntry {
  const s = p.summary;
  const asks = s.direct_asks + s.entry_asks;
  const bids = s.direct_bids + s.entry_bids;
  const bd = p.base.decimals ?? 0;
  const qd = p.quote.decimals ?? 0;
  const stats =
    baseStats.state === 'ok' && quoteStats.state === 'ok' ? usdStats({ stats: baseStats.value, decimals: bd }, { stats: quoteStats.value, decimals: qd }) : null;
  const last = stats?.last ?? null;
  const unit = t('market.stats.unitOf', { base: p.base.ticker, quote: p.quote.ticker });
  const vol = volume.state === 'ok' ? volume.value : null;
  const dep = depth.state === 'ok' ? depth.value : null;
  const loading = baseStats.state === 'loading' || quoteStats.state === 'loading' || volume.state === 'loading' || depth.state === 'loading';
  const unavailable = !loading && (baseStats.state === 'none' || quoteStats.state === 'none') && volume.state === 'none';
  return {
    key: p.key, kind: 'pair', name: pairLabelOf(p).toLowerCase(), loading, unavailable,
    sort: { last, change: stats?.changeBps ?? null, volume: vol ? unitsToNumber(vol.b, qd) : null, mcap: null, spread: null, depth: dep ? unitsToNumber(dep.quote, qd) : null },
    cells: {
      last: priceCell(last, last ? significantDp(last, 5, 2, 10) : 4, unit),
      change: changeCell(stats?.changeBps ?? null),
      volume: vol ? volumeCell(vol.b, qd, p.quote.ticker, t('market.list.pairFills', { n: vol.fills })) : EMPTY,
      mcap: EMPTY,
      spread: { text: null, title: t('market.list.pairSpread') },
      depth: dep ? pairDepthCell(dep, qd, p.quote.ticker, t('market.list.pairOrdersTitle', { asks, bids, conditionals: s.conditionals })) : EMPTY,
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

/** The entries in the chosen order. Missing figures go last in both directions (rows with no orders after those); ties keep the name order (then the key). */
export function sortEntries<E extends Pick<ListEntry, 'key' | 'name' | 'sort'> & { noOrders?: boolean }>(entries: readonly E[], s: SortState): E[] {
  const sign = s.dir === 'asc' ? 1 : -1;
  const byName = (a: E, b: E): number => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
  return [...entries].sort((a, b) => {
    if (s.key === 'market') return sign * byName(a, b);
    const x = a.sort[s.key];
    const y = b.sort[s.key];
    if (x === null || y === null) {
      if (x !== null || y !== null) return x === null ? 1 : -1;
      // neither has the figure: a row with no orders at all goes after the rows that merely lack it
      return (a.noOrders ? 1 : 0) - (b.noOrders ? 1 : 0) || byName(a, b);
    }
    const c = cmpValue(x, y);
    return c !== 0 ? sign * Math.sign(c) : byName(a, b);
  });
}
