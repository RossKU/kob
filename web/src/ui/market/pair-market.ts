// View models of the market panels of a token pair page (`#/market/<base>/<quote>`): depth chart, chart candles, recent fills and 24 h figures, in
// PAIR units (QUOTE per whole BASE). Pure and DOM-free; exact rationals / bigint for every figure, floats only for chart coordinates.
//
// The founder price rule: prices come ONLY from KAS-book fills. A pair fill (a pair order filled through the KAS route, by netting against an opposite
// pair order or from inventory) is VOLUME only: it never makes a price, a candle, a last price or a 24 h high / low.
//
//   * Depth: the pair book's levels (direct pair orders, if-done entries, the route through the two KAS books) cumulated per side.
//   * Candles: the indexer's pair candles (derived from the two KAS series) or, without them, the client-side ratio of the two KAS candle series.
//   * Fills: `/v1/pairs/{base}/{quote}/fills`: amounts of BASE and QUOTE, counterparty and time; no price.
//   * 24 h: last, change, high and low from the KAS-derived figures; volumes and the fill count from the pair fills (`volume_24h`).
import type { PairBookView, PairCandlesView, PairFillsView, PairLevelView, PairRateView } from '../../data/indexer-types';
import { formatUnits } from '../../kob/units';
import { cmpRational, ratToNumber, reduce, wholePrice, type PairToken, type Rational } from '../../kob/pair';
import { columnFraction, fixedUnits } from '../kit/format';
import { compactText, unitsToNumber, toneOf, bpsPercentText, type DepthSeries, type StatsModel } from './market-model';
import type { RatioCandle } from './usd-model';
import { usdText } from './usd-model';

export const DAY_MS = 86_400_000;

// ------------------------------------------------------------------------------------------------ depth

/** The pair book as a depth series (QUOTE per whole BASE, cumulative whole BASE). Null for a view of another pair. All levels of the view are used. */
export function pairDepthSeries(view: PairBookView, base: PairToken, quote: PairToken): DepthSeries | null {
  if (view.base !== base.covenantId || view.quote !== quote.covenantId) return null;
  const parse = (ls: readonly PairLevelView[]) =>
    ls.flatMap((l) => {
      const unit = reduce({ num: BigInt(l.price_num), den: BigInt(l.price_den) });
      const amount = BigInt(l.amount);
      return unit.num > 0n && amount > 0n ? [{ unit, price: ratToNumber(wholePrice(unit, base.decimals, quote.decimals)), amount }] : [];
    });
  const asks = parse(view.asks).sort((a, b) => cmpRational(a.unit, b.unit));
  const bids = parse(view.bids).sort((a, b) => cmpRational(b.unit, a.unit));
  const pts = (ls: { price: number; amount: bigint }[]) => {
    let cum = 0n;
    return ls.map((l) => ({ price: l.price, cum: unitsToNumber((cum += l.amount), base.decimals) }));
  };
  const bestBid = bids[0]?.price ?? null;
  const bestAsk = asks[0]?.price ?? null;
  return {
    bids: pts(bids),
    asks: pts(asks),
    mid: bestBid !== null && bestAsk !== null ? (bestBid + bestAsk) / 2 : null,
    estimated: false,
    bidTotal: bids.reduce((s, l) => s + l.amount, 0n),
    askTotal: asks.reduce((s, l) => s + l.amount, 0n),
  };
}

// ------------------------------------------------------------------------------------------------ pair candles (indexer, derived from the KAS series)

const decBig = (v: unknown): bigint | null => (typeof v === 'string' && /^\d+$/.test(v) ? BigInt(v) : null);

/**
 * The indexer's pair candles (`/v1/pairs/{base}/{quote}/candles`, derived from the two KAS series: a pair fill never makes a price) as the ratio
 * candles of the chart and the stats: rates in QUOTE per whole BASE (exact), volume = the PAIR volume of the bucket (pair-order fills, whole BASE
 * and whole QUOTE, their count), `filled` when neither token traded in KAS in the bucket. Null for a view of another pair or without a basis.
 */
export function pairRatioCandles(view: PairCandlesView, base: PairToken, quote: PairToken): RatioCandle[] | null {
  if (view.base !== base.covenantId || view.quote !== quote.covenantId) return null;
  const basis = decBig(view.price_basis) ?? 0n;
  if (basis <= 0n) return null;
  // B base units per `basis` base units of A -> whole B per whole A
  const whole = (r: PairRateView): Rational => reduce({ num: BigInt(r.num) * 10n ** BigInt(base.decimals), den: BigInt(r.den) * basis * 10n ** BigInt(quote.decimals) });
  return view.items
    .map((c) => ({
      t: c.t, o: whole(c.o), h: whole(c.h), l: whole(c.l), c: whole(c.c),
      volume: unitsToNumber(decBig(c.pair_volume_a) ?? 0n, base.decimals),
      quoteVolume: unitsToNumber(decBig(c.pair_volume_b) ?? 0n, quote.decimals),
      trades: c.pair_fills,
      filled: !c.a_traded && !c.b_traded,
    }))
    .filter((c) => c.o.num > 0n && c.h.num > 0n && c.l.num > 0n && c.c.num > 0n)
    .sort((a, b) => a.t - b.t);
}

// ------------------------------------------------------------------------------------------------ fills of a pair (volume only)

export interface PairFill {
  /** the indexer's fill id (unique) */
  id: number;
  txid: string;
  /** unix ms; null when the indexer does not know it */
  timeMs: number | null;
  daa: number;
  /** the taker's side on BASE: `buy` when a pair order selling BASE (an ask) was filled, `sell` when one buying BASE (a bid) was */
  side: 'buy' | 'sell';
  /** BASE base units filled */
  baseUnits: bigint;
  /** QUOTE base units the maker received (an ask) or paid (a bid); null when the indexer does not report it */
  quoteUnits: bigint | null;
  /** how it was filled: `route` (through the KAS books), `netting` (against an opposite pair order), `inventory` (the filler's tokens) */
  counterparty: string;
  settled: boolean;
}

/**
 * The fills of the pair BASE/QUOTE (`/v1/pairs/{base}/{quote}/fills`), newest first. VOLUME only: no price is read from them (the founder price
 * rule). A view of another pair yields none; malformed rows and duplicates are dropped.
 */
export function pairFillsOf(view: PairFillsView | null | undefined, base: PairToken, quote: PairToken): PairFill[] {
  if (!view || view.base !== base.covenantId || view.quote !== quote.covenantId) return [];
  const seen = new Set<number>();
  const out: PairFill[] = [];
  for (const f of view.items) {
    const a = decBig(f.amount_a);
    if (seen.has(f.id) || a === null || a <= 0n) continue;
    seen.add(f.id);
    out.push({
      id: f.id, txid: f.txid, timeMs: typeof f.ts === 'number' && f.ts > 0 ? f.ts : null, daa: f.daa, side: f.side === 'ask' ? 'buy' : 'sell', baseUnits: a,
      quoteUnits: decBig(f.amount_b), counterparty: typeof f.counterparty === 'string' ? f.counterparty : 'inventory', settled: !!f.settled,
    });
  }
  return out.sort((x, y) => y.daa - x.daa || y.id - x.id);
}

/** The BASE and QUOTE amount texts of every row with ONE number of decimals per column (the finest amount shown), trailing zeros kept. */
export function pairTapeColumns(fills: readonly PairFill[], base: PairToken, quote: PairToken): { base: string; quote: string }[] {
  const fa = columnFraction(fills.map((f) => f.baseUnits), base.decimals);
  const fb = columnFraction(fills.flatMap((f) => (f.quoteUnits === null ? [] : [f.quoteUnits])), quote.decimals);
  return fills.map((f) => ({ base: fixedUnits(f.baseUnits, base.decimals, fa), quote: f.quoteUnits === null ? '—' : fixedUnits(f.quoteUnits, quote.decimals, fb) }));
}

// ------------------------------------------------------------------------------------------------ 24 h figures

/** Where the volume tiles come from: the indexer's 24 h pair volume, the pair volume of the pair candles, or nothing. */
export type PairStatsSource = 'fills' | 'candles' | 'none';

export interface PairStatsOut {
  model: StatsModel;
  source: PairStatsSource;
  /** the number of pair fills behind the volume tiles */
  fills: number;
}

/** The 24 h pair volume of `/v1/pairs/{base}/{quote}/fills` (`volume_24h`): base units of BASE and QUOTE, and the fill count. */
export interface PairVolume { a: bigint; b: bigint; fills: number }

export const pairVolumeOf = (view: PairFillsView | null | undefined, base: PairToken, quote: PairToken): PairVolume | null =>
  view && view.base === base.covenantId && view.quote === quote.covenantId
    ? { a: decBig(view.volume_24h.amount_a) ?? 0n, b: decBig(view.volume_24h.amount_b) ?? 0n, fills: view.volume_24h.fills }
    : null;

/**
 * The stats strip of a pair. PRICES only from the KAS series: `last` / `changeBps` from the two tokens' 24 h KAS stats (usdStats), high / low from the
 * traded buckets of the candles in the 24 h before `refMs`. VOLUMES only from pair fills: the indexer's `volume_24h` (`volume`), else the pair volume
 * of the indexer's pair candles in the window (`pairCandles`), else none (the client-side ratio candles carry KAS volume, never pair volume).
 */
export function pairStatsModel(a: {
  last: Rational | null;
  changeBps: number | null;
  dp: number;
  candles: readonly RatioCandle[];
  /** `candles` are the indexer's pair candles (their volume is the pair volume); false: the client-side ratio of the two KAS series */
  pairCandles: boolean;
  /** the candle bucket length, ms */
  intervalMs: number;
  /** the newest chain time, unix ms (the end of the 24 h window) */
  refMs: number;
  volume: PairVolume | null;
  base: PairToken;
  quote: PairToken;
}): PairStatsOut {
  const start = a.refMs - DAY_MS;
  const bucket = a.candles.filter((c) => c.t + a.intervalMs > start && c.t <= a.refMs);
  let high: Rational | null = null;
  let low: Rational | null = null;
  for (const c of bucket) {
    if (c.filled) continue;
    if (high === null || cmpRational(c.h, high) > 0) high = c.h;
    if (low === null || cmpRational(c.l, low) < 0) low = c.l;
  }
  const m: StatsModel = {
    last: a.last ? usdText(a.last, a.dp) : null,
    lastSide: null,
    change: a.changeBps === null ? null : bpsPercentText(a.changeBps),
    changeTone: toneOf(a.changeBps),
    high: high ? usdText(high, a.dp) : null,
    low: low ? usdText(low, a.dp) : null,
    volume: null,
    volumeExact: null,
    quoteVolume: null,
    quoteVolumeExact: null,
    trades: null,
    spread: null,
    dp: a.dp,
  };
  if (a.volume) {
    m.volume = compactText(unitsToNumber(a.volume.a, a.base.decimals));
    m.volumeExact = formatUnits(a.volume.a, a.base.decimals, { group: ',' });
    m.quoteVolume = compactText(unitsToNumber(a.volume.b, a.quote.decimals));
    m.quoteVolumeExact = formatUnits(a.volume.b, a.quote.decimals, { group: ',' });
    m.trades = a.volume.fills;
    return { model: m, source: 'fills', fills: a.volume.fills };
  }
  if (a.pairCandles && bucket.length > 0) {
    const n = bucket.reduce((s, c) => s + c.trades, 0);
    m.volume = compactText(bucket.reduce((s, c) => s + c.volume, 0));
    m.quoteVolume = compactText(bucket.reduce((s, c) => s + c.quoteVolume, 0));
    m.trades = n;
    return { model: m, source: 'candles', fills: n };
  }
  return { model: m, source: 'none', fills: 0 };
}
