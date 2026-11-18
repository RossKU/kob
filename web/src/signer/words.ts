// The numbers of the signer's sentences at a reader's precision (summary.ts). Pure.
//
// A value is exact where it is short, else a few significant digits. It rounds in the direction
// that never makes the order look better than it is, so an "at least" rounds down and an "at most"
// rounds up.

/** The direction a figure rounds in. */
export type Rounding = 'down' | 'up' | 'nearest';

const group = (int: string): string => int.replace(/\B(?=(\d{3})+(?!\d))/g, ',');

/**
 * Formats `v` in units of 10^-`decimals` as text, grouped by thousands.
 *
 * It always shows the integer digits. Then it shows at most enough fraction digits for `sig`
 * significant digits in all, and no more than `decimals`, rounded as `mode` says. Trailing zeros
 * are cut unless `fixed` fraction digits are asked for.
 */
export function decimalText(v: bigint, decimals: number, o: { sig?: number; fixed?: number; mode?: Rounding } = {}): string {
  const neg = v < 0n;
  let a = neg ? -v : v;
  const unit = 10n ** BigInt(decimals);
  const intDigits = (a / unit).toString().length;
  // The fraction digits kept. That is a fixed count, else what `sig` significant digits leave
  // after the integer part, or after the leading zeros.
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

/**
 * Formats sompi as KAS the way a reader counts it. It shows two decimals from 1 KAS on ("24.00")
 * and two significant digits below ("0.0022").
 */
export const kasWords = (sompi: bigint, mode: Rounding = 'nearest'): string => {
  const abs = sompi < 0n ? -sompi : sompi;
  return abs >= 100_000_000n ? decimalText(sompi, 8, { fixed: 2, mode }) : decimalText(sompi, 8, { sig: 2, mode });
};

/** Formats a token amount in base units of `decimals`, exact when short, else to six digits. */
export const tokenWords = (units: bigint, decimals: number, mode: Rounding = 'nearest'): string => decimalText(units, decimals, { sig: 6, mode });

/**
 * Extra fraction digits of the ratios below. A price per KAS of a dear token is far below one base
 * unit.
 */
const RATIO_DIGITS = 12;

/**
 * Formats a price as KAS per whole token, to six significant digits. The price is sompi per `scale`
 * base units of a token of `decimals`.
 */
export function kasPerTokenWords(price: bigint, scale: bigint, decimals: number, mode: Rounding = 'nearest'): string {
  // Sompi per whole token, with RATIO_DIGITS more digits.
  const num = price * 10n ** BigInt(decimals) * 10n ** BigInt(RATIO_DIGITS);
  return decimalText(div(num, scale, mode), 8 + RATIO_DIGITS, { sig: 6, mode });
}

/**
 * Formats the same price the other way round, as whole tokens per KAS, to six significant digits.
 * This is an inverted market's price.
 */
export function tokensPerKasWords(price: bigint, scale: bigint, decimals: number, mode: Rounding = 'nearest'): string {
  if (price <= 0n) return '0';
  // Base units per KAS are 1e8 * scale / price, with RATIO_DIGITS more digits.
  const num = 100_000_000n * scale * 10n ** BigInt(RATIO_DIGITS);
  return decimalText(div(num, price, mode), decimals + RATIO_DIGITS, { sig: 6, mode });
}

/** Divides `a` by `b`, rounded as `mode` says. `b` must be positive. */
export function div(a: bigint, b: bigint, mode: Rounding): bigint {
  const q = a / b;
  const r = a % b;
  if (r === 0n) return q;
  if (mode === 'up') return q + 1n;
  if (mode === 'nearest' && r * 2n >= b) return q + 1n;
  return q;
}

/**
 * DAA scores per second, the network's nominal rate. The frame says times from a clock it does not
 * verify, so the nominal rate is enough.
 */
export const DAA_PER_SECOND = 10n;

/**
 * Says how far DAA score `target` is from `now`, as a reader says it. It is "now" when it is
 * reached, else "in 28 s", "in 5 min", "in 3 h" or "in 12 d". `words` holds the sentences of each
 * step, from the caller's dictionary.
 */
export function untilWords(target: bigint, now: bigint, words: { now: string; s(n: number): string; min(n: number): string; h(n: number): string; d(n: number): string }): string {
  const sec = Number((target - now) / DAA_PER_SECOND);
  if (sec <= 0) return words.now;
  if (sec < 90) return words.s(sec);
  if (sec < 90 * 60) return words.min(Math.round(sec / 60));
  if (sec < 36 * 3600) return words.h(Math.round(sec / 3600));
  return words.d(Math.round(sec / 86400));
}
