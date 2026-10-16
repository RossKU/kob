// The numbers of the signer's sentence at a reader's precision (summary.ts): exact where the value is short, else a few significant digits,
// rounded in the direction that never makes the order look better than it is (an "at least" rounds down, an "at most" up). Pure.

export type Rounding = 'down' | 'up' | 'nearest';

const group = (int: string): string => int.replace(/\B(?=(\d{3})+(?!\d))/g, ',');

/**
 * `v` in units of 10^-`decimals` as text: the integer digits always, then at most enough fraction digits for `sig` significant digits in all
 * (no more than `decimals`), rounded as `mode` says; trailing zeros are cut unless `fixed` fraction digits are asked for. Grouped by thousands.
 */
export function decimalText(v: bigint, decimals: number, o: { sig?: number; fixed?: number; mode?: Rounding } = {}): string {
  const neg = v < 0n;
  let a = neg ? -v : v;
  const unit = 10n ** BigInt(decimals);
  const intDigits = (a / unit).toString().length;
  // the fraction digits kept: a fixed count, else what `sig` significant digits leave after the integer part (or after the leading zeros)
  let keep: number;
  if (o.fixed !== undefined) keep = Math.min(o.fixed, decimals);
  else {
    const sig = o.sig ?? 6;
    if (a >= unit) keep = Math.max(0, sig - intDigits);
    else {
      const frac = a.toString().padStart(decimals, '0');
      const zeros = frac.length - frac.replace(/^0+/, '').length;
      keep = zeros + sig;
    }
    keep = Math.min(keep, decimals);
  }
  const drop = 10n ** BigInt(decimals - keep);
  const rest = a % drop;
  a -= rest;
  const mode = o.mode ?? 'nearest';
  if (rest > 0n && (mode === 'up' || (mode === 'nearest' && rest * 2n >= drop))) a += drop;
  const int = (a / unit).toString();
  let frac = (a % unit).toString().padStart(decimals, '0').slice(0, keep);
  if (o.fixed === undefined) frac = frac.replace(/0+$/, '');
  return `${neg ? '−' : ''}${group(int)}${frac ? `.${frac}` : ''}`;
}

/** KAS (sompi) as a reader counts it: two decimals from 1 KAS on ("24.00"), two significant digits below ("0.0022"). */
export const kasWords = (sompi: bigint, mode: Rounding = 'nearest'): string => {
  const abs = sompi < 0n ? -sompi : sompi;
  return abs >= 100_000_000n ? decimalText(sompi, 8, { fixed: 2, mode }) : decimalText(sompi, 8, { sig: 2, mode });
};

/** A token amount (base units of `decimals`): exact when short, else six significant digits. */
export const tokenWords = (units: bigint, decimals: number, mode: Rounding = 'nearest'): string => decimalText(units, decimals, { sig: 6, mode });

/** Extra fraction digits of the ratios below (a price per KAS of a dear token is far below one base unit). */
const RATIO_DIGITS = 12;

/** A price per whole token (sompi per `scale` base units, a token of `decimals`) as KAS per whole token, six significant digits. */
export function kasPerTokenWords(price: bigint, scale: bigint, decimals: number, mode: Rounding = 'nearest'): string {
  // sompi per whole token, with RATIO_DIGITS more digits
  const num = price * 10n ** BigInt(decimals) * 10n ** BigInt(RATIO_DIGITS);
  return decimalText(div(num, scale, mode), 8 + RATIO_DIGITS, { sig: 6, mode });
}

/** The same price the other way round: whole tokens per KAS (an inverted market's price), six significant digits. */
export function tokensPerKasWords(price: bigint, scale: bigint, decimals: number, mode: Rounding = 'nearest'): string {
  if (price <= 0n) return '0';
  // base units per KAS = 1e8 * scale / price, with RATIO_DIGITS more digits
  const num = 100_000_000n * scale * 10n ** BigInt(RATIO_DIGITS);
  return decimalText(div(num, price, mode), decimals + RATIO_DIGITS, { sig: 6, mode });
}

/** `a / b` rounded as `mode` says (b > 0). */
export function div(a: bigint, b: bigint, mode: Rounding): bigint {
  const q = a / b;
  const r = a % b;
  if (r === 0n) return q;
  if (mode === 'up') return q + 1n;
  if (mode === 'nearest' && r * 2n >= b) return q + 1n;
  return q;
}
