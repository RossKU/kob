// Pure model of the issuance view: UI state (strings) -> `IssueForm`, the wallet's remaining share, display helpers and the result
// view model. No DOM, no i18n: rows carry i18n KEY SUFFIXES, the component translates them. Amount arithmetic is exact (bigint).
import {
  formatUnits, parseDecimal, registryEntryFromIssue, type IssueForm, type IssueLimits, type IssuePlan, type IssueProgramChoice, type IssueToken,
} from '../../kob/issue';
import {
  curveNumbers, curveSteps, curveTable, defaultSaleForm, defaultSalePrice, keepPercentText, parsePercent, parseSale, saleSplit,
  type CurveNumbers, type CurveStep, type ParsedSale, type SaleForm,
} from '../../kob/curve';
import type { PlanIssue } from '../../kob/plan-types';
import type { KobWasm } from '../../kob/wasm';
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
  /** the token program: the reference's published public-mint build (default) or the standalone 3 / 3 build */
  program: IssueProgramChoice;
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
  /** SHA-256 of the picture file, 64 hex (from a picked file, or typed in the advanced field); empty = none */
  imageHash: string;
  /** the name of the picture file the hash was computed from, '' when the hash was typed or there is none (display only) */
  imageFile: string;
  website: string;
  /** X (Twitter) and Telegram accounts: a handle or an address */
  x: string;
  telegram: string;
  /** "After creation": keep all, sell at one price, sell on a curve */
  sale: SaleUi;
}

/**
 * The sale as typed. "Keep in my wallet" and the amount for sale are linked: the field typed last (`lead`) is kept and the other one derived
 * from the wallet's share, so a later change of the supply moves the derived one.
 */
export interface SaleUi extends SaleForm {
  lead: 'keep' | 'amount';
}

export function emptySaleUi(): SaleUi {
  return { ...defaultSaleForm(), lead: 'keep' };
}

export function emptyUiState(): IssueUiState {
  return { program: 'public-mint', name: '', ticker: '', decimals: '8', supply: '', extras: [], carrier: '', description: '', icon: '', imageHash: '', imageFile: '', website: '', x: '', telegram: '', sale: emptySaleUi() };
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
    program: state.program,
    name: state.name,
    ticker: state.ticker,
    decimals: parseDecimalsText(state.decimals),
    supply: state.supply.trim(),
    holders,
    carrier: state.carrier.trim(),
    description: state.description,
    icon: state.icon,
    imageHash: state.imageHash,
    website: state.website,
    ...(state.x.trim() ? { x: state.x.trim() } : {}),
    ...(state.telegram.trim() ? { telegram: state.telegram.trim() } : {}),
    ...(state.sale.mode !== 'keep' ? { sale: saleFormOf(state.sale) } : {}),
  };
}

/** The planner's sale: the amount only when it was typed last (else the keep percent decides). */
export function saleFormOf(u: SaleUi): SaleForm {
  const { lead, ...f } = u;
  return { ...f, amount: lead === 'amount' ? f.amount.trim() : '' };
}

/** The wallet's share of the supply, base units (null when unknown): what a sale takes from. */
export function saleShareOf(state: IssueUiState): bigint | null {
  const s = sharesOf(state);
  if (s.remaining === null || s.remaining <= 0n || !s.othersComplete) return null;
  return s.remaining;
}

/** The two linked fields as shown: the one typed last as typed, the other derived. */
export function linkedSaleFields(state: IssueUiState): { keepPct: string; amount: string } {
  const u = state.sale;
  const share = saleShareOf(state);
  const d = parseDecimalsText(state.decimals);
  if (share === null || Number.isNaN(d)) return { keepPct: u.keepPct, amount: u.amount };
  if (u.lead === 'amount') {
    const a = parseDecimal(u.amount, d);
    return { keepPct: 'value' in a ? keepPercentText(share, a.value) : u.keepPct, amount: u.amount };
  }
  const k = parsePercent(u.keepPct);
  return { keepPct: u.keepPct, amount: 'bps' in k ? formatUnits(saleSplit(share, k.bps).sell, d) : u.amount };
}

/** What the sale section shows while the form is typed: the split, and for a curve its table, steps and numbers. */
export interface SalePreview {
  issues: PlanIssue[];
  parsed: ParsedSale | null;
  /** whole tokens, grouped */
  sellText: string;
  keepText: string;
  curve: { steps: CurveStep[]; numbers: CurveNumbers } | null;
  /** a curve that could not be built (kob-wasm's reason) */
  curveError: string | null;
  /** the one-price default (sompi per whole token) for the amount */
  defaultPrice: bigint | null;
}

export function salePreview(kob: KobWasm, state: IssueUiState): SalePreview | null {
  if (state.sale.mode === 'keep') return null;
  const d = parseDecimalsText(state.decimals);
  const share = saleShareOf(state);
  if (Number.isNaN(d)) return { issues: [], parsed: null, sellText: '', keepText: '', curve: null, curveError: null, defaultPrice: null };
  const form = saleFormOf(state.sale);
  // the price field may be blank: the default is shown in it, so the preview uses it
  const sellOnly = parseSale({ ...form, mode: 'ask', price: '1' }, share, d).parsed;
  const defaultPrice = sellOnly ? defaultSalePrice(sellOnly.sell, 10n ** BigInt(d)) : null;
  const { issues, parsed } = parseSale(form.mode === 'ask' && form.price.trim() === '' && defaultPrice ? { ...form, price: formatUnits(defaultPrice, 8) } : form, share, d);
  const scale = 10n ** BigInt(d);
  let curve: SalePreview['curve'] = null;
  let curveError: string | null = null;
  if (parsed && parsed.mode === 'curve') {
    try {
      const t = curveTable(kob, { amount: parsed.sell, levels: parsed.levels, total: parsed.total, multipleMilli: parsed.multipleMilli, scale });
      curve = { steps: curveSteps(t.levelSize, t.table, parsed.sell), numbers: curveNumbers(t, parsed.sell, scale) };
    } catch (e) {
      curveError = e instanceof Error ? e.message : String(e);
    }
  }
  return {
    issues, parsed, curve, curveError, defaultPrice,
    sellText: parsed ? humanAmount(parsed.sell, d) : '',
    keepText: parsed ? humanAmount(parsed.keep, d) : '',
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

/** `<ticker>.registry-entry.json`: the token's facts in the format of a `registry/tokens.json` entry (for tools and community lists; no listing request). */
export const registryEntryFileName = (ticker: string): string => `${safeName(ticker)}.registry-entry.json`;
export const supplyDocFileName = (ticker: string): string => `${safeName(ticker)}.supply.json`;
export const metadataDocFileName = (ticker: string): string => `${safeName(ticker)}.metadata.json`;
export const proofDocFileName = (ticker: string): string => `${safeName(ticker)}.launch-proof.json`;

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
  /** the custody of the curve this genesis places */
  isCurve?: boolean;
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

export function holderViews(token: Pick<IssueToken, 'outputs' | 'decimals'>, walletKey: Hex | null, curveId?: Hex | null): HolderView[] {
  const me = walletKey?.toLowerCase() ?? null;
  return token.outputs.map((o) => ({
    ...(curveId && o.ownerScheme === 4 && o.owner === curveId ? { isCurve: true } : {}),
    index: o.index,
    owner: o.owner,
    ownerShort: shortId(o.owner, 14),
    ownerScheme: o.ownerScheme,
    amount: BigInt(o.amount),
    human: humanAmount(o.amount, token.decimals),
    isWallet: o.ownerScheme === 0 && o.owner.toLowerCase() === me,
  }));
}

/** What the review shows before the pre-sign screen: from the plan, so the numbers are the ones that will be signed. */
export function reviewSummary(plan: Pick<IssuePlan, 'token' | 'fee' | 'curve'>, walletKey: Hex | null): ReviewSummary {
  const { token } = plan;
  const supply = BigInt(token.supply);
  const carrierEach = BigInt(token.carrier);
  return {
    name: token.name,
    ticker: token.ticker,
    decimals: token.decimals,
    supplyBase: supply,
    supplyHuman: humanAmount(supply, token.decimals),
    holders: holderViews(token, walletKey, plan.curve?.covenantId ?? null),
    carrierEach,
    carrierTotal: carrierEach * BigInt(token.outputs.length) + (plan.curve ? BigInt(plan.curve.carrier) : 0n),
    fee: plan.fee,
    program: token.program,
    outputCount: token.outputs.length + (plan.curve ? 1 : 0),
  };
}

export interface ResultRow {
  /** i18n key suffix under `issue.result.` and test id suffix (`issue-result-<id>`) */
  id: 'tokenId' | 'templateHash' | 'extension' | 'txid' | 'prepTxid' | 'program';
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
  /** the KCC-23 launch proof (null: the docs carry none) */
  proofJson: string | null;
  proofFile: string;
}

/** The id of the creation's KCC-23 preparation, from supply.json (`kcc23.preparation_txid`). */
function prepTxidOf(docs: IssuePlan['docs']): string | null {
  const k = (docs.supply as { kcc23?: { preparation_txid?: unknown } }).kcc23?.preparation_txid;
  return typeof k === 'string' && /^[0-9a-f]{64}$/.test(k) ? k : null;
}

/** Rows and file contents of the result panel. Throws when the docs do not describe the issued token (`registryEntryFromIssue`). */
export function resultViewModel(plan: Pick<IssuePlan, 'token' | 'docs'>, txid: string, walletKey: Hex | null = null): ResultViewModel {
  const entry = registryEntryFromIssue(plan.token, plan.docs);
  return {
    rows: [
      { id: 'tokenId', value: plan.token.covenantId, copy: true },
      { id: 'txid', value: txid, copy: true },
      ...(prepTxidOf(plan.docs) ? [{ id: 'prepTxid' as const, value: prepTxidOf(plan.docs)!, copy: true }] : []),
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
    proofJson: plan.docs.launchProof ? jsonText(plan.docs.launchProof) : null,
    proofFile: proofDocFileName(plan.token.ticker),
  };
}
