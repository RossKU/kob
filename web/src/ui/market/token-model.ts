// View model of the token list and the token header: registry tokens + indexer tokens (tokens the registry does not list are tradable only through the
// open-list path, kob/open-token.ts), technical badges (program family, untradable reason, issuer powers), the localized `TICKER (abcd...1234)` label,
// the shared-ticker note, template-verification detail. Token issuance is permissionless and KOB makes no per-token judgement: nothing here says whether
// a token is verified, genuine or approved, and only technical facts (its program) decide whether this build trades it.
// Pure.
import type { IndexerTokenView } from '../../data/indexer-types';
import type { DeclaredToken } from '../../data/token-declarations';
import {
  longId, normalizeTicker, sanitizeUntrusted, verifyIndexerToken, type TokenInfo, type TokenRegistry, type UntradableReason,
} from '../../kob/registry';
import type { TemplateInfo } from '../../kob/types';

export type BadgeKind = 'untradable';
export type BadgeTone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

export interface TokenBadge {
  kind: BadgeKind;
  tone: BadgeTone;
  /** set for `untradable`: why this build cannot trade the token (registry `untradableReason`) */
  reason: UntradableReason | null;
}

/**
 * Badges of a registry token: when the build cannot trade it (its program is not one this build supports for trading, is not embedded, or differs
 * from the pinned one), an `untradable` badge naming the reason. A tradable token has none.
 */
export function tokenBadges(token: Pick<TokenInfo, 'tradable' | 'untradableReason'>): TokenBadge[] {
  return !token.tradable && token.untradableReason ? [{ kind: 'untradable', tone: 'bad', reason: token.untradableReason }] : [];
}

/**
 * `abcd…1234`: the short form of a covenant id NEXT TO A REGISTRY TICKER (identity is the covenant id and the template hash, never the ticker). A token outside the registry is shown with `unregisteredId` (8 + 8 hex).
 */
export const shortCovenantId = (c: string): string => (c.length >= 8 ? `${c.slice(0, 4)}…${c.slice(-4)}` : c);

/** `abcdef01…12345678`: 8 + 8 hex (64 bits) of a token the registry does not list, whose id is all that identifies it (a 4 + 4 fragment can be copied). */
export const unregisteredId = longId;

/**
 * `EXKCC (e5e5…e5e5)`: the ticker is NEVER shown without the covenant id fragment.
 * A token outside the registry has an EMPTY ticker (or an indexer ticker): 8 + 8 hex of its covenant id (`e5e5e5e5…e5e5e5e5`).
 */
export function tokenLabel(t: { ticker: string; covenantId: string; info?: TokenInfo | null; openList?: boolean; decimalsSource?: TokenInfo['decimalsSource'] }): string {
  // a row of a token outside the registry (`info: null`), an open-list token, one shown for display only (app/token-display.ts) or one without a
  // ticker: 8 + 8 hex; a registry ticker: 4 + 4. A name that IS the id is said once.
  const unregistered = t.ticker === '' || t.info === null || t.openList === true || t.info?.openList === true || !!t.decimalsSource;
  const short = unregistered ? unregisteredId(t.covenantId) : shortCovenantId(t.covenantId);
  return t.ticker === '' || t.ticker === short ? short : `${t.ticker} (${short})`;
}

/** Ticker for display: the registry ticker, else the short covenant id. */
export const tickerOrId = (ticker: string, covenantId: string): string => (ticker === '' ? unregisteredId(covenantId) : ticker);

// ------------------------------------------------------------------------------------------------ program powers

/** Powers of a token program that let the issuer act on holders: escrowed orders can fail or be lost at the issuer's discretion. */
export const ISSUER_CONTROL_POWERS = ['freeze', 'seize'] as const;
/** Every power the UI has a sentence for (`market.power.<name>`). */
export const KNOWN_POWERS = ['mint-authority', 'public-mint', 'burn', 'freeze', 'seize', 'blacklist'] as const;
export const hasIssuerControl = (powers: readonly string[] | null | undefined): boolean =>
  !!powers && powers.some((p) => (ISSUER_CONTROL_POWERS as readonly string[]).includes(p));

/**
 * Facts about a token that a trader needs wherever it is shown (matcher.md 1.2a, "Wallets MUST show the warnings wherever the token is shown"): the
 * two warnings the indexer derives from the registry's genesis record, and the program powers that let someone create more of it or block
 * addresses (freeze and seize have their own warning above). Facts, not a rating: nothing here says a token is safe or unsafe.
 */
export const TOKEN_FACT_IDS = ['genesis_unverified', 'live_mint_authority', 'mint-authority', 'public-mint', 'blacklist'] as const;
export type TokenFactId = (typeof TOKEN_FACT_IDS)[number];

/** The facts out of a token's warnings and powers, in the order of `TOKEN_FACT_IDS`; anything else in them is not a fact the UI has a sentence for. */
export const tokenFactIds = (warnings: readonly string[], powers: readonly string[]): TokenFactId[] => {
  const have = new Set([...warnings, ...powers]);
  return TOKEN_FACT_IDS.filter((id) => have.has(id));
};

/**
 * The warnings a registry entry implies (the same rule the indexer applies): `genesis_unverified` unless the entry says `genesis_verified: true`,
 * `live_mint_authority` when its genesis record lists a live mint authority. The indexer can only ADD to them; a token without an entry is unverified.
 */
function registryWarnings(info: TokenInfo | null): string[] {
  if (!info || info.openList) return ['genesis_unverified'];
  return [...(info.json.genesis_verified === true ? [] : ['genesis_unverified']), ...((info.json.genesis?.live_minters?.length ?? 0) > 0 ? ['live_mint_authority'] : [])];
}
const warningsOf = (v: IndexerTokenView | null): string[] => (v && Array.isArray(v.warnings) ? v.warnings : []);

/** `held`: a token the indexer does not list, known from the wallet's node-verified holdings of it (`kob/open-token.ts` `heldTokenView`) */
export type TokenSource = 'registry' | 'indexer' | 'pasted' | 'held';

export interface TokenRow {
  covenantId: string;
  /** registry ticker, or the SANITIZED ticker the indexer reported */
  ticker: string;
  name: string | null;
  /** a token the registry does not list, named by its verified KCC-23 declaration (`unknownRow`): marked "declared by its creator" where its name is shown */
  declared?: true;
  source: TokenSource;
  /** null for tokens that are not in the registry */
  info: TokenInfo | null;
  index: IndexerTokenView | null;
  /** template capabilities the indexer reported (may be empty) */
  powers: string[];
  /** the token program can freeze or seize balances: escrowed orders are at the issuer's discretion */
  issuerControl: boolean;
  /** the facts to show next to the token (`tokenFactIds` of its warnings and powers) */
  facts: TokenFactId[];
  templateId: string | null;
  badges: TokenBadge[];
  tradable: boolean;
  /** registry tokens: the untradable reason; unknown tokens: `unregistered` (tradable only through the open-list path) */
  reason: UntradableReason | null;
  /** cross-check of the indexer's view against the registry (registry tokens with an indexer entry) */
  indexerProblems: string[];
  openAsks: number | null;
  openBids: number | null;
  decimals: number | null;
  /** the market's scale: base units per whole token (the denominator of its prices; `10^decimals` capped at 10^9), null when unknown */
  scale: bigint | null;
  tick: bigint | null;
}

const powersOf = (v: IndexerTokenView | null): string[] => (v && Array.isArray(v.powers) ? v.powers : []);
/** The indexer can only ADD to the powers the shipped registry declares for the token's template: a warning is never removed by it. */
const unionPowers = (...lists: readonly (readonly string[])[]): string[] => [...new Set(lists.flat())];

function registryRow(info: TokenInfo, index: IndexerTokenView | null): TokenRow {
  const check = index ? verifyIndexerToken(info, index) : null;
  // registry capabilities always apply; whatever the indexer reports for the template is added, never subtracted
  const powers = unionPowers(info.capabilities, powersOf(index));
  return {
    covenantId: info.covenantId, ticker: info.ticker, name: info.name, source: 'registry', info, index, powers, issuerControl: hasIssuerControl(powers),
    facts: tokenFactIds(unionPowers(registryWarnings(info), warningsOf(index)), powers),
    templateId: index?.template_id ?? info.templateId, badges: tokenBadges(info),
    tradable: info.tradable, reason: info.untradableReason,
    indexerProblems: check ? check.problems.map((p) => p.code) : [],
    openAsks: index ? index.open_asks : null, openBids: index ? index.open_bids : null, decimals: info.decimals, scale: 10n ** BigInt(Math.min(info.decimals, 9)), tick: info.tick,
  };
}

/** The verified KCC-23 declaration of a token as far as known (data/token-declarations.ts `declaredToken`): null/undefined without one. */
export type DeclarationLookup = (covenantId: string) => DeclaredToken | null | undefined;

/**
 * A token the registry does not know (found through the indexer or pasted): not tradable from here, only through the open-list path (kob/open-token.ts).
 * Its verified KCC-23 declaration (`declared`) names it like a registry entry would: its ticker (where the indexer gives none), its name and decimals.
 */
export function unknownRow(reg: TokenRegistry, view: IndexerTokenView, source: TokenSource = 'indexer', declared?: DeclaredToken | null): TokenRow {
  const decl = declared && declared.covenantId.toLowerCase() === view.covenant_id.toLowerCase() ? declared : null;
  const ticker = sanitizeUntrusted(view.ticker ?? '', 16) || (decl?.symbol ? sanitizeUntrusted(decl.symbol, 16) : '');
  const decimals = view.decimals ?? decl?.decimals ?? null;
  // the template's capabilities in the shipped registry (found by template hash / id) always apply; the indexer's powers are added
  const tpl = reg.templates.find((t) => (view.template_hash && t.template_hash === view.template_hash) || (view.template_id && t.id === view.template_id));
  const powers = unionPowers(tpl?.capabilities ?? [], powersOf(view));
  return {
    covenantId: view.covenant_id, ticker, name: decl?.name ? sanitizeUntrusted(decl.name, 64) : null, ...(decl ? { declared: true as const } : {}), source, info: null, index: view, powers, issuerControl: hasIssuerControl(powers),
    facts: tokenFactIds(unionPowers(registryWarnings(null), warningsOf(view)), powers), templateId: view.template_id ?? null,
    badges: [],
    // this build's own registry does not list the token: it cannot trade it from the list
    tradable: false, reason: 'unregistered',
    // a token known only from the wallet's holdings (`held`) has a made-up view: its order counts are not known, never 0
    indexerProblems: [], openAsks: source === 'held' ? null : view.open_asks, openBids: source === 'held' ? null : view.open_bids,
    decimals,
    scale: view.scale != null ? BigInt(view.scale) : decimals != null ? 10n ** BigInt(Math.min(decimals, 9)) : null,
    tick: null,
  };
}

/**
 * Registry tokens first (registry order), then indexer-only tokens (those with open orders first, then by ticker). Duplicates by covenant id are
 * merged; an indexer-only token with `template_id: null` (a program off the strict list) is left out. `declared`: the verified KCC-23 declarations
 * that name the indexer-only tokens (`unknownRow`).
 */
export function buildTokenRows(
  reg: TokenRegistry, indexerTokens: readonly IndexerTokenView[] | null, pasted: readonly IndexerTokenView[] = [], declared?: DeclarationLookup,
): TokenRow[] {
  const byId = new Map((indexerTokens ?? []).map((t) => [t.covenant_id, t]));
  const rows: TokenRow[] = reg.tokens.map((t) => registryRow(t, byId.get(t.covenantId) ?? null));
  const seen = new Set(rows.map((r) => r.covenantId));
  const extra: TokenRow[] = [];
  for (const v of indexerTokens ?? []) {
    if (seen.has(v.covenant_id)) continue;
    // an indexer that says the token's program is on no strict list (`template_id: null`, no ticker: a retired program kept in its history)
    // has nothing to trade or show; an older executor that sends no `template_id` at all is unaffected
    if (v.template_id === null) continue;
    seen.add(v.covenant_id);
    extra.push(unknownRow(reg, v, 'indexer', declared?.(v.covenant_id)));
  }
  for (const v of pasted) {
    if (seen.has(v.covenant_id)) continue;
    seen.add(v.covenant_id);
    extra.push(unknownRow(reg, v, 'pasted', declared?.(v.covenant_id)));
  }
  extra.sort((a, b) => (b.openAsks ?? 0) + (b.openBids ?? 0) - ((a.openAsks ?? 0) + (a.openBids ?? 0)) || (a.ticker < b.ticker ? -1 : a.ticker > b.ticker ? 1 : 0));
  return [...rows, ...extra];
}

/**
 * Whether this build can trade the token, as far as a LIST can tell: a registry token by its program (`tradable`: a program this build supports for
 * trading, embedded and equal to the pinned one); a token outside the registry (open list, pasted) is tradable through the open-list path unless its
 * template hash is one this build does not embed (`pinned`: `kob.templates()`). A token without a template hash is kept.
 */
export function tradableHere(row: Pick<TokenRow, 'source' | 'info' | 'index'>, pinned: readonly TemplateInfo[]): boolean {
  if (row.source === 'registry') return row.info !== null && row.info.tradable;
  const hash = row.index?.template_hash;
  return !hash || pinned.some((p) => p.tokenSlots && p.hash === hash);
}

/** The rows a market list, selector or pair choice offers: only tokens this build can trade (see `tradableHere`). */
export const tradableRows = <R extends Pick<TokenRow, 'source' | 'info' | 'index'>>(rows: readonly R[], pinned: readonly TemplateInfo[]): R[] => rows.filter((r) => tradableHere(r, pinned));

/**
 * Covenant ids of the listed tokens that share their ticker with another listed token (case, look-alike letters and homoglyphs ignored: `KR0N` is `KRON`).
 * The UI shows a short note on those: tickers are not unique, the covenant id is the identity. Tokens without a ticker are never compared.
 */
export function sharedTickerIds(rows: readonly Pick<TokenRow, 'covenantId' | 'ticker'>[]): Set<string> {
  const byTicker = new Map<string, string[]>();
  for (const r of rows) {
    const key = normalizeTicker(r.ticker);
    if (r.ticker === '' || key === '') continue;
    byTicker.set(key, [...(byTicker.get(key) ?? []), r.covenantId]);
  }
  return new Set([...byTicker.values()].filter((ids) => new Set(ids).size > 1).flat());
}

/** Case-insensitive search over ticker, name and covenant id; homoglyph-normalised tickers match too (`kr0n` finds `KRON`). Empty query = everything. */
export function filterTokens(rows: readonly TokenRow[], query: string): TokenRow[] {
  const q = query.trim().toLowerCase();
  if (!q) return [...rows];
  const nq = normalizeTicker(q);
  return rows.filter(
    (r) =>
      r.ticker.toLowerCase().includes(q) ||
      shortCovenantId(r.covenantId).replace('…', '').toLowerCase().includes(q) ||
      normalizeTicker(r.ticker).includes(nq) ||
      (r.name ?? '').toLowerCase().includes(q) ||
      r.covenantId.toLowerCase().includes(q),
  );
}

/**
 * A token without metadata: outside the registry, no ticker and no decimals (its prices could only be shown in base units). Issuance is
 * permissionless, so such tokens are kept out of the default list; a search or the list's toggle shows them, a covenant id opens them.
 */
export const isNameless = (r: Pick<TokenRow, 'info' | 'ticker' | 'decimals'>): boolean => r.info === null && r.ticker === '' && r.decimals === null;

/** Pasted text -> a covenant id (64 hex chars, case-insensitive; surrounding whitespace and a `0x` prefix are tolerated), else null. */
export function parseCovenantId(text: string): string | null {
  const s = text.trim().replace(/^0x/i, '');
  return /^[0-9a-fA-F]{64}$/.test(s) ? s.toLowerCase() : null;
}

// ------------------------------------------------------------------------------------------------ template verification

export interface TemplateDetail {
  templateId: string;
  /** hash in the registry entry */
  registryHash: string;
  /** hash of the program embedded in this build (null: the build has no such program) */
  pinnedHash: string | null;
  matches: boolean;
  /** codes of `TemplateCheck.problems` (hash-mismatch, slots-mismatch, ...) */
  problems: string[];
  reviewed: boolean | null;
  program: string | null;
  slots: { inputs: number; outputs: number };
}

/** What the header shows under "template verification": the registry hash next to the hash pinned in this build, plus the registry's checks. */
export function templateDetail(token: TokenInfo, reg: TokenRegistry, pinned: readonly TemplateInfo[]): TemplateDetail {
  const check = reg.templateChecks.find((c) => c.id === token.templateId) ?? null;
  const tpl = reg.templates.find((t) => t.id === token.templateId) ?? null;
  const pin = token.program ? (pinned.find((p) => p.name === token.program) ?? null) : null;
  return {
    templateId: token.templateId,
    registryHash: token.templateHash,
    pinnedHash: pin ? pin.hash : null,
    matches: !!pin && pin.hash === token.templateHash && (check?.matchesPinned ?? false),
    problems: check ? check.problems : [],
    reviewed: tpl ? tpl.review_status === 'reviewed' : null,
    program: token.program,
    slots: token.slots,
  };
}
