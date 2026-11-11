// The chart's optional third-party line: kascov.io's price of the token (data/kascov-candles.ts) in the units of KOB's own chart, laid onto KOB's own bars. Pure.
//
// kascov's close is `quote_sompi / base_amount` sompi per base unit. KOB's chart shows KAS per WHOLE token with the market's decimals (kasPerToken, the same arithmetic as the
// candles), so the line uses the very same conversion, exact, and only then a float for the canvas. A flipped market (KAS/TOKEN) shows the reciprocal. A token pair
// (BASE over QUOTE) shows BASE's KAS price over QUOTE's KAS price at every bucket both legs have; a bucket one leg lacks is skipped.
//
// The line is laid onto the bars KOB already draws: for each bar, the close of the latest kascov bucket that starts before the bar's end (carried across a quiet stretch, as KOB
// carries its own close). That keeps the chart's time axis exactly KOB's (an extra point would add a position to it and shift the legend and the drawings), and a coarser kascov
// bucket (15 m under 1m and 5m bars) draws as a step. Bars before kascov's first candle have no point.
import type { KascovSpot } from '../../data/kascov';
import type { KascovCandle } from '../../data/kascov-candles';
import { t } from '../../i18n';
import { formatRational, ratToNumber, significantDp, type Rational } from '../../kob/pair';
import { PRICE_DP_CAP, PRICE_SIGNIFICANT_DIGITS } from '../kit/format';
import { invert, kasPerToken } from './orientation';

export interface OverlayPoint { t: number; price: Rational }
export interface OverlayLeg { candles: readonly KascovCandle[]; decimals: number }

/** KAS per whole token of a candle's close (exact). */
export const kasPerWholeToken = (c: KascovCandle, decimals: number): Rational => kasPerToken(c.quoteSompi, decimals, c.baseAmount);

/** A token's kascov line in KAS per token, or (`inverted`, a flipped market) tokens per KAS. Oldest first. */
export function tokenPoints(leg: OverlayLeg, inverted: boolean): OverlayPoint[] {
  const out: OverlayPoint[] = [];
  for (const c of leg.candles) {
    const p = kasPerWholeToken(c, leg.decimals);
    const price = inverted ? invert(p) : p;
    if (price) out.push({ t: c.t, price });
  }
  return out;
}

/** BASE over QUOTE: BASE's KAS price divided by QUOTE's, at every bucket time both legs have. */
export function pairPoints(base: OverlayLeg, quote: OverlayLeg): OverlayPoint[] {
  const q = new Map<number, Rational>();
  for (const c of quote.candles) q.set(c.t, kasPerWholeToken(c, quote.decimals));
  const out: OverlayPoint[] = [];
  for (const c of base.candles) {
    const d = q.get(c.t);
    if (!d || d.num <= 0n) continue;
    const b = kasPerWholeToken(c, base.decimals);
    out.push({ t: c.t, price: { num: b.num * d.den, den: b.den * d.num } });
  }
  return out;
}

/**
 * The line on KOB's bars: `barTimes` are the bars' open times in seconds (ascending, as the chart has them), `intervalMs` the bars' length. Per bar the price of the latest point
 * that starts before the bar's end, null while there is none.
 */
export function laidOnBars(points: readonly OverlayPoint[], barTimes: readonly number[], intervalMs: number): (Rational | null)[] {
  const out: (Rational | null)[] = [];
  let j = -1;
  for (const time of barTimes) {
    const end = time * 1000 + intervalMs;
    while (j + 1 < points.length && points[j + 1]!.t < end) j++;
    out.push(j >= 0 ? points[j]!.price : null);
  }
  return out;
}

/** The chart's data of the line: a point per bar that has a price (a bar with none is left out: the line starts where kascov's history does). */
export function lineData(values: readonly (Rational | null)[], barTimes: readonly number[]): { time: number; value: number }[] {
  const out: { time: number; value: number }[] = [];
  values.forEach((r, i) => {
    if (!r) return;
    const value = ratToNumber(r);
    if (Number.isFinite(value) && value > 0) out.push({ time: barTimes[i]!, value });
  });
  return out;
}

/** About 5 significant digits, at most 6 decimals, trailing zeros trimmed, never a bare zero: `0.01625`, `61.5`, `<0.000001`. */
export function overlayPriceText(r: Rational): string {
  const dp = significantDp(r, PRICE_SIGNIFICANT_DIGITS, 0, PRICE_DP_CAP);
  const shown = formatRational(r, dp, 'nearest', 0, ',');
  return shown === '0' ? `<${formatRational({ num: 1n, den: 10n ** BigInt(PRICE_DP_CAP) }, PRICE_DP_CAP, 'nearest', PRICE_DP_CAP)}` : shown;
}

// ------------------------------------------------------------------------------------------------ the market list's tooltip line

/** KAS per whole token of kascov's spot (`num` sompi per `den` base units), with the market's decimals (exact). */
export const spotPrice = (spot: KascovSpot, decimals: number): Rational => kasPerToken(spot.num, decimals, spot.den);

/** BASE over QUOTE from both tokens' spots: BASE's KAS price over QUOTE's (exact). */
export function spotPairPrice(base: KascovSpot, baseDecimals: number, quote: KascovSpot, quoteDecimals: number): Rational {
  const b = spotPrice(base, baseDecimals);
  const q = spotPrice(quote, quoteDecimals);
  return { num: b.num * q.den, den: b.den * q.num };
}

/**
 * The line kascov adds to a price cell's tooltip in the market list: `kascov: 0.01625 KAS (-35%)`, the percent being kascov's price against KOB's own price shown in that cell
 * (signed, whole percent); without a KOB price, `kascov: 0.01625 KAS`. Null without a kascov price. Display only, third party: nothing sorts or warns by it.
 */
export function kascovTitleLine(kascov: Rational | null, own: Rational | null, unit: string): string | null {
  if (!kascov || kascov.num <= 0n || kascov.den <= 0n) return null;
  const price = overlayPriceText(kascov);
  if (!own || own.num <= 0n) return t('market.list.kascovLine', { price, unit });
  const diff = kascov.num * own.den - own.num * kascov.den;
  const bps = ((diff < 0n ? -diff : diff) * 10_000n) / (own.num * kascov.den);
  const pct = (bps + 50n) / 100n;
  return t('market.list.kascovLinePct', { price, unit, percent: pct === 0n ? '0%' : `${diff < 0n ? '-' : '+'}${pct}%` });
}

/** `cell` with the kascov line as the last line of its tooltip (the tooltip it has stays as it is). */
export function withKascovLine<C extends { text: string | null; title: string | null }>(cell: C, line: string | null): C {
  return line === null ? cell : { ...cell, title: cell.title ? `${cell.title}\n${line}` : line };
}
