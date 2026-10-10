// Token issuance ("kob token issue" in the browser): form model, validation and planning over the kob-wasm `issue` export.
//
// The genesis is ONE transaction: the wallet's P2PK funding inputs (input 0 authorises the KIP-20 genesis group) -> 1..N token outputs of the
// fixed-supply KCC-20 reference program in its standard 3 / 3 configuration, `KCC20Ref` (or, chosen in the form, the published public-mint
// build `KCC20PublicMint`; covenant id derived as consensus does) + change. All protocol logic lives in Rust
// (`kob-protocol::issue`); this module only turns a human form into the wasm spec, selects funding and types the result. The result's
// `built` is an ordinary `BuiltTx`, so the wallet signing pipeline, `kob.finalize` and `kob.validate` handle it unchanged.
import { schnorr } from '@noble/curves/secp256k1.js';
import { atFloor, capForTotal, describeFee, rememberFeeChoice, withUrgency, type FeeContext, type RatePick } from './fee-policy';
import type { PlanIssue } from './plan-types';
import { normalizeTicker } from './registry';
import type { BuiltTx, Hex, Kcc20State, KeyUtxo, TokenUtxo, U64 } from './types';
import { KobError, type KobWasm } from './wasm';

// ------------------------------------------------------------------------------------------------ kob-wasm boundary

/** Constants and rules exported by `kob.raw.issueLimits()`: the UI reads them instead of hard-coding them. */
export interface IssueLimits {
  maxSupply: U64;
  maxGenesisOutputs: number;
  defaultCarrier: U64;
  defaultFeeRate: U64;
  minFeeRate: U64;
  maxStandardMass: U64;
  maxTokenInputs: number;
  maxTokenOutputs: number;
  maxDecimals: number;
  ticker: { minLength: number; maxLength: number; pattern: string };
  name: { minLength: number; maxLength: number };
  maxDisplayChars: number;
  ownerSchemes: number[];
  covenantOwnerScheme: number;
  /** KOB's standard program (the default): the KCC-20 reference with its 3 / 3 slots */
  program: 'KCC20Ref';
  registryTemplateId: string;
  /** the published public-mint build of the reference (request `program: 'public-mint'`): 3 / 3 slots */
  publicMint: { program: 'KCC20PublicMint'; registryTemplateId: string; maxTokenInputs: number; maxTokenOutputs: number };
  extensionClass: string;
  extensionCommitment: Hex;
}

export interface IssueHolderSpec {
  owner: Hex;
  ownerScheme: number;
  /** base units */
  amount: U64;
  borrowScheme?: number;
  borrowGuard?: Hex;
}

/** Request of the wasm `issue` export (kob-protocol JSON conventions). */
export interface IssueSpec {
  name: string;
  ticker: string;
  decimals: number;
  /** base units */
  supply: U64;
  holders: IssueHolderSpec[];
  extensionCommitment?: Hex;
  /** KAS (sompi) on every token output */
  carrier?: U64;
  feeRate?: U64;
  funding: KeyUtxo[];
  /** x-only key that receives the change (default: the owner of funding[0]) */
  changeTo?: Hex;
  allowBorrow?: boolean;
  description?: string;
  icon?: string;
  website?: string;
  network?: string;
  /** `3x3` (default, `KCC20Ref`) or `public-mint` (`KCC20PublicMint`, the published build of the reference) */
  program?: '3x3' | 'public-mint';
}

export interface IssueTokenOutput {
  index: number;
  amount: U64;
  owner: Hex;
  ownerScheme: number;
  borrowScheme: number;
  borrowGuard: Hex;
}

export interface IssueToken {
  covenantId: Hex;
  program: 'KCC20Ref' | 'KCC20PublicMint';
  templateHash: Hex;
  extensionCommitment: Hex;
  name: string;
  ticker: string;
  decimals: number;
  /** base units */
  supply: U64;
  /** KAS (sompi) on every token output */
  carrier: U64;
  outputs: IssueTokenOutput[];
}

/** An entry of `registry/tokens.json` (`registry/tokens.schema.json`, `$defs.token`), as the issuance proposes it. */
export interface RegistryTokenEntry {
  ticker: string;
  name: string;
  family: 'kcc20';
  covenant_id: Hex;
  template_id: string;
  extension_commitment: Hex | null;
  extension_class: string;
  decimals: number;
  /** legacy (protocol v2): not emitted for a new token (the registry reads and ignores them) */
  lot_size?: number | null;
  tick?: number | null;
  max_token_inputs?: number;
  max_token_outputs?: number;
  status: 'pending-review';
  verified: false;
  display?: { description?: string; website?: string; icon?: string; kcc23?: Record<string, unknown> };
}

export interface IssueDocs {
  /** `supply.json` (kob-token-supply/1): what was issued and where */
  supply: Record<string, unknown>;
  /** off-chain token metadata (KCC-23 style object) */
  metadata: Record<string, unknown>;
  registryEntry: RegistryTokenEntry;
}

export interface IssueResult {
  built: BuiltTx;
  token: IssueToken;
  docs: IssueDocs;
  warnings: string[];
}

interface RawIssue {
  issue(spec: string): string;
  issueLimits(): string;
}

/** Typed access to the issuance exports through `kob.raw` (older bindings do not have them). */
function rawIssue(kob: KobWasm): RawIssue {
  const r = kob.raw as unknown as Partial<RawIssue>;
  if (typeof r.issue !== 'function' || typeof r.issueLimits !== 'function') {
    throw new KobError('the loaded kob-wasm bindings do not export issue: rebuild them (npm run build:wasm)', 'issue');
  }
  return r as RawIssue;
}

const limitsCache = new WeakMap<object, IssueLimits>();

/** Limits and rules of the issuance flow (maximum supply, ticker rules, default carrier ...). */
export function issueLimits(kob: KobWasm): IssueLimits {
  let l = limitsCache.get(kob.raw);
  if (!l) {
    const raw = rawIssue(kob);
    try {
      l = JSON.parse(raw.issueLimits()) as IssueLimits;
    } catch (e) {
      throw new KobError(e instanceof Error ? e.message : String(e), 'issueLimits');
    }
    limitsCache.set(kob.raw, l);
  }
  return l;
}

/** Plans an issuance from an already-resolved spec: `kob.raw.issue` with typed input and output. Throws KobError with the protocol's reason. */
export function issueRaw(kob: KobWasm, spec: IssueSpec): IssueResult {
  const raw = rawIssue(kob);
  try {
    return JSON.parse(raw.issue(JSON.stringify(spec))) as IssueResult;
  } catch (e) {
    if (e instanceof KobError) throw e;
    throw new KobError(e instanceof Error ? e.message : String(e), 'issue');
  }
}

// ------------------------------------------------------------------------------------------------ amounts and tickers

/** Decimal text to an integer scaled by 10^fractionDigits. `precision` = more fractional digits than allowed. */
export function parseDecimal(text: string, fractionDigits: number): { value: bigint } | { error: 'syntax' | 'precision' } {
  const m = /^(\d+)(?:\.(\d+))?$/.exec(text.trim());
  if (!m) return { error: 'syntax' };
  const frac = m[2] ?? '';
  if (frac.length > fractionDigits) {
    // trailing zeros beyond the precision carry no information ("1.500" with 2 decimals is 1.5)
    if (/[^0]/.test(frac.slice(fractionDigits))) return { error: 'precision' };
  }
  const scaled = frac.slice(0, fractionDigits).padEnd(fractionDigits, '0');
  return { value: BigInt(m[1] + scaled) };
}

/** Base units to a decimal string ("1500" with 2 decimals -> "15"; "1505" -> "15.05"). */
export function formatUnits(value: bigint, decimals: number): string {
  const s = value.toString().padStart(decimals + 1, '0');
  const whole = s.slice(0, s.length - decimals);
  const frac = decimals === 0 ? '' : s.slice(s.length - decimals).replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : whole;
}

/** Homoglyph normalisation of `registry.rs::normalize_ticker` (the one implementation lives in registry.ts). */
export { normalizeTicker };

/** Zero-width, bidi and control characters the registry refuses in display text (`registry.rs::has_bad_char`). */
const BAD_CHARS = /[\p{Cc}​-‏‪-‮⁠-⁤⁦-⁩﻿]/u;

const HEX32 = /^[0-9a-f]{64}$/;

function isXOnlyKey(hex: Hex): boolean {
  try {
    schnorr.utils.lift_x(BigInt(`0x${hex}`));
    return true;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------------------------------------------ form

export interface IssueHolderForm {
  /** 64 hex chars: x-only key (scheme 0), key hash / script hash (1..3) or covenant id (4) */
  owner: Hex;
  ownerScheme: number;
  /** whole tokens, decimal text (up to `decimals` fractional digits) */
  amount: string;
  borrowScheme?: number;
  borrowGuard?: Hex;
}

/** The program a form issues: KOB's standard (the KCC-20 reference, 3 / 3) or the reference's published public-mint build (also 3 / 3). */
export type IssueProgramChoice = 'standard' | 'public-mint';

export interface IssueForm {
  /** absent = `standard` */
  program?: IssueProgramChoice;
  name: string;
  /** exactly as it will be issued: 2..12 characters A-Z 0-9 */
  ticker: string;
  decimals: number;
  /** whole tokens, decimal text (up to `decimals` fractional digits); the total supply is fixed forever */
  supply: string;
  /** empty = the whole supply to the maker */
  holders: IssueHolderForm[];
  /** KAS carried by every token output, decimal text; empty = the default (10 KAS) */
  carrier: string;
  description: string;
  icon: string;
  website: string;
  /** permit a non-zero borrow scheme on non-covenant holders (never set by the default form) */
  allowBorrow?: boolean;
}

export function emptyIssueForm(): IssueForm {
  return { name: '', ticker: '', decimals: 8, supply: '', holders: [], carrier: '', description: '', icon: '', website: '' };
}

/** A form after parsing: everything in base units / sompi. `holders` is empty when the form uses the default (all to the maker). */
interface ParsedForm {
  supply: bigint;
  holders: { owner: Hex; ownerScheme: number; amount: bigint; borrowScheme: number; borrowGuard: Hex }[];
  carrier: bigint;
}

const ZERO32 = '0'.repeat(64);

function issue(code: string, message: string, field?: string, params?: PlanIssue['params'], severity: PlanIssue['severity'] = 'error'): PlanIssue {
  return { code, severity, message, ...(field ? { field } : {}), ...(params ? { params } : {}) };
}

/** Validation with the parsed values (null when the numbers do not parse). Same rules as `kob-protocol::issue` and `registry.rs`. */
function analyze(form: IssueForm, limits: IssueLimits, registryTickers: Iterable<string> = []): { issues: PlanIssue[]; parsed: ParsedForm | null } {
  const issues: PlanIssue[] = [];
  let ok = true;
  const err = (code: string, message: string, field?: string, params?: PlanIssue['params']) => {
    ok = false;
    issues.push(issue(code, message, field, params));
  };

  // name
  const name = form.name.trim();
  if (name === '') err('name.empty', 'Enter a token name.', 'name');
  else if ([...name].length > limits.name.maxLength) err('name.too_long', `The name is at most ${limits.name.maxLength} characters.`, 'name', { max: limits.name.maxLength });
  else if (BAD_CHARS.test(name)) err('name.bad_chars', 'The name must not contain control, zero-width or direction-changing characters.', 'name');

  // ticker
  const t = form.ticker;
  if (t.length < limits.ticker.minLength || t.length > limits.ticker.maxLength) {
    err('ticker.length', `The ticker is ${limits.ticker.minLength} to ${limits.ticker.maxLength} characters.`, 'ticker', { min: limits.ticker.minLength, max: limits.ticker.maxLength });
  } else if (!new RegExp(limits.ticker.pattern).test(t)) {
    err('ticker.chars', 'The ticker uses only capital letters A-Z and digits 0-9.', 'ticker');
  } else {
    const norm = normalizeTicker(t);
    for (const existing of registryTickers) {
      if (existing === t) {
        err('ticker.exists', `The ticker ${t} is already in the registry.`, 'ticker', { ticker: t });
        break;
      }
      if (normalizeTicker(existing) === norm) {
        err('ticker.lookalike', `The ticker ${t} looks like the existing ticker ${existing} (O and 0, I, L and 1, S and 5, B and 8, rn and m count as the same).`, 'ticker', { ticker: t, existing });
        break;
      }
    }
  }

  // decimals
  const decimalsOk = Number.isInteger(form.decimals) && form.decimals >= 0 && form.decimals <= limits.maxDecimals;
  if (!decimalsOk) err('decimals.invalid', `Decimals is a whole number from 0 to ${limits.maxDecimals}.`, 'decimals', { max: limits.maxDecimals });
  const decimals = decimalsOk ? form.decimals : 0;

  // supply
  const maxSupply = BigInt(limits.maxSupply);
  let supply: bigint | null = null;
  const ps = parseDecimal(form.supply, decimals);
  if ('error' in ps) {
    if (!decimalsOk && ps.error === 'precision') {
      // the decimals error already explains it
    } else if (ps.error === 'precision') err('supply.precision', `The supply has more than ${decimals} decimal places.`, 'supply', { decimals });
    else err('supply.invalid', 'Enter the supply as a number, for example 1000000.', 'supply');
  } else if (ps.value === 0n) {
    err('supply.zero', 'The supply must be greater than zero.', 'supply');
  } else if (ps.value > maxSupply) {
    err('supply.too_large', `The supply is at most ${formatUnits(maxSupply, decimals)} tokens at ${decimals} decimals.`, 'supply', { max: maxSupply });
  } else {
    supply = ps.value;
  }

  // holders
  const holders: ParsedForm['holders'] = [];
  if (form.holders.length > limits.maxGenesisOutputs) {
    err('holders.too_many', `At most ${limits.maxGenesisOutputs} holders can receive tokens in the issuance.`, 'holders', { max: limits.maxGenesisOutputs });
  } else if (form.holders.length > slotsOf(form, limits).maxTokenOutputs) {
    const slots = slotsOf(form, limits);
    issues.push(
      issue(
        'holders.consolidation',
        `More than ${slots.maxTokenOutputs} holders: moving these token outputs together later takes several transfers of at most ${slots.maxTokenInputs} inputs each.`,
        'holders',
        { holders: form.holders.length, maxInputs: slots.maxTokenInputs },
        'warning',
      ),
    );
  }
  let sum = 0n;
  let sumOk = form.holders.length > 0;
  form.holders.forEach((h, i) => {
    const f = (k: string) => `holders[${i}].${k}`;
    const owner = h.owner.trim().toLowerCase();
    let holderOk = true;
    const herr = (code: string, message: string, field: string, params?: PlanIssue['params']) => {
      holderOk = false;
      err(code, message, field, params);
    };
    if (!Number.isInteger(h.ownerScheme) || !limits.ownerSchemes.includes(h.ownerScheme)) {
      herr('holder.scheme.invalid', `Holder ${i + 1}: unknown owner scheme.`, f('ownerScheme'));
    }
    if (!HEX32.test(owner)) {
      herr('holder.owner.invalid', `Holder ${i + 1}: the owner is 64 hexadecimal characters.`, f('owner'));
    } else if (h.ownerScheme === 0 && !isXOnlyKey(owner)) {
      // an unspendable key would burn the tokens
      herr('holder.owner.not_a_key', `Holder ${i + 1}: the owner is not a valid public key, the tokens could never be spent.`, f('owner'));
    }
    const pa = parseDecimal(h.amount, decimals);
    let amount = 0n;
    if ('error' in pa) {
      if (pa.error === 'precision') herr('holder.amount.precision', `Holder ${i + 1}: more than ${decimals} decimal places.`, f('amount'), { decimals });
      else herr('holder.amount.invalid', `Holder ${i + 1}: enter the amount as a number.`, f('amount'));
    } else if (pa.value === 0n) {
      herr('holder.amount.zero', `Holder ${i + 1}: the amount must be greater than zero.`, f('amount'));
    } else {
      amount = pa.value;
    }
    const borrowScheme = h.borrowScheme ?? 0;
    const borrowGuard = (h.borrowGuard ?? ZERO32).toLowerCase();
    if (!Number.isInteger(borrowScheme) || borrowScheme < 0 || borrowScheme > 3) {
      herr('holder.borrow.scheme', `Holder ${i + 1}: unknown borrow scheme.`, f('borrowScheme'));
    } else if (h.ownerScheme === limits.covenantOwnerScheme && (borrowScheme !== 0 || borrowGuard !== ZERO32)) {
      // borrows churn the outpoint and break outpoint-bound claims
      herr('holder.borrow.covenant', `Holder ${i + 1}: tokens held by a covenant cannot enable borrowing.`, f('borrowScheme'));
    } else if (borrowScheme !== 0 && !form.allowBorrow) {
      herr('holder.borrow.flag', `Holder ${i + 1}: borrowing needs the explicit allow-borrow option.`, f('borrowScheme'));
    } else if (borrowScheme === 0 && borrowGuard !== ZERO32) {
      herr('holder.borrow.guard', `Holder ${i + 1}: the borrow guard must be empty while borrowing is disabled.`, f('borrowGuard'));
    } else if (!HEX32.test(borrowGuard)) {
      herr('holder.borrow.guard', `Holder ${i + 1}: the borrow guard is 64 hexadecimal characters.`, f('borrowGuard'));
    }
    if (holderOk) {
      sum += amount;
      holders.push({ owner, ownerScheme: h.ownerScheme, amount, borrowScheme, borrowGuard });
    } else {
      sumOk = false;
    }
  });
  if (supply !== null && sumOk && form.holders.length > 0 && sum !== supply) {
    err('holders.sum_mismatch', `The holders receive ${formatUnits(sum, decimals)} tokens but the supply is ${formatUnits(supply, decimals)}.`, 'holders', {
      holders: sum,
      supply,
    });
  }

  // carrier
  let carrier = BigInt(limits.defaultCarrier);
  if (form.carrier.trim() !== '') {
    const pc = parseDecimal(form.carrier, 8);
    if ('error' in pc) {
      err(pc.error === 'precision' ? 'carrier.precision' : 'carrier.invalid', 'Enter the KAS per token output as a number with at most 8 decimal places.', 'carrier');
    } else if (pc.value === 0n) {
      err('carrier.zero', 'The KAS per token output must be greater than zero.', 'carrier');
    } else {
      carrier = pc.value;
    }
  }

  // display text (the registry's rules, so the proposed entry is accepted)
  const disp = (v: string, field: 'description' | 'website' | 'icon') => {
    const s = v.trim();
    if (s === '') return;
    if ([...s].length > limits.maxDisplayChars || BAD_CHARS.test(s)) {
      err(`${field}.invalid`, `The ${field} is at most ${limits.maxDisplayChars} characters without control characters.`, field, { max: limits.maxDisplayChars });
    } else if (field === 'website' && !s.startsWith('https://')) {
      err('website.https', 'The website must be an https:// address.', field);
    } else if (field === 'icon' && !(s.startsWith('https://') || s.startsWith('ipfs://'))) {
      err('icon.scheme', 'The icon must be an https:// or ipfs:// address.', field);
    }
  };
  disp(form.description, 'description');
  disp(form.website, 'website');
  disp(form.icon, 'icon');

  return { issues, parsed: ok && supply !== null ? { supply, holders, carrier } : null };
}

/** Token inputs / outputs per transfer of the program the form issues. */
export function slotsOf(form: Pick<IssueForm, 'program'>, limits: IssueLimits): { maxTokenInputs: number; maxTokenOutputs: number } {
  return form.program === 'public-mint' ? limits.publicMint : limits;
}

/** Validates an issuance form: `error` issues block the plan, `warning` issues are shown. `registryTickers` = tickers already in the registry. */
export function validateIssueForm(form: IssueForm, limits: IssueLimits, registryTickers?: Iterable<string>): PlanIssue[] {
  return analyze(form, limits, registryTickers).issues;
}

// ------------------------------------------------------------------------------------------------ planning

export class IssueFormError extends Error {
  readonly issues: PlanIssue[];
  constructor(issues: PlanIssue[]) {
    super(issues.map((i) => i.message).join(' '));
    this.name = 'IssueFormError';
    this.issues = issues;
  }
}

export interface PlanIssueOptions {
  /** the wallet's spendable P2PK KAS UTXOs (coinbase maturity already applied by the caller) */
  funding: KeyUtxo[];
  /** the wallet's x-only key: default sole holder, signer of every funding input */
  maker: Hex;
  network: string;
  /** sompi per mass unit (default = the relay minimum from the limits). An explicit rate: the fee policy (`fees`) is then not asked. */
  feeRate?: bigint;
  /** dynamic fee policy + the node's estimate (kob/fee-policy.ts): the issuance pays the NORMAL bucket; absent = the relay minimum */
  fees?: FeeContext;
  /** set by the planner: the pick behind `feeRate` */
  feePick?: RatePick;
  /** where change returns (default: maker) */
  changeTo?: Hex;
  /** tickers already in the registry, for the duplicate / lookalike check */
  registryTickers?: Iterable<string>;
  /** default: `issueLimits(kob)` */
  limits?: IssueLimits;
}

export interface IssuePlan extends IssueResult {
  /** the spec sent to kob-wasm (with the funding actually used) */
  spec: IssueSpec;
  /** network fee in sompi */
  fee: bigint;
}

/** Rough mass of a genesis with `outputs` token outputs, used only to size the funding selection (the exact fee comes from kob-wasm). */
const estimateFeeMass = (outputs: number): bigint => 2_000n + 1_500n * BigInt(outputs);

/**
 * Validates the form, selects funding (largest UTXOs first: fewest inputs; covering all carriers plus a fee reserve, growing the selection
 * while kob-wasm reports a shortfall) and plans the genesis. Throws `IssueFormError` for invalid forms and insufficient funds, `KobError`
 * for protocol rejections. Nothing is signed or sent here.
 */
export function planIssue(kob: KobWasm, form: IssueForm, opts0: PlanIssueOptions): IssuePlan {
  const opts = withUrgency(opts0, 'normal');
  try {
    return planIssueAt(kob, form, opts);
  } catch (e) {
    // the wallet cannot pay the fee at the picked rate: try the floor before giving up (the estimate must not make a payable issuance impossible)
    const low = e instanceof IssueFormError && e.issues.some((i) => i.code === 'funds.insufficient') ? atFloor(opts) : null;
    if (!low) throw e;
    try {
      return planIssueAt(kob, form, low);
    } catch {
      throw e;
    }
  }
}

function planIssueAt(kob: KobWasm, form: IssueForm, opts: PlanIssueOptions): IssuePlan {
  const limits = opts.limits ?? issueLimits(kob);
  const { issues, parsed } = analyze(form, limits, opts.registryTickers);
  if (!parsed) throw new IssueFormError(issues);
  const maker = opts.maker.toLowerCase();
  if (!HEX32.test(maker)) throw new IssueFormError([issue('maker.invalid', 'The wallet key is not a 32-byte x-only public key.', undefined)]);

  const feeRate = opts.feeRate ?? BigInt(limits.defaultFeeRate);
  const holders: IssueHolderSpec[] =
    parsed.holders.length > 0
      ? parsed.holders.map((h) => ({
          owner: h.owner,
          ownerScheme: h.ownerScheme,
          amount: h.amount.toString(),
          ...(h.borrowScheme !== 0 ? { borrowScheme: h.borrowScheme, borrowGuard: h.borrowGuard } : {}),
        }))
      : [{ owner: maker, ownerScheme: 0, amount: parsed.supply.toString() }];
  const outputs = BigInt(holders.length);

  // Only plain P2PK UTXOs of the maker can fund the genesis: the wallet signs every input with its one key.
  const candidates = opts.funding
    .filter((u) => u.pubkey.toLowerCase() === maker && !u.covenantId)
    .sort((a, b) => (BigInt(a.amount) === BigInt(b.amount) ? 0 : BigInt(a.amount) > BigInt(b.amount) ? -1 : 1));
  const carriers = parsed.carrier * outputs;
  const have = candidates.reduce((s, u) => s + BigInt(u.amount), 0n);
  const shortfall = () =>
    new IssueFormError([
      issue(
        'funds.insufficient',
        `Not enough KAS: the issuance locks ${formatUnits(carriers, 8)} KAS in ${holders.length} token output(s) (returned when the tokens are spent or consolidated) plus the network fee; the wallet has ${formatUnits(have, 8)} KAS spendable.`,
        undefined,
        { need: carriers, have },
      ),
    ]);
  if (have < carriers) throw shortfall();

  // smallest largest-first prefix covering the carriers and an estimated fee; grown on demand
  const reserve = feeRate * estimateFeeMass(holders.length);
  let count = 0;
  let acc = 0n;
  while (count < candidates.length && acc < carriers + reserve) acc += BigInt(candidates[count++].amount);
  const base = {
    name: form.name.trim(),
    ticker: form.ticker,
    decimals: form.decimals,
    supply: parsed.supply.toString(),
    holders,
    carrier: parsed.carrier.toString(),
    feeRate: feeRate.toString(),
    changeTo: (opts.changeTo ?? maker).toLowerCase(),
    ...(form.program === 'public-mint' ? { program: 'public-mint' as const } : {}),
    ...(form.allowBorrow ? { allowBorrow: true } : {}),
    ...(form.description.trim() ? { description: form.description.trim() } : {}),
    ...(form.icon.trim() ? { icon: form.icon.trim() } : {}),
    ...(form.website.trim() ? { website: form.website.trim() } : {}),
    network: opts.network,
  };
  for (;;) {
    let spec: IssueSpec = { ...base, funding: candidates.slice(0, count) };
    try {
      let r = issueRaw(kob, spec);
      // the total fee cap: a fee above it is rebuilt once at a lower rate (fee-policy.ts)
      const next = opts.fees ? capForTotal(opts.fees.policy, feeRate, BigInt(r.built.fee.fee)) : null;
      if (next !== null) {
        try {
          const lowered = { ...spec, feeRate: next.toString() };
          r = issueRaw(kob, lowered);
          spec = lowered;
        } catch {
          /* keep the first build */
        }
      }
      if (opts.fees && opts.feePick) rememberFeeChoice(r.built, describeFee(opts.fees.policy, opts.feePick, BigInt(spec.feeRate ?? feeRate), BigInt(r.built.fee.fee)));
      return { ...r, spec, fee: BigInt(r.built.fee.fee) };
    } catch (e) {
      if (e instanceof KobError && /insufficient funds/.test(e.message) && count < candidates.length) {
        count += 1;
        continue;
      }
      if (e instanceof KobError && /insufficient funds/.test(e.message)) throw shortfall();
      throw e;
    }
  }
}

// ------------------------------------------------------------------------------------------------ results

/**
 * The registry entry to submit by pull request (`registry/tokens.json`): status `pending-review`, `verified: false`, program = the
 * pinned template of the issued program (`kcc20-ref-3x3`, or `kcc20-ref-public-mint`). Checks that the docs describe this token before
 * handing them out.
 */
export function registryEntryFromIssue(token: IssueToken, docs: IssueDocs): RegistryTokenEntry {
  const e = docs.registryEntry;
  if (e.covenant_id !== token.covenantId || e.ticker !== token.ticker || e.extension_commitment !== token.extensionCommitment) {
    throw new Error('registry entry does not describe the issued token');
  }
  return { ...e, status: 'pending-review', verified: false };
}

/** The token outputs of the genesis as token UTXOs with decoded state, for the token tracker (outputs `0..n` of `built.tx.id`). */
export function issuedTokenUtxos(plan: { built: BuiltTx; token: IssueToken }): TokenUtxo[] {
  const { built, token } = plan;
  return token.outputs.map((o) => {
    const state: Kcc20State = {
      amount: o.amount,
      owner: o.owner,
      owner_scheme: o.ownerScheme,
      borrow_scheme: o.borrowScheme,
      borrow_guard: o.borrowGuard,
      extension_commitment: token.extensionCommitment,
    };
    return {
      transactionId: built.tx.id,
      index: o.index,
      amount: built.tx.outputs[o.index].value,
      covenantId: token.covenantId,
      state,
    };
  });
}
