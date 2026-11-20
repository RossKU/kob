// Pure model of the issuance view: UI state (strings) to `IssueForm`, the wallet's remaining
// share, display helpers and the result view model. It has no DOM and no i18n. Rows carry i18n KEY
// SUFFIXES and the component translates them. Amount arithmetic is exact (bigint).
import {
  formatUnits, parseDecimal, registryEntryFromIssue, type IssueForm, type IssueLimits, type IssuePlan, type IssueProgramChoice, type IssueToken,
} from '../../kob/issue';
import {
  curveNumbers, curveSpecOfSale, curveSteps, curveTable, defaultSaleForm, defaultSalePrice, keepPercentText, parsePercent, parseSale, saleSplit,
  type CurveNumbers, type CurveStep, type ParsedSale, type SaleForm,
} from '../../kob/curve';
import type { PlanIssue } from '../../kob/plan-types';
import type { KobWasm } from '../../kob/wasm';
import { curveNumbersOfState } from '../../kob/curve';
import type { CreationRecord } from '../../data/creations';
import type { IssueLock, PlannedSale } from '../../kob/issue';
import type { Hex } from '../../kob/types';
import { storedVerdict } from '../../wallet/covenant-probe';
import type { WalletAdapter } from '../../wallet/types';
import { unlockSigner } from '../orders/unlock';
import { groupDigits, shortId } from '../kit/format';

// ---- UI state

/**
 * An additional holder typed by the user. The wallet's own row is not stored. It is derived as the
 * supply minus the others.
 */
export interface HolderRow {
  /** 64 hex characters: the x-only public key of the receiving wallet. */
  owner: string;
  /** Whole tokens, as decimal text. */
  amount: string;
}

/** Everything the form holds, as the user typed it. */
export interface IssueUiState {
  /**
   * The token program: the reference's published public-mint build (default) or the standalone 3 /
   * 3 build.
   */
  program: IssueProgramChoice;
  name: string;
  ticker: string;
  /** A whole number as text ("8"), so an emptied field is representable. */
  decimals: string;
  supply: string;
  /** Additional holders. Empty means the whole supply goes to the connected wallet. */
  extras: HolderRow[];
  /** KAS per token output. Empty means the default of the protocol limits. */
  carrier: string;
  description: string;
  icon: string;
  /**
   * SHA-256 of the picture file, 64 hex, from a picked file or typed in the advanced field. Empty
   * means none.
   */
  imageHash: string;
  /**
   * The name of the picture file the hash was computed from, for display only. It is '' when the
   * hash was typed or there is none.
   */
  imageFile: string;
  website: string;
  /** X (Twitter) and Telegram accounts: a handle or an address. */
  x: string;
  telegram: string;
  /** "After creation": keep all, sell at one price, or sell on a curve. */
  sale: SaleUi;
  /** "Lock my share until": none, a number of days, or a date. */
  lock: LockUi;
}

/**
 * The lock choices: none, a number of days from the choice, or a date (YYYY-MM-DD, the start of
 * that day in the viewer's zone).
 */
export const LOCK_DAYS = ['30', '90', '180', '365'] as const;
export type LockChoice = 'none' | (typeof LOCK_DAYS)[number] | 'date';

/**
 * "Lock my share until" as chosen. `until` (unix ms) is fixed when a number of days is chosen, so
 * the plan does not move with the clock.
 */
export interface LockUi {
  choice: LockChoice;
  /** YYYY-MM-DD of the date input. */
  date: string;
  /** Unix ms of a number of days, as decimal text. It is '' otherwise. */
  until: string;
}

const DAY_MS = 86_400_000;

/** The time `days` days from `now`, rounded up to the whole minute (unix ms). */
export function lockUntilIn(days: number, now: number = Date.now()): string {
  return String(Math.ceil((now + days * DAY_MS) / 60_000) * 60_000);
}

/**
 * Whether the connected wallet can sign the unlock of a lock chosen here, by the check Portfolio's
 * Unlock uses (ui/orders/unlock.ts `unlockSigner`). Null with no lock, no wallet, or a wallet that
 * signs script inputs. 'unsupported' when its test signature failed, 'unproven' when it was never
 * checked. Either way the unlock would need the CLI and the wallet's private key.
 */
export function lockSignerNote(choice: LockChoice, adapter: Pick<WalletAdapter, 'id' | 'covenantSigning'> | null | undefined, pubkey: Hex | null | undefined): 'unproven' | 'unsupported' | null {
  if (choice === 'none' || !adapter || !pubkey) return null;
  if (unlockSigner(adapter, pubkey) === 'wallet') return null;
  return storedVerdict(adapter.id, pubkey) === 'unsupported' ? 'unsupported' : 'unproven';
}

/** A choice of the lock as its UI state. */
export function chooseLock(choice: LockChoice, prev: LockUi, now: number = Date.now()): LockUi {
  if (choice === 'none' || choice === 'date') return { ...prev, choice, until: '' };
  return { ...prev, choice, until: lockUntilIn(Number(choice), now) };
}

/**
 * The planner's `lockUntil` (unix ms, decimal text) of the choice. It is '' for none. A date is the
 * start of that day in the viewer's zone. A missing or unreadable date gives '0', which the
 * validator refuses (`lock.invalid`).
 */
export function lockUntilOf(l: LockUi): string {
  if (l.choice === 'none') return '';
  if (l.choice !== 'date') return l.until;
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(l.date.trim());
  if (!m) return '0';
  const ms = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime();
  return Number.isFinite(ms) && ms > 0 ? String(ms) : '0';
}

/**
 * The sale as typed. "Keep in my wallet" and the amount for sale are linked. The field typed last
 * (`lead`) is kept and the other one is derived from the wallet's share, so a later change of the
 * supply moves the derived one.
 */
export interface SaleUi extends SaleForm {
  lead: 'keep' | 'amount';
}

export function emptySaleUi(): SaleUi {
  return { ...defaultSaleForm(), lead: 'keep' };
}

export function emptyUiState(): IssueUiState {
  return { program: 'public-mint', name: '', ticker: '', decimals: '8', supply: '', extras: [], carrier: '', description: '', icon: '', imageHash: '', imageFile: '', website: '', x: '', telegram: '', sale: emptySaleUi(), lock: { choice: 'none', date: '', until: '' } };
}

// ---- Small filters

/**
 * The ticker input: capitals as typed, whitespace dropped. Other characters are kept so the
 * validator can explain them.
 */
export function filterTicker(input: string, maxChars = 32): string {
  return input.replace(/\s+/g, '').toUpperCase().slice(0, maxChars);
}

/**
 * The decimals input: digits only, at most 2. "8" stays a number, and a pasted "8.0" cannot sneak
 * in a fraction.
 */
export function filterDecimals(input: string): string {
  return input.replace(/\D+/g, '').slice(0, 2);
}

/** A hex key typed or pasted, trimmed and lower-cased, as the protocol wants lower-case hex. */
export function filterOwner(input: string): string {
  return input.replace(/\s+/g, '').toLowerCase().slice(0, 64);
}

/**
 * Decimals text to a number. It is NaN when empty or invalid, and the validator then reports
 * `decimals.invalid`.
 */
export function parseDecimalsText(text: string): number {
  return /^\d{1,2}$/.test(text.trim()) ? Number(text.trim()) : Number.NaN;
}

// ---- Shares

export interface Shares {
  /** The total supply in base units. Null when the supply or the decimals do not parse. */
  supply: bigint | null;
  /** The sum of the extra holders' parsable, positive amounts. */
  others: bigint;
  /**
   * The supply minus `others`. Null when the supply is unknown. Negative means the others exceed
   * the supply, an error.
   */
  remaining: bigint | null;
  /** False when an extra row's amount does not parse. Its own validation message explains. */
  othersComplete: boolean;
}

/** The wallet's share, the supply minus what the extra holders receive, all in base units. */
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

/**
 * True when the wallet's own row is part of the holder list. It is left out when the others take
 * the whole supply.
 */
export function walletRowIncluded(state: IssueUiState): boolean {
  if (state.extras.length === 0) return false;
  const s = sharesOf(state);
  return s.remaining === null || s.remaining !== 0n || !s.othersComplete;
}

/** The remaining share as decimal text of whole tokens. It is "" when unknown or negative. */
export function remainingText(state: IssueUiState): string {
  const s = sharesOf(state);
  const decimals = parseDecimalsText(state.decimals);
  if (s.remaining === null || s.remaining < 0n || Number.isNaN(decimals)) return '';
  return formatUnits(s.remaining, decimals);
}

// ---- UI state to IssueForm

/**
 * Maps the UI state to the planner's `IssueForm`. With no extra holders the list is empty, which
 * is the protocol default of everything to the maker. Otherwise the wallet is holder 0 with the
 * remaining amount, followed by the extras (scheme 0 = a wallet key). A wallet share of zero drops
 * the wallet row. A negative one becomes "0", so validation fails with a holder amount error and
 * the view adds `holders.exceeds`.
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
    ...(state.lock && lockUntilOf(state.lock) !== '' ? { lockUntil: lockUntilOf(state.lock) } : {}),
  };
}

/**
 * The planner's sale. It has the amount only when it was typed last, else the keep percent decides.
 */
export function saleFormOf(u: SaleUi): SaleForm {
  const { lead, ...f } = u;
  return { ...f, amount: lead === 'amount' ? f.amount.trim() : '' };
}

/** The wallet's share of the supply in base units, or null when unknown. A sale takes from it. */
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

/**
 * The sale of a creation finished later (its record): the curve from its state, or the one-price
 * sale it offers to place.
 */
export function saleOfRecord(rec: Pick<CreationRecord, 'curve' | 'askSale'>): PlannedSale | undefined {
  if (rec.curve) {
    const st = rec.curve.state.state;
    const numbers = curveNumbersOfState(st);
    const table = st.table.map((p) => BigInt(p));
    return {
      mode: 'curve', sell: BigInt(st.total), keep: 0n, output: rec.curve.custody, total: numbers.total,
      curve: { table: { levelSize: BigInt(st.levelSize), table, realised: numbers.total }, minFill: BigInt(st.minFill), numbers },
    };
  }
  if (rec.askSale) {
    const sell = BigInt(rec.askSale.sell);
    return { mode: 'ask', sell, keep: 0n, output: 0, total: 0n, price: BigInt(rec.askSale.price) };
  }
  return undefined;
}

/**
 * What the sale section shows while the form is typed: the split, and for a curve its table, steps
 * and numbers.
 */
export interface SalePreview {
  issues: PlanIssue[];
  parsed: ParsedSale | null;
  /** Whole tokens, grouped. */
  sellText: string;
  keepText: string;
  curve: { steps: CurveStep[]; numbers: CurveNumbers } | null;
  /** A curve that could not be built, with kob-wasm's reason. */
  curveError: string | null;
  /** The one-price default (sompi per whole token) for the amount. */
  defaultPrice: bigint | null;
}

export function salePreview(kob: KobWasm, state: IssueUiState): SalePreview | null {
  if (state.sale.mode === 'keep') return null;
  const d = parseDecimalsText(state.decimals);
  const share = saleShareOf(state);
  if (Number.isNaN(d)) return { issues: [], parsed: null, sellText: '', keepText: '', curve: null, curveError: null, defaultPrice: null };
  const form = saleFormOf(state.sale);
  // The price field may be blank. The default is shown in it, so the preview uses it.
  const sellOnly = parseSale({ ...form, mode: 'ask', price: '1' }, share, d).parsed;
  const defaultPrice = sellOnly ? defaultSalePrice(sellOnly.sell, 10n ** BigInt(d)) : null;
  const { issues, parsed } = parseSale(form.mode === 'ask' && form.price.trim() === '' && defaultPrice ? { ...form, price: formatUnits(defaultPrice, 8) } : form, share, d);
  const scale = 10n ** BigInt(d);
  let curve: SalePreview['curve'] = null;
  let curveError: string | null = null;
  if (parsed && parsed.mode === 'curve') {
    try {
      const t = curveTable(kob, curveSpecOfSale(parsed, scale));
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

/**
 * The plain message key of a curve the protocol refused for its start price: below 1 sompi or the
 * tip, or a chromatic start too low for a higher price at every semitone. Null for any other error.
 */
export function curveErrorKey(raw: string): 'issue.sale.lowStart' | null {
  return /first level would be priced|too low for a higher price at every semitone/.test(raw) ? 'issue.sale.lowStart' : null;
}

/** Findings only the view model knows. They are translated under `issue.*`, not `issues.*`. */
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

// ---- Issue field to input

/**
 * Where an `IssueForm` finding belongs in the UI.
 * - A top-level field name ("name", "ticker", ...).
 * - `holder-<row>-owner|amount` with the UI row, where row 0 is the wallet's own share and the
 *   extras are 1..n.
 * - `holders` for the list as a whole.
 * - Null for a general finding with no field.
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

/** Groups findings by UI field ("" is general), keeping order. */
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

/**
 * Exact KAS missing for the issuance, from the `funds.insufficient` finding (`need` is carriers,
 * `have` is spendable).
 */
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

// ---- Display helpers

/** Token amount in human units with digit grouping: 100000000000 at 8 decimals gives "1,000". */
export function humanAmount(base: bigint | string, decimals: number): string {
  return groupDigits(formatUnits(BigInt(base), decimals));
}

/**
 * The supply hint under the input: "1,000,000 tokens (100000000000000 base units)". It is null when
 * the supply does not parse.
 */
export function supplyPreview(state: IssueUiState): { human: string; base: string } | null {
  const s = sharesOf(state);
  const decimals = parseDecimalsText(state.decimals);
  if (s.supply === null || Number.isNaN(decimals) || s.supply <= 0n) return null;
  return { human: humanAmount(s.supply, decimals), base: s.supply.toString() };
}

/** The default carrier shown as the placeholder: "10". */
export function defaultCarrierText(limits: Pick<IssueLimits, 'defaultCarrier'>): string {
  return formatUnits(BigInt(limits.defaultCarrier), 8);
}

// ---- File names and JSON

const safeName = (ticker: string): string => ticker.replace(/[^A-Za-z0-9_-]/g, '_') || 'token';

/**
 * `<ticker>.registry-entry.json`: the token's facts in the format of a `registry/tokens.json`
 * entry, for tools and community lists. It is not a listing request.
 */
export const registryEntryFileName = (ticker: string): string => `${safeName(ticker)}.registry-entry.json`;
export const supplyDocFileName = (ticker: string): string => `${safeName(ticker)}.supply.json`;
export const metadataDocFileName = (ticker: string): string => `${safeName(ticker)}.metadata.json`;
export const proofDocFileName = (ticker: string): string => `${safeName(ticker)}.launch-proof.json`;

/** Pretty JSON with a trailing newline, as files end with one. */
export const jsonText = (value: unknown): string => `${JSON.stringify(value, null, 2)}\n`;

// ---- Review and result

export interface HolderView {
  index: number;
  owner: string;
  ownerShort: string;
  ownerScheme: number;
  /** Base units. */
  amount: bigint;
  /** Whole tokens with grouping. */
  human: string;
  isWallet: boolean;
  /** The custody of the curve this genesis places. */
  isCurve?: boolean;
  /**
   * The wallet's share held by the creation's creator lock. It opens at `lockedUntil` (unix ms).
   */
  lockedUntil?: bigint;
}

export interface ReviewSummary {
  name: string;
  ticker: string;
  decimals: number;
  supplyBase: bigint;
  supplyHuman: string;
  holders: HolderView[];
  /** KAS (sompi) locked in each token output and in total. */
  carrierEach: bigint;
  carrierTotal: bigint;
  /** The network fee in sompi. */
  fee: bigint;
  program: string;
  outputCount: number;
  /** The creator lock: what it holds and when it opens (unix ms). Null means none. */
  lock: { amount: bigint; human: string; until: bigint } | null;
}

export function holderViews(token: Pick<IssueToken, 'outputs' | 'decimals'>, walletKey: Hex | null, curveId?: Hex | null, lock?: Pick<IssueLock, 'authority' | 'key' | 'until'> | null): HolderView[] {
  const me = walletKey?.toLowerCase() ?? null;
  return token.outputs.map((o) => ({
    ...(curveId && o.ownerScheme === 4 && o.owner === curveId ? { isCurve: true } : {}),
    ...(lock && o.ownerScheme === 3 && o.owner === lock.authority && lock.key === me ? { lockedUntil: BigInt(lock.until) } : {}),
    index: o.index,
    owner: o.owner,
    ownerShort: shortId(o.owner, 14),
    ownerScheme: o.ownerScheme,
    amount: BigInt(o.amount),
    human: humanAmount(o.amount, token.decimals),
    isWallet: o.ownerScheme === 0 && o.owner.toLowerCase() === me,
  }));
}

/**
 * What the review shows before the pre-sign screen. It comes from the plan, so the numbers are the
 * ones that will be signed.
 */
export function reviewSummary(plan: Pick<IssuePlan, 'token' | 'fee' | 'curve' | 'lock'>, walletKey: Hex | null): ReviewSummary {
  const { token } = plan;
  const holders = holderViews(token, walletKey, plan.curve?.covenantId ?? null, plan.lock ?? null);
  const locked = holders.filter((h) => h.lockedUntil !== undefined);
  const lockAmount = locked.reduce((a, h) => a + h.amount, 0n);
  const supply = BigInt(token.supply);
  const carrierEach = BigInt(token.carrier);
  return {
    name: token.name,
    ticker: token.ticker,
    decimals: token.decimals,
    supplyBase: supply,
    supplyHuman: humanAmount(supply, token.decimals),
    holders,
    carrierEach,
    carrierTotal: carrierEach * BigInt(token.outputs.length) + (plan.curve ? BigInt(plan.curve.carrier) : 0n),
    fee: plan.fee,
    program: token.program,
    outputCount: token.outputs.length + (plan.curve ? 1 : 0),
    lock: locked.length && plan.lock ? { amount: lockAmount, human: humanAmount(lockAmount, token.decimals), until: BigInt(plan.lock.until) } : null,
  };
}

export interface ResultRow {
  /** The i18n key suffix under `issue.result.` and the test id suffix (`issue-result-<id>`). */
  id: 'tokenId' | 'templateHash' | 'extension' | 'txid' | 'prepTxid' | 'program';
  value: string;
  /** Offer a copy button. */
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
  /** The KCC-23 launch proof. Null means the docs carry none. */
  proofJson: string | null;
  proofFile: string;
}

/** The id of the creation's KCC-23 preparation, from supply.json (`kcc23.preparation_txid`). */
function prepTxidOf(docs: IssuePlan['docs']): string | null {
  const k = (docs.supply as { kcc23?: { preparation_txid?: unknown } }).kcc23?.preparation_txid;
  return typeof k === 'string' && /^[0-9a-f]{64}$/.test(k) ? k : null;
}

/**
 * Rows and file contents of the result panel. It throws when the docs do not describe the issued
 * token (`registryEntryFromIssue`).
 */
export function resultViewModel(plan: Pick<IssuePlan, 'token' | 'docs'> & Partial<Pick<IssuePlan, 'lock'>>, txid: string, walletKey: Hex | null = null): ResultViewModel {
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
    holders: holderViews(plan.token, walletKey, null, plan.lock ?? null),
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
