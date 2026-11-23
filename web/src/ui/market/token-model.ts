// View model of the token list and the token header. It merges registry tokens with indexer
// tokens. A token the registry does not list is tradable only through the open-list path
// (kob/open-token.ts). It also builds the technical badges (program family, untradable reason,
// issuer powers), the localized `TICKER (abcd...1234)` label, the shared-ticker note and the
// template-verification detail.
//
// Token issuance is permissionless and KOB makes no per-token judgement. Nothing here says whether
// a token is verified, genuine or approved. Only technical facts, such as its program, decide
// whether this build trades it. The module is pure.
import type { IndexerTokenView } from '../../data/indexer-types';
import type { OpenTokenIssue } from '../../kob/open-token';
import type { GenesisCheck } from '../../data/genesis-check';
import type { KascovSpot } from '../../data/kascov';
import type { DeclaredToken } from '../../data/token-declarations';
import {
  longId, normalizeTicker, readsAsKas, sanitizeUntrusted, verifyIndexerToken, type TokenInfo, type TokenRegistry, type UntradableReason,
} from '../../kob/registry';
import type { TemplateInfo } from '../../kob/types';
import { TOKEN_PROGRAMS, programDefaultDecimals } from '../../kob/programs.generated';

export type BadgeKind = 'untradable';
export type BadgeTone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

export interface TokenBadge {
  kind: BadgeKind;
  tone: BadgeTone;
  /** Set for `untradable`: why this build cannot trade the token (registry `untradableReason`). */
  reason: UntradableReason | null;
}

/**
 * Badges of a registry token. When the build cannot trade it, the badge is `untradable` and names
 * the reason. That is the case when its program is not supported for trading, is not embedded, or
 * differs from the pinned one. A tradable token has no badge.
 */
export function tokenBadges(token: Pick<TokenInfo, 'tradable' | 'untradableReason'>): TokenBadge[] {
  return !token.tradable && token.untradableReason ? [{ kind: 'untradable', tone: 'bad', reason: token.untradableReason }] : [];
}

/**
 * Short form `abcd…1234` of a covenant id, shown next to a registry ticker. Identity is the
 * covenant id and the template hash, never the ticker. A token outside the registry is shown with
 * `unregisteredId` (8 + 8 hex).
 */
export const shortCovenantId = (c: string): string => (c.length >= 8 ? `${c.slice(0, 4)}…${c.slice(-4)}` : c);

/**
 * Form `abcdef01…12345678`, 8 + 8 hex (64 bits), for a token the registry does not list.
 * Its id is all that identifies it, and a 4 + 4 fragment can be copied.
 */
export const unregisteredId = longId;

/**
 * Label such as `EXKCC (e5e5…e5e5)`. The ticker is never shown without the covenant id fragment.
 * A token outside the registry has an empty ticker or an indexer ticker. It is shown as 8 + 8 hex
 * of its covenant id, such as `e5e5e5e5…e5e5e5e5`.
 */
export function tokenLabel(t: { ticker: string; covenantId: string; info?: TokenInfo | null; openList?: boolean; decimalsSource?: TokenInfo['decimalsSource'] }): string {
  // These rows use 8 + 8 hex:
  // - a token outside the registry (`info: null`) or an open-list token
  // - a token shown for display only (app/token-display.ts)
  // - a token without a ticker
  // A registry ticker uses 4 + 4. A name that is the id is said once.
  const unregistered = t.ticker === '' || t.info === null || t.openList === true || t.info?.openList === true || !!t.decimalsSource;
  const short = unregistered ? unregisteredId(t.covenantId) : shortCovenantId(t.covenantId);
  return t.ticker === '' || t.ticker === short ? short : `${t.ticker} (${short})`;
}

/** Ticker for display: the registry ticker, else the short covenant id. */
export const tickerOrId = (ticker: string, covenantId: string): string => (ticker === '' ? unregisteredId(covenantId) : ticker);

// ---------------------------------------------------------------------------------- program powers

/**
 * Powers of a token program that let the issuer act on holders. Escrowed orders can fail or be lost
 * at the issuer's discretion.
 */
export const ISSUER_CONTROL_POWERS = ['freeze', 'seize'] as const;
/** Every power the UI has a sentence for (`market.power.<name>`). */
export const KNOWN_POWERS = ['mint-authority', 'public-mint', 'burn', 'freeze', 'seize', 'blacklist'] as const;
export const hasIssuerControl = (powers: readonly string[] | null | undefined): boolean =>
  !!powers && powers.some((p) => (ISSUER_CONTROL_POWERS as readonly string[]).includes(p));

/**
 * Facts about a token that a trader needs wherever it is shown (matcher.md 1.2a, "Wallets MUST show
 * the warnings wherever the token is shown"). They are the two warnings the indexer derives from
 * the registry's genesis record, and the program powers that let someone create more of the token
 * or block addresses. Freeze and seize have their own warning above. These are facts, not a
 * rating. Nothing here says a token is safe or unsafe.
 */
export const INDEXER_FACT_IDS = ['genesis_unverified', 'live_mint_authority', 'mint-authority', 'public-mint', 'blacklist'] as const;
/**
 * The facts KOB derives itself and never takes from an indexer's words.
 * - `genesis_kascov`: kascov.io marks the token verified. It is a third-party chip in place of
 *   "Genesis not verified".
 * - `genesis_verified` and `genesis_failed`: the result of KOB's own genesis check, run when a
 *   token page opens (data/genesis-check.ts).
 * - `mint_allowance`: the public minter a verified genesis holds.
 * An indexer that sends one of these names as a warning is ignored.
 */
export const DERIVED_FACT_IDS = ['genesis_failed', 'genesis_kascov', 'genesis_verified', 'genesis_verified_unreviewed', 'mint_allowance'] as const;
/** The facts the legend above the market list explains, in the order of a token's facts. */
export const TOKEN_FACT_IDS = [
  'genesis_failed', 'genesis_unverified', 'genesis_kascov', 'genesis_verified', 'genesis_verified_unreviewed', 'live_mint_authority', 'mint-authority', 'public-mint', 'blacklist',
] as const;
export type TokenFactId = (typeof INDEXER_FACT_IDS)[number] | (typeof DERIVED_FACT_IDS)[number];
const FACT_ORDER: readonly TokenFactId[] = [...TOKEN_FACT_IDS, 'mint_allowance'];

/**
 * The facts out of a token's warnings and powers, in the order of `TOKEN_FACT_IDS`. Anything else
 * in them, a derived fact's name included, is not a fact an indexer can state.
 */
export const tokenFactIds = (warnings: readonly string[], powers: readonly string[]): TokenFactId[] => {
  const have = new Set([...warnings, ...powers]);
  return FACT_ORDER.filter((id) => (INDEXER_FACT_IDS as readonly string[]).includes(id) && have.has(id));
};

// ---------------------------------------------------------------------------------- status display

/**
 * The facts that make a token "High risk". Only what lets someone dilute the supply or seize or
 * freeze balances counts, and only when it is confirmed. They are derived from the token's own
 * facts and never from a per-token text. The token shows one chip, "High risk", with a
 * tap-to-open reason that lists these.
 * - `genesis_failed`: KOB's own genesis check found a problem, such as a genesis output that is
 *   not the program's.
 * - `live_mint_authority`: the genesis record lists a mint authority that still exists, so the
 *   supply can grow.
 * - `mint_unbounded`: a verified genesis holds a minter that mints any amount.
 * - `issuer_control`: the program can freeze or seize balances (`freeze` or `seize`).
 *
 * The program capability `mint-authority` alone is not High risk. It is a power of the program
 * and not a fact about this token. Whether a mint authority is still live is
 * `live_mint_authority`, confirmed by the registry's `live_minters` or the indexer's warning.
 *
 * These are not High risk either:
 * - `genesis_unverified`, because the check could not run
 * - `genesis_kascov` and `genesis_verified*`
 * - `public-mint`, a minter with a cap (`mint_allowance`)
 * - `blacklist` and `burn`
 * - a program pending review, because the profile table has no machine-readable finding
 * - the registry's `risks` texts, which are informational
 * - the indexer cross-check findings, which have their own banner
 * - a decimals disagreement
 */
export const RISK_CODES = ['genesis_failed', 'live_mint_authority', 'mint_unbounded', 'issuer_control'] as const;
export type RiskCode = (typeof RISK_CODES)[number];

type RiskRow = Pick<TokenRow, 'facts' | 'powers'> & Partial<Pick<TokenRow, 'genesis'>>;

export function tokenRiskCodes(r: RiskRow): RiskCode[] {
  const out: RiskCode[] = [];
  if (r.facts.includes('genesis_failed') || r.genesis?.status === 'failed') out.push('genesis_failed');
  if (r.facts.includes('live_mint_authority')) out.push('live_mint_authority');
  if (r.genesis?.status === 'verified' && r.genesis.mintUnbounded === true) out.push('mint_unbounded');
  if (hasIssuerControl(r.powers)) out.push('issuer_control');
  return out;
}

/**
 * The facts that "High risk" states. They are a failed check, a live or program mint authority and
 * an unbounded mint. Only these show next to a token by default, as the reasons of that one chip
 * and in the order ticket's panel. Every other fact is a status that a normal token does not show.
 * It sits in the collapsed "Status and checks" section of the details. Those facts are a pass,
 * kascov's remark, a capped public mint, blacklist and an unverified genesis.
 */
export function riskFactIds(r: RiskRow): TokenFactId[] {
  const risks = tokenRiskCodes(r);
  const states = new Set<TokenFactId>();
  if (risks.includes('genesis_failed')) states.add('genesis_failed');
  if (risks.includes('live_mint_authority')) states.add('live_mint_authority');
  if (risks.includes('mint_unbounded')) states.add('mint_allowance');
  return r.facts.filter((f) => states.has(f));
}

/** Whether the profile table marks the program (a name of `programs.generated.ts`) `reviewed`. */
const programReviewed = (program: string): boolean => TOKEN_PROGRAMS.some((p) => p.name === program && p.review === 'reviewed');

const GENESIS_OPEN: readonly TokenFactId[] = ['genesis_unverified', 'genesis_kascov'];
/** The facts of a row, its genesis line replaced by `id` (null removes it) and `extra` added. */
const withGenesisFact = (facts: readonly TokenFactId[], id: TokenFactId | null, extra: readonly TokenFactId[] = []): TokenFactId[] => {
  const rest = facts.filter((f) => !GENESIS_OPEN.includes(f));
  const next = new Set<TokenFactId>([...rest, ...(id ? [id] : []), ...extra]);
  return FACT_ORDER.filter((f) => next.has(f));
};

/**
 * The warnings a registry entry implies, by the same rule the indexer applies.
 * - `genesis_unverified` unless the entry says `genesis_verified: true`.
 * - `live_mint_authority` when its genesis record lists a live mint authority.
 * The indexer can only add to them. A token without an entry is unverified.
 */
function registryWarnings(info: TokenInfo | null): string[] {
  if (!info || info.openList) return ['genesis_unverified'];
  return [...(info.json.genesis_verified === true ? [] : ['genesis_unverified']), ...((info.json.genesis?.live_minters?.length ?? 0) > 0 ? ['live_mint_authority'] : [])];
}
const warningsOf = (v: IndexerTokenView | null): string[] => (v && Array.isArray(v.warnings) ? v.warnings : []);

/**
 * Where a token row came from.
 * - `held`: the indexer does not list the token. It is known from the wallet's node-verified
 *   holdings (`kob/open-token.ts` `heldTokenView`).
 * - `public`: the indexer does not list the token either. It is known from token outputs that the
 *   indexer names and the node confirmed, whoever holds them (`kob/open-token.ts`
 *   `findPublicToken`).
 * - `kascov`: only the third-party list kascov.io names the token (data/kascov.ts). The KOB indexer
 *   knows no order of it.
 */
export type TokenSource = 'registry' | 'indexer' | 'pasted' | 'held' | 'public' | 'kascov';

/**
 * What kascov.io contributed to a row (ui/market/kascov-merge.ts). It is a display layer of the
 * creator's claims, never facts. `ticker` and `name` are shown only where KOB has none of its own
 * (`usedTicker` and `usedName`). A claim that differs from what KOB derives is flagged, and KOB's
 * value stays.
 */
export interface KascovMeta {
  ticker: string | null;
  name: string | null;
  usedTicker: boolean;
  usedName: boolean;
  /** kascov marks the token verified. A third-party chip, never KOB's own fact. */
  verified: boolean;
  /** kascov's ticker differs from the one KOB derived (registry, declaration or indexer). */
  tickerDiffers: boolean;
  /** kascov's declared decimals differ from the decimals KOB derived. */
  decimalsDiffers: boolean;
  decimals: number | null;
  /** The icon's committed hash and sources (data/kascov.ts `iconSources`). Null without one. */
  imageHash: string | null;
  imageUrl: string | null;
  /**
   * kascov's total supply of the token in base units (data/kascov.ts `supply`). Only the market
   * list's market cap uses it, labelled as kascov's.
   */
  supply?: bigint | null;
  /**
   * kascov prices the token and its spot (data/kascov.ts). Only the chart's optional third-party
   * line is offered by it (PriceChart.tsx).
   */
  priced?: boolean;
  spot?: KascovSpot | null;
}

export interface TokenRow {
  covenantId: string;
  /** Registry ticker, or the sanitized ticker the indexer reported. */
  ticker: string;
  name: string | null;
  /**
   * A token the registry does not list, named by its verified KCC-23 declaration (`unknownRow`).
   * It is marked "declared, bound by ancestry" where its name is shown.
   */
  declared?: true;
  source: TokenSource;
  /** Null for tokens that are not in the registry. */
  info: TokenInfo | null;
  index: IndexerTokenView | null;
  /** Template capabilities the indexer reported. May be empty. */
  powers: string[];
  /** The token program can freeze or seize balances, so escrowed orders are at its mercy. */
  issuerControl: boolean;
  /**
   * The facts to show next to the token, from `tokenFactIds` of its warnings and powers. Kascov's
   * chip and KOB's own genesis check replace the genesis line, see the end of this file.
   */
  facts: TokenFactId[];
  /**
   * KOB's own genesis check of the token, on a token page only (`applyGenesisCheck`). It adds to
   * the genesis fact's sentence why it stays unverified and the public mint allowance.
   */
  genesis?: GenesisCheck;
  templateId: string | null;
  badges: TokenBadge[];
  tradable: boolean;
  /**
   * Registry tokens: the untradable reason. Unknown tokens: `unregistered`, tradable only through
   * the open-list path.
   */
  reason: UntradableReason | null;
  /** Cross-check of the indexer view against the registry, for registry tokens it lists. */
  indexerProblems: string[];
  openAsks: number | null;
  openBids: number | null;
  decimals: number | null;
  /**
   * The market's scale in base units per whole token, the denominator of its prices. It is
   * `10^decimals` capped at 10^9, or null when unknown.
   */
  scale: bigint | null;
  tick: bigint | null;
  /** The name, ticker and icon a third-party list (kascov.io) gave this token when merged. */
  kascov?: KascovMeta | null;
}

const powersOf = (v: IndexerTokenView | null): string[] => (v && Array.isArray(v.powers) ? v.powers : []);
/**
 * The indexer can only add to the powers the shipped registry declares for the token's template.
 * It never removes one.
 */
const unionPowers = (...lists: readonly (readonly string[])[]): string[] => [...new Set(lists.flat())];

function registryRow(info: TokenInfo, index: IndexerTokenView | null): TokenRow {
  const check = index ? verifyIndexerToken(info, index) : null;
  // Registry capabilities always apply. What the indexer reports for the template is added.
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

/**
 * The verified KCC-23 declaration of a token as far as known (data/token-declarations.ts
 * `declaredToken`). Null or undefined without one.
 */
export type DeclarationLookup = (covenantId: string) => DeclaredToken | null | undefined;

/**
 * A token the registry does not know, found through the indexer or pasted. It is not tradable from
 * here, only through the open-list path (kob/open-token.ts). Its verified KCC-23 declaration
 * (`declared`) names it like a registry entry would, with its ticker where the indexer gives none,
 * its name and its decimals.
 */
export function unknownRow(reg: TokenRegistry, view: IndexerTokenView, source: TokenSource = 'indexer', declared?: DeclaredToken | null): TokenRow {
  const decl = declared && declared.covenantId.toLowerCase() === view.covenant_id.toLowerCase() ? declared : null;
  const ticker = sanitizeUntrusted(view.ticker ?? '', 16) || (decl?.symbol ? sanitizeUntrusted(decl.symbol, 16) : '');
  // Decimals come from the indexer's value, else the token's KCC-23 declaration, else its program's
  // default (KaspaRocket convention: 3).
  const program = TOKEN_PROGRAMS.find((p) => (view.template_hash && p.templateHash === view.template_hash) || (view.template_id && p.registryId === view.template_id));
  const decimals = view.decimals ?? decl?.decimals ?? programDefaultDecimals(program?.name)?.decimals ?? null;
  // The template's capabilities in the shipped registry, found by template hash or id, always
  // apply. The indexer's powers are added.
  const tpl = reg.templates.find((t) => (view.template_hash && t.template_hash === view.template_hash) || (view.template_id && t.id === view.template_id));
  const powers = unionPowers(tpl?.capabilities ?? [], powersOf(view));
  return {
    covenantId: view.covenant_id, ticker, name: decl?.name ? sanitizeUntrusted(decl.name, 64) : null, ...(decl ? { declared: true as const } : {}), source, info: null, index: view, powers, issuerControl: hasIssuerControl(powers),
    facts: tokenFactIds(unionPowers(registryWarnings(null), warningsOf(view)), powers), templateId: view.template_id ?? null,
    badges: [],
    // This build's own registry does not list the token, so it cannot trade it from the list.
    tradable: false, reason: 'unregistered',
    // A token known only from the wallet's holdings (`held`) has a made-up view. Its order counts
    // are not known, never 0.
    indexerProblems: [], openAsks: source === 'held' || source === 'public' ? null : view.open_asks, openBids: source === 'held' || source === 'public' ? null : view.open_bids,
    decimals,
    scale: view.scale != null ? BigInt(view.scale) : decimals != null ? 10n ** BigInt(Math.min(decimals, 9)) : null,
    tick: null,
  };
}

/**
 * Builds the token rows. Registry tokens come first in registry order. Indexer-only tokens follow,
 * those with open orders first, then by ticker. Duplicates by covenant id are merged. An
 * indexer-only token with `template_id: null`, a program off the strict list, is left out.
 * `declared` holds the verified KCC-23 declarations that name the indexer-only tokens
 * (`unknownRow`).
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
    // An indexer that says the token's program is on no strict list has nothing to trade or show.
    // That is `template_id: null` with no ticker. An executor that sends no `template_id` at all is
    // unaffected.
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
 * Whether this build can trade the token, as far as a list can tell.
 * - A registry token is judged by its program (`tradable`): one this build supports for trading,
 *   embedded and equal to the pinned one.
 * - A token outside the registry (open list, pasted) is tradable through the open-list path unless
 *   its template hash is one this build does not embed (`pinned`: `kob.templates()`).
 * A token without a template hash is kept.
 */
export function tradableHere(row: Pick<TokenRow, 'source' | 'info' | 'index'>, pinned: readonly TemplateInfo[]): boolean {
  if (row.source === 'registry') return row.info !== null && row.info.tradable;
  const hash = row.index?.template_hash;
  return !hash || pinned.some((p) => p.tokenSlots && p.hash === hash);
}

/**
 * The rows a market list, selector or pair choice offers. Only tokens this build can trade are
 * kept (see `tradableHere`).
 */
export const tradableRows = <R extends Pick<TokenRow, 'source' | 'info' | 'index'>>(rows: readonly R[], pinned: readonly TemplateInfo[]): R[] => rows.filter((r) => tradableHere(r, pinned));

/**
 * A favourite no list names now. Its id is all the row has, and its page decides what can be done
 * with it.
 */
export const idOnlyRow = (covenantId: string): TokenRow => ({
  covenantId, ticker: '', name: null, source: 'public', info: null, index: null, powers: [], issuerControl: false, facts: [], templateId: null, badges: [], tradable: false,
  reason: 'unregistered', indexerProblems: [], openAsks: null, openBids: null, decimals: null, scale: null, tick: null,
});

/**
 * The rows of a pair selector, with each token on screen named as its page names it. A token no
 * list names has no row of its own and is added.
 */
export function withShownRows(rows: readonly TokenRow[], shown: readonly (TokenRow | null | undefined)[]): TokenRow[] {
  let out = [...rows];
  for (const s of shown) {
    if (!s) continue;
    out = out.some((r) => r.covenantId === s.covenantId) ? out.map((r) => (r.covenantId === s.covenantId ? { ...r, ticker: s.ticker } : r)) : [...out, s];
  }
  return out;
}

/**
 * Standalone mode. An open-list token is traded only with decimals known without the indexer, from
 * its declaration or its program's default. The scale that its orders seen since the mode started
 * use is no evidence of the market's. `carried` means the indexer gave the market's scale before
 * the mode, in the token list the page read (`IndexerCarry.scaleOf`). The scale its row names is
 * then the one the token was traded on before the switch, with the same trust.
 */
export function scaleNeedsIndexer(row: Pick<TokenRow, 'source' | 'info'>, reason: OpenTokenIssue | null, carried = false): boolean {
  const i = row.info;
  if (row.source === 'registry') return false;
  if (reason === 'no-scale') return true;
  if (carried && i?.openScale === 'orders') return false;
  return !!i?.openList && i.openScale !== 'declared' && i.openScale !== 'program';
}

/**
 * Covenant ids of the listed tokens that share their ticker with another listed token. Case,
 * look-alike letters and homoglyphs are ignored, so `KR0N` is `KRON`. The UI shows a short note on
 * those, because tickers are not unique and the covenant id is the identity. Tokens without a
 * ticker are never compared. A ticker that reads as KAS (`readsAsKas`) is shared with native KAS,
 * alone too.
 */
export function sharedTickerIds(rows: readonly Pick<TokenRow, 'covenantId' | 'ticker'>[]): Set<string> {
  const byTicker = new Map<string, string[]>();
  const kasLike = new Set<string>();
  for (const r of rows) {
    if (readsAsKas(r.ticker)) kasLike.add(r.covenantId);
    const key = normalizeTicker(r.ticker);
    if (r.ticker === '' || key === '') continue;
    byTicker.set(key, [...(byTicker.get(key) ?? []), r.covenantId]);
  }
  return new Set([...kasLike, ...[...byTicker.values()].filter((ids) => new Set(ids).size > 1).flat()]);
}

/**
 * Case-insensitive search over ticker, name and covenant id. Homoglyph-normalised tickers match
 * too, so `kr0n` finds `KRON`. An empty query gives everything.
 */
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
 * A token without metadata: outside the registry, with no ticker and no decimals, so its prices
 * could only be shown in base units. Issuance is permissionless, so such tokens are kept out of the
 * default list. A search or the list's toggle shows them, and a covenant id opens them.
 */
export const isNameless = (r: Pick<TokenRow, 'info' | 'ticker' | 'decimals'>): boolean => r.info === null && r.ticker === '' && r.decimals === null;

/**
 * Turns pasted text into a covenant id of 64 hex chars, case-insensitive. Surrounding whitespace
 * and a `0x` prefix are tolerated. Anything else gives null.
 */
export function parseCovenantId(text: string): string | null {
  const s = text.trim().replace(/^0x/i, '');
  return /^[0-9a-fA-F]{64}$/.test(s) ? s.toLowerCase() : null;
}

// --------------------------------------------------------------------------- template verification

export interface TemplateDetail {
  templateId: string;
  /** Hash in the registry entry. */
  registryHash: string;
  /** Hash of the program embedded in this build. Null when the build has no such program. */
  pinnedHash: string | null;
  matches: boolean;
  /** Codes of `TemplateCheck.problems`, such as hash-mismatch and slots-mismatch. */
  problems: string[];
  reviewed: boolean | null;
  program: string | null;
  slots: { inputs: number; outputs: number };
}

/**
 * What the header shows under "template verification": the registry hash next to the hash pinned
 * in this build, plus the registry's checks.
 */
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

// ----------------------------------------------------------------------- the genesis line of a row

/**
 * kascov's remark on a row. A token it marks verified shows the chip "Verified by kascov" in place
 * of "Genesis not verified". The chip is third party and labelled as such. KOB's own check
 * (`applyGenesisCheck`) replaces it once it has run. A row whose genesis the KOB registry verified
 * has no such line and is untouched.
 */
export function withKascovRemark(row: TokenRow, verified: boolean): TokenRow {
  if (!verified || !row.facts.includes('genesis_unverified')) return row;
  return { ...row, facts: withGenesisFact(row.facts, 'genesis_kascov') };
}

/**
 * The genesis line of a row after KOB's own check, on a token page.
 * - Verified shows "Genesis verified", which is KOB's own fact, plus the public mint the genesis
 *   holds.
 * - Failed shows "Genesis check failed", whatever kascov says.
 * - Unavailable keeps what the row had, kascov's chip or else "Genesis not verified", and adds the
 *   reason.
 * A row the registry verified, which has no genesis line, is untouched.
 */
export function applyGenesisCheck(row: TokenRow, check: GenesisCheck | null | undefined): TokenRow {
  if (!check || !row.facts.some((f) => GENESIS_OPEN.includes(f))) return row;
  switch (check.status) {
    case 'verified':
      // The check accepts every program this build embeds, prototypes and programs pending review
      // included. Only a reviewed program's pass is plain.
      return { ...row, genesis: check, facts: withGenesisFact(row.facts, programReviewed(check.program) ? 'genesis_verified' : 'genesis_verified_unreviewed', check.mintAllowance !== null || check.mintUnbounded === true ? ['mint_allowance'] : []) };
    case 'failed':
      return { ...row, genesis: check, facts: withGenesisFact(row.facts, 'genesis_failed') };
    case 'unavailable':
      return { ...row, genesis: check };
  }
}
