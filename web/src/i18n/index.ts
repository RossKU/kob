// Minimal typed i18n (English only). No runtime dependency.
//
// The dictionary is split by UI area, in `src/i18n/en/<area>.ts`. Each file exports an object such
// as `{ 'area.key': 'text with {param}' }` with flat dotted keys prefixed by the area name. All
// files are merged at build time with `import.meta.glob`, so adding an area never touches shared
// files.
//
// Placeholders are `{name}`. The dictionary is plural-free by design, and sentences are phrased
// with counts as parameters. Use `formatNumber` for locale-aware numbers. A time is the kit's
// (`formatRowTime`, ui/kit/format.ts), in 24-hour form and one order everywhere.

import { formatTokenAmount } from '../kob/units';

export type Params = Record<string, string | number | bigint>;

type Dict = Record<string, string>;

function merge(mods: Record<string, unknown>): Dict {
  const out: Dict = {};
  for (const m of Object.values(mods)) Object.assign(out, (m as { default: Dict }).default);
  return out;
}

const dictionary: Dict = merge(import.meta.glob('./en/*.ts', { eager: true }));

const LOCALE = 'en-US';

export function has(key: string): boolean {
  return key in dictionary;
}

/**
 * Translates `key`. Falls back to the key itself (visible in the UI and caught by the completeness
 * test).
 */
export function t(key: string, params?: Params): string {
  const raw = dictionary[key] ?? key;
  if (!params) return raw;
  return raw.replace(/\{(\w+)\}/g, (m, name: string) => (name in params ? String(params[name]) : m));
}

/**
 * The decimals of a price in the market list when neither the token nor its market names any. They
 * apply only when an indexer reports no market scale.
 */
export const ASSUMED_TOKEN_DECIMALS = 8;
/**
 * Used where nothing states a token's decimals: no declaration, no program default, and no scale of
 * an order or a market. None are invented, and the chain's integer amount is shown as it is, with a
 * note.
 */
export const RAW_TOKEN_DECIMALS = 0;

let tokenAmountFormatter: ((token: string | null, units: bigint) => string) | null = null;

/**
 * The app's words for an amount of a token (app/token-display.ts). It uses the decimals when
 * something states them, else the raw amount, with a note.
 */
export function setTokenAmountFormatter(f: ((token: string | null, units: bigint) => string) | null): void {
  tokenAmountFormatter = f;
}

/**
 * An amount of token `token` (a covenant id, or null when not named) given in the chain's integer
 * units. It is shown in whole tokens with its name where something states its decimals. Without
 * the app's formatter, in tests or in a sentence about no particular token, it shows the raw
 * amount, with a note.
 */
export function tokenAmountText(token: string | null | undefined, units: bigint): string {
  if (tokenAmountFormatter) return tokenAmountFormatter(token ?? null, units);
  return t('token.amount.plain', { amount: formatTokenAmount(units, RAW_TOKEN_DECIMALS, { group: ',' }) });
}

/**
 * Findings whose params carry token amounts in the chain's integer units. They are said in whole
 * tokens, of the `token` param when there is one.
 */
const TOKEN_AMOUNT_CODES = new Set(['cancel.stray-other-extension', 'cancel.strays-abandoned', 'cancel.stray-other-token', 'cancel.top-up', 'cancel.insufficient-tokens']);
const TOKEN_AMOUNT_PARAMS = ['amount', 'need', 'have', 'short'];

/**
 * Translates a finding `{code, message, params}` from the planning and decoding layers. The key is
 * `<prefix>.<code>`, and the finding's English `message` is the last resort.
 */
export function tIssue(prefix: string, issue: { code: string; message: string; params?: Params }): string {
  const key = `${prefix}.${issue.code}`;
  let params = issue.params;
  if (params && TOKEN_AMOUNT_CODES.has(issue.code)) {
    const token = typeof params.token === 'string' ? params.token : null;
    params = { ...params };
    for (const n of TOKEN_AMOUNT_PARAMS) {
      const v = params[n];
      if (v !== undefined && /^\d+$/.test(String(v))) params[n] = tokenAmountText(token, BigInt(v));
    }
  }
  // `{message}` is the finding's own English, technical message when the params do not name one. A
  // builder refusal carries only that.
  if (has(key)) return t(key, { message: issue.message, ...params });
  return issue.message;
}

export function formatNumber(n: number | bigint, opts?: Intl.NumberFormatOptions): string {
  return new Intl.NumberFormat(LOCALE, opts).format(n);
}

/** Test helper: every key of the dictionary, for the completeness test. */
export function allKeys(): string[] {
  return Object.keys(dictionary);
}
export function rawEntry(key: string): string | undefined {
  return dictionary[key];
}
