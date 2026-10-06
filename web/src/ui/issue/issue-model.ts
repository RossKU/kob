// Pure model of the issuance view: UI state (strings) -> `IssueForm`, the wallet's remaining share, display helpers and the result
// view model. No DOM, no i18n: rows carry i18n KEY SUFFIXES, the component translates them. Amount arithmetic is exact (bigint).
import { formatUnits, parseDecimal, registryEntryFromIssue, type IssueForm, type IssueLimits, type IssuePlan, type IssueToken } from '../../kob/issue';
import type { PlanIssue } from '../../kob/plan-types';
import type { Hex } from '../../kob/types';
import { groupDigits, shortId } from '../kit/format';

// ------------------------------------------------------------------------------------------------ UI state

/** An additional holder typed by the user. The wallet's own row is not stored: it is derived (supply minus the others). */
export interface HolderRow {
  /** 64 hex characters: x-only public key of the receiving wallet */
  owner: string;
  /** whole tokens, decimal text */
  amount: string;
}

/** Everything the form holds, as the user typed it. */
export interface IssueUiState {
  name: string;
  ticker: string;
  /** whole number as text ("8") so an emptied field is representable */
  decimals: string;
  supply: string;
  /** additional holders; empty = the whole supply goes to the connected wallet */
  extras: HolderRow[];
  /** KAS per token output; empty = the default of the protocol limits */
  carrier: string;
  description: string;
  icon: string;
  website: string;
}

export function emptyUiState(): IssueUiState {
  return { name: '', ticker: '', decimals: '8', supply: '', extras: [], carrier: '', description: '', icon: '', website: '' };
}

// ------------------------------------------------------------------------------------------------ small filters

/** The ticker input: capitals as typed, whitespace dropped (other characters are kept so the validator can explain them). */
export function filterTicker(input: string, maxChars = 32): string {
  return input.replace(/\s+/g, '').toUpperCase().slice(0, maxChars);
}

/** The decimals input: digits only (at most 2), so "8" stays a number and paste of "8.0" cannot sneak in a fraction. */
export function filterDecimals(input: string): string {
  return input.replace(/\D+/g, '').slice(0, 2);
}

/** A hex key typed or pasted: trimmed and lower-cased (the protocol wants lower-case hex). */
export function filterOwner(input: string): string {
  return input.replace(/\s+/g, '').toLowerCase().slice(0, 64);
}

/** Decimals text -> number; NaN when empty or invalid (the validator reports `decimals.invalid` for NaN). */
export function parseDecimalsText(text: string): number {
  return /^\d{1,2}$/.test(text.trim()) ? Number(text.trim()) : Number.NaN;
}

// ------------------------------------------------------------------------------------------------ shares

export interface Shares {
  /** the total supply in base units; null when the supply or the decimals do not parse */
  supply: bigint | null;
  /** sum of the extra holders' parsable, positive amounts */
  others: bigint;
  /** supply - others; null when the supply is unknown. Negative = the others exceed the supply (an error). */
  remaining: bigint | null;
  /** an extra row's amount does not parse (its own validation message explains) */
  othersComplete: boolean;
}

/** The wallet's share = the supply minus what the extra holders receive, all in base units. */
export function sharesOf(state: IssueUiState): Shares {
  const decimals = parseDecimalsText(state.decimals);
  const ps = Number.isNaN(decimals) ? ({ error: 'syntax' } as const) : parseDecimal(state.supply, decimals);
  const supply = 'value' in ps ? ps.value : null;
  let others = 0n;
  let othersComplete = true;
  for (const h of state.extras) {
    const pa = Number.isNaN(decimals) ? ({ error: 'syntax' } as const) : parseDecimal(h.amount, decimals);
    if ('value' in pa) others += pa.value;
    else othersComplete = false;
  }
  return { supply, others, remaining: supply === null ? null : supply - others, othersComplete };
}

/** True when the wallet's own row is part of the holder list (it is left out when the others take the whole supply). */
export function walletRowIncluded(state: IssueUiState): boolean {
  if (state.extras.length === 0) return false;
  const s = sharesOf(state);
  return s.remaining === null || s.remaining !== 0n || !s.othersComplete;
}

/** Remaining share as decimal text of whole tokens ("" when unknown or negative). */
export function remainingText(state: IssueUiState): string {
  const s = sharesOf(state);
  const decimals = parseDecimalsText(state.decimals);
  if (s.remaining === null || s.remaining < 0n || Number.isNaN(decimals)) return '';
  return formatUnits(s.remaining, decimals);
}

// ------------------------------------------------------------------------------------------------ UI state -> IssueForm

/**
 * Maps the UI state to the planner's `IssueForm`. With no extra holders the list is empty (protocol default: everything to the maker).
 * Otherwise the wallet is holder 0 with the remaining amount, followed by the extras (scheme 0 = a wallet key). A wallet share of zero
 * drops the wallet row; a negative one becomes "0" so validation fails with a holder amount error (the view adds `holders.exceeds`).
 */
export function formToIssueForm(state: IssueUiState, walletKey: Hex | null): IssueForm {
  const holders: IssueForm['holders'] = [];
  if (state.extras.length > 0) {
    if (walletRowIncluded(state)) {
      const rem = remainingText(state);
      holders.push({ owner: (walletKey ?? '').toLowerCase(), ownerScheme: 0, amount: rem === '' ? '0' : rem });
    }
    for (const h of state.extras) holders.push({ owner: h.owner.trim().toLowerCase(), ownerScheme: 0, amount: h.amount.trim() });
  }
  return {
    name: state.name,
    ticker: state.ticker,
    decimals: parseDecimalsText(state.decimals),
    supply: state.supply.trim(),
    holders,
    carrier: state.carrier.trim(),
    description: state.description,
    icon: state.icon,
    website: state.website,
  };
}

/** Findings only the view model knows (they are translated under `issue.*`, not `issues.*`). */
export interface ModelIssue {
  code: 'holders.exceeds' | 'holders.duplicate';
  severity: 'error' | 'warning';
  params?: Record<string, string>;
}

export function modelIssues(state: IssueUiState, walletKey: Hex | null, decimalsFallback = 0): ModelIssue[] {
  const out: ModelIssue[] = [];
  const s = sharesOf(state);
  if (s.remaining !== null && s.remaining < 0n) {
    const d = Number.isNaN(parseDecimalsText(state.decimals)) ? decimalsFallback : parseDecimalsText(state.decimals);
    out.push({ code: 'holders.exceeds', severity: 'error', params: { others: formatUnits(s.others, d), supply: formatUnits(s.supply ?? 0n, d) } });
  }
  if (walletKey) {
    const me = walletKey.toLowerCase();
    if (state.extras.some((h) => h.owner.trim().toLowerCase() === me)) out.push({ code: 'holders.duplicate', severity: 'warning' });
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ issue field -> input

/**
 * Where an `IssueForm` finding belongs in the UI: a top-level field name ("name", "ticker", ...), `holder-<row>-owner|amount` with the UI
 * row (row 0 = the wallet's own share, extras are 1..n), `holders` (the list as a whole) or null (general, no field).
 */
export function uiFieldOf(field: string | undefined, walletRow: boolean): string | null {
  if (!field) return null;
  const m = /^holders\[(\d+)\]\.(\w+)$/.exec(field);
  if (m) {
    const row = Number(m[1]) + (walletRow ? 0 : 1);
    return `holder-${row}-${m[2] === 'owner' ? 'owner' : m[2] === 'amount' ? 'amount' : 'other'}`;
  }
  return field;
}

/** Groups findings by UI field ("" = general), keeping order. */
export function groupByField<T extends PlanIssue>(issues: T[], walletRow: boolean): Map<string, T[]> {
  const map = new Map<string, T[]>();
  for (const i of issues) {
    const key = uiFieldOf(i.field, walletRow) ?? '';
    const list = map.get(key) ?? [];
    list.push(i);
    map.set(key, list);
  }
  return map;
}

/** Exact KAS missing for the issuance, from the `funds.insufficient` finding (`need` = carriers, `have` = spendable). */
export function shortfallOf(issue: Pick<PlanIssue, 'code' | 'params'>): { need: bigint; have: bigint; missing: bigint } | null {
  if (issue.code !== 'funds.insufficient' || !issue.params) return null;
  try {
    const need = BigInt(issue.params.need as string | number | bigint);
    const have = BigInt(issue.params.have as string | number | bigint);
    return { need, have, missing: need > have ? need - have : 0n };
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------------------------------------ display helpers

/** Token amount in human units with digit grouping: 100000000000 at 8 decimals -> "1,000". */
export function humanAmount(base: bigint | string, decimals: number): string {
  return groupDigits(formatUnits(BigInt(base), decimals));
}

/** The supply hint under the input: "1,000,000 tokens (100000000000000 base units)"; null when it does not parse. */
export function supplyPreview(state: IssueUiState): { human: string; base: string } | null {
  const s = sharesOf(state);
  const decimals = parseDecimalsText(state.decimals);
  if (s.supply === null || Number.isNaN(decimals) || s.supply <= 0n) return null;
  return { human: humanAmount(s.supply, decimals), base: s.supply.toString() };
}

/** The default carrier shown as placeholder: "10". */
export function defaultCarrierText(limits: Pick<IssueLimits, 'defaultCarrier'>): string {
  return formatUnits(BigInt(limits.defaultCarrier), 8);
}

// ------------------------------------------------------------------------------------------------ file names and JSON

const safeName = (ticker: string): string => ticker.replace(/[^A-Za-z0-9_-]/g, '_') || 'token';

/** `<ticker>.registry-entry.json`: the entry to add to `registry/tokens.json` by pull request. */
export const registryEntryFileName = (ticker: string): string => `${safeName(ticker)}.registry-entry.json`;
export const supplyDocFileName = (ticker: string): string => `${safeName(ticker)}.supply.json`;
export const metadataDocFileName = (ticker: string): string => `${safeName(ticker)}.metadata.json`;

/** Pretty JSON with a trailing newline (files end with one). */
export const jsonText = (value: unknown): string => `${JSON.stringify(value, null, 2)}\n`;

// ------------------------------------------------------------------------------------------------ review + result

export interface HolderView {
  index: number;
  owner: string;
  ownerShort: string;
  ownerScheme: number;
  /** base units */
  amount: bigint;
  /** whole tokens with grouping */
  human: string;
  isWallet: boolean;
}

export interface ReviewSummary {
  name: string;
  ticker: string;
  decimals: number;
  supplyBase: bigint;
  supplyHuman: string;
  holders: HolderView[];
  /** KAS (sompi) locked in each token output and in total */
  carrierEach: bigint;
  carrierTotal: bigint;
  /** network fee (sompi) */
  fee: bigint;
  program: string;
  outputCount: number;
}

export function holderViews(token: Pick<IssueToken, 'outputs' | 'decimals'>, walletKey: Hex | null): HolderView[] {
  const me = walletKey?.toLowerCase() ?? null;
  return token.outputs.map((o) => ({
    index: o.index,
    owner: o.owner,
    ownerShort: shortId(o.owner, 8, 6),
    ownerScheme: o.ownerScheme,
    amount: BigInt(o.amount),
    human: humanAmount(o.amount, token.decimals),
    isWallet: o.ownerScheme === 0 && o.owner.toLowerCase() === me,
  }));
}

/** What the review shows before the pre-sign screen: from the plan, so the numbers are the ones that will be signed. */
export function reviewSummary(plan: Pick<IssuePlan, 'token' | 'fee'>, walletKey: Hex | null): ReviewSummary {
  const { token } = plan;
  const supply = BigInt(token.supply);
  const carrierEach = BigInt(token.carrier);
  return {
    name: token.name,
    ticker: token.ticker,
    decimals: token.decimals,
    supplyBase: supply,
    supplyHuman: humanAmount(supply, token.decimals),
    holders: holderViews(token, walletKey),
    carrierEach,
    carrierTotal: carrierEach * BigInt(token.outputs.length),
    fee: plan.fee,
    program: token.program,
    outputCount: token.outputs.length,
  };
}

export interface ResultRow {
  /** i18n key suffix under `issue.result.` and test id suffix (`issue-result-<id>`) */
  id: 'tokenId' | 'templateHash' | 'extension' | 'txid' | 'program';
  value: string;
  /** offer a copy button */
  copy: boolean;
}

export interface ResultViewModel {
  rows: ResultRow[];
  holders: HolderView[];
  registryEntryJson: string;
  registryEntryFile: string;
  supplyJson: string;
  supplyFile: string;
  metadataJson: string;
  metadataFile: string;
  /** the result panel always says: unverified, not in the registry yet (pending review once merged) */
  status: { verified: false; pendingReview: true };
}

/** Rows and file contents of the result panel. Throws when the docs do not describe the issued token (`registryEntryFromIssue`). */
export function resultViewModel(plan: Pick<IssuePlan, 'token' | 'docs'>, txid: string, walletKey: Hex | null = null): ResultViewModel {
  const entry = registryEntryFromIssue(plan.token, plan.docs);
  return {
    rows: [
      { id: 'tokenId', value: plan.token.covenantId, copy: true },
      { id: 'txid', value: txid, copy: true },
      { id: 'program', value: plan.token.program, copy: false },
      { id: 'templateHash', value: plan.token.templateHash, copy: true },
      { id: 'extension', value: plan.token.extensionCommitment, copy: true },
    ],
    holders: holderViews(plan.token, walletKey),
    registryEntryJson: jsonText(entry),
    registryEntryFile: registryEntryFileName(plan.token.ticker),
    supplyJson: jsonText(plan.docs.supply),
    supplyFile: supplyDocFileName(plan.token.ticker),
    metadataJson: jsonText(plan.docs.metadata),
    metadataFile: metadataDocFileName(plan.token.ticker),
    status: { verified: false, pendingReview: true },
  };
}
