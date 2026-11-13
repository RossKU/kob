// Locale-independent, bigint-safe money arithmetic.
// It covers KAS and sompi, token base units and display text, state prices and prices per whole
// token, tick rounding, and basis-point math. Nothing here touches Intl or the locale, so the UI
// formats separators itself.
//
// Sompi, token amounts and prices are always `bigint`. A JS number loses precision above 2^53, so
// it cannot hold a 64-bit amount.

/** Sompi in one KAS. */
export const SOMPI_PER_KAS = 100_000_000n;
/** Decimal places of KAS. */
export const KAS_DECIMALS = 8;
/** Basis points in one whole. */
export const BPS = 10_000n;
/** Largest value of an on-chain signed 64-bit integer. Every state field is an I64. */
export const I64_MAX = (1n << 63n) - 1n;

// --- Integer helpers ---

/** Integer division rounding up. The divisor must be positive and the dividend not negative. */
export function ceilDiv(a: bigint, b: bigint): bigint {
  if (b <= 0n) throw new RangeError('ceilDiv: divisor must be positive');
  if (a < 0n) throw new RangeError('ceilDiv: dividend must be >= 0');
  return (a + b - 1n) / b;
}

/** Integer division rounding down, with the same operand limits as `ceilDiv`. */
export function floorDiv(a: bigint, b: bigint): bigint {
  if (b <= 0n) throw new RangeError('floorDiv: divisor must be positive');
  if (a < 0n) throw new RangeError('floorDiv: dividend must be >= 0');
  return a / b;
}

/** Division rounded to the nearest integer, ties up. Same operand limits as `ceilDiv`. */
export function roundDiv(a: bigint, b: bigint): bigint {
  if (b <= 0n) throw new RangeError('roundDiv: divisor must be positive');
  if (a < 0n) throw new RangeError('roundDiv: dividend must be >= 0');
  return (2n * a + b) / (2n * b);
}

/** The smaller of two bigints. */
export const minBig = (a: bigint, b: bigint): bigint => (a < b ? a : b);
/** The larger of two bigints. */
export const maxBig = (a: bigint, b: bigint): bigint => (a > b ? a : b);

/** Returns 10 to the power `n` for a non-negative integer `n`. */
export const pow10 = (n: number): bigint => {
  if (!Number.isInteger(n) || n < 0) throw new RangeError('pow10: exponent must be a non-negative integer');
  return 10n ** BigInt(n);
};

/** Returns `amount * bps / 10_000` rounded down, as the covenants and `kob-protocol` do. */
export const bpsFloor = (amount: bigint, bps: bigint): bigint => (amount * bps) / BPS;
/** Returns `amount * bps / 10_000` rounded up. */
export const bpsCeil = (amount: bigint, bps: bigint): bigint => ceilDiv(amount * bps, BPS);

/**
 * Returns the quote value of `n` base units at `rate` quote units per whole token of `scale` base
 * units.
 *
 * This is `n * rate / scale`, the protocol's quote rule (docs/spec/order-types.md "Amounts, prices
 * and rounding"). Round `up` for what a maker receives and `down` for what a maker pays.
 * The result is exact and equals the covenants' split multiplication (kob-wasm `quote`) whenever
 * that fits an i64. kob-wasm returns null beyond that. Use it for views and estimates. Disclosures
 * and plans take the per-kind kob-wasm helpers.
 */
export function quoteOf(n: bigint, rate: bigint, scale: bigint, round: 'up' | 'down'): bigint {
  if (scale <= 0n) throw new RangeError('quoteOf: scale must be positive');
  if (n < 0n || rate < 0n) throw new RangeError('quoteOf: amount and rate must be >= 0');
  return round === 'up' ? ceilDiv(n * rate, scale) : (n * rate) / scale;
}

/** Formats basis points as a percentage with two decimals, so 300n gives "3.00". */
export const formatBps = (bps: bigint): string => formatUnits(bps, 2, { trim: false });

/** Returns `v` when it fits a non-negative I64, else throws a RangeError. */
export function assertI64(v: bigint, what = 'value'): bigint {
  if (v < 0n || v > I64_MAX) throw new RangeError(`${what} does not fit a signed 64-bit integer`);
  return v;
}
/** True when `v` fits a non-negative I64. */
export const fitsI64 = (v: bigint): boolean => v >= 0n && v <= I64_MAX;

// --- Decimal parse and format ---

/** Thrown by the strict parsers. Its `code` is stable for the UI. */
export class UnitsError extends Error {
  readonly code: 'empty' | 'format' | 'too_many_decimals' | 'negative';
  constructor(code: UnitsError['code'], message: string) {
    super(message);
    this.name = 'UnitsError';
    this.code = code;
  }
}

/**
 * Parses a non-negative decimal string into base units with `decimals` fractional digits.
 *
 * Only `.` is a decimal point, because the UI normalises the locale first. Surrounding whitespace
 * is ignored. More fractional digits than `decimals` is an error and is never rounded, because the
 * amount must not change between the text field and the transaction.
 */
export function parseUnits(text: string, decimals: number): bigint {
  const s = text.trim();
  if (s === '') throw new UnitsError('empty', 'empty amount');
  if (s.startsWith('-')) throw new UnitsError('negative', 'negative amount');
  const m = /^(\d*)(?:\.(\d*))?$/.exec(s);
  if (!m || (m[1] === '' && (m[2] ?? '') === '')) throw new UnitsError('format', `not a number: "${text}"`);
  const whole = m[1] || '0';
  const frac = m[2] ?? '';
  if (frac.length > decimals) {
    // Trailing zeros beyond the precision are harmless. Only non-zero digits would lose money.
    if (/[^0]/.test(frac.slice(decimals))) throw new UnitsError('too_many_decimals', `more than ${decimals} decimal places`);
  }
  return BigInt(whole) * pow10(decimals) + BigInt(frac.slice(0, decimals).padEnd(decimals, '0') || '0');
}

/** Like `parseUnits` but returns null instead of throwing, for form validation. */
export function tryParseUnits(text: string, decimals: number): bigint | null {
  try {
    return parseUnits(text, decimals);
  } catch (e) {
    if (e instanceof UnitsError) return null;
    throw e;
  }
}

/** Options of `formatUnits`. */
export interface FormatOptions {
  /** Drops trailing zeros of the fraction. Defaults to true. */
  trim?: boolean;
  /** Thousands separator of the integer part. Defaults to none. */
  group?: string;
  /** Decimal separator. Defaults to ".". */
  point?: string;
  /** Keeps at least this many fractional digits when trimming. Defaults to 0. */
  minFraction?: number;
  /** Rounds half up to at most this many fractional digits. Defaults to all of them. */
  maxFraction?: number;
}

/** Formats base units with `decimals` fractional digits exactly, without floating point. */
export function formatUnits(value: bigint, decimals: number, opt: FormatOptions = {}): string {
  const neg = value < 0n;
  let v = neg ? -value : value;
  let d = decimals;
  if (opt.maxFraction !== undefined && opt.maxFraction < decimals) {
    v = roundDiv(v, pow10(decimals - opt.maxFraction));
    d = opt.maxFraction;
  }
  const scale = pow10(d);
  let whole = (v / scale).toString();
  let frac = d > 0 ? (v % scale).toString().padStart(d, '0') : '';
  if (opt.trim ?? true) {
    frac = frac.replace(/0+$/, '');
    if (frac.length < (opt.minFraction ?? 0)) frac = frac.padEnd(Math.min(opt.minFraction ?? 0, d), '0');
  }
  if (opt.group) whole = whole.replace(/\B(?=(\d{3})+(?!\d))/g, opt.group);
  return `${neg ? '-' : ''}${whole}${frac ? (opt.point ?? '.') + frac : ''}`;
}

/** Parses a KAS amount into sompi. */
export const parseKas = (text: string): bigint => parseUnits(text, KAS_DECIMALS);
/** Parses a KAS amount into sompi, or returns null. */
export const tryParseKas = (text: string): bigint | null => tryParseUnits(text, KAS_DECIMALS);
/** Formats sompi as a KAS amount. */
export const formatKas = (sompi: bigint, opt?: FormatOptions): string => formatUnits(sompi, KAS_DECIMALS, opt);

/** Parses a token amount into base units. */
export const parseTokenAmount = (text: string, decimals: number): bigint => parseUnits(text, decimals);
/** Parses a token amount into base units, or returns null. */
export const tryParseTokenAmount = (text: string, decimals: number): bigint | null => tryParseUnits(text, decimals);
/** Returns the decimals a scale stands for, a power of ten up to 10^18, or null for any other. */
export function decimalsOfScale(scale: bigint): number | null {
  for (let d = 0, p = 1n; d <= 18; d++, p *= 10n) if (p === scale) return d;
  return null;
}

/** Formats base units of a token as a decimal amount. */
export const formatTokenAmount = (base: bigint, decimals: number, opt?: FormatOptions): string => formatUnits(base, decimals, opt);

// --- State price and price per whole token ---

/** How a division rounds. */
export type Rounding = 'up' | 'down' | 'nearest';

function divRound(a: bigint, b: bigint, mode: Rounding): bigint {
  return mode === 'up' ? ceilDiv(a, b) : mode === 'down' ? floorDiv(a, b) : roundDiv(a, b);
}

// An order's `price` is quote units per `scale` base units (docs/spec/order-types.md "Amounts,
// prices and rounding"). The wallet sets `scale` to `10^decimals` through kob-wasm `defaultScale`,
// capped at 10^9. The state price is then the price per whole token, and the conversions below
// differ from the identity only for a token of more than 9 decimals or an order of a foreign scale.

/**
 * Converts a state price, in quote units per `scale` base units, to quote units per whole token.
 */
export function statePriceToTokenPrice(price: bigint, decimals: number, scale: bigint, mode: Rounding = 'nearest'): bigint {
  return divRound(price * pow10(decimals), scale, mode);
}

/**
 * Converts quote units per whole token to the state price per `scale` base units.
 * The rounding is explicit because its direction matters. A sell price rounds up and a buy price
 * rounds down, see `roundToTickSafe` and `safeRounding`.
 */
export function tokenPriceToStatePrice(perToken: bigint, decimals: number, scale: bigint, mode: Rounding): bigint {
  return divRound(perToken * scale, pow10(decimals), mode);
}

/**
 * Formats a state price, in sompi per `scale` base units, as KAS per whole token.
 * The text is exact up to `maxFraction` digits, which default to 8.
 */
export function formatPricePerToken(price: bigint, decimals: number, scale: bigint, opt: FormatOptions = {}): string {
  // A price difference, such as the spread of a crossed book, can be negative. Format the
  // magnitude and then sign it. A difference that rounds to zero stays unsigned, never `-0.00`.
  if (price < 0n) {
    const text = formatPricePerToken(-price, decimals, scale, opt);
    return /[1-9]/.test(text) ? `-${text}` : text;
  }
  // KAS per token is price * 10^decimals / scale / 1e8. Scale the numerator to `maxFraction`
  // fractional digits of KAS and round half up.
  const maxFraction = opt.maxFraction ?? 8;
  const digits = BigInt(Math.max(maxFraction, 0));
  const scaled = roundDiv(price * pow10(decimals) * pow10(Number(digits)), scale * SOMPI_PER_KAS);
  return formatUnits(scaled, Number(digits), { ...opt, maxFraction: undefined });
}

/**
 * Parses KAS-per-token text such as "0.0025" into the state price in sompi per `scale` base units.
 * It rounds as asked and throws UnitsError on malformed input.
 */
export function parsePricePerToken(text: string, decimals: number, scale: bigint, mode: Rounding): bigint {
  return tokenPriceToStatePrice(parseKas(text), decimals, scale, mode);
}

// --- Ticks ---

/**
 * Returns the rounding that never worsens the maker's limit. A sell rounds up and a buy rounds
 * down.
 */
export const safeRounding = (side: 'sell' | 'buy'): Rounding => (side === 'sell' ? 'up' : 'down');

/** True when `price` is a multiple of `tick`. A tick of zero or less means no tick. */
export const isOnTick = (price: bigint, tick: bigint): boolean => tick <= 0n || price % tick === 0n;

/**
 * Rounds a price in sompi per whole token to a multiple of `tick`. A tick of zero or less changes
 * nothing.
 */
export function roundToTick(price: bigint, tick: bigint, mode: Rounding): bigint {
  if (tick <= 0n) return price;
  return divRound(price, tick, mode) * tick;
}

/** Rounds in the safe direction of `side`, up for a sell and down for a buy. */
export const roundToTickSafe = (price: bigint, tick: bigint, side: 'sell' | 'buy'): bigint => roundToTick(price, tick, safeRounding(side));

/**
 * Returns the price when it is on the tick, else null. Callers use it to refuse off-tick prices.
 */
export const requireTick = (price: bigint, tick: bigint): bigint | null => (isOnTick(price, tick) ? price : null);
