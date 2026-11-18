// View model of the pair page (`#/market/<base>/<quote>`). It turns the token/token book of
// `GET /v1/pairs/{base}/{quote}/book` into display rows.
//
// Prices are exact rationals (kob/pair.ts). QUOTE per whole BASE is
// `price_num / price_den x 10^(baseDecimals - quoteDecimals)`. It is rendered as exact decimal
// text, with asks rounded up and bids down, so it is never better than the level. Floats are used
// only for the depth bars.
//
// Three sources are kept apart and labelled:
// - `direct`: resting KobPair orders.
// - `entry`: if-done pair entries resting at their limit.
// - `route`: implied through the two KAS books.
//
// A pair order enforces only its own price. A filler may net opposite pair orders, route them
// through the KAS books or fill them from inventory, so a crossing of any sources is fillable. A
// crossed book is shown as it is, with a negative spread. The module is pure and DOM-free.
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
 * One token of a pair page.
 * - `ready`: the token can trade.
 * - `loading`: an answer that decides it is on its way.
 * - `blocked`: the token page has no market either. `reason` is the key of the sentence.
 * - `no-scale`: nothing tells its decimals, because there is no order yet and none declared. They
 *   are never assumed.
 * - `unavailable`: carries the key of what is missing.
 * - `unknown`: nothing identifies the token. `reason` is the key of the fact that is missing, if
 *   one is known.
 */
export type PairSide =
  | { state: 'ready'; info: TokenInfo }
  | { state: 'loading' }
  | { state: 'blocked'; row: TokenRow; reason: string | null }
  | { state: 'no-scale'; row: TokenRow }
  | { state: 'unavailable'; row: TokenRow; reason: string }
  | { state: 'unknown'; reason: string | null };

const PROGRAM_ISSUES: readonly (OpenTokenIssue | null)[] = ['program-unknown', 'program-ambiguous', 'program-unreviewed'];

/**
 * The state of one token of a pair, by the rules of the token page in its order. `hasIndexer` says
 * whether an open-list token's orders can be read at all.
 */
export function pairSideOf(m: Pick<MarketToken, 'row' | 'ticketInfo' | 'open' | 'pub' | 'resolving'>, hasIndexer: boolean): PairSide {
  const { row, open, pub } = m;
  if (!row) return m.resolving ? { state: 'loading' } : { state: 'unknown', reason: pub.reason ? `market.public.${pub.reason}` : null };
  if (row.source === 'registry' && !row.tradable) return { state: 'blocked', row, reason: row.reason ? `market.reason.${row.reason}` : null };
  if (m.ticketInfo && row.tradable) return { state: 'ready', info: m.ticketInfo };
  // A listed row whose program is unclear, or a row only kascov.io names, is decided by what the
  // node confirms.
  if (m.resolving) return { state: 'loading' };
  if (PROGRAM_ISSUES.includes(open.reason)) return { state: 'blocked', row, reason: `market.open.${open.reason}` };
  if (open.reason === 'no-scale') return { state: 'no-scale', row };
  if (row.source === 'kascov' && pub.reason) return { state: 'unavailable', row, reason: `market.public.${pub.reason}` };
  if (open.reason) return { state: 'unavailable', row, reason: `market.open.${open.reason}` };
  // The token's orders are being read, or are read in the render after this one.
  return hasIndexer ? { state: 'loading' } : { state: 'unavailable', row, reason: 'market.open.book-unavailable' };
}

export interface PairLevelRow {
  side: 'ask' | 'bid';
  source: PairLevelSource;
  /** QUOTE base units per BASE base unit, the indexer's exact rational. */
  unitPrice: Rational;
  /** QUOTE per whole BASE. */
  price: Rational;
  priceText: string;
  /**
   * The price as the cell shows it. It has fewer decimals or K, M, B when the column is too wide,
   * and never a cut digit. `priceText` is the exact one when this is not.
   */
  priceShown: string;
  priceTitle: string | null;
  /** BASE base units. */
  amount: bigint;
  /** Never a cut digit. It has fewer decimals or K, M, B when the column is too wide. */
  amountText: string;
  /** The exact amount when `amountText` is rounded or compact. */
  amountTitle: string | null;
  /**
   * The QUOTE (whole) the level is worth, `amount x price`. Never a cut digit. It has fewer
   * decimals or K, M, B when the column is too wide.
   */
  totalText: string;
  /** The whole total when `totalText` is rounded or compact. */
  totalTitle: string | null;
  orders: number;
  /** 0 to 1 of the largest level on the page, the depth bar width. */
  bar: number;
}

export interface PairBookModel {
  /** Ascending, so the best is first. */
  asks: PairLevelRow[];
  /** Descending, so the best is first. */
  bids: PairLevelRow[];
  bestAsk: PairLevelRow | null;
  bestBid: PairLevelRow | null;
  /**
   * Best ask minus best bid as signed text. Negative when the book is crossed. Null when a side is
   * empty.
   */
  spreadText: string | null;
  /**
   * The spread relative to the mid price in percent, signed, with 2 decimals. Null when a side is
   * empty.
   */
  spreadPct: string | null;
  /**
   * The mid price of the touches as text, with the same decimals as the price column. Null when a
   * side is empty.
   */
  midText: string | null;
  /** Fraction digits of the price column, fixed for every row with trailing zeros kept. */
  dp: number;
  /**
   * Fraction digits of the amount column. It is the finest amount shown, so every row has the same
   * number of decimals.
   */
  amountDp: number;
  empty: boolean;
}

/** The line between the asks and the bids of the pair book, as an i18n key and its parameters. */
export function spreadLine(m: Pick<PairBookModel, 'spreadText' | 'spreadPct' | 'midText'>): { key: string; params: Record<string, string> } {
  if (m.spreadText === null || m.midText === null || m.spreadPct === null) return { key: 'pair.book.oneSided', params: {} };
  return { key: 'pair.book.spread', params: { mid: m.midText, spread: m.spreadText, pct: m.spreadPct } };
}

/**
 * One book for the two orientations of a pair. A pair order is oriented, because its base and its
 * quote are in its covenant. The indexer lists the orders of B/A in the B/A book only. The matcher
 * nets both orientations alike, since the order selling one token for the other is the
 * counterparty however it is oriented.
 *
 * So the page shows A/B with the resting orders of B/A turned round. A B/A ask sells B for A and
 * is an A/B bid. A B/A bid sells A for B and is an A/B ask. The price is inverted exactly, because
 * B per A is the inverse of A per B, both per base unit. The amount is in A, rounded down. The
 * route levels of B/A are left out. They are the same KAS-book quotes as A/B's, turned round.
 */
export function mergeOrientations(ab: PairBookView, ba: PairBookView | null | undefined): PairBookView {
  if (!ba || ba.base !== ab.quote || ba.quote !== ab.base) return ab;
  const flip = (l: PairLevelView): PairLevelView | null => {
    const num = BigInt(l.price_num);
    const den = BigInt(l.price_den);
    if (num <= 0n || den <= 0n || l.source === 'route') return null;
    // `l` is `amount` base units of B at num/den base units of A per base unit of B.
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
 * Whether a pair shown as BASE/QUOTE is to be turned round. That is the case when its base is
 * issued by a federated bridge and its quote is not. The genesis check found the bridge minter KOB
 * pins, kUSDT's BridgeMint. Only a check that ran says so. An open or failed check turns nothing.
 */
export function bridgeQuoteFlip(base: GenesisCheck | null | undefined, quote: GenesisCheck | null | undefined): boolean {
  const bridged = (g: GenesisCheck | null | undefined) => g?.status === 'verified' && g.bridge === true;
  return bridged(base) && !bridged(quote);
}

/**
 * Order of the sources at one price. Resting pair orders come first, direct and then if-done
 * entries, and the implied route last.
 */
const SOURCE_RANK: Record<PairLevelSource, number> = { direct: 0, entry: 1, route: 2 };

const toRat = (l: PairLevelView): Rational => reduce({ num: BigInt(l.price_num), den: BigInt(l.price_den) });

/**
 * Builds the book rows, with `depth` rows per side after sorting. Asks ascend and bids descend.
 * Ties go direct, then entry, then route, because a resting order is a firmer quote than an
 * implied one. A view for another pair, with base and quote swapped or unknown, yields null.
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
  // The kit's price display rule is about 5 significant digits and at most PRICE_DP_CAP fraction
  // digits. The whole price is the cell's title.
  const dp = ref ? Math.min(PRICE_DP_CAP, significantDp(ref, PRICE_SIGNIFICANT_DIGITS)) : 2;
  // The amount column. The finest amount of the shown levels fixes the decimals of every row.
  const amountDp = columnFraction([...asks0, ...bids0].map((r) => r.amount), base.decimals);
  const max = [...asks0, ...bids0].reduce((m, r) => (r.amount > m ? r.amount : m), 0n);
  // Never a cut digit. The column shows fewer decimals or K, M, B when it is too wide, and the
  // exact amount is in the row's title.
  const shownAmounts = [...asks0, ...bids0];
  const fitted = fitAmountColumn(shownAmounts.map((x) => x.amount), base.decimals, { fraction: amountDp, unit: base.ticker });
  const amountCells = new Map(shownAmounts.map((r, i) => [r, fitted[i]!]));
  // The total column is what each level is worth in QUOTE. Every row has one number of decimals,
  // and a digit is never cut.
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
    // Best ask minus best bid, signed. A crossed book has a negative spread. That is a bid above an
    // ask, with the matchers a moment behind.
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
 * The pick of a click on a level. An ask means "buy BASE at this price" and a bid "sell BASE at
 * this price". The ticket takes only the price and keeps its side. The price is a state price,
 * QUOTE base units per whole BASE of `scale(BASE)` base units. It is rounded so the order reaches
 * the level. A buy at the ask rounds up and a sell at the bid rounds down, like the price column.
 */
export function pickOf(row: Pick<PairLevelRow, 'side' | 'unitPrice'>, base: PairToken): { side: 'buy' | 'sell'; price: bigint } {
  const n = row.unitPrice.num * base.scale;
  const d = row.unitPrice.den;
  return row.side === 'ask' ? { side: 'buy', price: (n + d - 1n) / d } : { side: 'sell', price: n / d };
}

/**
 * The unordered identity of a pair. It is the same for BASE/QUOTE and QUOTE/BASE, because the two
 * orientations are one market.
 */
export const unorderedPairKey = (a: string, b: string): string => {
  const [x, y] = [a.toLowerCase(), b.toLowerCase()];
  return x < y ? `${x}/${y}` : `${y}/${x}`;
};

/**
 * The live pair-order counts of BASE/QUOTE, with those of QUOTE/BASE turned round. `GET /v1/pairs`
 * lists each orientation as a pair of its own. An ask of QUOTE/BASE sells QUOTE for BASE, as a bid
 * of BASE/QUOTE does, so asks and bids swap. Conditionals add. The result is null when neither
 * orientation is listed.
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
