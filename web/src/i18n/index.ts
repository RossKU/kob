// Minimal typed i18n (English only). No runtime dependency.
//
// The dictionary is split by UI area: `src/i18n/en/<area>.ts`, each `export default { 'area.key': 'text with {param}' }`-style object with FLAT dotted
// keys prefixed by the area name. All files are merged at build time with `import.meta.glob`, so adding an area never touches shared files.
//
// Placeholders: `{name}`; plural-free by design (sentences are phrased with counts as parameters). Use `formatNumber/formatDateTime` for locale-aware output.

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

/** Translates `key`. Falls back to the key itself (visible in the UI and caught by the completeness test). */
export function t(key: string, params?: Params): string {
  const raw = dictionary[key] ?? key;
  if (!params) return raw;
  return raw.replace(/\{(\w+)\}/g, (m, name: string) => (name in params ? String(params[name]) : m));
}

/** The decimals assumed for a token nothing states the decimals of: the Kaspa convention (1e8 base units = 1 token, like KAS and sompi). */
export const ASSUMED_TOKEN_DECIMALS = 8;

let tokenAmountFormatter: ((token: string | null, units: bigint) => string) | null = null;

/** The app's words for an amount of a token (app/token-display.ts: its registry decimals, this browser's issuance of it, else 8 assumed). */
export function setTokenAmountFormatter(f: ((token: string | null, units: bigint) => string) | null): void {
  tokenAmountFormatter = f;
}

/**
 * An amount of token `token` (covenant id; null: not named) given in the chain's integer units, in whole tokens with its name: never "base
 * units" (founder decision 2026-10-17). Without the app's formatter (tests, a sentence about no particular token): 8 decimals assumed, said so.
 */
export function tokenAmountText(token: string | null | undefined, units: bigint): string {
  if (tokenAmountFormatter) return tokenAmountFormatter(token ?? null, units);
  return t('token.amount.plain', { amount: formatTokenAmount(units, ASSUMED_TOKEN_DECIMALS, { group: ',' }), decimals: ASSUMED_TOKEN_DECIMALS });
}

/** Findings whose params carry token amounts in the chain's integer units (said in whole tokens, of the `token` param when there is one). */
const TOKEN_AMOUNT_CODES = new Set(['cancel.stray-other-extension', 'cancel.strays-abandoned', 'cancel.stray-other-token', 'cancel.top-up', 'cancel.insufficient-tokens']);
const TOKEN_AMOUNT_PARAMS = ['amount', 'need', 'have', 'short'];

/** Translates a finding `{code, message, params}` from the planning / decoding layers: key `<prefix>.<code>`, the finding's English `message` as the last resort. */
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
  // `{message}` is the finding's own (English, technical) message when the params do not name one: a builder refusal carries only that
  if (has(key)) return t(key, { message: issue.message, ...params });
  return issue.message;
}

export function formatNumber(n: number | bigint, opts?: Intl.NumberFormatOptions): string {
  return new Intl.NumberFormat(LOCALE, opts).format(n);
}

export function formatDateTime(unixSeconds: number | bigint, opts?: Intl.DateTimeFormatOptions): string {
  return new Intl.DateTimeFormat(LOCALE, { dateStyle: 'medium', timeStyle: 'short', ...opts }).format(new Date(Number(unixSeconds) * 1000));
}

/** Test helper: every key of the dictionary (for the completeness test). */
export function allKeys(): string[] {
  return Object.keys(dictionary);
}
export function rawEntry(key: string): string | undefined {
  return dictionary[key];
}
