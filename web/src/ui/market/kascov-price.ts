// The one small third-party line of a market page: kascov.io lists a price for the token (data/kascov.ts `priceKas`, optional: kascov does not publish one yet) and it differs from KOB's own
// reference price by more than 20%. Display only: KOB's prices come from its own order book and trades, and this price feeds no warning, no ordering, no check and no figure. Pure.
import { t } from '../../i18n';
import { formatRational, type Rational } from '../../kob/pair';
import { basisOf, bigOrNull } from './market-model';
import { kasPerToken } from './orientation';
import { displayPriceDp } from '../kit/format';
import type { StatsView } from '../../data/indexer-types';

/** The note shows only when kascov's price is further than this (basis points of KOB's reference) from KOB's own. */
export const KASCOV_PRICE_DIVERGENCE_BPS = 2_000n;

const TEN18 = 10n ** 18n;

/** A positive finite number as an exact-enough rational (18 fraction digits); null when it is not one the note may use. */
function rationalOf(n: number): Rational | null {
  if (!Number.isFinite(n) || n <= 0 || n > 1e15) return null;
  const [int, frac = ''] = n.toFixed(18).split('.');
  const num = BigInt(int! + frac.padEnd(18, '0'));
  return num > 0n ? { num, den: TEN18 } : null;
}

/** KOB's own reference of a token's market in KAS per whole token: the last trade, else the mid of the book; null when it has neither. */
export function ownReferenceKas(v: Pick<StatsView, 'price_basis' | 'last' | 'mid'> | null | undefined, decimals: number): Rational | null {
  const basis = v ? basisOf(v.price_basis) : null;
  const p = v ? (bigOrNull(v.last) ?? bigOrNull(v.mid)) : null;
  return basis && p !== null && p > 0n ? kasPerToken(p, decimals, basis) : null;
}

export interface KascovPriceNote {
  /** `0.012` (KAS per token, about five significant digits, trailing zeros trimmed) */
  price: string;
  /** signed whole percent of kascov's price against KOB's reference: `-35%`, `+150%` */
  percent: string;
  /** the line: "kascov lists 0.012 KAS (-35%)" */
  text: string;
  /** what it is: an outside reference, not a KOB price */
  title: string;
}

/** The note for kascov's `listed` price against KOB's own `reference` (KAS per whole token), or null: nothing listed, nothing to compare, or the two are within 20%. */
export function kascovPriceNote(listed: number | null | undefined, reference: Rational | null): KascovPriceNote | null {
  if (typeof listed !== 'number' || reference === null || reference.num <= 0n) return null;
  const k = rationalOf(listed);
  if (!k) return null;
  // |k - r| / r in basis points, exact: (kn * rd - rn * kd) / (rn * kd)
  const diff = k.num * reference.den - reference.num * k.den;
  const abs = diff < 0n ? -diff : diff;
  const bps = (abs * 10_000n) / (reference.num * k.den);
  if (bps <= KASCOV_PRICE_DIVERGENCE_BPS) return null;
  const pct = ((bps + 50n) / 100n).toString(); // rounded: a float price such as 0.0125 is a hair below 0.0125
  const percent = `${diff < 0n ? '-' : '+'}${pct}%`;
  const dp = Math.max(0, displayPriceDp(k.num * 10n ** 8n / k.den));
  const shown = formatRational(k, dp, 'nearest', 0).replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '');
  // below the last displayed decimal: say so, never a bare 0
  const price = shown === '0' ? `<${formatRational({ num: 1n, den: 10n ** BigInt(dp) }, dp, 'nearest', dp)}` : shown;
  return { price, percent, text: t('market.kascov.priceNote', { price, percent }), title: t('market.kascov.priceNoteTitle') };
}
