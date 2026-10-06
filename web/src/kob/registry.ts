// Token registry: validation of `registry/tokens.json`, template-hash-verified listing, lookalike protection and on-chain verification helpers.
//
// The registry file is data, not authority. The token list is OPEN (founder decision 2026-09-30): what gates trading is the TEMPLATE, not the token.
// A token is TRADABLE in this build if
//   * it is not `delisted` (explicit delisting is the only token-level block), and
//   * its template is `reviewed`, supported (a program compiled into kob-wasm) and equals the wasm's pinned template
//     (hash, prefix, suffix, slot limits): a registry entry can never make the app trade a program it does not embed, and
//   (protocol v3 has no lots and no price tick: an order's scale is `10^decimals` of the token and any price in sompi is valid, so the
//   registry's legacy `lot_size` and `tick` are optional, read from older registry files and ignored, exactly as the Rust validator does.)
// A registry entry is never treated more strictly than a token with no entry (open-list token, kob/open-token.ts): status `pending-review` and
// `verified: false` do not block trading. They are shown (badges, label, caution lines) exactly like the open-list token's `unverified`.
// Everything else is viewable (balances, positions) but not tradable, with a machine-readable reason.
//
// Mirrors crates/kob-protocol/src/registry.rs (validation rules, `normalize_ticker`, `display_name`); keys of the JSON are snake_case.
import type { IndexerTokenView } from '../data/indexer-types';
import { extensionOfState, familyOfProgram, isKronState } from './token-state';
import type { Hex, TokenProgram, TokenState } from './types';
import type { KobWasm } from './wasm';

export const REGISTRY_SCHEMA_VERSION = 1;
export const NETWORKS = ['mainnet', 'testnet-10', 'devnet'] as const;
export const MAX_DECIMALS = 18;
export const FAMILIES = ['kcc20', 'kron'] as const;
export type Family = (typeof FAMILIES)[number];
export type TokenStatus = 'pending-review' | 'listed' | 'delisted';
export type ReviewStatus = 'pending-review' | 'reviewed';
export type ExtensionClass = 'none' | 'fixed-supply-standard' | 'other';

/** What a token program's authorities can do beyond owner-authorised transfers (registry `capabilities`, mirrors `Capability` of registry.rs). */
export const CAPABILITIES = ['mint-authority', 'public-mint', 'burn', 'freeze', 'seize', 'blacklist'] as const;
export type Capability = (typeof CAPABILITIES)[number];

/** Registry template id -> kob-wasm token program (only the programs compiled into this build; both families). */
export const PROGRAM_OF_TEMPLATE_ID: Readonly<Record<string, TokenProgram>> = {
  'kcc20-ref-3x3': 'KCC20Ref',
  'kcc20-ref-8x8': 'KCC20Ref_8x8',
  'kcc20-kaspacom-0-2-5': 'KCC20KaspaCom_0_2_5',
  'kron-2433': 'KronToken2433',
  'kron-2732': 'KronToken2732',
};

// ------------------------------------------------------------------------------------------------ JSON shapes (snake_case)

export interface RegistryTemplateJson {
  id: string; family: Family; template_hash: Hex; prefix_len: number; suffix_len: number; state_len: number;
  max_token_inputs: number; max_token_outputs: number;
  escrow: { owner_scheme?: number; borrow_scheme?: number; id_type?: number; is_minter?: number; delivery_id_type?: number };
  review_status: ReviewStatus; source: { path: string; upstream?: string; note?: string };
  /** what the program's authorities can do (freeze, seize and blacklist are labelled in every UI); absent = nothing found */
  capabilities?: Capability[];
  /** declared risks of the program that are not a capability (missing hardening, signing assumptions); absent = none declared */
  risks?: string[];
}
export interface RegistryTokenJson {
  ticker: string; name: string; family: Family; covenant_id: Hex; template_id: string; extension_commitment: Hex | null; extension_class: ExtensionClass;
  decimals: number;
  /** legacy (protocol v2 lots and price tick): optional, read from older registry files and ignored (registry.rs) */
  lot_size?: number | null; tick?: number | null;
  max_token_inputs?: number; max_token_outputs?: number; status: TokenStatus; verified: boolean;
  /** confirmed genuine (needs verified and listed); absent = false (shown as unverified) */
  official?: boolean;
  /**
   * the token's genesis (the origin of its covenant id) was verified against chain data by the registry maintainers (contracts review pass);
   * absent / false = NOT verified. `warning` is an optional maintainer note shown next to the token (e.g. "genesis could not be verified").
   */
  genesis_verified?: boolean;
  /** what the genesis check (C1) and the live-mint-authority check (C2) found (`kob registry verify-genesis` re-derives it from registry/evidence/) */
  genesis?: RegistryGenesisJson;
  warning?: string;
  display?: { description?: string; website?: string; icon?: string; kcc23?: Record<string, unknown> };
}
/** A token's genesis record (mirrors `GenesisRecord` of registry.rs). */
export interface RegistryGenesisJson {
  txid: Hex; daa_score: number; outputs: number[]; supply: number; minter_outputs: number[];
  /** live mint-authority cells (`txid:index`): [] = none, absent = not determined; official needs [] */
  live_minters?: string[];
  checked_at_daa: number; source: string;
}
export interface RegistryJson {
  $schema?: string; schema_version: number; network: string; templates: RegistryTemplateJson[]; tokens: RegistryTokenJson[];
}

/** The structural shape A1's `toTokenMarket` consumes. */
export interface TokenMarketInput {
  ticker: string; covenant_id: Hex; template_id: string; extension_commitment: Hex | null; decimals: number;
  /** legacy price tick: always null (protocol v3 has none; any price in sompi per whole token is valid) */
  tick: number | null;
}

// ------------------------------------------------------------------------------------------------ issue codes

export const REGISTRY_ISSUE_CODES = [
  'json', 'unknown-field', 'wrong-type', 'schema-version', 'unknown-network', 'wrong-network', 'bad-hex', 'duplicate-template-id', 'duplicate-template-hash',
  'template-invalid', 'unknown-template', 'family-mismatch', 'extension', 'duplicate-identity', 'duplicate-covenant-id', 'bad-ticker', 'confusable-ticker',
  'bad-display', 'out-of-range', 'decimals', 'slot-limit-mismatch', 'not-listable',
] as const;
export type RegistryIssueCode = (typeof REGISTRY_ISSUE_CODES)[number];
export interface RegistryIssue { code: RegistryIssueCode; message: string; path: string }

export class RegistryError extends Error {
  readonly issues: RegistryIssue[];
  constructor(issues: RegistryIssue[]) {
    super(`registry invalid: ${issues.map((i) => `${i.path}: ${i.message}`).join('; ')}`);
    this.name = 'RegistryError';
    this.issues = issues;
  }
}

/**
 * Why a token cannot be traded in this build (`null` = tradable). `unverified` is only given to tokens the registry does not know (token-model
 * `unknownRow`: no template at hand); a registry entry is never untradable for being unverified or pending review.
 */
export const UNTRADABLE_REASONS = [
  'delisted', 'unverified', 'template-pending-review', 'template-unsupported', 'template-mismatch',
] as const;
export type UntradableReason = (typeof UNTRADABLE_REASONS)[number];

// ------------------------------------------------------------------------------------------------ text safety

const BAD_CHAR = /[\u0000-\u001F\u007F-\u009F​-‏‪-‮⁠-⁤⁦-⁩﻿]/u;
export const hasBadChar = (s: string): boolean => BAD_CHAR.test(s);

/**
 * Non-Latin capitals that are drawn like a Latin letter (Cyrillic, Greek, Armenian; a small UTS #39 confusables subset), keyed by the UPPERCASE
 * form: `toUpperCase` runs first, so the lowercase twins (а, е, о, р, с, х ...) are covered too. Letters with no lookalike are not listed.
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
 * Homoglyph skeleton of a ticker or name. Steps: NFKC (full-width, ligatures, roman numerals), strip combining marks,
 * uppercase, fold Cyrillic / Greek / Armenian lookalikes to Latin, then the ASCII rules that are IDENTICAL to Rust `normalize_ticker`:
 * RN -> M, VV -> W, O -> 0, I and L -> 1, S -> 5, B -> 8. Registry tickers are ASCII, so only that last step applies to them; untrusted
 * (indexer / pasted) text gets the whole chain.
 */
export function normalizeTicker(ticker: string): string {
  const upper = ticker.normalize('NFKC').normalize('NFD').replace(/\p{M}/gu, '').toUpperCase().normalize('NFKC');
  const latin = [...upper].map((c) => CONFUSABLE_LATIN[c] ?? c).join('');
  return [...latin.replace(/RN/g, 'M').replace(/VV/g, 'W')].map((c) => (c === 'O' ? '0' : c === 'I' || c === 'L' ? '1' : c === 'S' ? '5' : c === 'B' ? '8' : c)).join('');
}

/** Skeleton of a display NAME: like `normalizeTicker`, with spaces, dots, dashes and underscores dropped ("Kaspa Coin" ~ "KASPACOIN"). */
export function normalizeName(name: string): string {
  return normalizeTicker(name).replace(/[\s.\-_·]+/gu, '');
}

/** Makes an UNTRUSTED name (indexer, pasted) safe to render: control, zero-width and bidi characters become U+FFFD, length capped. */
export function sanitizeUntrusted(s: string, max = 32): string {
  const cleaned = [...s].map((c) => (BAD_CHAR.test(c) ? '�' : c)).join('');
  const chars = [...cleaned];
  return chars.length > max ? chars.slice(0, max).join('') + '…' : cleaned;
}

// ------------------------------------------------------------------------------------------------ normalized model

export interface TemplateCheck {
  id: string;
  /** kob-wasm program this template maps to (null: not supported by this build) */
  program: TokenProgram | null;
  /** registry values agree with the wasm's pinned template (hash, prefix, suffix, state length, slot limits) */
  matchesPinned: boolean;
  problems: string[];
}

export interface TokenInfo {
  ticker: string; name: string; family: Family; covenantId: Hex; templateId: string; templateHash: Hex;
  extensionCommitment: Hex | null; extensionClass: ExtensionClass; decimals: number;
  /** minimum price increment, sompi per whole token: always null (= 1 sompi). Protocol v3 has no price tick; the registry's legacy `tick` is ignored. */
  tick: bigint | null;
  status: TokenStatus; verified: boolean; display: RegistryTokenJson['display'] | null;
  /** confirmed genuine by KOB (registry `official`) */
  official: boolean;
  /** registry `genesis_verified`: true / false as declared, null when the registry says nothing (= not verified) */
  genesisVerified?: boolean | null;
  /** maintainer warning of the registry entry (`warning`) */
  warning?: string | null;
  /** capabilities of the token's program template (registry `capabilities`), possibly empty */
  capabilities: Capability[];
  /** set when the registry in use is NOT the build-pinned default (registry-source.ts): its short hash. Such a token is never official / verified by KOB. */
  customRegistry?: string | null;
  /** synthesised from the indexer for an open-list token that has no registry entry (kob/open-token.ts): unverified, decimals 0, tick 1 */
  openList?: boolean;
  /** kob-wasm program (null when this build cannot handle the template) */
  program: TokenProgram | null;
  /** the registry template agrees with the wasm's pinned template (hash, prefix, suffix, slots) */
  templateMatchesPinned: boolean;
  prefixLen: number; suffixLen: number; slots: { inputs: number; outputs: number };
  tradable: boolean; untradableReason: UntradableReason | null;
  json: RegistryTokenJson;
}

export interface TokenRegistry {
  network: string;
  templates: RegistryTemplateJson[];
  templateChecks: TemplateCheck[];
  tokens: TokenInfo[];
  byCovenantId: ReadonlyMap<Hex, TokenInfo>;
  /** set when this is not the pinned default registry (registry-source.ts `markCustomRegistry`) */
  custom?: { shortHash: string };
}

// ------------------------------------------------------------------------------------------------ validation

const HEX32 = /^[0-9a-f]{64}$/;
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isInt = (v: unknown): v is number => typeof v === 'number' && Number.isSafeInteger(v);

const ROOT_KEYS = ['$schema', 'schema_version', 'network', 'templates', 'tokens'];
const TEMPLATE_KEYS = ['id', 'family', 'template_hash', 'prefix_len', 'suffix_len', 'state_len', 'max_token_inputs', 'max_token_outputs', 'escrow', 'review_status', 'capabilities', 'risks', 'source'];
const ESCROW_KEYS = ['owner_scheme', 'borrow_scheme', 'id_type', 'is_minter', 'delivery_id_type'];
const SOURCE_KEYS = ['path', 'upstream', 'note'];
const TOKEN_KEYS = ['ticker', 'name', 'family', 'covenant_id', 'template_id', 'extension_commitment', 'extension_class', 'decimals', 'lot_size', 'tick', 'max_token_inputs', 'max_token_outputs', 'status', 'verified', 'official', 'genesis_verified', 'genesis', 'warning', 'display'];
const GENESIS_KEYS = ['txid', 'daa_score', 'outputs', 'supply', 'minter_outputs', 'live_minters', 'checked_at_daa', 'source'];
const OUTPOINT = /^[0-9a-f]{64}:(0|[1-9][0-9]{0,9})$/;
const DISPLAY_KEYS = ['description', 'website', 'icon', 'kcc23'];
const STATE_LEN: Record<Family, number> = { kcc20: 112, kron: 46 };

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
    if (t.family === 'kron' && t.prefix_len !== 0) bad('kron templates have no prefix (state span at offset 0)');
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
  });

  // ---- tokens
  const identities = new Map<string, string>();
  const covs = new Map<string, string>();
  const norm = new Map<string, string>();
  (doc.tokens as unknown[]).forEach((raw, idx) => {
    const p = `$.tokens[${idx}]`;
    if (!isObj(raw)) return add('wrong-type', p, 'token must be an object');
    noExtra(raw, TOKEN_KEYS, p);
    const tok = raw as unknown as RegistryTokenJson;
    const tk = String(tok.ticker);
    if (typeof tok.ticker !== 'string' || tk.length < 2 || tk.length > 12) add('bad-ticker', p, `ticker \`${short(tk)}\`: length must be 2..=12`);
    else if (!/^[A-Z0-9]+$/.test(tk)) add('bad-ticker', p, `ticker \`${short(tk)}\`: only ASCII uppercase letters and digits are allowed`);
    else {
      const n = normalizeTicker(tk);
      const prev = norm.get(n);
      if (prev === tk) add('bad-ticker', p, `ticker \`${tk}\`: duplicate ticker`);
      else if (prev) add('confusable-ticker', p, `tickers ${prev} and ${tk} are confusable (both normalise to ${n})`);
      else norm.set(n, tk);
    }
    const disp = (m: string) => add('bad-display', p, `token ${short(tk)}: ${m}`);
    if (typeof tok.name !== 'string' || !tok.name.trim() || [...tok.name].length > 64 || hasBadChar(tok.name)) disp('name must be 1..=64 printable characters (no control, zero-width or bidi characters)');
    if (tok.display !== undefined) {
      if (!isObj(tok.display)) disp('display must be an object');
      else {
        noExtra(tok.display, DISPLAY_KEYS, `${p}.display`);
        const d = tok.display;
        for (const k of ['description', 'website', 'icon'] as const) {
          const v = d[k];
          if (v !== undefined && (typeof v !== 'string' || [...v].length > 512 || hasBadChar(v))) disp(`${k} is too long or has control characters`);
        }
        if (typeof d.website === 'string' && !d.website.startsWith('https://')) disp('website must be an https:// URL');
        if (typeof d.icon === 'string' && !(d.icon.startsWith('https://') || d.icon.startsWith('ipfs://'))) disp('icon must be an https:// or ipfs:// URL');
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
    // `lot_size` / `tick` are legacy (protocol v2): read and ignored, never validated (registry.rs)
    if (!['pending-review', 'listed', 'delisted'].includes(tok.status)) add('wrong-type', `${p}.status`, 'status must be pending-review, listed or delisted');
    if (typeof tok.verified !== 'boolean') add('wrong-type', `${p}.verified`, 'verified must be a boolean');
    if (tok.official !== undefined && typeof tok.official !== 'boolean') add('wrong-type', `${p}.official`, 'official must be a boolean');
    if (tok.genesis_verified !== undefined && typeof tok.genesis_verified !== 'boolean') add('wrong-type', `${p}.genesis_verified`, 'genesis_verified must be a boolean');
    if (tok.warning !== undefined && (typeof tok.warning !== 'string' || !tok.warning.trim() || [...tok.warning].length > 512 || hasBadChar(tok.warning))) {
      add('bad-display', `${p}.warning`, `token ${tk}: warning must be 1..=512 printable characters`);
    }
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
        if (typeof g.source !== 'string' || !g.source || [...g.source].length > 512 || hasBadChar(g.source)) gbad('genesis.source must be 1..=512 printable characters');
        if (g.live_minters !== undefined) {
          if (!Array.isArray(g.live_minters) || g.live_minters.some((o) => typeof o !== 'string' || !OUTPOINT.test(o))) gbad('genesis.live_minters entries must be <txid>:<index>');
          else if (g.live_minters.length > 0 && tok.warning === undefined) gbad('a token with a live mint authority needs a warning (active mint authority)');
        }
      }
    }
    if (tok.genesis_verified === true && tok.verified !== true) add('not-listable', p, `token ${tk}: genesis_verified needs verified (the covenant id and template hash checked first)`);
    if (tok.official === true) {
      if (!tok.verified) add('not-listable', p, `token ${tk}: cannot be official: not verified against chain`);
      if (tok.status !== 'listed') add('not-listable', p, `token ${tk}: cannot be official: only a listed token can be official`);
      if (tok.genesis_verified !== true) add('not-listable', p, `token ${tk}: cannot be official without genesis_verified: true (every genesis output checked)`);
      if (!isObj(g) || !Array.isArray(g.live_minters) || g.live_minters.length !== 0) {
        add('not-listable', p, `token ${tk}: cannot be official without a genesis record that found no live mint authority (live_minters: [])`);
      }
    }
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
    if (tok.status === 'listed') {
      const nl = (m: string) => add('not-listable', p, `token ${tk}: cannot be listed: ${m}`);
      if (!tok.verified) nl('not verified against chain');
      if (tpl.review_status !== 'reviewed') nl('its template is not reviewed');
    }
  });
  return { json: doc as unknown as RegistryJson, issues };
}

// ------------------------------------------------------------------------------------------------ template verification

/** Compares every registry template with the wasm's pinned templates: mapped program, hash, prefix / suffix / state length, slot limits. */
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

function assess(tok: RegistryTokenJson, tpl: RegistryTemplateJson, check: TemplateCheck): UntradableReason | null {
  if (tok.status === 'delisted') return 'delisted';
  if (tpl.review_status !== 'reviewed') return 'template-pending-review';
  if (!check.program) return 'template-unsupported';
  if (!check.matchesPinned) return 'template-mismatch';
  return null;
}

// ------------------------------------------------------------------------------------------------ parse

export interface ParseOptions {
  /** the wasm build the app runs: template hashes are verified against its pinned templates */
  kob: KobWasm;
  /** require this network (a mainnet build must refuse a testnet registry) */
  network?: string;
}

/** Parses, validates (all rules of the Rust validator) and template-verifies a registry. Throws `RegistryError` listing every finding. */
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
    const reason = assess(t, tpl, check);
    return {
      ticker: t.ticker, name: t.name, family: t.family, covenantId: t.covenant_id, templateId: t.template_id, templateHash: tpl.template_hash,
      extensionCommitment: t.extension_commitment, extensionClass: t.extension_class, decimals: t.decimals,
      tick: null, status: t.status, verified: t.verified,
      official: t.official === true, genesisVerified: t.genesis_verified ?? null, warning: t.warning ?? null, capabilities: tpl.capabilities ?? [], display: t.display ?? null, program: check.program, templateMatchesPinned: check.matchesPinned, prefixLen: tpl.prefix_len, suffixLen: tpl.suffix_len,
      slots: { inputs: tpl.max_token_inputs, outputs: tpl.max_token_outputs }, tradable: reason === null, untradableReason: reason, json: t,
    };
  });
  return { network: reg.network, templates: reg.templates, templateChecks, tokens, byCovenantId: new Map(tokens.map((t) => [t.covenantId, t])) };
}

// ------------------------------------------------------------------------------------------------ lookups and display

export const tokenById = (reg: TokenRegistry, covenantId: Hex): TokenInfo | undefined => reg.byCovenantId.get(covenantId);
export const tradableTokens = (reg: TokenRegistry): TokenInfo[] => reg.tokens.filter((t) => t.tradable);
export const templateById = (reg: TokenRegistry, id: string): RegistryTemplateJson | undefined => reg.templates.find((t) => t.id === id);

/**
 * `TICKER (abcd…1234) [verified]`: never the name alone, so lookalike tokens are told apart by covenant id. `[delisted]` overrides `[official]`, which overrides
 * `[verified]`; a `pending-review` token says so (`[unverified, pending review]`, `[verified, pending review]`).
 */
export function displayName(token: Pick<TokenInfo, 'ticker' | 'covenantId' | 'status' | 'verified'> & { official?: boolean; customRegistry?: string | null }): string {
  const c = token.covenantId;
  const short = c.length >= 8 ? `${c.slice(0, 4)}…${c.slice(-4)}` : c;
  const base = token.customRegistry && token.verified ? `listed in custom registry ${token.customRegistry}` : token.official && token.status !== 'pending-review' ? 'official' : token.verified ? 'verified' : 'unverified';
  const state = token.status === 'delisted' ? '[delisted]' : token.status === 'pending-review' ? `[${base}, pending review]` : `[${base}]`;
  // a token synthesised from the indexer has its short covenant id as ticker: not twice
  return token.ticker === short ? `${short} ${state}` : `${token.ticker} (${short}) ${state}`;
}

/** The registry plus one more token (an open-list token synthesised from the indexer): what the pre-sign screen decodes against. */
export function withExtraToken(reg: TokenRegistry, token: TokenInfo): TokenRegistry {
  if (reg.byCovenantId.has(token.covenantId)) return reg;
  return { ...reg, tokens: [...reg.tokens, token], byCovenantId: new Map([...reg.byCovenantId, [token.covenantId, token]]) };
}

/** Shape A1's `toTokenMarket` consumes (registry JSON keys, snake_case). */
export function tokenMarketInput(token: TokenInfo): TokenMarketInput {
  const j = token.json;
  return {
    ticker: j.ticker, covenant_id: j.covenant_id, template_id: j.template_id, extension_commitment: j.extension_commitment, decimals: j.decimals,
    tick: null,
  };
}

// ------------------------------------------------------------------------------------------------ on-chain verification

export interface VerifyProblem { code: string; message: string }
export interface VerifyResult { ok: boolean; problems: VerifyProblem[] }

/** A token UTXO as seen on chain: its covenant id, its P2SH script public key (kaspa string form) and its decoded token state (either family). */
export interface ChainTokenUtxo { covenantId: Hex | null; scriptPublicKey: string; state: TokenState }

/**
 * A token UTXO belongs to the registered token only if its covenant id equals the registry id, its state carries the registry's extension
 * commitment and its script public key is EXACTLY `tokenScriptPublicKey(program, state)` (which proves the template hash, prefix and suffix).
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

/** Cross-checks the indexer's view of a token with the registry (template hash, extension commitment, covenant id, decimals). Missing data is reported, not assumed. */
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
  // the token's standard scale (10^decimals, at most 10^9): a different scale means a different book (prices of another whole token)
  const scale = 10n ** BigInt(Math.min(token.decimals, 9));
  if (view.scale != null && BigInt(view.scale) !== scale) problems.push({ code: 'scale-mismatch', message: 'the indexer lists another price scale' });
  return { ok: problems.length === 0 && missing.length === 0, problems, missing };
}

// ------------------------------------------------------------------------------------------------ lookalike protection

export type LookalikeLevel = 'none' | 'unknown' | 'shared' | 'strong';
export interface Lookalike { token: TokenInfo; kind: 'same-ticker' | 'confusable-ticker' | 'confusable-name' }
export interface LookalikeReport {
  /**
   * `none`: this exact covenant id is registered; `unknown`: not registered, no lookalike; `strong`: not registered AND its ticker equals / resembles a
   * registered token's; `shared`: the same, but every registered token it resembles is NOT official and carries a maintainer `warning` (e.g. the KRON
   * test token PEPE, whose ticker collides with the well-known PEPE): the registry itself says that token is not a genuine reference, so a namesake
   * is a ticker collision, not an impersonation of it (still not confirmed genuine either)
   */
  level: LookalikeLevel;
  /** the registry entry of this covenant id, if any */
  known: TokenInfo | null;
  lookalikes: Lookalike[];
  /** English fallback; the UI translates from `level` + `lookalikes` */
  message: string;
}

/**
 * For a token the user pasted or the indexer lists (UNTRUSTED ticker + covenant id): a token with the same ticker, or one that is confusable
 * after homoglyph normalisation (KR0N vs KRON), but a DIFFERENT covenant id than a registered token is a strong scam signal naming the real one.
 */
export function lookalikeReport(reg: TokenRegistry, ticker: string, covenantId: Hex, name?: string): LookalikeReport {
  const known = reg.byCovenantId.get(covenantId) ?? null;
  if (known) {
    return { level: 'none', known, lookalikes: [], message: `${displayName(known)} is registered` };
  }
  // The untrusted ticker (and name, when the source has one) are compared as homoglyph skeletons with each registered TICKER and each
  // registered NAME: a token called "Kaspa Coin" or ticker "КRON" (Cyrillic К) copies a registered token as well as "KR0N" does.
  const n = normalizeTicker(ticker);
  const nn = name ? normalizeName(name) : '';
  const claims = [n, normalizeName(ticker), ...(nn ? [nn] : [])].filter((x) => x.length > 0);
  const lookalikes: Lookalike[] = [];
  for (const t of reg.tokens) {
    if (t.covenantId === covenantId) continue;
    if (t.ticker === ticker.toUpperCase()) lookalikes.push({ token: t, kind: 'same-ticker' });
    else if (normalizeTicker(t.ticker) === n) lookalikes.push({ token: t, kind: 'confusable-ticker' });
    else if (claims.includes(normalizeTicker(t.ticker)) || claims.includes(normalizeName(t.name))) lookalikes.push({ token: t, kind: 'confusable-name' });
  }
  if (!lookalikes.length) return { level: 'unknown', known: null, lookalikes, message: 'this token is not in the KOB registry: check its covenant id yourself' };
  const real = lookalikes.map((l) => displayName(l.token)).join(', ');
  // impersonation needs a target the registry vouches for, or at least does not disown: a non-official entry with a maintainer warning is not one
  if (lookalikes.every((l) => !l.token.official && !!l.token.warning)) {
    return {
      level: 'shared', known: null, lookalikes,
      message: `"${sanitizeUntrusted(ticker)}" (${covenantId.slice(0, 8)}…) shares its ticker with ${real}, which the KOB registry marks as not official: neither is confirmed genuine, check the covenant id`,
    };
  }
  return {
    level: 'strong', known: null, lookalikes,
    message: `WARNING: "${sanitizeUntrusted(ticker)}" (${covenantId.slice(0, 8)}…) copies the ticker of ${real}: it is NOT that token`,
  };
}
