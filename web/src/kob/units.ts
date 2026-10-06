// Locale-independent, bigint-safe money arithmetic: KAS <-> sompi, token base units <-> display, state prices <-> per whole token,
// tick rounding and basis-point math. Nothing here touches Intl or the locale: the UI formats separators itself.
//
// Sompi, token amounts and prices are ALWAYS `bigint` (a JS number loses precision above 2^53; a 64-bit sompi amount does not fit).

export const SOMPI_PER_KAS = 100_000_000n;
export const KAS_DECIMALS = 8;
export const BPS = 10_000n;
/** Largest value the on-chain 64-bit signed integers can hold (every state field is an I64). */
export const I64_MAX = (1n << 63n) - 1n;

// ------------------------------------------------------------------------------------------------ integer helpers

/** Integer division rounding up; both operands must be positive-ish (b > 0, a >= 0). */
export function ceilDiv(a: bigint, b: bigint): bigint {
  if (b <= 0n) throw new RangeError('ceilDiv: divisor must be positive');
  if (a < 0n) throw new RangeError('ceilDiv: dividend must be >= 0');
  return (a + b - 1n) / b;
}

/** Integer division rounding down (b > 0, a >= 0); the explicit twin of ceilDiv. */
export function floorDiv(a: bigint, b: bigint): bigint {
  if (b <= 0n) throw new RangeError('floorDiv: divisor must be positive');
  if (a < 0n) throw new RangeError('floorDiv: dividend must be >= 0');
  return a / b;
}

/** Division rounded to the nearest integer, ties up (a >= 0, b > 0). */
export function roundDiv(a: bigint, b: bigint): bigint {
  if (b <= 0n) throw new RangeError('roundDiv: divisor must be positive');
  if (a < 0n) throw new RangeError('roundDiv: dividend must be >= 0');
  return (2n * a + b) / (2n * b);
}

export const minBig = (a: bigint, b: bigint): bigint => (a < b ? a : b);
export const maxBig = (a: bigint, b: bigint): bigint => (a > b ? a : b);

export const pow10 = (n: number): bigint => {
  if (!Number.isInteger(n) || n < 0) throw new RangeError('pow10: exponent must be a non-negative integer');
  return 10n ** BigInt(n);
};

/** `amount * bps / 10_000`, rounded down (the covenants and `kob-protocol` floor every bps computation). */
export const bpsFloor = (amount: bigint, bps: bigint): bigint => (amount * bps) / BPS;
/** `amount * bps / 10_000`, rounded up. */
export const bpsCeil = (amount: bigint, bps: bigint): bigint => ceilDiv(amount * bps, BPS);

/**
 * The protocol's quote rule (docs/spec/order-types.md "Amounts, prices and rounding"): the quote value of `n` base units at `rate` quote units
 * per whole token of `scale` base units, `n * rate / scale`, rounded `up` for what a maker RECEIVES and `down` for what a maker PAYS. Exact
 * (bigint): equal to the covenants' split multiplication (kob-wasm `quote`) whenever that fits an i64 (kob-wasm returns null beyond). Use it for
 * views and estimates; disclosures and plans take the per-kind kob-wasm helpers.
 */
export function quoteOf(n: bigint, rate: bigint, scale: bigint, round: 'up' | 'down'): bigint {
  if (scale <= 0n) throw new RangeError('quoteOf: scale must be positive');
  if (n < 0n || rate < 0n) throw new RangeError('quoteOf: amount and rate must be >= 0');
  return round === 'up' ? ceilDiv(n * rate, scale) : (n * rate) / scale;
}

/** Value as a percentage string with two decimals, e.g. 300n bps -> "3.00". */
export const formatBps = (bps: bigint): string => formatUnits(bps, 2, { trim: false });

export function assertI64(v: bigint, what = 'value'): bigint {
  if (v < 0n || v > I64_MAX) throw new RangeError(`${what} does not fit a signed 64-bit integer`);
  return v;
}
export const fitsI64 = (v: bigint): boolean => v >= 0n && v <= I64_MAX;

// ------------------------------------------------------------------------------------------------ decimal parse / format

/** Thrown by the strict parsers; `code` is stable for the UI. */
export class UnitsError extends Error {
  readonly code: 'empty' | 'format' | 'too_many_decimals' | 'negative';
  constructor(code: UnitsError['code'], message: string) {
    super(message);
    this.name = 'UnitsError';
    this.code = code;
  }
}

/**
 * Parses a non-negative decimal string into base units with `decimals` fractional digits. Only `.` is a decimal point (the UI
 * normalises the locale first); surrounding whitespace is ignored. More fractional digits than `decimals` is an ERROR, never a
 * silent rounding: money must not change between the text field and the transaction.
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
    // trailing zeros beyond the precision are harmless ("1.500000000" for 8 decimals): only non-zero digits would lose money
    if (/[^0]/.test(frac.slice(decimals))) throw new UnitsError('too_many_decimals', `more than ${decimals} decimal places`);
  }
  return BigInt(whole) * pow10(decimals) + BigInt(frac.slice(0, decimals).padEnd(decimals, '0') || '0');
}

/** Like parseUnits but returns null instead of throwing (form validation). */
export function tryParseUnits(text: string, decimals: number): bigint | null {
  try {
    return parseUnits(text, decimals);
  } catch (e) {
    if (e instanceof UnitsError) return null;
    throw e;
  }
}

export interface FormatOptions {
  /** drop trailing zeros of the fraction (default true) */
  trim?: boolean;
  /** thousands separator for the integer part (default none) */
  group?: string;
  /** decimal separator (default ".") */
  point?: string;
  /** keep at least this many fractional digits when trimming (default 0) */
  minFraction?: number;
  /** round the display to at most this many fractional digits (half up); default = all */
  maxFraction?: number;
}

/** Formats non-negative (or negative) base units with `decimals` fractional digits, exactly (no floating point). */
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

export const parseKas = (text: string): bigint => parseUnits(text, KAS_DECIMALS);
export const tryParseKas = (text: string): bigint | null => tryParseUnits(text, KAS_DECIMALS);
export const formatKas = (sompi: bigint, opt?: FormatOptions): string => formatUnits(sompi, KAS_DECIMALS, opt);

export const parseTokenAmount = (text: string, decimals: number): bigint => parseUnits(text, decimals);
export const tryParseTokenAmount = (text: string, decimals: number): bigint | null => tryParseUnits(text, decimals);
export const formatTokenAmount = (base: bigint, decimals: number, opt?: FormatOptions): string => formatUnits(base, decimals, opt);

// ------------------------------------------------------------------------------------------------ state price <-> price per whole token

export type Rounding = 'up' | 'down' | 'nearest';

function divRound(a: bigint, b: bigint, mode: Rounding): bigint {
  return mode === 'up' ? ceilDiv(a, b) : mode === 'down' ? floorDiv(a, b) : roundDiv(a, b);
}

// An order's `price` is quote units per `scale` base units (protocol v3, docs/spec/order-types.md "Amounts, prices and rounding"); the wallet
// sets `scale = 10^decimals` (kob-wasm `defaultScale`, at most 10^9), so the state price IS the price per whole token. The conversions below
// differ from the identity only for a token of more than 9 decimals (or an order of a foreign scale).

/** State price (quote units per `scale` base units) -> quote units per whole token (`10^decimals` base units). */
export function statePriceToTokenPrice(price: bigint, decimals: number, scale: bigint, mode: Rounding = 'nearest'): bigint {
  return divRound(price * pow10(decimals), scale, mode);
}

/**
 * Quote units per whole token -> the state price (per `scale` base units). Rounding is explicit because the direction matters: a sell
 * price rounds UP and a buy price DOWN (see roundToTickSafe / safeRounding).
 */
export function tokenPriceToStatePrice(perToken: bigint, decimals: number, scale: bigint, mode: Rounding): bigint {
  return divRound(perToken * scale, pow10(decimals), mode);
}

/** Exact decimal string of a state price (sompi per `scale` base units) in KAS per whole token (up to `maxFraction` digits, default 8). */
export function formatPricePerToken(price: bigint, decimals: number, scale: bigint, opt: FormatOptions = {}): string {
  // KAS per token = price * 10^decimals / scale / 1e8; scale the numerator to `maxFraction` fractional digits of KAS and round half up
  const maxFraction = opt.maxFraction ?? 8;
  const digits = BigInt(Math.max(maxFraction, 0));
  const scaled = roundDiv(price * pow10(decimals) * pow10(Number(digits)), scale * SOMPI_PER_KAS);
  return formatUnits(scaled, Number(digits), { ...opt, maxFraction: undefined });
}

/** KAS-per-token text (e.g. "0.0025") -> the state price (sompi per `scale` base units), rounded as asked. Throws UnitsError on malformed input. */
export function parsePricePerToken(text: string, decimals: number, scale: bigint, mode: Rounding): bigint {
  return tokenPriceToStatePrice(parseKas(text), decimals, scale, mode);
}

// ------------------------------------------------------------------------------------------------ ticks

/** The rounding that never makes the maker's limit worse than typed: a sell price rounds up, a buy price rounds down. */
export const safeRounding = (side: 'sell' | 'buy'): Rounding => (side === 'sell' ? 'up' : 'down');

export const isOnTick = (price: bigint, tick: bigint): boolean => tick <= 0n || price % tick === 0n;

/** Rounds a price (sompi per whole token) to a multiple of `tick`. `tick <= 0` means "no tick": unchanged. */
export function roundToTick(price: bigint, tick: bigint, mode: Rounding): bigint {
  if (tick <= 0n) return price;
  return divRound(price, tick, mode) * tick;
}

/** Rounds in the safe direction of `side` (sell up, buy down). */
export const roundToTickSafe = (price: bigint, tick: bigint, side: 'sell' | 'buy'): bigint => roundToTick(price, tick, safeRounding(side));

/** Like roundToTickSafe but returns null when the price is not on the tick (the "refuse" policy). */
export const requireTick = (price: bigint, tick: bigint): bigint | null => (isOnTick(price, tick) ? price : null);
