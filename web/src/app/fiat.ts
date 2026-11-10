// The optional USD display of KAS values (Settings, "Display currency"). Pure and DOM-free; the browser never asks a price API (the indexer serves the price,
// data/kas-price.ts), and this module only decides what to show:
//
//   * USD is shown for KAS-denominated VALUES the viewer reads (the Portfolio's summary and balances, the market list's price / volume / market cap / depth, the
//     market header's last price and 24 h stats). The order book, the ticket, the order and fill tables, the Activity feed and everything signed stay in KAS,
//     and a price the user types or signs is never converted.
//   * a token's USD value = its value in KAS (what the app already computes from the last price) x the KAS price. An estimate, said so everywhere it shows.
//   * the quote must be there and not older than `KAS_USD_MAX_AGE_S`: otherwise KAS is shown, with a notice (never a wrong or stale number, silently).
//   * the age is the larger of the clock's and the time since this browser first saw the quote's `at` (a wrong clock must not make an old quote look young).
//   * testnet KAS has no value: the USD is the MAINNET price, for display only, and the note says so.
import type { KasUsdQuote } from '../data/kas-price';
import { t } from '../i18n';
import { formatRational, significantDp, type Rational } from '../kob/pair';
import { formatKas, SOMPI_PER_KAS } from '../kob/units';
import type { DisplayCurrency } from './currency-pref';

/** A quote older than this (10 minutes) is not used: the display falls back to KAS. (Here, not in data/, so the UI kit's Amount can use this module.) */
export const KAS_USD_MAX_AGE_S = 10 * 60;

/** The KAS price as an exact rational (USD per KAS, nine decimals: the price is a display figure and the products below are exact on it). */
export interface Rate {
  usd: number;
  num: bigint;
  den: bigint;
}

const RATE_DEN = 1_000_000_000n;

/** The rate of a price in USD per KAS; null for a price that is not a positive finite number below the server's limit. */
export function rateOf(usd: number): Rate | null {
  if (!Number.isFinite(usd) || usd <= 0 || usd >= 1_000) return null;
  const num = BigInt(Math.round(usd * 1e9));
  return num > 0n ? { usd, num, den: RATE_DEN } : null;
}

/** What the views read when USD is in force: the rate, the quote it came from and whether the KAS is testnet's. */
export interface Fiat {
  rate: Rate;
  quote: KasUsdQuote;
  testnet: boolean;
}

/** The state of the feed the views see (app/kas-price.ts). */
export interface PriceFeed {
  quote: KasUsdQuote | null;
  /** the local time (ms) this `quote` (its `at`) was first received */
  seenAtMs: number | null;
  /** `loading`: not answered yet; `unavailable`: the indexer serves no price (404); `error`: the last read failed; `ok` */
  status: 'idle' | 'loading' | 'ok' | 'unavailable' | 'error';
}

export type FiatState =
  | { kind: 'kas' }
  | { kind: 'usd'; fiat: Fiat; ageS: number }
  | { kind: 'fallback'; reason: 'loading' | 'unavailable' | 'stale' | 'missing'; ageS: number | null };

/** Seconds since the quote was taken: the larger of the clock's age and the time since it was first received here, never negative. */
export function quoteAgeS(q: KasUsdQuote, seenAtMs: number | null, nowMs: number): number {
  const byClock = nowMs / 1000 - q.at;
  const bySeen = seenAtMs === null ? 0 : (nowMs - seenAtMs) / 1000;
  return Math.max(0, byClock, bySeen);
}

/** What to show: KAS (chosen), USD (chosen and the price is there and fresh), or KAS with the reason USD is not in force. */
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

/** The Fiat in force, or null (every view that shows a value takes it as "USD, or KAS when null"). */
export const fiatOf = (s: FiatState): Fiat | null => (s.kind === 'usd' ? s.fiat : null);

// ------------------------------------------------------------------------------------------------ conversion and text

/** `sompi` of KAS as a USD amount, exact on the rate. */
export const sompiToUsd = (sompi: bigint, rate: Rate): Rational => ({ num: sompi * rate.num, den: SOMPI_PER_KAS * rate.den });

/** KAS per whole token (a rational) as USD per whole token. */
export const kasPerTokenToUsd = (kas: Rational, rate: Rate): Rational => ({ num: kas.num * rate.num, den: kas.den * rate.den });

/** The USD amount as whole cents, half up. */
export const usdCents = (usd: Rational): bigint => (usd.num * 100n * 2n + usd.den) / (usd.den * 2n);

/**
 * `$1,066.77`: two decimals from a dollar up; below a dollar about four significant digits (`$0.0576`, `$0.00001234`), never fewer than two decimals and never
 * a zero for something that is there (`<$0.000000000001`). Thousands grouped.
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
 * `usdText`, with millions and up compact (`$56.61M`) once the text would be longer than `width` characters (a table cell never cuts a digit). Not the exact
 * figure: the tooltip has it.
 */
export function usdTextFit(usd: Rational, width: number): string {
  const full = usdText(usd);
  if (full.length <= width || usd.num < 0n) return full;
  const cents = usdCents(usd);
  for (const [suffix, unit] of COMPACT) {
    if (cents >= unit * 100n) {
      // hundredths of the suffix's unit, half up
      const hundredths = (cents * 2n + unit) / (unit * 2n);
      return `$${hundredths / 100n}.${String(hundredths % 100n).padStart(2, '0')}${suffix}`;
    }
  }
  return full;
}

/**
 * A dollar amount compact for a tile or an axis, never rounded up: `$9,840.12` below a hundred thousand dollars, then `$412k`, `$9.81M`, `$1.20B`, `$3.50T`.
 * (The exact figure goes in the tooltip.)
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
  // below a hundred thousand: floored to the cent from a dollar up, significant digits below (the text of `usdText`)
  return whole >= 1n ? `$${whole.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',')}.${String(cents % 100n).padStart(2, '0')}` : usdText(usd);
}

/** A KAS value (sompi) as a compact dollar amount for a tile or an axis (`usdCompact`). */
export const sompiToUsdCompact = (sompi: bigint, fiat: Fiat): string => usdCompact(sompiToUsd(sompi, fiat.rate));

/** The tooltip of a value shown in USD whose KAS figure is `kasText` (`35,000`): the exact KAS and the rate used. */
export const kasTitleOf = (kasText: string, fiat: Fiat): string => `${kasText} KAS · ${rateTitle(fiat)}`;

// ------------------------------------------------------------------------------------------------ words

const EXCHANGE_NAMES: Record<string, string> = { bybit: 'Bybit', kucoin: 'KuCoin', gate: 'Gate', mexc: 'MEXC', bitget: 'Bitget', htx: 'HTX' };

/** Where a quote came from, in words: `coingecko`, or `vwap:bybit,kucoin` (the exchanges' volume-weighted KAS/USDT price, USDT taken as USD). */
export function sourceWords(source: string): { name: string; usdt: boolean } {
  if (source === 'coingecko') return { name: 'CoinGecko', usdt: false };
  if (source.startsWith('vwap:')) return { name: source.slice(5).split(',').map((n) => EXCHANGE_NAMES[n] ?? n).join(', '), usdt: true };
  return { name: source, usdt: false };
}

/** `under a minute`, `3 min`, `1 h 5 min`: how long ago, for the note. */
export function ageWords(ageS: number): string {
  const min = Math.floor(Math.max(0, ageS) / 60);
  if (min < 1) return t('fiat.age.under');
  if (min < 60) return t('fiat.age.min', { n: min });
  return t('fiat.age.hours', { h: Math.floor(min / 60), m: min % 60 });
}

/** The estimate sentence of a USD display (a note on the page, a tooltip on a value). */
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

/** A KAS value (`sompi`) as the Portfolio shows it: dollars (`$1,066.77`) when USD is in force, else KAS with thousands grouped (no unit). */
export const kasValueText = (sompi: bigint, fiat: Fiat | null): string => (fiat ? usdText(sompiToUsd(sompi, fiat.rate)) : formatKas(sompi, { group: ',' }));

/** The tooltip of a value shown in USD: its exact KAS figure and the rate used; none while KAS is shown. */
export const kasValueTitle = (sompi: bigint, fiat: Fiat | null): string | undefined => (fiat ? `${formatKas(sompi, { group: ',' })} KAS · ${rateTitle(fiat)}` : undefined);
