// View model of the pair page (`#/market/<base>/<quote>`): the token/token book of `GET /v1/pairs/{base}/{quote}/book` turned into display rows.
// Prices are exact rationals (kob/pair.ts): QUOTE per WHOLE BASE = price_num / price_den x 10^(baseDecimals - quoteDecimals), rendered as exact decimal
// text (asks rounded up, bids down: never better than the level); floats only for the depth bars. Three sources are kept apart and labelled:
// `direct` (resting KobPair orders), `entry` (if-done pair entries resting at their limit) and `route` (implied through the two KAS books).
// A pair order enforces only its own price (founder option 2): a filler may net opposite pair orders, route them through the KAS books or fill them
// from inventory, so a crossing of ANY sources is fillable: a backlog the matchers are catching up with. Pure and DOM-free.
import type { PairBookView, PairLevelSource, PairLevelView } from '../../data/indexer-types';
import { cmpRational, formatRational, ratToNumber, reduce, significantDp, wholePrice, type PairToken, type Rational } from '../../kob/pair';
import { columnFraction, fixedUnits } from '../kit/format';

export interface PairLevelRow {
  side: 'ask' | 'bid';
  source: PairLevelSource;
  /** QUOTE base units per BASE base unit (the indexer's exact rational) */
  unitPrice: Rational;
  /** QUOTE per whole BASE */
  price: Rational;
  priceText: string;
  /** BASE base units */
  amount: bigint;
  amountText: string;
  /** QUOTE (whole) the level is worth: amount x price */
  totalText: string;
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
  /** best ask - best bid as text (its absolute value when crossed), null when a side is empty */
  spreadText: string | null;
  /** fraction digits of the price column (fixed for every row, trailing zeros kept) */
  dp: number;
  /** fraction digits of the amount column: the finest amount shown, so every row has the same number of decimals */
  amountDp: number;
  empty: boolean;
  /**
   * The best bid is above the best ask. Pair orders net each other and fill through the KAS route or from inventory, so a crossing of any sources
   * (direct, entry, route) is fillable: a backlog the matchers are catching up with (each fill once the crossing pays its network fee).
   */
  crossed: boolean;
}

/** The line between the asks and the bids of the pair book: an i18n key and its parameters. */
export function spreadLine(m: Pick<PairBookModel, 'spreadText' | 'crossed'>, quote: string): { key: string; params: Record<string, string> } {
  if (m.spreadText === null) return { key: 'pair.book.oneSided', params: {} };
  return { key: m.crossed ? 'pair.book.crossed' : 'pair.book.spread', params: { spread: m.spreadText, quote } };
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
  const mk = (l: PairLevelView, side: 'ask' | 'bid'): Omit<PairLevelRow, 'priceText' | 'amountText' | 'bar' | 'totalText'> => {
    const unitPrice = toRat(l);
    return { side, source: l.source, unitPrice, price: wholePrice(unitPrice, base.decimals, quote.decimals), amount: BigInt(l.amount), orders: l.orders };
  };
  const srcOrder = (a: { source: PairLevelSource }, b: { source: PairLevelSource }): number => (SOURCE_RANK[a.source] ?? 3) - (SOURCE_RANK[b.source] ?? 3);
  const asks0 = view.asks.map((l) => mk(l, 'ask')).sort((a, b) => cmpRational(a.unitPrice, b.unitPrice) || srcOrder(a, b)).slice(0, depth);
  const bids0 = view.bids.map((l) => mk(l, 'bid')).sort((a, b) => cmpRational(b.unitPrice, a.unitPrice) || srcOrder(a, b)).slice(0, depth);
  const ref = asks0[0]?.price ?? bids0[0]?.price ?? null;
  const dp = ref ? significantDp(ref, 6) : 2;
  // the amount column: the finest amount of the shown levels fixes the decimals of every row
  const amountDp = columnFraction([...asks0, ...bids0].map((r) => r.amount), base.decimals);
  const max = [...asks0, ...bids0].reduce((m, r) => (r.amount > m ? r.amount : m), 0n);
  const finish = (r: Omit<PairLevelRow, 'priceText' | 'amountText' | 'bar' | 'totalText'>): PairLevelRow => {
    const total = reduce({ num: r.amount * r.unitPrice.num, den: r.unitPrice.den });
    return {
      ...r,
      priceText: formatRational(r.price, dp, r.side === 'ask' ? 'up' : 'down', dp, ','),
      amountText: fixedUnits(r.amount, base.decimals, amountDp),
      totalText: formatRational({ num: total.num, den: total.den * 10n ** BigInt(quote.decimals) }, Math.min(quote.decimals, dp + 2), 'down', Math.min(quote.decimals, dp + 2), ','),
      bar: max > 0n ? ratToNumber({ num: r.amount, den: max }) : 0,
    };
  };
  const asks = asks0.map(finish);
  const bids = bids0.map(finish);
  const bestAsk = asks[0] ?? null;
  const bestBid = bids[0] ?? null;
  let spreadText: string | null = null;
  let crossed = false;
  if (bestAsk && bestBid) {
    const d = reduce({ num: bestAsk.price.num * bestBid.price.den - bestBid.price.num * bestAsk.price.den, den: bestAsk.price.den * bestBid.price.den });
    crossed = d.num < 0n;
    spreadText = formatRational(d.num < 0n ? { num: -d.num, den: d.den } : d, dp, 'nearest', dp, ',');
  }
  return { asks, bids, bestAsk, bestBid, spreadText, dp, amountDp, empty: asks.length + bids.length === 0, crossed };
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
