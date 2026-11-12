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
  if (price !== 0n && /^-?0(\.0*)?$/.test(t)) return formatPricePerToken(price, decimals, scale, { maxFraction: 18 });
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

// ------------------------------------------------------------------------------------------------ amount columns that never cut digits

/** Characters a numeric cell of the order book shows without cutting a digit (its narrowest column, desktop and phone). */
export const CELL_CHARS = 10;

/** One cell of an amount column: the text shown and, when that text is not the exact amount, the exact amount for the tooltip. */
export interface AmountCell {
  text: string;
  /** the exact amount (thousands grouped, trailing zeros trimmed, with the unit when given); null when `text` is already exact */
  exact: string | null;
}

const COMPACT: readonly [string, number][] = [['T', 12], ['B', 9], ['M', 6], ['K', 3]];

/** A whole-number compact form: `56.61M` for 56,609,953.4 (two decimals, half up), `1,234.57T` beyond the largest suffix. */
function compactUnits(value: bigint, decimals: number): string {
  const whole = value / pow10(decimals);
  for (const [suffix, exp] of COMPACT) {
    if (whole >= pow10(exp)) return `${formatUnits(value, decimals + exp, { maxFraction: 2, trim: false, group: ',' })}${suffix}`;
  }
  return formatUnits(value, decimals, { maxFraction: 2, trim: false, group: ',' });
}

/** Whether a column text reads as exactly `value` (no `<`, no suffix, no rounded-away digit). */
function shownExactly(text: string, value: bigint, decimals: number): boolean {
  const m = /^(\d+)(?:\.(\d+))?$/.exec(text.replace(/,/g, ''));
  if (!m) return false;
  const frac = (m[2] ?? '').replace(/0+$/, '');
  if (frac.length > decimals) return false;
  return BigInt(m[1]!) * pow10(decimals) + BigInt(frac.padEnd(decimals, '0') || '0') === value;
}

/** `fixedUnits`, except a positive amount that rounds to zero shows as `<0.01` (never a plain zero for something that is there). */
function fixedOrBelow(value: bigint, decimals: number, fraction: number): string {
  const t = fixedUnits(value, decimals, fraction);
  if (value > 0n && /^[0.,]+$/.test(t)) return fraction > 0 ? `<0.${'0'.repeat(fraction - 1)}1` : '<1';
  return t;
}

/**
 * The texts of an amount column (`values` in base units of `decimals`) that fit `width` characters WITHOUT cutting a digit: the column's own fixed
 * decimals (`fraction`, default: the finest amount shown) when every row fits; else fewer decimals for the whole column, so the largest amount keeps
 * every whole digit (a positive amount that would read 0 shows `<0.001`); else, when even the whole digits do not fit, compact K / M / B / T
 * (`56.61M`). A text that is not the exact amount carries the exact amount (`exact`) for the tooltip. Null values show `—`.
 */
export function fitAmountColumn(values: readonly (bigint | null | undefined)[], decimals: number, o: { fraction?: number; width?: number; unit?: string; minFraction?: number } = {}): AmountCell[] {
  const width = o.width ?? CELL_CHARS;
  const fraction = o.fraction ?? columnFraction(values, decimals);
  const exactOf = (v: bigint): string => `${formatUnits(v, decimals, { group: ',' })}${o.unit ? ` ${o.unit}` : ''}`;
  const cell = (v: bigint | null | undefined, text: (v: bigint) => string): AmountCell => {
    if (v === null || v === undefined) return { text: '—', exact: null };
    const shown = text(v);
    return { text: shown, exact: shownExactly(shown, v, decimals) ? null : exactOf(v) };
  };
  const present = values.filter((v): v is bigint => v !== null && v !== undefined);
  const longest = (f: (v: bigint) => string): number => present.reduce((m, v) => Math.max(m, f(v).length), 0);
  const full = (v: bigint) => fixedUnits(v, decimals, fraction);
  if (longest(full) <= width) return values.map((v) => cell(v, full));
  const small = Math.min(fraction, o.minFraction ?? 2);
  const intLen = longest((v) => fixedUnits(v, decimals, 0));
  // `<0.001` takes one character more than `0.001`: a one-digit column keeps room for it
  const f = Math.min(fraction, Math.max(0, width - Math.max(intLen, 2) - 1));
  // the whole digits fit with at least two decimals (or every decimal the column has): fewer decimals for the whole column
  if (intLen <= width && f >= small) return values.map((v) => cell(v, (x) => fixedOrBelow(x, decimals, f)));
  // else thousands and up are compact, smaller amounts keep two decimals
  return values.map((v) => cell(v, (x) => (x >= pow10(decimals + 3) ? compactUnits(x, decimals) : fixedOrBelow(x, decimals, small))));
}

/**
 * `fitAmountColumn` for a column already formatted as decimal text with ONE number of decimals (a price column: `25,300,000.00`): the same
 * texts when they fit `width`, else fewer decimals (down to none: `2,165,696` reads better than `2.17M`) or K / M / B when even the whole
 * digits do not fit (`25.30B`), `exact` = the original text. Texts that are not plain numbers (`—`)
 * pass unchanged.
 */
export function fitDecimalTexts(texts: readonly string[], o: { width?: number; unit?: string } = {}): AmountCell[] {
  const width = o.width ?? CELL_CHARS;
  if (texts.every((s) => s.length <= width)) return texts.map((text) => ({ text, exact: null }));
  const parsed = texts.map((s) => /^(\d[\d,]*)(?:\.(\d+))?$/.exec(s));
  const dp = parsed.reduce((m, p) => Math.max(m, p?.[2]?.length ?? 0), 0);
  const values = parsed.map((p) => (p ? BigInt(p[1]!.replace(/,/g, '') + (p[2] ?? '').padEnd(dp, '0')) : null));
  const cells = fitAmountColumn(values, dp, { fraction: dp, width, minFraction: 0 });
  return cells.map((c, i) => (values[i] === null ? { text: texts[i]!, exact: null } : { text: c.text, exact: c.exact === null ? null : `${texts[i]}${o.unit ? ` ${o.unit}` : ''}` }));
}

const SUBSCRIPT = '₀₁₂₃₄₅₆₇₈₉';

/** The digits of a number `compactZeros` shortened, for its tooltip; nothing for one it left as it is. */
export const zerosTitle = (text: string | null | undefined): string | undefined => (text && compactZeros(text) !== text ? text : undefined);

/**
 * A number with four or more zeros after its point, for a CARD that is read on its own
 * (never in a table or a strip: a column of figures keeps one notation, as `0.000835` beside `0.0₄85` cannot be compared by eye): `0.00002547` reads `0.0₄2547` (the subscript counts the zeros). For display only: the
 * book keeps its digits, as its rows are compared by eye, and the ticket and the final confirmation keep theirs. Text around the number stays as it is.
 */
export const compactZeros = (text: string): string =>
  text.replace(/(^|[^\d.,])0\.(0{4,})(?=[1-9])/g, (_m, before: string, zeros: string) => `${before}0.0${[...String(zeros.length)].map((d) => SUBSCRIPT[Number(d)]).join('')}`);

// ------------------------------------------------------------------------------------------------ one time format

const pad2 = (n: number): string => String(n).padStart(2, '0');
const clock = (d: Date, seconds: boolean): string => `${pad2(d.getHours())}:${pad2(d.getMinutes())}${seconds ? `:${pad2(d.getSeconds())}` : ''}`;
const dateOf = (d: Date): string => `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;

/**
 * The one time format of every table and log row (founder, 2026-11-11): today `HH:MM:SS`, any other day `YYYY-MM-DD HH:MM`; 24-hour, in the viewer's zone, never the
 * reader's locale order (`11/10/26` reads as two different days). `nowMs` is the clock of the page (a prop in tests). A missing time reads `—`.
 */
export function formatRowTime(ms: number | null | undefined, nowMs: number = Date.now()): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return '—';
  const d = new Date(ms);
  const today = dateOf(new Date(nowMs));
  return dateOf(d) === today ? clock(d, true) : `${dateOf(d)} ${clock(d, false)}`;
}

/**
 * The time of a CARD (a phone's list row): `MM-DD HH:MM` within the current year (today included), `YYYY-MM-DD HH:MM` before; 24-hour, in the viewer's zone. A missing time reads `—`.
 */
export function formatRowTimeShort(ms: number | null | undefined, nowMs: number = Date.now()): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return '—';
  const d = new Date(ms);
  const date = dateOf(d);
  return `${d.getFullYear() === new Date(nowMs).getFullYear() ? date.slice(5) : date} ${clock(d, false)}`;
}

/** The full timestamp with the viewer's zone, for the `title` of a row time: `2026-11-10 22:35:02 GMT+9`. */
export function rowTimeTitle(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return '';
  const d = new Date(ms);
  const zone = new Intl.DateTimeFormat('en-US', { timeZoneName: 'short' }).formatToParts(d).find((p) => p.type === 'timeZoneName')?.value ?? '';
  return `${dateOf(d)} ${clock(d, true)}${zone ? ` ${zone}` : ''}`;
}

/** `formatRowTimeShort` of a unix-seconds time; null reads `—`. */
export const formatRowTimeShortUnix = (unix: number | bigint | null | undefined, nowMs: number = Date.now()): string => (unix === null || unix === undefined ? '—' : formatRowTimeShort(Number(unix) * 1000, nowMs));

/** `formatRowTime` of a unix-seconds time (the tables of the Portfolio); null reads `—`. */
export const formatRowTimeUnix = (unix: number | bigint | null | undefined, nowMs: number = Date.now()): string => (unix === null || unix === undefined ? '—' : formatRowTime(Number(unix) * 1000, nowMs));

/** The day alone of a time, `2026-11-10` (a date column, e.g. when a supporter first gave); a missing time reads `—`. */
export const formatRowDate = (ms: number | null | undefined): string => (ms === null || ms === undefined || !Number.isFinite(ms) ? '—' : dateOf(new Date(ms)));

/** `rowTimeTitle` of a unix-seconds time. */
export const rowTimeTitleUnix = (unix: number | bigint | null | undefined): string => (unix === null || unix === undefined ? '' : rowTimeTitle(Number(unix) * 1000));

// ------------------------------------------------------------------------------------------------ one price display rule

/**
 * Fraction digits that show a price with about `significant` significant digits (min 2, max 10). Chosen once per market from a reference
 * price, so every row of a column has the same number of decimals (tabular alignment).
 */
export function priceDecimals(refPerTokenSompi: bigint | null, significant = 5): number {
  if (refPerTokenSompi === null || refPerTokenSompi <= 0n) return 4;
  const intPart = refPerTokenSompi / SOMPI_PER_KAS;
  let dp: number;
  if (intPart > 0n) dp = significant - intPart.toString().length;
  else {
    // leading zeros after the point: 0.0025 KAS = 250000 sompi -> 8 - 6 = 2 zeros
    const zeros = 8 - refPerTokenSompi.toString().length;
    dp = zeros + significant;
  }
  return Math.min(10, Math.max(2, dp));
}


/** Most fraction digits a price is ever displayed with (founder, 2026-11-11); the exact price stays in the `title`. */
export const PRICE_DP_CAP = 6;
/** Significant digits a displayed price shows. */
export const PRICE_SIGNIFICANT_DIGITS = 5;

/**
 * Fraction digits of a displayed price (KAS per whole token): the digits that show about five significant digits of the market's reference price (`priceDecimals`, market-model.ts:
 * 33.4 KAS -> 3, 0.025 KAS -> 6), at most `PRICE_DP_CAP`. `refPerTokenSompi`: the market's reference price (last trade, else mid) in sompi per whole token (null: 4 digits).
 * `grouping`: the fraction digits of the price grouping in force (the book's selector); a coarser grouping shows fewer digits, a finer one never more than the rule above.
 * Chosen once per market and shared by the book, the chart axis, the stats and the tables, so the same price reads the same everywhere.
 */
export function displayPriceDp(refPerTokenSompi: bigint | null | undefined, grouping?: number | null): number {
  const sig = Math.min(PRICE_DP_CAP, priceDecimals(refPerTokenSompi ?? null, PRICE_SIGNIFICANT_DIGITS));
  return grouping === undefined || grouping === null ? sig : Math.max(0, Math.min(sig, Math.trunc(grouping)));
}

/** The fraction digits a price grouping step shows exactly (the book's selector: a step of 0.01 reads two decimals), at most `PRICE_DP_CAP`; the `grouping` of `displayPriceDp`. */
export const groupingPriceDp = (group: bigint, decimals: number, scale: bigint | null | undefined): number | null => tickPriceFraction(group, decimals, scale, 0, PRICE_DP_CAP);

/** The one `displayPriceDp` of a column that holds several markets' prices (the Portfolio tables): the finest any of them needs, so every decimal point lines up. */
export function columnPriceDp(refsPerTokenSompi: Iterable<bigint | null | undefined>): number {
  let dp = 0;
  let any = false;
  for (const r of refsPerTokenSompi) {
    if (r === null || r === undefined || r <= 0n) continue;
    any = true;
    dp = Math.max(dp, displayPriceDp(r));
  }
  return any ? dp : displayPriceDp(null);
}

/** A price (sompi per `basis` base units) as KAS per whole token with exactly `dp` fraction digits (grouped): what a table cell shows; its title is `exactPriceText`. */
export const displayPriceText = (price: bigint, decimals: number, basis: bigint, dp: number): string => fixedPricePerToken(price, decimals, basis, dp);

/** The same price in full (every digit, trailing zeros trimmed, thousands grouped): the `title` of a rounded price cell. */
export function exactPriceText(price: bigint, decimals: number, basis: bigint): string {
  if (basis <= 0n) return formatKas(price, { group: ',' });
  return formatPricePerToken(price, decimals, basis, { maxFraction: 18, group: ',' });
}
