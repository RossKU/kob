// Minimal typed i18n (English only). No runtime dependency.
//
// The dictionary is split by UI area: `src/i18n/en/<area>.ts`, each `export default { 'area.key': 'text with {param}' }`-style object with FLAT dotted
// keys prefixed by the area name. All files are merged at build time with `import.meta.glob`, so adding an area never touches shared files.
//
// Placeholders: `{name}`; plural-free by design (sentences are phrased with counts as parameters). Use `formatNumber/formatDateTime` for locale-aware output.

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

/** Translates a finding `{code, message, params}` from the planning / decoding layers: key `<prefix>.<code>`, the finding's English `message` as the last resort. */
export function tIssue(prefix: string, issue: { code: string; message: string; params?: Params }): string {
  const key = `${prefix}.${issue.code}`;
  // `{message}` is the finding's own (English, technical) message when the params do not name one: a builder refusal carries only that
  if (has(key)) return t(key, { message: issue.message, ...issue.params });
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
