// View model of the market list (`#/market`). It has one row per tradable token's KAS market
// (TOKEN/KAS). The Favorites view adds one row per favourite token pair (BASE/QUOTE). The rows
// carry the 24 h figures the indexer already serves.
//
// A token's KAS market uses these sources:
// - `GET /v1/stats/<token>`, one call per token. It gives the last price, the 24 h change, the
//   24 h KAS volume and trade count, the best bid and ask, the spread, and the KAS the book offers
//   within 2 % of its mid (`depth_2pct_quote`). An indexer that sends none shows "—".
// - `GET /v1/tokens` gives the open order counts, shown in the depth tooltip.
// - The market cap is the last trade price times the token's total supply (`supplyOf`). The supply
//   is the registry's verified genesis record of a token with no way to mint more. Otherwise it
//   is the supply kascov.io reports for a token it marks verified, labelled as kascov's. A row
//   has no market cap without both figures.
//
// A favourite token pair uses these sources:
// - Last price and change implied through the two KAS markets. This is the same `usdStats` the
//   pair page uses. Prices come only from KAS-book fills.
// - The 24 h pair volume in QUOTE, from `GET /v1/pairs/<base>/<quote>/fills?limit=1`
//   (`volume_24h`).
// - The depth within 2 % of the mid, in QUOTE (`pairDepthOf`), from
//   `GET /v1/pairs/<base>/<quote>/book`.
// - The live pair order counts of `GET /v1/pairs`, shown in the depth tooltip. A pair without live
//   orders has none.
// A pair row has no bid or ask. The pair book is on the pair page.
//
// Each figure is a cell with text and a tooltip. Prices show about five significant digits, or the
// market's tick when the registry has one. A cell turns compact (K, M, B) when it would be too
// wide, and never cuts a digit. The exact figure is in the tooltip. A row whose figures are still
// loading or unavailable shows skeletons or `—`. Any column sorts, and missing figures always go
// last. The search covers pairs by either token or by `BASE/QUOTE`. The module is pure.
import type { PairSummaryView, StatsView } from '../../data/indexer-types';
import { ASSUMED_TOKEN_DECIMALS, t } from '../../i18n';
import { exponentOf } from '../../kob/open-token';
import { cmpRational, formatRational, significantDp, type Rational } from '../../kob/pair';
import { kasPerTokenToUsd, rateTitle, sompiToUsd, usdText, usdTextFit, type Fiat, type Rate } from '../../app/fiat';
import { displayPriceDp, fitAmountColumn, fitDecimalTexts } from '../kit/format';
import { basisOf, basisToTokenSompi, bigOrNull, bpsPercentText, changeMarks, thinBookUnder, toneOf, unitsToNumber, type Tone } from './market-model';
import { kascovTitleLine, spotPairPrice, spotPrice, withKascovLine } from './kascov-overlay';
import { kasPerToken } from './orientation';
import type { PairDepth, PairVolume } from './pair-market';
import { mergePairSummaries } from './pair-model';
import type { FavoritePair } from './favorites';
import { filterTokens, idOnlyRow, tokenLabel, type TokenRow } from './token-model';
import { usdStats } from './usd-model';

// ------------------------------------------------------------------------------------- async slots

/** A read has given what it will for now: an answer, or no read under way (failed or empty). */
export const settled = (read: { data: unknown; loading: boolean }): boolean => !read.loading || (read.data !== undefined && read.data !== null);

/**
 * One fetched figure. It is still loading, there (`value`), or unavailable. Unavailable means no
 * indexer, a route it does not serve, or an error.
 */
export type Slot<T> = { state: 'loading' } | { state: 'ok'; value: T } | { state: 'none' };

export const LOADING: Slot<never> = { state: 'loading' };
export const NONE: Slot<never> = { state: 'none' };

/**
 * Runs `fn` over `items` with at most `limit` calls in flight. The list asks the indexer for one
 * token at a time per worker, because its heavy routes are rate limited. The pool stops taking new
 * items once `signal` aborts. A rejected call does not stop the others, because `fn` handles its
 * own errors.
 */
export async function runPool<T>(items: readonly T[], limit: number, fn: (item: T) => Promise<void>, signal?: AbortSignal): Promise<void> {
  let next = 0;
  const worker = async (): Promise<void> => {
    while (next < items.length && !signal?.aborted) {
      const item = items[next++]!;
      try {
        await fn(item);
      } catch {
        /* The item's own failure. The others go on. */
      }
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(limit, items.length)) }, worker));
}

// --------------------------------------------------------------------------------- the list's rows

/** The rows of one page of a list, as the viewer chooses. */
export const LIST_PAGE_SIZES = [20, 50, 100] as const;
export type ListPageSize = (typeof LIST_PAGE_SIZES)[number];
export const DEFAULT_LIST_PAGE_SIZE: ListPageSize = 20;

/** A stored page size, or `fallback` for anything else. */
export const listPageSizeOf = (raw: string | null | undefined, fallback: ListPageSize = DEFAULT_LIST_PAGE_SIZE): ListPageSize => LIST_PAGE_SIZES.find((n) => String(n) === raw) ?? fallback;

/**
 * One page of a list. `page` counts from 0 and never passes the last page, `pages` is how many
 * there are, and the rows are `from` (included) to `to` (not included).
 */
export interface PageWindow { page: number; pages: number; from: number; to: number }

/**
 * The page `page` of `total` rows, `size` rows each. A list that got shorter shows its last page.
 */
export function pageWindow(total: number, size: number, page: number): PageWindow {
  const pages = Math.max(1, Math.ceil(total / size));
  const p = Math.min(Math.max(0, page), pages - 1);
  return { page: p, pages, from: p * size, to: Math.min(total, (p + 1) * size) };
}

/**
 * The keys in their place in `kept`, where a key that left is dropped. Keys `kept` does not have
 * follow in the order of `keys`. A figure that arrives moves no row.
 */
export function keepOrder(kept: readonly string[], keys: readonly string[]): string[] {
  const now = new Set(keys);
  const out = kept.filter((k) => now.has(k));
  const had = new Set(out);
  return [...out, ...keys.filter((k) => !had.has(k))];
}

// ------------------------------------------------------------------------------------------- cells

/**
 * A figure as a cell shows it. A null `text` means no figure: a skeleton while the row loads, else
 * `—`.
 */
export interface Cell {
  text: string | null;
  title: string | null;
  tone?: Tone | null;
  /** A change over a thin window (few trades). It is shown muted and the tooltip says how many. */
  thin?: boolean;
  /**
   * A change over a thin book, where the smaller side within 2 % is worth under $100. It is shown
   * muted and the tooltip says so.
   */
  thinBook?: boolean;
}

const EMPTY: Cell = { text: null, title: null };

/**
 * Characters of a price or volume cell before it turns compact (K, M, B). The tooltip has the exact
 * figure.
 */
export const LIST_CELL_CHARS = 12;

/**
 * Exact-enough text of a rational for a tooltip: up to 18 fraction digits, zeros trimmed, grouped.
 */
const exactText = (r: Rational): string => formatRational(r, 18, 'nearest', 0, ',');

/**
 * A price cell with `dp` fraction digits, compact when too wide. The tooltip holds the exact price
 * and its unit ("QUOTE per BASE").
 */
export function priceCell(r: Rational | null, dp: number, unit: string): Cell {
  if (!r || r.num <= 0n) return EMPTY;
  const fit = fitDecimalTexts([formatRational(r, dp, 'nearest', dp, ',')], { width: LIST_CELL_CHARS })[0]!;
  return { text: fit.text, title: t('market.list.priceTitle', { price: exactText(r), unit }) };
}

/**
 * A 24 h change cell such as `+2.87%`, coloured by its sign. `trades` is the market's 24 h trade
 * count, when the indexer reports it. A change over fewer than `THIN_TRADES_24H` trades is muted
 * and its tooltip says "Based on N trades in 24 h" (`thin`). A `thinBook` (`thinBookUnder`) is
 * muted the same way and has its own tooltip line (`thinBook`). The text and the cell do not
 * change.
 */
export function changeCell(bps: number | null, trades?: number | null, thinBookUsd: Rational | null = null): Cell {
  if (bps === null) return EMPTY;
  const marks = changeMarks(bps, trades, thinBookUsd);
  if (!marks) return { text: bpsPercentText(bps), title: null, tone: toneOf(bps) };
  return { text: bpsPercentText(bps), title: marks.note, tone: 'flat', ...(marks.thinTrades ? { thin: true } : {}), ...(marks.thinBook ? { thinBook: true } : {}) };
}

/**
 * A volume cell for an amount in base units of a `decimals` asset. It shows two decimals, turns
 * compact when too wide, and shows `<0.01` for a positive amount that would read zero. The tooltip
 * holds the exact amount with its unit and `note`, the trade count.
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

/**
 * A figure of the KAS markets shown in USD (Settings, Display currency; app/fiat.ts). `kas` is its
 * KAS cell and `usd` the same value in dollars. The text is `$1,066.77`, compact before it would
 * cut a digit. When the text is compact, the tooltip leads with the exact dollars. It then has the
 * KAS cell's own tooltip, with the exact KAS figure and its unit, and the rate used. So the KAS
 * figure is always one hover away. A cell without a figure stays as it is.
 */
export function usdCell(kas: Cell, usd: Rational | null, fiat: Fiat): Cell {
  if (kas.text === null || !usd) return kas;
  const exact = usdText(usd);
  const text = usdTextFit(usd, LIST_CELL_CHARS);
  const title = [text !== exact ? exact : null, kas.title ?? kas.text, rateTitle(fiat)].filter((x): x is string => !!x).join(' · ');
  return { ...kas, text, title };
}

// ----------------------------------------------------------------------------------------- entries

export type SortKey = 'market' | 'last' | 'change' | 'volume' | 'mcap' | 'spread' | 'depth';
export type SortDir = 'asc' | 'desc';
export interface SortState { key: SortKey; dir: SortDir }

/** The list opens by 24 h volume, the most traded market first. */
export const DEFAULT_SORT: SortState = { key: 'volume', dir: 'desc' };

/** One row of either table. */
export interface ListEntry {
  /** The covenant id, or `<base>/<quote>`. */
  key: string;
  kind: 'token' | 'pair';
  /**
   * The shown market name, lower case, which the `market` sort uses. It is the token's label or
   * `BASE (id) / QUOTE (id)`.
   */
  name: string;
  /** A figure is still being fetched. */
  loading: boolean;
  /** The 24 h figures could not be read: no indexer, a route it does not serve, or an error. */
  unavailable: boolean;
  /**
   * A token listed from kascov.io that no order of the KOB indexer names. The depth cell says "no
   * orders" and the row sorts after every other row without a figure.
   */
  noOrders?: boolean;
  /** The values the columns sort by. Null means none, and always sorts last. */
  sort: { last: Rational | null; change: number | null; volume: number | null; mcap: number | null; spread: number | null; depth: number | null };
  cells: { last: Cell; change: Cell; volume: Cell; mcap: Cell; spread: Cell; depth: Cell };
}

const finiteInt = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? Math.trunc(v) : null);

/**
 * Fraction digits of a token's KAS price. It follows the kit's one display rule, about five
 * significant digits of `ref` and at most 6. The registry tick is not used, because it can need 8
 * at 33 KAS. The exact price is the cell's title.
 */
export function tokenPriceDp(decimals: number, ref: bigint | null, basis: bigint | null): number {
  return displayPriceDp(ref !== null && basis ? basisToTokenSompi(ref, decimals, basis) : null);
}

/** The open order counts as a tooltip note (`3 asks · 4 bids`). Null when neither is known. */
const ordersNote = (asks: number | null, bids: number | null): string | null =>
  asks === null && bids === null ? null : t('market.list.ordersTitle', { asks: asks ?? 0, bids: bids ?? 0 });

/** The total supply a market cap divides by, in base units, and where it comes from. */
export interface Supply { units: bigint; source: 'registry' | 'kascov' }

/**
 * A token's fixed total supply, or null when KOB has no sound figure. There are two sources.
 * 1. The registry's genesis record, when it is verified and the token has no way to mint more.
 *    That means no minter output, no public-mint allowance and no live minter. It is a fact that
 *    can be re-derived from the chain.
 * 2. Else the supply kascov.io reports (data/kascov.ts). It applies only to a token kascov marks
 *    `verified`, meaning it checked every event of the token's history and the supply is
 *    conserved. It also needs the declared decimals to agree with the ones the market uses. It is
 *    not used for a token kascov says can still be issued. It is third party and unchecked by
 *    KOB, and the cell says so.
 * The indexer holds no token output, so it cannot know a supply.
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
 * The market cap cell in KAS. It is the last trade price (`last`, KAS per whole token) times the
 * supply. `decimals` is the token's decimals, and the supply is in base units of it. The tooltip
 * holds the exact figure and the supply it used with its source. There is nothing without a price
 * or a supply. The sort value is whole KAS.
 */
export function marketCapCell(last: Rational | null, supply: Supply | null, decimals: number, ticker: string): { cell: Cell; value: number | null; sompi?: bigint } {
  if (!last || last.num <= 0n || !supply) return { cell: EMPTY, value: null };
  const sompi = (last.num * supply.units * 100_000_000n) / (last.den * 10n ** BigInt(decimals));
  const whole = exactText({ num: supply.units, den: 10n ** BigInt(decimals) });
  const note = t(supply.source === 'registry' ? 'market.list.mcapSupplyRegistry' : 'market.list.mcapSupplyKascov', { supply: whole, unit: ticker });
  return { cell: volumeCell(sompi, 8, 'KAS', note), value: unitsToNumber(sompi, 8), sompi };
}

/**
 * The depth cell of a token in KAS (`depth_2pct_quote`, sompi). It is empty for an empty book or an
 * indexer that does not send it.
 */
export function depthCell(sompi: bigint | null, note: string | null): { cell: Cell; value: number | null } {
  return sompi === null ? { cell: EMPTY, value: null } : { cell: volumeCell(sompi, 8, 'KAS', note), value: unitsToNumber(sompi, 8) };
}

/**
 * A token only kascov.io names (`source: 'kascov'`). Nothing is fetched for it, every figure is
 * empty and the depth cell says "no orders".
 */
export function kascovEntry(row: TokenRow): ListEntry {
  return {
    key: row.covenantId, kind: 'token', name: tokenLabel(row).toLowerCase(), loading: false, unavailable: false, noOrders: true,
    sort: { last: null, change: null, volume: null, mcap: null, spread: null, depth: null },
    cells: { last: EMPTY, change: EMPTY, volume: EMPTY, mcap: EMPTY, spread: EMPTY, depth: { text: t('market.list.noOrders'), title: t('market.list.noOrdersTitle') } },
  };
}

/**
 * A token only the wallet's holdings name (`source: 'held'`, held-list.ts). The indexer lists no
 * order of it, so nothing is fetched and every figure reads "—". It sorts after the rows with
 * figures, as a row without orders does.
 */
export function heldEntry(row: TokenRow): ListEntry {
  return {
    key: row.covenantId, kind: 'token', name: tokenLabel(row).toLowerCase(), loading: false, unavailable: false, noOrders: true,
    sort: { last: null, change: null, volume: null, mcap: null, spread: null, depth: null },
    cells: { last: EMPTY, change: EMPTY, volume: EMPTY, mcap: EMPTY, spread: EMPTY, depth: EMPTY },
  };
}

/**
 * A token's KAS market (TOKEN/KAS) from its `/v1/stats` slot. The open order counts of
 * `/v1/tokens`, used in the depth tooltip, are there before the stats. With `fiat`, meaning the
 * USD display is in force, the price, the 24 h volume, the market cap and the depth are shown in
 * dollars (`usdCell`). The change and the spread are ratios and stay. The sort values stay in KAS,
 * which gives the same order.
 */
export function tokenEntry(row: TokenRow, slot: Slot<StatsView>, fiat: Fiat | null = null, rate: Rate | null = null): ListEntry {
  const label = tokenLabel(row);
  const v = slot.state === 'ok' ? slot.value : null;
  const asks = row.openAsks ?? finiteInt(v?.open_asks);
  const bids = row.openBids ?? finiteInt(v?.open_bids);
  const base: ListEntry = {
    key: row.covenantId, kind: 'token', name: label.toLowerCase(), loading: slot.state === 'loading', unavailable: slot.state === 'none',
    sort: { last: null, change: null, volume: null, mcap: null, spread: null, depth: null },
    cells: { last: EMPTY, change: EMPTY, volume: EMPTY, mcap: EMPTY, spread: EMPTY, depth: EMPTY },
  };
  // kascov.io's price is a line of the price cell's tooltip. It needs the decimals the price is
  // read with, which are its own market's, else the row's.
  const kSpot = row.kascov?.spot ?? null;
  const kLine = (decimals: number | null, own: Rational | null): string | null => (kSpot && decimals !== null ? kascovTitleLine(spotPrice(kSpot, decimals), own, 'KAS') : null);
  if (!v) return kLine(row.decimals, null) ? { ...base, cells: { ...base.cells, last: { text: null, title: kLine(row.decimals, null) } } } : base;
  const basis = basisOf(v.price_basis);
  // Nothing else states them, so use the scale of its market, as the token page reads it. An
  // indexer that reports 0 decimals for "unknown" and no market scale would show its price per
  // base unit, 1e8 too low against its page.
  const reported = finiteInt(v.decimals);
  const scaled = typeof row.index?.market_scale === 'number' ? exponentOf(row.index.market_scale) : null;
  const decimals = row.decimals ?? scaled ?? (reported !== null && reported > 0 ? reported : ASSUMED_TOKEN_DECIMALS);
  const unit = t('market.stats.unitOf', { base: row.ticker || label, quote: 'KAS' });
  const last = bigOrNull(v.last);
  const bid = bigOrNull(v.best_bid);
  const ask = bigOrNull(v.best_ask);
  const dp = tokenPriceDp(decimals, last ?? bigOrNull(v.mid) ?? ask ?? bid, basis);
  const kas = (p: bigint | null): Rational | null => (p !== null && p > 0n && basis ? kasPerToken(p, decimals, basis) : null);
  const lastR = kas(last);
  const bps = finiteInt(v.change_24h_bps);
  const qv = bigOrNull(v.quote_volume_24h);
  const trades = finiteInt(v.trades_24h);
  const spreadBps = finiteInt(v.spread_bps);
  const supply = supplyOf(row);
  const mcap = marketCapCell(lastR, supply, decimals, row.ticker || label);
  const depthSompi = bigOrNull(v.depth_2pct_quote);
  const depth = depthCell(depthSompi, ordersNote(asks, bids));
  const inUsd = (kas: Cell, sompi: bigint | null): Cell => (fiat && sompi !== null ? usdCell(kas, sompiToUsd(sompi, fiat.rate), fiat) : kas);
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
      last: withKascovLine(fiat && lastR ? usdCell(priceCell(lastR, dp, unit), kasPerTokenToUsd(lastR, fiat.rate), fiat) : priceCell(lastR, dp, unit), kLine(decimals, lastR)),
      change: changeCell(bps, trades, thinBookUnder(v, rate, supply?.units)),
      volume: inUsd(volumeCell(qv, 8, 'KAS', trades === null ? null : t('market.list.trades', { n: trades })), qv),
      mcap: inUsd(mcap.cell, mcap.sompi ?? null),
      depth: inUsd(depth.cell, depthSompi),
      spread: spreadBps === null ? { text: null, title: quotes } : { text: bpsPercentText(spreadBps).replace('+', ''), title: quotes },
    },
  };
}

/**
 * The depth cell of a pair (QUOTE base units within 2 % of the mid). A lower bound is marked with a
 * leading "≥" and says so.
 */
export function pairDepthCell(d: PairDepth, decimals: number, unit: string, note: string | null): Cell {
  const c = volumeCell(d.quote, decimals, unit, note);
  return d.atLeast ? { text: c.text === null ? null : `≥${c.text}`, title: `${c.title ?? ''} · ${t('market.list.depthAtLeast')}` } : c;
}

/** A favourite token pair as the list shows it. */
export interface ListedPair {
  key: string;
  base: TokenRow;
  quote: TokenRow;
  /**
   * Its live order counts, both orientations together (`mergePairSummaries`). Null when `/v1/pairs`
   * lists neither.
   */
  summary: PairSummaryView | null;
}

/**
 * A pair's figures are in whole tokens, never on assumed decimals. Without both tokens' decimals it
 * has no figures.
 */
export const pairHasDecimals = (p: Pick<ListedPair, 'base' | 'quote'>): boolean => p.base.decimals !== null && p.quote.decimals !== null;

/**
 * The rows of `wanted`, in that order. Each token is as the list has it, with the decimals of the
 * scale of its first order when it has none of its own, else by its id alone. A favourite stays
 * reachable whatever the lists say of it now.
 */
export function listedPairs(wanted: readonly FavoritePair[], rows: readonly TokenRow[], summaries?: readonly PairSummaryView[] | null): ListedPair[] {
  const byId = new Map(rows.map((r) => [r.covenantId.toLowerCase(), r]));
  const side = (id: string): TokenRow => {
    const r = byId.get(id) ?? idOnlyRow(id);
    return r.decimals === null && typeof r.index?.market_scale === 'number' ? { ...r, decimals: exponentOf(r.index.market_scale) } : r;
  };
  return wanted.map(({ base, quote }) => ({
    key: `${base}/${quote}`, base: side(base), quote: side(quote), summary: mergePairSummaries(base, quote, summaries),
  }));
}

/** `BASE (id) / QUOTE (id)`: both tokens with their id fragments. */
export const pairLabelOf = (p: Pick<ListedPair, 'base' | 'quote'>): string => `${tokenLabel(p.base)} / ${tokenLabel(p.quote)}`;

/**
 * A token pair. Its last price and change are implied through the two KAS markets, as on the pair
 * page, and no price ever comes from a pair fill. It also shows the 24 h pair volume in QUOTE and
 * the depth within 2 % of the mid in QUOTE, with the live pair order counts in its tooltip. The
 * spread cell says the best prices are on the pair page.
 */
export function pairEntry(p: ListedPair, baseStats: Slot<StatsView>, quoteStats: Slot<StatsView>, volume: Slot<PairVolume | null>, depth: Slot<PairDepth | null> = NONE): ListEntry {
  const s = p.summary;
  const orders = s ? t('market.list.pairOrdersTitle', { asks: s.direct_asks + s.entry_asks, bids: s.direct_bids + s.entry_bids, conditionals: s.conditionals }) : null;
  const bd = p.base.decimals ?? 0;
  const qd = p.quote.decimals ?? 0;
  const stats =
    pairHasDecimals(p) && baseStats.state === 'ok' && quoteStats.state === 'ok' ? usdStats({ stats: baseStats.value, decimals: bd }, { stats: quoteStats.value, decimals: qd }) : null;
  const last = stats?.last ?? null;
  const unit = t('market.stats.unitOf', { base: p.base.ticker, quote: p.quote.ticker });
  // kascov.io's price of the pair, only when it prices both tokens (the ratio of the two).
  const bs = p.base.kascov?.spot ?? null;
  const qs = p.quote.kascov?.spot ?? null;
  const pairKascovLine = bs && qs && pairHasDecimals(p) ? kascovTitleLine(spotPairPrice(bs, bd, qs, qd), last, p.quote.ticker) : null;
  const vol = volume.state === 'ok' ? volume.value : null;
  const dep = depth.state === 'ok' ? depth.value : null;
  const loading = baseStats.state === 'loading' || quoteStats.state === 'loading' || volume.state === 'loading' || depth.state === 'loading';
  const unavailable = !loading && (baseStats.state === 'none' || quoteStats.state === 'none') && volume.state === 'none';
  return {
    key: p.key, kind: 'pair', name: pairLabelOf(p).toLowerCase(), loading, unavailable,
    sort: { last, change: stats?.changeBps ?? null, volume: vol ? unitsToNumber(vol.b, qd) : null, mcap: null, spread: null, depth: dep ? unitsToNumber(dep.quote, qd) : null },
    cells: {
      last: withKascovLine(priceCell(last, last ? significantDp(last, 5, 2, 10) : 4, unit), pairKascovLine),
      change: changeCell(stats?.changeBps ?? null),
      volume: vol ? volumeCell(vol.b, qd, p.quote.ticker, t('market.list.pairFills', { n: vol.fills })) : EMPTY,
      mcap: EMPTY,
      spread: { text: null, title: t('market.list.pairSpread') },
      depth: dep ? pairDepthCell(dep, qd, p.quote.ticker, orders) : EMPTY,
    },
  };
}

// --------------------------------------------------------------------------------- search and sort

/**
 * Pairs matching the list's search. `BASE/QUOTE`, where either part may be empty, matches each
 * side against its token. Anything else matches a pair when it matches either token by ticker,
 * name, covenant id or look-alike letters (`filterTokens`). An empty query gives every pair.
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

/**
 * The direction a column sorts in when its header is first chosen. Names and spreads go ascending
 * and every other figure descending.
 */
export const firstDir = (key: SortKey): SortDir => (key === 'market' || key === 'spread' ? 'asc' : 'desc');

/**
 * A header was chosen. The same column turns its direction over, another column starts in its own
 * first direction.
 */
export const nextSort = (cur: SortState, key: SortKey): SortState => (cur.key === key ? { key, dir: cur.dir === 'asc' ? 'desc' : 'asc' } : { key, dir: firstDir(key) });

/** `aria-sort` of a column header. */
export const ariaSort = (cur: SortState, key: SortKey): 'ascending' | 'descending' | 'none' => (cur.key !== key ? 'none' : cur.dir === 'asc' ? 'ascending' : 'descending');

const cmpValue = (a: Rational | number, b: Rational | number): number =>
  typeof a === 'number' && typeof b === 'number' ? a - b : typeof a === 'object' && typeof b === 'object' ? cmpRational(a, b) : 0;

/**
 * The entries in the chosen order. Missing figures go last in both directions, with rows that have
 * no orders after those. Ties keep the name order, then the key.
 */
export function sortEntries<E extends Pick<ListEntry, 'key' | 'name' | 'sort'> & { noOrders?: boolean }>(entries: readonly E[], s: SortState): E[] {
  const sign = s.dir === 'asc' ? 1 : -1;
  const byName = (a: E, b: E): number => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
  return [...entries].sort((a, b) => {
    if (s.key === 'market') return sign * byName(a, b);
    const x = a.sort[s.key];
    const y = b.sort[s.key];
    if (x === null || y === null) {
      if (x !== null || y !== null) return x === null ? 1 : -1;
      // Neither has the figure. A row with no orders at all goes after the rows that merely lack
      // it.
      return (a.noOrders ? 1 : 0) - (b.noOrders ? 1 : 0) || byName(a, b);
    }
    const c = cmpValue(x, y);
    return c !== 0 ? sign * Math.sign(c) : byName(a, b);
  });
}
