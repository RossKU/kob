// View model of the pair page (`#/market/<base>/<quote>`): the token/token book of `GET /v1/pairs/{base}/{quote}/book` turned into display rows.
// Prices are exact rationals (kob/pair.ts): QUOTE per WHOLE BASE = price_num / price_den x 10^(baseDecimals - quoteDecimals), rendered as exact decimal
// text (asks rounded up, bids down: never better than the level); floats only for the depth bars. Three sources are kept apart and labelled:
// `direct` (resting KobPair orders), `entry` (if-done pair entries resting at their limit) and `route` (implied through the two KAS books).
// A pair order enforces only its own price (founder option 2): a filler may net opposite pair orders, route them through the KAS books or fill them
// from inventory, so a crossing of ANY sources is fillable. A crossed book is shown as it is (a negative spread). Pure and DOM-free.
import type { GenesisCheck } from '../../data/genesis-check';
import type { PairBookView, PairLevelSource, PairLevelView, PairSummaryView } from '../../data/indexer-types';
import { cmpRational, formatRational, ratToNumber, reduce, significantDp, wholePrice, type PairToken, type Rational } from '../../kob/pair';
import type { OpenTokenIssue } from '../../kob/open-token';
import type { TokenInfo } from '../../kob/registry';
import { columnFraction, fitAmountColumn, fitDecimalTexts, PRICE_DP_CAP, PRICE_SIGNIFICANT_DIGITS } from '../kit/format';
import { exactIfRounded } from './market-model';
import type { TokenRow } from './token-model';
import type { MarketToken } from './use-market-token';

/**
 * One token of a pair page: `ready` to trade; `loading` while an answer that decides it is on its way; `blocked` where the token page has no market
 * either (`reason`: the key of the sentence); `no-scale` while nothing tells its decimals (no order yet, none declared; never assumed); `unavailable`
 * with the key of what is missing; `unknown` where nothing identifies the token (`reason`: the key of the fact that is missing, if one is known).
 */
export type PairSide =
  | { state: 'ready'; info: TokenInfo }
  | { state: 'loading' }
  | { state: 'blocked'; row: TokenRow; reason: string | null }
  | { state: 'no-scale'; row: TokenRow }
  | { state: 'unavailable'; row: TokenRow; reason: string }
  | { state: 'unknown'; reason: string | null };

const PROGRAM_ISSUES: readonly (OpenTokenIssue | null)[] = ['program-unknown', 'program-ambiguous', 'program-unreviewed'];

/** The state of one token of a pair: the rules of the token page, in its order. `hasIndexer`: an open-list token's orders can be read at all. */
export function pairSideOf(m: Pick<MarketToken, 'row' | 'ticketInfo' | 'open' | 'pub' | 'resolving'>, hasIndexer: boolean): PairSide {
  const { row, open, pub } = m;
  if (!row) return m.resolving ? { state: 'loading' } : { state: 'unknown', reason: pub.reason ? `market.public.${pub.reason}` : null };
  if (row.source === 'registry' && !row.tradable) return { state: 'blocked', row, reason: row.reason ? `market.reason.${row.reason}` : null };
  if (m.ticketInfo && row.tradable) return { state: 'ready', info: m.ticketInfo };
  // a listed row whose program is unclear, or a row only kascov.io names, is decided by what the node confirms
  if (m.resolving) return { state: 'loading' };
  if (PROGRAM_ISSUES.includes(open.reason)) return { state: 'blocked', row, reason: `market.open.${open.reason}` };
  if (open.reason === 'no-scale') return { state: 'no-scale', row };
  if (row.source === 'kascov' && pub.reason) return { state: 'unavailable', row, reason: `market.public.${pub.reason}` };
  if (open.reason) return { state: 'unavailable', row, reason: `market.open.${open.reason}` };
  // the token's orders are being read, or are read in the render after this one
  return hasIndexer ? { state: 'loading' } : { state: 'unavailable', row, reason: 'market.open.book-unavailable' };
}

export interface PairLevelRow {
  side: 'ask' | 'bid';
  source: PairLevelSource;
  /** QUOTE base units per BASE base unit (the indexer's exact rational) */
  unitPrice: Rational;
  /** QUOTE per whole BASE */
  price: Rational;
  priceText: string;
  /** the price as the cell shows it (fewer decimals / K / M / B when the column is too wide; never a cut digit) and `priceText` when that is not exact */
  priceShown: string;
  priceTitle: string | null;
  /** BASE base units */
  amount: bigint;
  /** never a cut digit: fewer decimals or K / M / B when the column is too wide */
  amountText: string;
  /** the exact amount when `amountText` is rounded or compact */
  amountTitle: string | null;
  /** QUOTE (whole) the level is worth: amount x price; never a cut digit (fewer decimals or K / M / B when the column is too wide) */
  totalText: string;
  /** the whole total when `totalText` is rounded or compact */
  totalTitle: string | null;
  orders: number;
  /** 0..1 of the largest level on the page (depth bar width) */
  bar: number;
}

export interface PairBookModel {
  /** ascending (best = first) */
  asks: PairLevelRow[];
  /** descending (best = first) */
  bids: PairLevelRow[];
  bestAsk: PairLevelRow | null;
  bestBid: PairLevelRow | null;
  /** best ask - best bid as text, signed (negative when the book is crossed), null when a side is empty */
  spreadText: string | null;
  /** the spread relative to the mid price in percent, signed, 2 decimals; null when a side is empty */
  spreadPct: string | null;
  /** the mid price of the touches as text (same decimals as the price column); null when a side is empty */
  midText: string | null;
  /** fraction digits of the price column (fixed for every row, trailing zeros kept) */
  dp: number;
  /** fraction digits of the amount column: the finest amount shown, so every row has the same number of decimals */
  amountDp: number;
  empty: boolean;
}

/** The line between the asks and the bids of the pair book: an i18n key and its parameters. */
export function spreadLine(m: Pick<PairBookModel, 'spreadText' | 'spreadPct' | 'midText'>): { key: string; params: Record<string, string> } {
  if (m.spreadText === null || m.midText === null || m.spreadPct === null) return { key: 'pair.book.oneSided', params: {} };
  return { key: 'pair.book.spread', params: { mid: m.midText, spread: m.spreadText, pct: m.spreadPct } };
}

/**
 * One book for the two orientations of a pair. A pair order is oriented (its base and its quote are in its covenant), and the indexer lists the
 * orders of B/A in the B/A book only; the matcher nets both orientations alike (the order selling one token for the other is the counterparty,
 * however it is oriented). So the page shows A/B with the resting orders of B/A turned round: a B/A ask (it sells B for A) is an A/B bid, a B/A bid
 * (it sells A for B) an A/B ask; the price inverted exactly (B per A = the inverse of A per B, both per base unit) and the amount in A
 * (rounded down). The route levels of B/A are left out: they are the same KAS-book quotes as A/B's, turned round.
 */
export function mergeOrientations(ab: PairBookView, ba: PairBookView | null | undefined): PairBookView {
  if (!ba || ba.base !== ab.quote || ba.quote !== ab.base) return ab;
  const flip = (l: PairLevelView): PairLevelView | null => {
    const num = BigInt(l.price_num);
    const den = BigInt(l.price_den);
    if (num <= 0n || den <= 0n || l.source === 'route') return null;
    // l: `amount` base units of B at num/den base units of A per base unit of B
    const amountA = (BigInt(l.amount) * num) / den;
    if (amountA <= 0n) return null;
    return { source: l.source, price_num: den.toString(), price_den: num.toString(), amount: amountA.toString(), orders: l.orders, reversed: true };
  };
  const more = (ls: readonly PairLevelView[]) => ls.map(flip).filter((l): l is PairLevelView => l !== null);
  const byPrice = (asc: boolean) => (a: PairLevelView, b: PairLevelView) => {
    const c = cmpRational(toRat(a), toRat(b));
    return asc ? c : -c;
  };
  return {
    ...ab,
    asks: [...ab.asks, ...more(ba.bids)].sort(byPrice(true)),
    bids: [...ab.bids, ...more(ba.asks)].sort(byPrice(false)),
  };
}

/**
 * Whether a pair shown as BASE/QUOTE is to be turned round: its base is issued by a federated bridge (its genesis check found the bridge minter KOB
 * pins, kUSDT's BridgeMint) and its quote is not. Only a check that ran says so: an open or failed check turns nothing.
 */
export function bridgeQuoteFlip(base: GenesisCheck | null | undefined, quote: GenesisCheck | null | undefined): boolean {
  const bridged = (g: GenesisCheck | null | undefined) => g?.status === 'verified' && g.bridge === true;
  return bridged(base) && !bridged(quote);
}

/** Order of the sources at one price: resting pair orders first (direct, then if-done entries), the implied route last. */
const SOURCE_RANK: Record<PairLevelSource, number> = { direct: 0, entry: 1, route: 2 };

const toRat = (l: PairLevelView): Rational => reduce({ num: BigInt(l.price_num), den: BigInt(l.price_den) });

/**
 * Builds the book rows. `depth` rows per side (after sorting); asks ascending, bids descending, ties: direct, then entry, then route (a resting
 * order is a firmer quote than an implied one). A view for another pair (base / quote swapped or unknown) yields null.
 */
export function buildPairBook(view: PairBookView, base: PairToken, quote: PairToken, depth = 15): PairBookModel | null {
  if (view.base !== base.covenantId || view.quote !== quote.covenantId) return null;
  const mk = (l: PairLevelView, side: 'ask' | 'bid'): Omit<PairLevelRow, 'priceText' | 'priceShown' | 'priceTitle' | 'amountText' | 'amountTitle' | 'bar' | 'totalText' | 'totalTitle'> => {
    const unitPrice = toRat(l);
    return { side, source: l.source, unitPrice, price: wholePrice(unitPrice, base.decimals, quote.decimals), amount: BigInt(l.amount), orders: l.orders };
  };
  const srcOrder = (a: { source: PairLevelSource }, b: { source: PairLevelSource }): number => (SOURCE_RANK[a.source] ?? 3) - (SOURCE_RANK[b.source] ?? 3);
  const asks0 = view.asks.map((l) => mk(l, 'ask')).sort((a, b) => cmpRational(a.unitPrice, b.unitPrice) || srcOrder(a, b)).slice(0, depth);
  const bids0 = view.bids.map((l) => mk(l, 'bid')).sort((a, b) => cmpRational(b.unitPrice, a.unitPrice) || srcOrder(a, b)).slice(0, depth);
  const ref = asks0[0]?.price ?? bids0[0]?.price ?? null;
  // the kit's price display rule (about 5 significant digits, at most PRICE_DP_CAP fraction digits); the whole price is the cell's title
  const dp = ref ? Math.min(PRICE_DP_CAP, significantDp(ref, PRICE_SIGNIFICANT_DIGITS)) : 2;
  // the amount column: the finest amount of the shown levels fixes the decimals of every row
  const amountDp = columnFraction([...asks0, ...bids0].map((r) => r.amount), base.decimals);
  const max = [...asks0, ...bids0].reduce((m, r) => (r.amount > m ? r.amount : m), 0n);
  // never a cut digit: fewer decimals or K / M / B when the column is too wide (the exact amount is in the row's title)
  const shownAmounts = [...asks0, ...bids0];
  const fitted = fitAmountColumn(shownAmounts.map((x) => x.amount), base.decimals, { fraction: amountDp, unit: base.ticker });
  const amountCells = new Map(shownAmounts.map((r, i) => [r, fitted[i]!]));
  // the total column: what each level is worth in QUOTE, one number of decimals for every row, never a cut digit
  const totalOf = (r: { amount: bigint; unitPrice: Rational }): Rational => {
    const total = reduce({ num: r.amount * r.unitPrice.num, den: r.unitPrice.den });
    return { num: total.num, den: total.den * 10n ** BigInt(quote.decimals) };
  };
  const totalDp = Math.min(quote.decimals, dp + 2);
  const totalFitted = fitDecimalTexts(shownAmounts.map((r) => formatRational(totalOf(r), totalDp, 'down', totalDp, ',')), { unit: quote.ticker });
  const totalCells = new Map(shownAmounts.map((r, i) => [r, totalFitted[i]!]));
  const finish = (r: Omit<PairLevelRow, 'priceText' | 'priceShown' | 'priceTitle' | 'amountText' | 'amountTitle' | 'bar' | 'totalText' | 'totalTitle'>): Omit<PairLevelRow, 'priceShown' | 'priceTitle'> => ({
    ...r,
    priceText: formatRational(r.price, dp, r.side === 'ask' ? 'up' : 'down', dp, ','),
    amountText: amountCells.get(r)!.text,
    amountTitle: amountCells.get(r)!.exact,
    totalText: totalCells.get(r)!.text,
    totalTitle: totalCells.get(r)!.exact,
    bar: max > 0n ? ratToNumber({ num: r.amount, den: max }) : 0,
  });
  const asks1 = asks0.map(finish);
  const bids1 = bids0.map(finish);
  const prices = fitDecimalTexts([...asks1, ...bids1].map((r) => r.priceText));
  const titleOf = (r: Omit<PairLevelRow, 'priceShown' | 'priceTitle'>, cell: { exact: string | null }): string | null => cell.exact ?? exactIfRounded(r.priceText, formatRational(r.price, 18, 'nearest', 0, ','));
  const asks = asks1.map((r, i) => ({ ...r, priceShown: prices[i]!.text, priceTitle: titleOf(r, prices[i]!) }));
  const bids = bids1.map((r, i) => ({ ...r, priceShown: prices[asks1.length + i]!.text, priceTitle: titleOf(r, prices[asks1.length + i]!) }));
  const bestAsk = asks[0] ?? null;
  const bestBid = bids[0] ?? null;
  let spreadText: string | null = null;
  let spreadPct: string | null = null;
  let midText: string | null = null;
  if (bestAsk && bestBid) {
    // best ask - best bid, signed: a crossed book (a bid above an ask, the matchers a moment behind) has a negative spread
    const d = reduce({ num: bestAsk.price.num * bestBid.price.den - bestBid.price.num * bestAsk.price.den, den: bestAsk.price.den * bestBid.price.den });
    const signed = (r: Rational, places: number, group: string): string => (r.num < 0n ? '-' : '') + formatRational({ num: r.num < 0n ? -r.num : r.num, den: r.den }, places, 'nearest', places, group);
    const mid = reduce({ num: bestAsk.price.num * bestBid.price.den + bestBid.price.num * bestAsk.price.den, den: bestAsk.price.den * bestBid.price.den * 2n });
    spreadText = signed(d, dp, ',');
    midText = signed(mid, dp, ',');
    if (mid.num > 0n) spreadPct = signed(reduce({ num: d.num * 100n * mid.den, den: d.den * mid.num }), 2, '');
  }
  return { asks, bids, bestAsk, bestBid, spreadText, spreadPct, midText, dp, amountDp, empty: asks.length + bids.length === 0 };
}

/**
 * The prefill of the ticket from a click on a level: an ask means "buy BASE at this price", a bid "sell BASE at this price"; the price is a state
 * price (QUOTE base units per whole BASE of `scale(BASE)` base units), rounded so the order reaches the level: a buy at the ask rounded up, a
 * sell at the bid rounded down (the same rounding as the price column).
 */
export function pickOf(row: Pick<PairLevelRow, 'side' | 'unitPrice'>, base: PairToken): { side: 'buy' | 'sell'; price: bigint } {
  const n = row.unitPrice.num * base.scale;
  const d = row.unitPrice.den;
  return row.side === 'ask' ? { side: 'buy', price: (n + d - 1n) / d } : { side: 'sell', price: n / d };
}

/** The unordered identity of a pair: the same for BASE/QUOTE and QUOTE/BASE (the two orientations are one market). */
export const unorderedPairKey = (a: string, b: string): string => {
  const [x, y] = [a.toLowerCase(), b.toLowerCase()];
  return x < y ? `${x}/${y}` : `${y}/${x}`;
};

/**
 * The live pair-order counts of BASE/QUOTE with those of QUOTE/BASE turned round (`GET /v1/pairs` lists each orientation as a pair of its own): an ask of
 * QUOTE/BASE sells QUOTE for BASE, a bid of BASE/QUOTE, so asks and bids swap; conditionals add. Null when neither orientation is listed.
 */
export function mergePairSummaries(base: string, quote: string, summaries: readonly PairSummaryView[] | null | undefined): PairSummaryView | null {
  const [b, q] = [base.toLowerCase(), quote.toLowerCase()];
  const ab = summaries?.find((s) => s.base.toLowerCase() === b && s.quote.toLowerCase() === q) ?? null;
  const ba = summaries?.find((s) => s.base.toLowerCase() === q && s.quote.toLowerCase() === b) ?? null;
  if (!ab && !ba) return null;
  return {
    base: b,
    quote: q,
    direct_asks: (ab?.direct_asks ?? 0) + (ba?.direct_bids ?? 0),
    direct_bids: (ab?.direct_bids ?? 0) + (ba?.direct_asks ?? 0),
    entry_asks: (ab?.entry_asks ?? 0) + (ba?.entry_bids ?? 0),
    entry_bids: (ab?.entry_bids ?? 0) + (ba?.entry_asks ?? 0),
    conditionals: (ab?.conditionals ?? 0) + (ba?.conditionals ?? 0),
  };
}
