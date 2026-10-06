// Builder (kob-wasm / kob-protocol) and node refusals in the user's words (C5 R-3). The raw English text of a refusal is a developer message
// ("token-holding order: custody token UTXO required"): the UI shows a translated sentence for the refusals a wallet user can meet and keeps
// the raw text behind "Details" (kit `RawDetails`). Unknown refusals get a generic sentence; nothing raw is shown inline.
// DOM-free: runs in node tests.
import { t, type Params } from './index';

interface Rule {
  re: RegExp;
  code: string;
  params?: (m: RegExpExecArray) => Params;
}

/** First match wins. Codes are `build.<code>` keys of en/build.ts. */
const RULES: Rule[] = [
  { re: /insufficient funds: need (\d+) sompi, have (\d+) sompi/i, code: 'insufficient-funds' },
  { re: /not refundable before DAA (\d+)/i, code: 'not-yet-refundable', params: (m) => ({ daa: m[1] }) },
  { re: /custody token UTXO required|has no custody token|custody is required|custody token UTXO is not on the node/i, code: 'custody-missing' },
  { re: /mix extension commitments|another extension commitment/i, code: 'mixed-extension' },
  { re: /is not a supported KCC-20 or KRON program|prefix\/suffix lengths do not match|owner scheme .* is not supported/i, code: 'unsupported-token' },
  { re: /token inputs hold \d+ <|needs? .*units; custody, strays and top-up hold|the replacement needs tokens|need token inputs/i, code: 'not-enough-tokens' },
  { re: /\bmass\b/i, code: 'too-large' },
  { re: /is not active yet/i, code: 'not-active' },
  { re: /no covenant id|is not of this token|not owned by this/i, code: 'wrong-utxo' },
  { re: /already spent|input .* (is )?(spent|missing)|orphan|double spend/i, code: 'spent' },
  { re: /must be positive|must be >= 0|would be negative|min(imum)? ?fill|scale must/i, code: 'invalid-terms' },
  { re: /expir|lock ?time|deadline/i, code: 'timing' },
];

export interface BuildErrorClass {
  code: string;
  params: Params;
}

/** The kind of a raw refusal (`other` when it is not one a wallet user is expected to meet). */
export function classifyBuildError(raw: string): BuildErrorClass {
  const text = raw.replace(/^kob-wasm [a-z]+: (invalid request: )?/i, '');
  for (const r of RULES) {
    const m = r.re.exec(text);
    if (m) return { code: r.code, params: r.params ? r.params(m) : {} };
  }
  return { code: 'other', params: {} };
}

/** A translated sentence for a raw refusal (the raw text itself belongs behind "Details"). */
export function buildErrorText(raw: string): string {
  const c = classifyBuildError(raw);
  return t(`build.${c.code}`, c.params);
}

/**
 * Finding codes whose param carries a raw builder / node text, and which param: the sentence gets the translated `buildErrorText` in its
 * place and the raw text is shown behind "Details".
 */
export const RAW_PARAM: Readonly<Record<string, string>> = {
  BUILD_REJECTED: 'reason',
  PLAN_MISMATCH: 'reason',
  'cancel.build-failed': 'message',
};

/** The raw builder / node text a finding carries (null for findings that have none). */
export function rawOf(issue: { code: string; message: string; params?: Params }): string | null {
  const p = RAW_PARAM[issue.code];
  if (!p) return null;
  const v = issue.params?.[p];
  return v !== undefined ? String(v) : issue.message;
}

/** The finding with its raw param replaced by the translated sentence (for display; `rawOf` of the original goes behind "Details"). */
export function withoutRaw<I extends { code: string; message: string; params?: Params }>(issue: I): I {
  const p = RAW_PARAM[issue.code];
  const raw = rawOf(issue);
  if (!p || raw === null) return issue;
  const text = buildErrorText(raw);
  return { ...issue, message: p === 'message' ? text : issue.message, params: { ...issue.params, [p]: text } };
}
