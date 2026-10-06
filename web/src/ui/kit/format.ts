// Display formatting shared by the UI kit and the views. Pure and bigint-safe (never `Number` for sompi or token amounts): every function
// delegates to kob/units.ts, which does exact integer arithmetic.
import { formatKas, formatPricePerToken, formatTokenAmount, formatUnits, pow10, SOMPI_PER_KAS } from '../../kob/units';

/** `abcdef...123456` -> `abcd...3456` (head/tail characters). Strings that are already short come back unchanged. */
export function shortId(id: string, head = 4, tail = 4): string {
  const s = String(id);
  if (s.length <= head + tail + 1) return s;
  return `${s.slice(0, head)}…${s.slice(-tail)}`;
}

/** Address form: keeps the network prefix visible (`kaspa:qq...xyz`). */
export function shortAddress(address: string, tail = 6): string {
  const i = address.indexOf(':');
  if (i < 0) return shortId(address, 6, tail);
  const prefix = address.slice(0, i + 1);
  const body = address.slice(i + 1);
  return body.length <= 8 + tail ? address : `${prefix}${body.slice(0, 6)}…${body.slice(-tail)}`;
}

/** KAS text of a sompi amount: up to 8 decimals, trailing zeros trimmed. */
export const kasText = (sompi: bigint): string => formatKas(sompi);

/** Token amount text with the token's decimals, trailing zeros trimmed. */
export const tokenText = (base: bigint, decimals: number): string => formatTokenAmount(base, decimals);

/** Token amount capped to `maxFraction` fractional digits (half up), for dense tables. */
export const tokenTextShort = (base: bigint, decimals: number, maxFraction = 4): string => formatUnits(base, decimals, { maxFraction });

/**
 * A state price (sompi per whole token of `scale` base units) as KAS per WHOLE token. Normally 8 fractional digits; a price too small for that
 * (rounds to 0) gets more digits so a real price never displays as zero.
 */
export function pricePerTokenText(price: bigint, decimals: number, scale: bigint): string {
  if (scale <= 0n) return kasText(price);
  const t = formatPricePerToken(price, decimals, scale, { maxFraction: 8 });
  if (price > 0n && /^0(\.0*)?$/.test(t)) return formatPricePerToken(price, decimals, scale, { maxFraction: 18 });
  return t;
}

/** The wallet's scale of a token: base units per whole token, `10^decimals` capped at 10^9 (kob-wasm `defaultScale`). */
export const scaleOfDecimals = (decimals: number): bigint => pow10(Math.min(Math.max(decimals, 0), 9));

/** Signed KAS text: `+1.5` / `-0.25` / `0`. */
export function signedKasText(sompi: bigint): string {
  return sompi > 0n ? `+${kasText(sompi)}` : kasText(sompi);
}

/** `kas` sompi, `token` base units, `price` a state price (sompi per whole token of `scale` base units) shown as KAS per whole token */
export type AmountKind = 'kas' | 'token' | 'price';

export interface AmountSpec {
  kind: AmountKind;
  value: bigint | null | undefined;
  /** token decimals (token, price) */
  decimals?: number;
  /** the price's scale: base units per whole token of its denominator (price; default 10^decimals capped at 10^9) */
  scale?: bigint;
  /** show at most this many fractional digits (token) */
  maxFraction?: number;
  /**
   * show EXACTLY this many fractional digits (trailing zeros kept, half-up rounding): the fixed precision of a table column, so decimal
   * points line up. Wins over `maxFraction`.
   */
  fraction?: number;
}

/** The plain text an `<Amount>` renders; `—` for a missing value. */
export function amountText(a: AmountSpec): string {
  if (a.value === null || a.value === undefined) return '—';
  if (a.fraction !== undefined) {
    switch (a.kind) {
      case 'kas':
        return fixedUnits(a.value, 8, a.fraction, '');
      case 'token':
        return fixedUnits(a.value, a.decimals ?? 0, a.fraction, '');
      case 'price':
        return fixedPricePerToken(a.value, a.decimals ?? 0, a.scale ?? scaleOfDecimals(a.decimals ?? 0), a.fraction, '');
    }
  }
  switch (a.kind) {
    case 'kas':
      return kasText(a.value);
    case 'token':
      return a.maxFraction !== undefined ? tokenTextShort(a.value, a.decimals ?? 0, a.maxFraction) : tokenText(a.value, a.decimals ?? 0);
    case 'price':
      return pricePerTokenText(a.value, a.decimals ?? 0, a.scale ?? scaleOfDecimals(a.decimals ?? 0));
  }
}

/** Thousands grouping of the integer part of a plain decimal string (display only; never fed back to a parser). */
export function groupDigits(text: string, sep = ','): string {
  const m = /^(-?)(\d+)(\.\d+)?$/.exec(text);
  if (!m) return text;
  return `${m[1]}${m[2].replace(/\B(?=(\d{3})+(?!\d))/g, sep)}${m[3] ?? ''}`;
}

// ------------------------------------------------------------------------------------------------ fixed-decimal columns
//
// A column of numbers lines up only when every row shows the same number of decimals. The helpers below derive that count ONCE per market /
// view / column (from the tick, the token's decimals, or the finest value shown) and format every row with it, trailing zeros kept.

/** Number of trailing decimal zeros of a non-negative integer (0 for 0 and for numbers not divisible by 10). */
export function trailingZeros(n: bigint): number {
  if (n <= 0n) return 0;
  let z = 0;
  let x = n;
  while (x % 10n === 0n) {
    x /= 10n;
    z++;
  }
  return z;
}

/** `value` base units with exactly `fraction` fractional digits (half-up; zero-padded when `fraction` exceeds `decimals`), thousands grouped. */
export function fixedUnits(value: bigint, decimals: number, fraction: number, group = ','): string {
  const f = Math.max(0, Math.trunc(fraction));
  if (f >= decimals) {
    const t = formatUnits(value, decimals, { trim: false, group });
    if (f === decimals) return t;
    return decimals === 0 ? `${t}.${'0'.repeat(f)}` : `${t}${'0'.repeat(f - decimals)}`;
  }
  return formatUnits(value, decimals, { maxFraction: f, trim: false, group });
}

/** KAS per whole token of a state price (sompi per `scale` base units), exactly `fraction` fractional digits, thousands grouped. */
export function fixedPricePerToken(price: bigint, decimals: number, scale: bigint, fraction: number, group = ','): string {
  if (scale <= 0n) return fixedUnits(price, 8, fraction, group);
  return formatPricePerToken(price, decimals, scale, { maxFraction: Math.max(0, fraction), trim: false, group });
}

/** The fraction digits an amount of `decimals` needs to be shown exactly: `decimals` minus its trailing zeros (0..decimals). */
export function unitsFraction(value: bigint, decimals: number): number {
  if (value <= 0n) return 0;
  return Math.max(0, decimals - trailingZeros(value));
}

/** The finest fraction any of the values needs (a column of amounts: the shared fixed decimals). 0 for no values. */
export function columnFraction(values: Iterable<bigint | null | undefined>, decimals: number): number {
  let f = 0;
  for (const v of values) if (v !== null && v !== undefined) f = Math.max(f, unitsFraction(v < 0n ? -v : v, decimals));
  return f;
}


/** Fixed fraction digits of a KAS column whose values are multiples of `tick` sompi (8 minus the tick's trailing zeros); 8 without a tick. */
export function tickKasFraction(tick: bigint | null | undefined): number {
  return tick && tick > 0n ? unitsFraction(tick, 8) : 8;
}

export const PRICE_FRACTION_MIN = 2;
export const PRICE_FRACTION_MAX = 10;

/**
 * Fixed fraction digits of a KAS-per-token price column from the market's tick (sompi per whole token of `scale` base units): the fewest digits
 * that show every tick-aligned price exactly, bounded to [min, max]. Null when there is no tick (the caller falls back to a precision from the
 * price magnitude).
 */
export function tickPriceFraction(tick: bigint | null | undefined, decimals: number, scale: bigint | null | undefined, min = PRICE_FRACTION_MIN, max = PRICE_FRACTION_MAX): number | null {
  if (!tick || tick <= 0n || !scale || scale <= 0n) return null;
  // price per token = tick * 10^decimals / (scale * 1e8) KAS: find the smallest f with that value * 10^f an integer
  const num = tick * pow10(decimals);
  const den = scale * SOMPI_PER_KAS;
  for (let f = 0; f <= max; f++) if ((num * pow10(f)) % den === 0n) return Math.min(max, Math.max(min, f));
  return max;
}
