// The optional USD display of KAS values (Settings, "Display currency"). It is pure and DOM-free.
// The browser never asks a price API. The indexer serves the price (data/kas-price.ts), and this
// module only decides what to show.
//
// - USD is shown for KAS-denominated values the viewer reads: the Portfolio's summary and
//   balances, the market list's price, volume, market cap and depth, and the market header's last
//   price and 24 h stats.
// - The order book, the ticket, the order and fill tables, the Activity feed and everything signed
//   stay in KAS. A price the user types or signs is never converted.
// - A token's USD value is its value in KAS (what the app already computes from the last price)
//   times the KAS price. It is an estimate, and every place that shows it says so.
// - The quote must be present and not older than `KAS_USD_MAX_AGE_S`. Otherwise KAS is shown with
//   a notice, never a wrong or stale number silently.
// - The age is the larger of the clock's age and the time since this browser first saw the
//   quote's `at`, so a wrong clock cannot make an old quote look young.
// - Testnet KAS has no value. The USD is the mainnet price, for display only, and the note says so.
import type { KasUsdQuote } from '../data/kas-price';
import { t } from '../i18n';
import { formatRational, significantDp, type Rational } from '../kob/pair';
import { formatKas, SOMPI_PER_KAS } from '../kob/units';
import type { DisplayCurrency } from './currency-pref';

/**
 * A quote older than this (10 minutes) is not used and the display falls back to KAS. It lives
 * here and not in data/ so that the UI kit's Amount can use this module.
 */
export const KAS_USD_MAX_AGE_S = 10 * 60;

/**
 * The KAS price as an exact rational, in USD per KAS with nine decimals. The price is a display
 * figure, and the products below are exact on it.
 */
export interface Rate {
  usd: number;
  num: bigint;
  den: bigint;
}

const RATE_DEN = 1_000_000_000n;

/** The rate of a price in USD per KAS. Null unless it is a positive finite number below 1,000. */
export function rateOf(usd: number): Rate | null {
  if (!Number.isFinite(usd) || usd <= 0 || usd >= 1_000) return null;
  const num = BigInt(Math.round(usd * 1e9));
  return num > 0n ? { usd, num, den: RATE_DEN } : null;
}

/** What the views read when USD is in force: the rate, its quote and whether KAS is testnet's. */
export interface Fiat {
  rate: Rate;
  quote: KasUsdQuote;
  testnet: boolean;
}

/** The state of the feed the views see (app/kas-price.ts). */
export interface PriceFeed {
  quote: KasUsdQuote | null;
  /** The local time in ms at which this `quote` (its `at`) was first received. */
  seenAtMs: number | null;
  /**
   * `loading` means not answered yet, `unavailable` means the indexer serves no price (404), and
   * `error` means the last read failed.
   */
  status: 'idle' | 'loading' | 'ok' | 'unavailable' | 'error';
}

/** What is in force: KAS, USD with its fiat, or KAS as a fallback with the reason. */
export type FiatState =
  | { kind: 'kas' }
  | { kind: 'usd'; fiat: Fiat; ageS: number }
  | { kind: 'fallback'; reason: 'loading' | 'unavailable' | 'stale' | 'missing'; ageS: number | null };

/**
 * Seconds since the quote was taken: the larger of the clock's age and the time since it was first
 * received here. Never negative.
 */
export function quoteAgeS(q: KasUsdQuote, seenAtMs: number | null, nowMs: number): number {
  const byClock = nowMs / 1000 - q.at;
  const bySeen = seenAtMs === null ? 0 : (nowMs - seenAtMs) / 1000;
  return Math.max(0, byClock, bySeen);
}

/** What to show: KAS, USD (chosen, with a fresh price), or KAS with the reason USD is off. */
export function resolveFiat(input: { currency: DisplayCurrency; feed: PriceFeed; nowMs: number; testnet: boolean }): FiatState {
  if (input.currency !== 'usd') return { kind: 'kas' };
  const { quote, seenAtMs, status } = input.feed;
  if (!quote) {
    return { kind: 'fallback', reason: status === 'unavailable' ? 'unavailable' : status === 'loading' || status === 'idle' ? 'loading' : 'missing', ageS: null };
  }
  const ageS = quoteAgeS(quote, seenAtMs, input.nowMs);
  const rate = rateOf(quote.usd);
  if (!rate) return { kind: 'fallback', reason: 'missing', ageS: null };
  if (ageS > KAS_USD_MAX_AGE_S) return { kind: 'fallback', reason: 'stale', ageS };
  return { kind: 'usd', fiat: { rate, quote, testnet: input.testnet }, ageS };
}

/** The Fiat in force, or null. Every view that shows a value reads null as "KAS". */
export const fiatOf = (s: FiatState): Fiat | null => (s.kind === 'usd' ? s.fiat : null);

// Conversion and text.

/** `sompi` of KAS as a USD amount, exact on the rate. */
export const sompiToUsd = (sompi: bigint, rate: Rate): Rational => ({ num: sompi * rate.num, den: SOMPI_PER_KAS * rate.den });

/** KAS per whole token, as a rational, converted to USD per whole token. */
export const kasPerTokenToUsd = (kas: Rational, rate: Rate): Rational => ({ num: kas.num * rate.num, den: kas.den * rate.den });

/** The USD amount as whole cents, half up. */
export const usdCents = (usd: Rational): bigint => (usd.num * 100n * 2n + usd.den) / (usd.den * 2n);

/**
 * Formats a USD amount as `$1,066.77`, with thousands grouped. From a dollar up it has two
 * decimals. Below a dollar it has about four significant digits (`$0.0576`, `$0.00001234`), never
 * fewer than two decimals, and a nonzero amount never shows as zero (`<$0.000000000001`).
 */
export function usdText(usd: Rational): string {
  if (usd.num < 0n) return `-${usdText({ num: -usd.num, den: usd.den })}`;
  if (usd.num === 0n) return '$0.00';
  const dp = usd.num >= usd.den ? 2 : significantDp(usd, 4, 2, 12);
  const text = formatRational(usd, dp, 'nearest', 2, ',');
  return /^0\.0+$/.test(text) ? `<$0.${'0'.repeat(11)}1` : `$${text}`;
}

const COMPACT: readonly [string, bigint][] = [['T', 10n ** 12n], ['B', 10n ** 9n], ['M', 10n ** 6n]];

/**
 * `usdText`, with millions and up made compact (`$56.61M`) once the text would be longer than
 * `width` characters, so a table cell never cuts a digit. It is not the exact figure. The tooltip
 * has that.
 */
export function usdTextFit(usd: Rational, width: number): string {
  const full = usdText(usd);
  if (full.length <= width || usd.num < 0n) return full;
  const cents = usdCents(usd);
  for (const [suffix, unit] of COMPACT) {
    if (cents >= unit * 100n) {
      // Hundredths of the suffix's unit, half up.
      const hundredths = (cents * 2n + unit) / (unit * 2n);
      return `$${hundredths / 100n}.${String(hundredths % 100n).padStart(2, '0')}${suffix}`;
    }
  }
  return full;
}

/**
 * A dollar amount made compact for a tile or an axis, never rounded up. Below a hundred thousand
 * dollars it is `$9,840.12`, then `$412k`, `$9.81M`, `$1.20B`, `$3.50T`. The exact figure goes in
 * the tooltip.
 */
export function usdCompact(usd: Rational): string {
  if (usd.num < 0n) return `-${usdCompact({ num: -usd.num, den: usd.den })}`;
  const cents = (usd.num * 100n) / usd.den; // floor
  const whole = cents / 100n;
  const text = (n: bigint, suffix: string) => `$${n / 100n}.${String(n % 100n).padStart(2, '0')}${suffix}`;
  if (whole >= 10n ** 12n) return text(cents / 10n ** 12n, 'T');
  if (whole >= 10n ** 9n) return text(cents / 10n ** 9n, 'B');
  if (whole >= 10n ** 6n) return text(cents / 10n ** 6n, 'M');
  if (whole >= 100_000n) return `$${whole / 1000n}k`;
  if (usd.num === 0n) return '$0.00';
  // Below a hundred thousand: floored to the cent from a dollar up, and significant digits below
  // that, as in `usdText`.
  return whole >= 1n ? `$${whole.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',')}.${String(cents % 100n).padStart(2, '0')}` : usdText(usd);
}

/** A KAS value (sompi) as a compact dollar amount for a tile or an axis (`usdCompact`). */
export const sompiToUsdCompact = (sompi: bigint, fiat: Fiat): string => usdCompact(sompiToUsd(sompi, fiat.rate));

/** The tooltip of a USD value whose KAS figure is `kasText`: the exact KAS and the rate used. */
export const kasTitleOf = (kasText: string, fiat: Fiat): string => `${kasText} KAS · ${rateTitle(fiat)}`;

// Words.

const EXCHANGE_NAMES: Record<string, string> = { bybit: 'Bybit', kucoin: 'KuCoin', gate: 'Gate', mexc: 'MEXC', bitget: 'Bitget', htx: 'HTX' };

/**
 * Where a quote came from, in words. The source is `coingecko` or `vwap:bybit,kucoin`, the
 * exchanges' volume-weighted KAS/USDT price with USDT taken as USD.
 */
export function sourceWords(source: string): { name: string; usdt: boolean } {
  if (source === 'coingecko') return { name: 'CoinGecko', usdt: false };
  if (source.startsWith('vwap:')) return { name: source.slice(5).split(',').map((n) => EXCHANGE_NAMES[n] ?? n).join(', '), usdt: true };
  return { name: source, usdt: false };
}

/** `under a minute`, `3 min` or `1 h 5 min`: how long ago, for the note. */
export function ageWords(ageS: number): string {
  const min = Math.floor(Math.max(0, ageS) / 60);
  if (min < 1) return t('fiat.age.under');
  if (min < 60) return t('fiat.age.min', { n: min });
  return t('fiat.age.hours', { h: Math.floor(min / 60), m: min % 60 });
}

/** The estimate sentence of a USD display, as a note on the page or a tooltip on a value. */
export function estimateNote(f: Fiat, ageS: number): string {
  const w = sourceWords(f.quote.source);
  const base = t(w.usdt ? 'fiat.note.usdt' : 'fiat.note', { source: w.name, age: ageWords(ageS) });
  return f.testnet ? `${base} ${t('fiat.note.testnet')}` : base;
}

/** The one-line muted notice of a fallback: why KAS is shown although USD was chosen. */
export function fallbackNotice(s: Extract<FiatState, { kind: 'fallback' }>): string {
  if (s.reason === 'loading') return t('fiat.fallback.loading');
  if (s.reason === 'stale') return t('fiat.fallback.stale', { age: ageWords(s.ageS ?? 0) });
  if (s.reason === 'unavailable') return t('fiat.fallback.unavailable');
  return t('fiat.fallback.missing');
}

/** The short tooltip tail of one USD value: the price it used. */
export const rateTitle = (f: Fiat): string => t('fiat.cell.rate', { price: usdText({ num: f.rate.num, den: f.rate.den }) });

/** A KAS value (`sompi`) as the Portfolio shows it: dollars when USD is on, else grouped KAS. */
export const kasValueText = (sompi: bigint, fiat: Fiat | null): string => (fiat ? usdText(sompiToUsd(sompi, fiat.rate)) : formatKas(sompi, { group: ',' }));

/** The tooltip of a value shown in USD: its exact KAS figure and the rate. None for KAS values. */
export const kasValueTitle = (sompi: bigint, fiat: Fiat | null): string | undefined => (fiat ? `${formatKas(sompi, { group: ',' })} KAS · ${rateTitle(fiat)}` : undefined);
