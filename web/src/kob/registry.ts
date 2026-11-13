// Token registry: validation of `registry/tokens.json`, template-hash-verified program support and
// on-chain verification helpers.
//
// The registry file is data, not authority. It pins facts anyone can re-derive from the chain
// (covenant id, program, extension commitment, decimals, genesis record) and the token programs
// this build supports. KOB makes no per-token judgement. See docs/spec/token-metadata.md.
//
// Rules.
// - The token list is open. The program gates trading, not the token.
// - A token is tradable if its template is reviewed (`review_status: reviewed`), is a program
//   compiled into kob-wasm and equals the wasm's pinned template (hash, prefix, suffix, slot
//   limits). A registry entry can never make the app trade a program it does not embed.
// - A registry entry is never treated more strictly than a token with no entry (open-list token,
//   kob/open-token.ts). `verified: false` does not block trading.
// - Older files may carry `status`, `official` and `warning`, and `lot_size` and `tick`. They are
//   read and ignored. An order's scale is `10^decimals` and any price in sompi is valid.
// - Anything else is viewable (balances, positions) but not tradable, with a machine-readable
//   reason.
//
// Mirrors crates/kob-protocol/src/registry.rs (validation rules, `normalize_ticker`,
// `display_name`). The JSON keys are snake_case.
import type { IndexerTokenView } from '../data/indexer-types';
import { PROGRAM_OF_REGISTRY_ID, TOKEN_PROGRAMS } from './programs.generated';
import { extensionOfState, familyOfProgram, isKronState } from './token-state';
import type { Hex, TokenProgram, TokenState } from './types';
import type { KobWasm } from './wasm';

/** Registry schema version this build reads. */
export const REGISTRY_SCHEMA_VERSION = 1;
/** The networks a registry may name. */
export const NETWORKS = ['mainnet', 'testnet-10', 'devnet'] as const;
/** Most decimals a registry token may declare. */
export const MAX_DECIMALS = 18;
/** The token families. */
export const FAMILIES = ['kcc20', 'kron'] as const;
/** A token family: `kcc20` or `kron`. */
export type Family = (typeof FAMILIES)[number];
/** Whether a template's engineering review is done. */
export type ReviewStatus = 'pending-review' | 'reviewed';
/** The kind of extension commitment a token carries. */
export type ExtensionClass = 'none' | 'fixed-supply-standard' | 'other';

/**
 * What a token program's authorities can do beyond owner-authorised transfers.
 * Mirrors `Capability` of registry.rs and the registry `capabilities` field.
 */
export const CAPABILITIES = ['mint-authority', 'public-mint', 'burn', 'freeze', 'seize', 'blacklist'] as const;
export type Capability = (typeof CAPABILITIES)[number];

/**
 * Maps a registry template id to its kob-wasm token program.
 * Generated from the profile table, so it covers every program kob-protocol embeds.
 */
export const PROGRAM_OF_TEMPLATE_ID: Readonly<Record<string, TokenProgram>> = PROGRAM_OF_REGISTRY_ID;

// ------------------------------------------------------------------------ JSON shapes (snake_case)

/** A template entry of the registry JSON. */
export interface RegistryTemplateJson {
  id: string; family: Family; template_hash: Hex; prefix_len: number; suffix_len: number; state_len: number;
  max_token_inputs: number; max_token_outputs: number;
  escrow: { owner_scheme?: number; borrow_scheme?: number; id_type?: number; is_minter?: number; delivery_id_type?: number };
  review_status: ReviewStatus; source: { path: string; upstream?: string; note?: string };
  /** What the program's authorities can do. Absent means nothing found. */
  capabilities?: Capability[];
  /**
   * Declared risks of the program that are not a capability (missing hardening, signing
   * assumptions). Absent means none declared.
   */
  risks?: string[];
}
/** A token entry of the registry JSON. */
export interface RegistryTokenJson {
  ticker: string; name: string; family: Family; covenant_id: Hex; template_id: string; extension_commitment: Hex | null; extension_class: ExtensionClass;
  decimals: number;
  /** Legacy lot size and price tick: optional, read from older registry files and ignored. */
  lot_size?: number | null; tick?: number | null;
  /**
   * Per-token judgements of older files (listing `status`, `official`, maintainer `warning`).
   * They are read and ignored, and not even their type is checked. They never reach TokenInfo or
   * tradability, and registry.rs strips them the same way.
   */
  status?: unknown; official?: unknown; warning?: unknown;
  max_token_inputs?: number; max_token_outputs?: number;
  /**
   * The covenant id and template hash were checked against a chain UTXO of the token.
   * This is a re-derivable fact, not an endorsement.
   */
  verified: boolean;
  /**
   * Every output of the token's genesis group (the origin of its covenant id) was checked against
   * its program, by `kob registry verify-genesis` over registry/evidence/. False means checked
   * and not clean. Absent means not checked.
   */
  genesis_verified?: boolean;
  /**
   * What the genesis check and the live-mint-authority check found.
   * `kob registry verify-genesis` re-derives it from registry/evidence/.
   */
  genesis?: RegistryGenesisJson;
  display?: { description?: string; website?: string; icon?: string; x?: string; telegram?: string; kcc23?: Record<string, unknown> };
}
/** A token's genesis record (mirrors `GenesisRecord` of registry.rs). */
export interface RegistryGenesisJson {
  txid: Hex; daa_score: number; outputs: number[]; supply: number; minter_outputs: number[];
  /**
   * Allowance of the genesis PublicMint outputs (kcc20-ref-public-mint). Absent means no public
   * minter. The maximum supply is `supply + mint_allowance`.
   */
  mint_allowance?: number;
  /**
   * The upstream build of the public-mint app whose PublicMint and TokenSeed actors the genesis
   * holds (`kcc20-reference@c8a0871` or `kcc20-reference@27f8403`). Absent means it holds none.
   */
  issuer?: string;
  /**
   * Live mint-authority cells (`txid:index`). [] means none and absent means not determined.
   * A non-empty list means the supply can still grow (API warning `live_mint_authority`).
   */
  live_minters?: string[];
  checked_at_daa: number; source: string;
}
/** The registry file: the templates and tokens of one network. */
export interface RegistryJson {
  $schema?: string; schema_version: number; network: string; templates: RegistryTemplateJson[]; tokens: RegistryTokenJson[];
}

/** The structural shape `toTokenMarket` consumes. */
export interface TokenMarketInput {
  ticker: string; covenant_id: Hex; template_id: string; extension_commitment: Hex | null; decimals: number;
  /** Legacy price tick: always null. Any price in sompi per whole token is valid. */
  tick: number | null;
}

// ------------------------------------------------------------------------------------- issue codes

/** The codes of the issues validation reports. */
export const REGISTRY_ISSUE_CODES = [
  'json', 'unknown-field', 'wrong-type', 'schema-version', 'unknown-network', 'wrong-network', 'bad-hex', 'duplicate-template-id', 'duplicate-template-hash',
  'template-invalid', 'unknown-template', 'family-mismatch', 'extension', 'duplicate-identity', 'duplicate-covenant-id', 'bad-ticker',
  'bad-display', 'out-of-range', 'decimals', 'slot-limit-mismatch', 'genesis',
] as const;
/** One issue code. */
export type RegistryIssueCode = (typeof REGISTRY_ISSUE_CODES)[number];
/** One validation finding with its JSON path. */
export interface RegistryIssue { code: RegistryIssueCode; message: string; path: string }

/** Thrown by `parseRegistry` with every finding. */
export class RegistryError extends Error {
  readonly issues: RegistryIssue[];
  constructor(issues: RegistryIssue[]) {
    super(`registry invalid: ${issues.map((i) => `${i.path}: ${i.message}`).join('; ')}`);
    this.name = 'RegistryError';
    this.issues = issues;
  }
}

/**
 * Why a token cannot be traded in this build (`null` means tradable).
 * The reasons concern the token's program only, never a judgement of the token.
 * `unregistered` is given only to tokens the registry does not know (token-model `unknownRow`,
 * where no template is at hand). A registry entry is never untradable for being unverified.
 */
export const UNTRADABLE_REASONS = [
  'unregistered', 'template-pending-review', 'template-unsupported', 'template-mismatch',
] as const;
/** One reason a token cannot be traded. */
export type UntradableReason = (typeof UNTRADABLE_REASONS)[number];

// ------------------------------------------------------------------------------------- text safety

const BAD_CHAR = /[\u0000-\u001F\u007F-\u009F​-‏‪-‮⁠-⁤⁦-⁩﻿]/u;
/** True if `s` has a control, bidi or zero-width character. */
export const hasBadChar = (s: string): boolean => BAD_CHAR.test(s);

/**
 * Makes untrusted free text (an indexer's error message) plain for display.
 * Control, bidi and zero-width characters are removed, because they can reorder or hide what is
 * shown. Runs of white space become one space. The text is cut at `max` characters.
 */
export function plainUntrusted(s: string, max = 300): string {
  const kept = [...s].map((c) => (BAD_CHAR.test(c) ? (/\s/.test(c) ? ' ' : '') : c)).join('').replace(/\s+/g, ' ').trim();
  const chars = [...kept];
  return chars.length > max ? chars.slice(0, max).join('') + '…' : kept;
}

/**
 * Non-Latin capitals drawn like a Latin letter, keyed by the uppercase form.
 * It is a small subset of the UTS #39 confusables for Cyrillic, Greek and Armenian. `toUpperCase`
 * runs first, so the lowercase twins are covered too. Letters with no lookalike are not listed.
 */
const CONFUSABLE_LATIN: Readonly<Record<string, string>> = {
  // Cyrillic
  'А': 'A', 'В': 'B', 'Е': 'E', 'Ё': 'E', 'К': 'K', 'М': 'M', 'Н': 'H', 'О': 'O', 'Р': 'P',
  'С': 'C', 'Т': 'T', 'У': 'Y', 'Х': 'X', 'І': 'I', 'Ї': 'I', 'Ј': 'J', 'Ѕ': 'S', 'Ү': 'Y',
  'Ԛ': 'Q', 'Ԝ': 'W', 'Ӏ': 'I', 'З': '3',
  // Greek
  'Α': 'A', 'Β': 'B', 'Ε': 'E', 'Ζ': 'Z', 'Η': 'H', 'Ι': 'I', 'Κ': 'K', 'Μ': 'M', 'Ν': 'N',
  'Ο': 'O', 'Ρ': 'P', 'Τ': 'T', 'Υ': 'Y', 'Χ': 'X',
  // Armenian
  'Օ': 'O', 'Ս': 'U',
};

/**
 * Homoglyph skeleton of a ticker or name, for what the app shows and finds, never for a refusal.
 * It serves the search (`kr0n` finds `KRON`), the "another token uses this ticker" note
 * (`sharedTickerIds`) and the issuance form's look-alike warning.
 * The registry refuses no ticker for resembling another, because that would be KOB choosing which
 * token may carry it. The covenant id is the identity.
 * Steps: NFKC, strip combining marks, uppercase, fold Cyrillic / Greek / Armenian lookalikes to
 * Latin, then RN -> M, VV -> W, O -> 0, I and L -> 1, S -> 5, B -> 8.
 */
export function normalizeTicker(ticker: string): string {
  const upper = ticker.normalize('NFKC').normalize('NFD').replace(/\p{M}/gu, '').toUpperCase().normalize('NFKC');
  const latin = [...upper].map((c) => CONFUSABLE_LATIN[c] ?? c).join('');
  return [...latin.replace(/RN/g, 'M').replace(/VV/g, 'W')].map((c) => (c === 'O' ? '0' : c === 'I' || c === 'L' ? '1' : c === 'S' ? '5' : c === 'B' ? '8' : c)).join('');
}

/**
 * Makes an untrusted name (indexer, pasted) safe to render.
 * Control, zero-width and bidi characters become U+FFFD, and the length is capped.
 */
export function sanitizeUntrusted(s: string, max = 32): string {
  const cleaned = [...s].map((c) => (BAD_CHAR.test(c) ? '�' : c)).join('');
  const chars = [...cleaned];
  return chars.length > max ? chars.slice(0, max).join('') + '…' : cleaned;
}

// -------------------------------------------------------------------------------- normalized model

/** The result of checking one registry template against the wasm's pinned templates. */
export interface TemplateCheck {
  id: string;
  /** The kob-wasm program this template maps to, or null if this build does not support it. */
  program: TokenProgram | null;
  /** The registry values agree with the wasm's pinned template (hash, lengths, slot limits). */
  matchesPinned: boolean;
  problems: string[];
}

/** A registry token with its template check and tradability. */
export interface TokenInfo {
  ticker: string; name: string; family: Family; covenantId: Hex; templateId: string; templateHash: Hex;
  extensionCommitment: Hex | null; extensionClass: ExtensionClass; decimals: number;
  /**
   * Minimum price increment, sompi per whole token. Always null, meaning 1 sompi.
   * The registry's legacy `tick` is ignored.
   */
  tick: bigint | null;
  display: RegistryTokenJson['display'] | null;
  /** Capabilities of the token's program template (registry `capabilities`), possibly empty. */
  capabilities: Capability[];
  /**
   * Synthesised from the indexer for an open-list token that has no registry entry
   * (kob/open-token.ts). Its decimals are inferred and its tick is 1.
   */
  openList?: boolean;
  /**
   * Display only (app/token-display.ts): where the decimals of a token the registry does not list
   * came from.
   * - `declared`: its KCC-23 declaration on chain, verified against its covenant id.
   * - `issued`: this browser's issuance of it.
   * - `program`: the default decimals of its token program, for a token that declares none. This
   *   is a launchpad's convention (KaspaRocket 3), which `decimalsConvention` names.
   * - `scale`: the scale of an order or its market.
   * - `assumed`: 8 decimals (the Kaspa convention), with its covenant id as the name.
   * Such a token is never traded or verified.
   */
  decimalsSource?: 'declared' | 'issued' | 'program' | 'scale' | 'assumed';
  /**
   * The source of a program default's decimals as the app names it (`KaspaRocket convention`).
   * Set with `decimalsSource` or `openScale` `program`.
   */
  decimalsConvention?: string;
  /**
   * Display only: the facts on which the token's registry entry (or its market's price scale) and
   * its KCC-23 declaration disagree. The on-chain values are shown, flagged. Absent when they
   * agree or the token declares nothing.
   */
  declarationConflicts?: ('decimals' | 'symbol' | 'name')[];
  /**
   * For an open-list token, where its decimals (and scale) came from.
   * - `declared`: its KCC-23 declaration. Its orders, if any, agree.
   * - `program`: its token program's default (a launchpad's convention, KaspaRocket 3). Its
   *   orders, if any, agree.
   * - `orders`: the scale its orders use.
   * - `entered`: the user entered it for the token's first order.
   * The sources are tried in the order declared, program, orders, entered.
   */
  openScale?: 'orders' | 'entered' | 'declared' | 'program';
  /**
   * For an open-list KCC-20 token, where its extension commitment came from and how it compares
   * with commitments known without the indexer.
   */
  extension?: ExtensionCheck;
  /** Kob-wasm program (null when this build cannot handle the template). */
  program: TokenProgram | null;
  /** The registry template agrees with the wasm's pinned template (hash, prefix, suffix, slots). */
  templateMatchesPinned: boolean;
  prefixLen: number; suffixLen: number; slots: { inputs: number; outputs: number };
  tradable: boolean; untradableReason: UntradableReason | null;
  json: RegistryTokenJson;
}

/**
 * Where an open-list KCC-20 token's extension commitment came from, and how it compares with
 * commitments known without the indexer.
 * The commitment is the indexer's pick (`source`): its token row, else one of its listed orders.
 * A covenant id carries a second commitment only if its genesis minted one. So the pick is
 * cross-checked against `references`: the registry's genesis-verified entry of the covenant id,
 * and the commitments of the wallet's own token UTXOs of it, which the node confirmed (their
 * address commits to the state). `mismatch` lists the references that name another commitment.
 */
export interface ExtensionCheck {
  commitment: Hex;
  /**
   * `holdings` is a token the indexer does not list, read from the wallet's node-verified token
   * UTXOs of it (`heldTokenView`).
   */
  source: 'indexer' | 'orders' | 'holdings' | 'chain';
  references: ExtensionReference[];
  mismatch: ExtensionReference[];
}
/** A commitment known without the indexer, and where it came from. */
export interface ExtensionReference { commitment: Hex; source: 'registry-genesis' | 'holdings' }

/** A parsed registry: its templates, their checks and the tokens by covenant id. */
export interface TokenRegistry {
  network: string;
  templates: RegistryTemplateJson[];
  templateChecks: TemplateCheck[];
  tokens: TokenInfo[];
  byCovenantId: ReadonlyMap<Hex, TokenInfo>;
}

// -------------------------------------------------------------------------------------- validation

const HEX32 = /^[0-9a-f]{64}$/;
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isInt = (v: unknown): v is number => typeof v === 'number' && Number.isSafeInteger(v);

const ROOT_KEYS = ['$schema', 'schema_version', 'network', 'templates', 'tokens'];
const TEMPLATE_KEYS = ['id', 'family', 'template_hash', 'prefix_len', 'suffix_len', 'state_len', 'max_token_inputs', 'max_token_outputs', 'escrow', 'review_status', 'capabilities', 'risks', 'source'];
const ESCROW_KEYS = ['owner_scheme', 'borrow_scheme', 'id_type', 'is_minter', 'delivery_id_type'];
const SOURCE_KEYS = ['path', 'upstream', 'note'];
const TOKEN_KEYS = ['ticker', 'name', 'family', 'covenant_id', 'template_id', 'extension_commitment', 'extension_class', 'decimals', 'lot_size', 'tick', 'max_token_inputs', 'max_token_outputs', 'verified', 'genesis_verified', 'genesis', 'display',
  // Judgements of older files. They are read and ignored (see RegistryTokenJson).
  'status', 'official', 'warning'];
const GENESIS_KEYS = ['txid', 'daa_score', 'outputs', 'supply', 'minter_outputs', 'mint_allowance', 'issuer', 'live_minters', 'checked_at_daa', 'source'];
/**
 * The pinned upstream builds of the public-mint app a genesis record may name.
 * Mirrors `IssuerVersion::label` of public_mint.rs.
 */
const ISSUER_BUILDS = ['kcc20-reference@c8a0871', 'kcc20-reference@27f8403'];
const OUTPOINT = /^[0-9a-f]{64}:(0|[1-9][0-9]{0,9})$/;
const DISPLAY_KEYS = ['description', 'website', 'icon', 'x', 'telegram', 'kcc23'];
const STATE_LEN: Record<Family, number> = { kcc20: 112, kron: 46 };

/**
 * Checks a template against the profile of the known program its hash names.
 * Mirrors `check_registry_template` of profile.rs. Returns the differences, or null when there are
 * none or the hash is unknown.
 */
function profileDisagreement(t: RegistryTemplateJson): string | null {
  const p = TOKEN_PROGRAMS.find((x) => x.templateHash === t.template_hash);
  if (!p) return null;
  const diffs: string[] = [];
  const cmp = (what: string, a: unknown, b: unknown) => {
    if (a !== b) diffs.push(`${what} ${String(a)} (profile ${p.name} says ${String(b)})`);
  };
  cmp('family', t.family, p.codec);
  cmp('prefix_len', t.prefix_len, p.prefixLen);
  cmp('state_len', t.state_len, p.stateLen);
  cmp('suffix_len', t.suffix_len, p.suffixLen);
  cmp('max_token_inputs', t.max_token_inputs, p.maxTokenInputs);
  cmp('max_token_outputs', t.max_token_outputs, p.maxTokenOutputs);
  // The custody owner is what the order templates pin. A registry may name either KRON delivery
  // type (0 or 3).
  cmp('escrow custody owner', p.codec === 'kcc20' ? t.escrow.owner_scheme : t.escrow.id_type, p.custodyOwner);
  return diffs.length ? diffs.join('; ') : null;
}

function validate(doc: unknown, expectedNetwork?: string): { json: RegistryJson; issues: RegistryIssue[] } {
  const issues: RegistryIssue[] = [];
  const add = (code: RegistryIssueCode, path: string, message: string) => issues.push({ code, path, message });
  if (!isObj(doc)) {
    add('wrong-type', '$', 'the registry must be a JSON object');
    return { json: doc as RegistryJson, issues };
  }
  const noExtra = (o: Record<string, unknown>, allowed: string[], path: string) => {
    for (const k of Object.keys(o)) if (!allowed.includes(k)) add('unknown-field', `${path}.${k}`, `unknown field \`${k}\``);
  };
  const short = (s: unknown) => (typeof s === 'string' && s.length > 24 ? s.slice(0, 24) + '…' : String(s));
  noExtra(doc, ROOT_KEYS, '$');
  if (doc.schema_version !== REGISTRY_SCHEMA_VERSION) add('schema-version', '$.schema_version', `unsupported schema_version ${String(doc.schema_version)} (this build reads ${REGISTRY_SCHEMA_VERSION})`);
  if (typeof doc.network !== 'string' || !(NETWORKS as readonly string[]).includes(doc.network)) add('unknown-network', '$.network', `unknown network \`${String(doc.network)}\``);
  else if (expectedNetwork && doc.network !== expectedNetwork) add('wrong-network', '$.network', `registry is for network \`${doc.network}\`, expected \`${expectedNetwork}\``);
  if (!Array.isArray(doc.templates)) add('wrong-type', '$.templates', 'templates must be an array');
  if (!Array.isArray(doc.tokens)) add('wrong-type', '$.tokens', 'tokens must be an array');
  if (!Array.isArray(doc.templates) || !Array.isArray(doc.tokens)) return { json: doc as unknown as RegistryJson, issues };

  // ---- templates
  const byId = new Map<string, RegistryTemplateJson>();
  const byHash = new Map<string, string>();
  (doc.templates as unknown[]).forEach((raw, idx) => {
    const p = `$.templates[${idx}]`;
    if (!isObj(raw)) return add('wrong-type', p, 'template must be an object');
    noExtra(raw, TEMPLATE_KEYS, p);
    const t = raw as unknown as RegistryTemplateJson;
    const bad = (m: string) => add('template-invalid', p, `template \`${t.id}\`: ${m}`);
    if (typeof t.id !== 'string' || !/^[a-z0-9_-]+$/.test(t.id)) bad('id must be non-empty lowercase [a-z0-9_-]');
    if (byId.has(t.id)) add('duplicate-template-id', p, `duplicate template id \`${t.id}\``);
    byId.set(t.id, t);
    if (!FAMILIES.includes(t.family)) bad('unknown family');
    if (typeof t.template_hash !== 'string' || !HEX32.test(t.template_hash)) add('bad-hex', `${p}.template_hash`, `expected 64 lowercase hex characters, got \`${short(t.template_hash)}\``);
    else {
      const prev = byHash.get(t.template_hash);
      if (prev) add('duplicate-template-hash', p, `templates \`${prev}\` and \`${t.id}\` have the same template hash`);
      byHash.set(t.template_hash, t.id);
    }
    for (const k of ['prefix_len', 'suffix_len', 'state_len', 'max_token_inputs', 'max_token_outputs'] as const) if (!isInt(t[k]) || t[k] < 0) bad(`${k} must be a non-negative integer`);
    if (FAMILIES.includes(t.family) && t.state_len !== STATE_LEN[t.family]) bad(`state_len ${t.state_len} but family ${t.family} has ${STATE_LEN[t.family]}`);
    if (!(t.suffix_len > 0)) bad('suffix_len must be > 0');
    if (!(t.max_token_inputs >= 1 && t.max_token_outputs >= 1 && t.max_token_inputs <= 64 && t.max_token_outputs <= 64)) bad(`slot limits ${t.max_token_inputs}/${t.max_token_outputs} must be within 1..=64`);
    if (!isObj(t.escrow)) bad('escrow must be an object');
    else {
      noExtra(t.escrow, ESCROW_KEYS, `${p}.escrow`);
      const s = t.escrow;
      if (t.family === 'kcc20') {
        if (s.owner_scheme !== 4 || s.borrow_scheme !== 0) bad('kcc20 escrow must be owner_scheme 4 (covenant id) with borrow_scheme 0');
        if (s.id_type !== undefined || s.is_minter !== undefined || s.delivery_id_type !== undefined) bad('kcc20 escrow must not carry kron fields');
      } else if (t.family === 'kron') {
        if (s.id_type !== 2 || s.is_minter !== 0) bad('kron escrow must be id_type 2 (covenant id) with is_minter 0');
        if (s.delivery_id_type !== 0 && s.delivery_id_type !== 3) bad('kron delivery_id_type must be 3 (address presence) or 0 (pubkey)');
        if (s.owner_scheme !== undefined || s.borrow_scheme !== undefined) bad('kron escrow must not carry kcc20 fields');
      }
    }
    if (t.review_status !== 'pending-review' && t.review_status !== 'reviewed') bad('review_status must be pending-review or reviewed');
    if (t.capabilities !== undefined) {
      if (!Array.isArray(t.capabilities) || t.capabilities.some((c) => !(CAPABILITIES as readonly unknown[]).includes(c))) bad('capabilities must be a list of ' + CAPABILITIES.join(', '));
      else if (new Set(t.capabilities).size !== t.capabilities.length) bad('duplicate capability');
    }
    if (t.risks !== undefined && (!Array.isArray(t.risks) || t.risks.some((r) => typeof r !== 'string' || r.length < 1 || r.length > 512 || hasBadChar(r)))) bad('risks must be a list of texts (1..512 characters)');
    if (!isObj(t.source) || typeof t.source.path !== 'string' || !t.source.path) bad('source.path is empty');
    else noExtra(t.source, SOURCE_KEYS, `${p}.source`);
    const disagreement = isObj(t.escrow) ? profileDisagreement(t) : null;
    if (disagreement) bad(`disagrees with the embedded program profile: ${disagreement}`);
  });

  // ---- tokens
  const identities = new Map<string, string>();
  const covs = new Map<string, string>();
  (doc.tokens as unknown[]).forEach((raw, idx) => {
    const p = `$.tokens[${idx}]`;
    if (!isObj(raw)) return add('wrong-type', p, 'token must be an object');
    noExtra(raw, TOKEN_KEYS, p);
    const tok = raw as unknown as RegistryTokenJson;
    const tk = String(tok.ticker);
    if (typeof tok.ticker !== 'string' || tk.length < 2 || tk.length > 12) add('bad-ticker', p, `ticker \`${short(tk)}\`: length must be 2..=12`);
    else if (!/^[A-Z0-9]+$/.test(tk)) add('bad-ticker', p, `ticker \`${short(tk)}\`: only ASCII uppercase letters and digits are allowed`);
    // No uniqueness: a shared or look-alike ticker is not refused (registry.rs). Choosing which
    // token may carry a ticker would be curation.
    const disp = (m: string) => add('bad-display', p, `token ${short(tk)}: ${m}`);
    if (typeof tok.name !== 'string' || !tok.name.trim() || [...tok.name].length > 64 || hasBadChar(tok.name)) disp('name must be 1..=64 printable characters (no control, zero-width or bidi characters)');
    if (tok.display !== undefined) {
      if (!isObj(tok.display)) disp('display must be an object');
      else {
        noExtra(tok.display, DISPLAY_KEYS, `${p}.display`);
        const d = tok.display;
        for (const k of ['description', 'website', 'icon', 'x', 'telegram'] as const) {
          const v = d[k];
          if (v !== undefined && (typeof v !== 'string' || [...v].length > 512 || hasBadChar(v))) disp(`${k} is too long or has control characters`);
        }
        if (typeof d.website === 'string' && !d.website.startsWith('https://')) disp('website must be an https:// URL');
        if (typeof d.icon === 'string' && !(d.icon.startsWith('https://') || d.icon.startsWith('ipfs://'))) disp('icon must be an https:// or ipfs:// URL');
        if (typeof d.x === 'string' && !d.x.startsWith('https://')) disp('x must be an https:// URL');
        if (typeof d.telegram === 'string' && !d.telegram.startsWith('https://')) disp('telegram must be an https:// URL');
        if (d.kcc23 !== undefined && !isObj(d.kcc23)) disp('kcc23 must be a JSON object');
      }
    }
    if (typeof tok.covenant_id !== 'string' || !HEX32.test(tok.covenant_id)) add('bad-hex', `${p}.covenant_id`, `expected 64 lowercase hex characters, got \`${short(tok.covenant_id)}\``);
    if (tok.extension_commitment !== null && (typeof tok.extension_commitment !== 'string' || !HEX32.test(tok.extension_commitment))) {
      add('bad-hex', `${p}.extension_commitment`, `expected 64 lowercase hex characters or null, got \`${short(tok.extension_commitment)}\``);
    }
    const prevCov = covs.get(tok.covenant_id);
    if (prevCov) add('duplicate-covenant-id', p, `tokens ${prevCov} and ${tk} share covenant id ${short(tok.covenant_id)}`);
    covs.set(tok.covenant_id, tk);
    if (!isInt(tok.decimals) || tok.decimals < 0 || tok.decimals > MAX_DECIMALS) add('decimals', p, `token ${tk}: decimals ${String(tok.decimals)} must be 0..=${MAX_DECIMALS}`);
    // `lot_size` and `tick` are legacy, and `status`, `official` and `warning` are removed
    // judgements. They are read and ignored, never validated (registry.rs).
    if (typeof tok.verified !== 'boolean') add('wrong-type', `${p}.verified`, 'verified must be a boolean');
    if (tok.genesis_verified !== undefined && typeof tok.genesis_verified !== 'boolean') add('wrong-type', `${p}.genesis_verified`, 'genesis_verified must be a boolean');
    const g = tok.genesis;
    if (g !== undefined) {
      const gp = `${p}.genesis`;
      const gbad = (m: string) => add('wrong-type', gp, `token ${tk}: ${m}`);
      if (!isObj(g)) gbad('genesis must be an object');
      else {
        noExtra(g, GENESIS_KEYS, gp);
        if (tok.genesis_verified === undefined) gbad('a genesis record needs genesis_verified');
        if (typeof g.txid !== 'string' || !HEX32.test(g.txid)) add('bad-hex', `${gp}.txid`, `expected 64 lowercase hex characters, got \`${short(g.txid)}\``);
        for (const k of ['daa_score', 'checked_at_daa', 'supply'] as const) if (!isInt(g[k]) || g[k] < 0) gbad(`genesis.${k} must be a non-negative integer`);
        const indices = (v: unknown): v is number[] => Array.isArray(v) && v.every((x) => isInt(x) && x >= 0);
        const outs = indices(g.outputs) ? g.outputs : null;
        if (!outs || outs.length === 0 || outs.some((x, i) => i > 0 && x <= outs[i - 1])) gbad('genesis.outputs must be a non-empty, strictly increasing list of output indices');
        if (!indices(g.minter_outputs) || (outs && g.minter_outputs.some((m) => !outs.includes(m)))) gbad('genesis.minter_outputs must be genesis outputs');
        if (g.mint_allowance !== undefined) {
          if (!isInt(g.mint_allowance) || g.mint_allowance < 0) gbad('genesis.mint_allowance must be a non-negative integer');
          else if (!Array.isArray(g.minter_outputs) || g.minter_outputs.length === 0) gbad('genesis.mint_allowance needs the genesis minter outputs that hold it');
        }
        if (g.issuer !== undefined && (typeof g.issuer !== 'string' || !ISSUER_BUILDS.includes(g.issuer))) gbad('genesis.issuer must name a pinned upstream build of the public-mint app (kcc20-reference@<commit>)');
        if (typeof g.source !== 'string' || !g.source || [...g.source].length > 512 || hasBadChar(g.source)) gbad('genesis.source must be 1..=512 printable characters');
        if (g.live_minters !== undefined) {
          if (!Array.isArray(g.live_minters) || g.live_minters.some((o) => typeof o !== 'string' || !OUTPOINT.test(o))) gbad('genesis.live_minters entries must be <txid>:<index>');
        }
      }
    }
    if (tok.genesis_verified === true && tok.verified !== true) add('genesis', p, `token ${tk}: genesis_verified needs verified (the covenant id and template hash checked first)`);
    if (!['none', 'fixed-supply-standard', 'other'].includes(tok.extension_class)) add('wrong-type', `${p}.extension_class`, 'unknown extension_class');
    const tpl = byId.get(tok.template_id);
    if (!tpl) return add('unknown-template', p, `token ${tk}: unknown template \`${tok.template_id}\``);
    if (tpl.family !== tok.family) add('family-mismatch', p, `token ${tk}: family ${tok.family} but template \`${tpl.id}\` is family ${tpl.family}`);
    const ext = (m: string) => add('extension', p, `token ${tk}: ${m}`);
    if (tok.family === 'kron') {
      if (tok.extension_commitment !== null) ext('kron tokens have no extension_commitment (must be null)');
      if (tok.extension_class !== 'none') ext('kron tokens must have extension_class `none`');
    } else if (tok.family === 'kcc20') {
      if (tok.extension_commitment === null) ext('kcc20 tokens must carry an extension_commitment (fungibility only holds among equal commitments)');
      else if (tok.extension_class === 'none' && HEX32.test(tok.extension_commitment) && tok.extension_commitment !== '00'.repeat(32)) ext('extension_class `none` requires an all-zero extension_commitment');
    }
    if (tok.max_token_inputs !== undefined || tok.max_token_outputs !== undefined) {
      const a = tok.max_token_inputs ?? tpl.max_token_inputs;
      const b = tok.max_token_outputs ?? tpl.max_token_outputs;
      if (a !== tpl.max_token_inputs || b !== tpl.max_token_outputs) add('slot-limit-mismatch', p, `token ${tk}: slot limits ${a}/${b} differ from template \`${tpl.id}\` (${tpl.max_token_inputs}/${tpl.max_token_outputs})`);
    }
    const key = `${tok.family}|${tok.covenant_id}|${tpl.template_hash}|${tok.extension_commitment ?? ''}`;
    const prevKey = identities.get(key);
    if (prevKey) add('duplicate-identity', p, `tokens ${prevKey} and ${tk} have the same identity (family, covenant id, template hash, extension commitment)`);
    identities.set(key, tk);
  });
  return { json: doc as unknown as RegistryJson, issues };
}

// --------------------------------------------------------------------------- template verification

/**
 * Compares every registry template with the wasm's pinned templates.
 * It checks the mapped program, hash, prefix, suffix and state length, and the slot limits.
 */
export function checkTemplates(kob: KobWasm, templates: RegistryTemplateJson[]): TemplateCheck[] {
  const pinned = kob.templates();
  return templates.map((t): TemplateCheck => {
    const problems: string[] = [];
    let expected = PROGRAM_OF_TEMPLATE_ID[t.id] ?? null;
    const byHash = pinned.find((p) => p.hash === t.template_hash && p.tokenSlots);
    if (!expected && byHash) expected = byHash.name as TokenProgram;
    const p = expected ? pinned.find((x) => x.name === expected && x.tokenSlots) : undefined;
    if (!expected || !p) return { id: t.id, program: null, matchesPinned: false, problems: ['program-unknown'] };
    if (familyOfProgram(expected) !== t.family) return { id: t.id, program: null, matchesPinned: false, problems: ['family-mismatch'] };
    if (p.hash !== t.template_hash) problems.push('hash-mismatch');
    if (p.prefixLen !== t.prefix_len) problems.push('prefix-mismatch');
    if (p.suffixLen !== t.suffix_len) problems.push('suffix-mismatch');
    if (p.stateLen !== t.state_len) problems.push('state-len-mismatch');
    if (!p.tokenSlots || p.tokenSlots[0] !== t.max_token_inputs || p.tokenSlots[1] !== t.max_token_outputs) problems.push('slots-mismatch');
    return { id: t.id, program: expected, matchesPinned: problems.length === 0, problems };
  });
}

/**
 * Why this build cannot trade a token of this template, or null if it can.
 * It looks at the program allowlist and the wasm pin only, never at the token.
 */
function assess(tpl: RegistryTemplateJson, check: TemplateCheck): UntradableReason | null {
  if (tpl.review_status !== 'reviewed') return 'template-pending-review';
  if (!check.program) return 'template-unsupported';
  if (!check.matchesPinned) return 'template-mismatch';
  return null;
}

// ------------------------------------------------------------------------------------------- parse

/** Options of `parseRegistry`. */
export interface ParseOptions {
  /** The wasm build the app runs. Template hashes are verified against its pinned templates. */
  kob: KobWasm;
  /** Require this network, so a mainnet build refuses a testnet registry. */
  network?: string;
}

/**
 * Parses and template-verifies a registry, applying all rules of the Rust validator.
 * Throws `RegistryError` listing every finding.
 */
export function parseRegistry(json: string | unknown, opts: ParseOptions): TokenRegistry {
  let doc: unknown = json;
  if (typeof json === 'string') {
    try {
      doc = JSON.parse(json);
    } catch (e) {
      throw new RegistryError([{ code: 'json', path: '$', message: `invalid registry json: ${e instanceof Error ? e.message : String(e)}` }]);
    }
  }
  const { json: reg, issues } = validate(doc, opts.network);
  if (issues.length) throw new RegistryError(issues);
  const templateChecks = checkTemplates(opts.kob, reg.templates);
  const tplById = new Map(reg.templates.map((t) => [t.id, t]));
  const checkById = new Map(templateChecks.map((c) => [c.id, c]));
  const tokens = reg.tokens.map((t): TokenInfo => {
    const tpl = tplById.get(t.template_id)!;
    const check = checkById.get(t.template_id)!;
    const reason = assess(tpl, check);
    return {
      ticker: t.ticker, name: t.name, family: t.family, covenantId: t.covenant_id, templateId: t.template_id, templateHash: tpl.template_hash,
      extensionCommitment: t.extension_commitment, extensionClass: t.extension_class, decimals: t.decimals,
      tick: null,
      capabilities: tpl.capabilities ?? [], display: t.display ?? null, program: check.program, templateMatchesPinned: check.matchesPinned, prefixLen: tpl.prefix_len, suffixLen: tpl.suffix_len,
      slots: { inputs: tpl.max_token_inputs, outputs: tpl.max_token_outputs }, tradable: reason === null, untradableReason: reason, json: t,
    };
  });
  return { network: reg.network, templates: reg.templates, templateChecks, tokens, byCovenantId: new Map(tokens.map((t) => [t.covenantId, t])) };
}

// ----------------------------------------------------------------------------- lookups and display

/** The token with this covenant id, if the registry has it. */
export const tokenById = (reg: TokenRegistry, covenantId: Hex): TokenInfo | undefined => reg.byCovenantId.get(covenantId);
/** The tokens this build can trade. */
export const tradableTokens = (reg: TokenRegistry): TokenInfo[] => reg.tokens.filter((t) => t.tradable);
/** The registry template with this id. */
export const templateById = (reg: TokenRegistry, id: string): RegistryTemplateJson | undefined => reg.templates.find((t) => t.id === id);

/**
 * `TICKER (abcd…1234)`, never the name alone.
 * Tokens with the same ticker are told apart by covenant id.
 */
export function displayName(token: Pick<TokenInfo, 'ticker' | 'covenantId'>): string {
  const c = token.covenantId;
  const short = shortId(c);
  // A token synthesised from the indexer has its covenant id fragment as ticker. It is shown
  // once, in the long form.
  return token.ticker === short || token.ticker === longId(c) ? longId(c) : `${token.ticker} (${short})`;
}

/** `abcd…1234`: 4 + 4 hex of a covenant id, enough next to a registry ticker. */
export const shortId = (c: string): string => (c.length >= 8 ? `${c.slice(0, 4)}…${c.slice(-4)}` : c);

/**
 * `abcdef01…12345678`: 8 + 8 hex (64 bits) of a covenant id.
 * It is for a token the registry does not list, whose id is all that identifies it. A 4 + 4
 * fragment (32 bits) can be matched by a token made for that purpose.
 */
export const longId = (c: string): string => (c.length > 16 ? `${c.slice(0, 8)}…${c.slice(-8)}` : c);

/**
 * `abcdef012345…123456789abc`: 12 + 12 hex (96 bits) of a covenant id.
 * It is the fragment beside a token in the market list and in its header (`IdCopy`). Name,
 * ticker and picture are free to copy and the picture carries about 35 bits, so the id is the
 * only identity there. 96 bits cannot be ground for off chain. The full id is the tooltip, what
 * the button copies and what a confirmation checks.
 */
export const wideId = (c: string): string => (c.length > 24 ? `${c.slice(0, 12)}…${c.slice(-12)}` : c);

/**
 * The registry plus one more token (an open-list token synthesised from the indexer).
 * The pre-sign screen decodes against it.
 */
export function withExtraToken(reg: TokenRegistry, token: TokenInfo): TokenRegistry {
  if (reg.byCovenantId.has(token.covenantId)) return reg;
  return { ...reg, tokens: [...reg.tokens, token], byCovenantId: new Map([...reg.byCovenantId, [token.covenantId, token]]) };
}

/** The shape `toTokenMarket` consumes (registry JSON keys, snake_case). */
export function tokenMarketInput(token: TokenInfo): TokenMarketInput {
  const j = token.json;
  return {
    ticker: j.ticker, covenant_id: j.covenant_id, template_id: j.template_id, extension_commitment: j.extension_commitment, decimals: j.decimals,
    tick: null,
  };
}

// --------------------------------------------------------------------------- on-chain verification

/** One problem found by an on-chain check. */
export interface VerifyProblem { code: string; message: string }
/** The outcome of an on-chain check. */
export interface VerifyResult { ok: boolean; problems: VerifyProblem[] }

/**
 * A token UTXO as seen on chain: its covenant id, its P2SH script public key (kaspa string form)
 * and its decoded token state (either family).
 */
export interface ChainTokenUtxo { covenantId: Hex | null; scriptPublicKey: string; state: TokenState }

/**
 * Checks that a token UTXO belongs to the registered token.
 * Its covenant id must equal the registry id, its state must carry the registry's extension
 * commitment, and its script public key must be exactly `tokenScriptPublicKey(program, state)`.
 * That proves the template hash, prefix and suffix.
 */
export function verifyTokenUtxo(kob: KobWasm, token: TokenInfo, utxo: ChainTokenUtxo): VerifyResult {
  const problems: VerifyProblem[] = [];
  if (utxo.covenantId !== token.covenantId) problems.push({ code: 'covenant-id-mismatch', message: `covenant id ${utxo.covenantId ?? 'none'} is not the registered ${token.covenantId}` });
  if (token.family === 'kron' ? !isKronState(utxo.state) : token.extensionCommitment !== null && extensionOfState(utxo.state) !== token.extensionCommitment) {
    problems.push({ code: 'extension-mismatch', message: 'the extension commitment differs from the registry' });
  }
  if (!token.templateMatchesPinned) problems.push({ code: 'template-mismatch', message: 'the registry template differs from the token program pinned in this build' });
  if (!token.program) problems.push({ code: 'unsupported-program', message: 'this build does not embed the token program' });
  else {
    let expected: string | null = null;
    try {
      expected = kob.tokenScriptPublicKey(token.program, utxo.state);
    } catch {
      problems.push({ code: 'bad-state', message: 'the token state cannot be encoded' });
    }
    if (expected !== null && expected !== utxo.scriptPublicKey) problems.push({ code: 'script-mismatch', message: 'the UTXO script is not the registered token program over this state' });
  }
  return { ok: problems.length === 0, problems };
}

/**
 * Cross-checks the indexer's view of a token with the registry (template hash, extension
 * commitment, covenant id, decimals). Missing data is reported, not assumed.
 */
export function verifyIndexerToken(token: TokenInfo, view: IndexerTokenView): VerifyResult & { missing: string[] } {
  const problems: VerifyProblem[] = [];
  const missing: string[] = [];
  if (view.covenant_id !== token.covenantId) problems.push({ code: 'covenant-id-mismatch', message: 'the indexer reports another covenant id' });
  if (view.template_hash == null) missing.push('template_hash');
  else if (view.template_hash !== token.templateHash) problems.push({ code: 'template-hash-mismatch', message: 'the indexer reports another token template hash' });
  if (token.extensionCommitment !== null) {
    if (view.extension_commitment == null) missing.push('extension_commitment');
    else if (view.extension_commitment !== token.extensionCommitment) problems.push({ code: 'extension-mismatch', message: 'the indexer reports another extension commitment' });
  }
  if (view.decimals != null && view.decimals !== token.decimals) problems.push({ code: 'decimals-mismatch', message: 'the indexer reports other decimals' });
  // The token's standard scale (10^decimals, at most 10^9). A different scale means a different
  // book, with prices of another whole token.
  const scale = 10n ** BigInt(Math.min(token.decimals, 9));
  if (view.scale != null && BigInt(view.scale) !== scale) problems.push({ code: 'scale-mismatch', message: 'the indexer lists another price scale' });
  return { ok: problems.length === 0 && missing.length === 0, problems, missing };
}
