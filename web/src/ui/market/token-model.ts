// View model of the token list and the token header: registry tokens + indexer tokens (unknown ones are UNVERIFIED; they are tradable only through the open-list path, kob/open-token.ts),
// status badges, lookalike protection, the localized `TICKER (abcd...1234) [state]` label, template-verification detail. Pure.
import type { IndexerTokenView, TokenStanding } from '../../data/indexer-types';
import {
  longId, lookalikeReport, normalizeTicker, sanitizeUntrusted, verifyIndexerToken, type LookalikeReport, type TokenInfo, type TokenRegistry, type UntradableReason,
} from '../../kob/registry';
import type { TemplateInfo } from '../../kob/types';

export type BadgeKind = 'official' | 'verified' | 'unverified' | 'delisted' | 'pending-review' | 'untradable' | 'custom-registry';
export type BadgeTone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

export interface TokenBadge {
  kind: BadgeKind;
  tone: BadgeTone;
  /** set for `untradable`: why this build cannot trade the token (registry `untradableReason`) */
  reason: UntradableReason | null;
}

/** Reasons already told by the status badge: no second `untradable` badge for them. */
const CONVEYED: ReadonlySet<UntradableReason> = new Set(['delisted', 'unverified']);

/**
 * What the executor's standing (`GET /v1/tokens`) adds to the pinned registry's own word about a registry token, the ONE rule every label and badge
 * shares (the market list and page, the pair page, My orders, balances: the same registry token must read the same everywhere):
 *   * `delisted` always wins;
 *   * `unverified` is the executor not CONFIRMING the token as official. The word has no room for "verified": it is what the executor says of every
 *     token that is not official, so against a registry token that only claims `verified` it adds nothing (the registry's `verified` stands: a
 *     soak or testnet registry with verified, non-official tokens reads `[verified]` on every page). Against a token the registry claims as
 *     `official` it is a real conflict, and the more cautious word wins;
 *   * `official` never upgrades anything (trust comes from the signed / shipped registry).
 */
export function indexerDowngrade(token: { official?: boolean }, standing: TokenStanding | null): 'delisted' | 'unverified' | null {
  if (standing === 'delisted') return 'delisted';
  if (standing === 'unverified' && token.official === true) return 'unverified';
  return null;
}

/**
 * Badges of a registry token: one status (verified / unverified / delisted / pending-review) and, when the build cannot trade it for another
 * reason (template not reviewed, unsupported, or different from the pinned one), an `untradable` badge naming the reason.
 */
export function tokenBadges(
  token: Pick<TokenInfo, 'status' | 'verified' | 'tradable' | 'untradableReason'> & { official?: boolean; customRegistry?: string | null },
  standing: TokenStanding | null = null,
): TokenBadge[] {
  const out: TokenBadge[] = [];
  // Trust comes from the SIGNED / shipped registry (`official`, `verified`), never from the indexer: the executor's `standing` may only DOWNGRADE, and
  // only as far as `indexerDowngrade` says.
  const down = indexerDowngrade(token, standing);
  if (token.status === 'delisted' || down === 'delisted') out.push({ kind: 'delisted', tone: 'bad', reason: null });
  else if (token.status === 'pending-review') {
    // pending review does not block trading: the token is shown like an open-list token, plus the pending-review badge (and unverified when nothing vouches for it)
    out.push({ kind: 'pending-review', tone: 'warn', reason: null });
    if (down === 'unverified' || !token.verified) out.push({ kind: 'unverified', tone: 'warn', reason: null });
  } else if (down === 'unverified') out.push({ kind: 'unverified', tone: 'warn', reason: null });
  // a token of a registry that is not the pinned default is only "listed in custom registry": never official / verified by KOB
  else if (token.customRegistry && token.verified) out.push({ kind: 'custom-registry', tone: 'warn', reason: null });
  else if (token.official === true && token.verified) out.push({ kind: 'official', tone: 'ok', reason: null });
  else if (token.verified) out.push({ kind: 'verified', tone: 'ok', reason: null });
  else out.push({ kind: 'unverified', tone: 'warn', reason: null });
  if (!token.tradable && token.untradableReason && !CONVEYED.has(token.untradableReason)) {
    out.push({ kind: 'untradable', tone: 'bad', reason: token.untradableReason });
  }
  return out;
}

/** State word of the label: delisted beats verified (same rule as `displayName`); a pending-review registry entry says so (`*-pending`). The standing goes through `indexerDowngrade`, like the badges. */
export type LabelState = 'official' | 'verified' | 'unverified' | 'delisted' | 'custom' | 'verified-pending' | 'unverified-pending';
export function labelState(t: Pick<TokenInfo, 'status' | 'verified'> & { official?: boolean; customRegistry?: string | null }, standing: TokenStanding | null = null): LabelState {
  const down = indexerDowngrade(t, standing);
  return t.status === 'delisted' || down === 'delisted'
    ? 'delisted'
    : t.status === 'pending-review'
      ? down === 'unverified' || !t.verified
        ? 'unverified-pending'
        : 'verified-pending'
      : down === 'unverified'
        ? 'unverified'
        : t.customRegistry && t.verified
          ? 'custom'
          : t.official === true && t.verified
            ? 'official'
            : t.verified
              ? 'verified'
              : 'unverified';
}

/**
 * `abcd…1234`: the short form of a covenant id NEXT TO A REGISTRY TICKER (registry tickers are unique and look-alike checked; identity is the
 * covenant id and the template hash, never the ticker). A token outside the registry is shown with `unregisteredId` (8 + 8 hex).
 */
export const shortCovenantId = (c: string): string => (c.length >= 8 ? `${c.slice(0, 4)}…${c.slice(-4)}` : c);

/** `abcdef01…12345678`: 8 + 8 hex (64 bits) of a token the registry does not list, whose id is all that identifies it (a 4 + 4 fragment can be copied). */
export const unregisteredId = longId;

/**
 * `EXKCC (e5e5…e5e5) [state]`: the ticker is NEVER shown without the covenant id fragment. `stateWord` is the (translated) state text.
 * A token outside the registry has an EMPTY ticker (or an indexer ticker): 8 + 8 hex of its covenant id (`e5e5e5e5…e5e5e5e5 [state]`).
 */
export function tokenLabel(t: { ticker: string; covenantId: string; info?: TokenInfo | null; openList?: boolean }, stateWord: string): string {
  // a row of a token outside the registry (`info: null`), an open-list token or one without a ticker: 8 + 8 hex; a registry ticker: 4 + 4
  const unregistered = t.ticker === '' || t.info === null || t.openList === true || t.info?.openList === true;
  const short = unregistered ? unregisteredId(t.covenantId) : shortCovenantId(t.covenantId);
  return t.ticker === '' ? `${short} [${stateWord}]` : `${t.ticker} (${short}) [${stateWord}]`;
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

export type TokenSource = 'registry' | 'indexer' | 'pasted';

export interface TokenRow {
  covenantId: string;
  /** registry ticker, or the SANITIZED ticker the indexer reported */
  ticker: string;
  name: string | null;
  source: TokenSource;
  /** null for tokens that are not in the registry */
  info: TokenInfo | null;
  index: IndexerTokenView | null;
  /** the executor's standing of the token when the indexer sent one */
  standing: TokenStanding | null;
  /** template capabilities the indexer reported (may be empty) */
  powers: string[];
  /** the token program can freeze or seize balances: escrowed orders are at the issuer's discretion */
  issuerControl: boolean;
  templateId: string | null;
  /**
   * genesis verification status from the shipped registry (`genesis_verified`): 'verified' only when the registry says so; a registry token whose
   * entry does not say is 'unverified'; null for tokens outside the registry (no statement at all)
   */
  genesis: 'verified' | 'unverified' | null;
  labelState: LabelState;
  /** short hash of the non-default registry this token is listed in (label state `custom`), else null */
  customHash: string | null;
  badges: TokenBadge[];
  tradable: boolean;
  /** registry tokens: the untradable reason; unknown tokens: always `unverified` */
  reason: UntradableReason | null;
  /** lookalike analysis of the ticker (unknown tokens only) */
  lookalike: LookalikeReport | null;
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
  // the indexer's standing can only downgrade, and only as far as `indexerDowngrade` says; `official` comes from the registry flag alone
  const down = indexerDowngrade(info, index?.standing ?? null);
  const standing: TokenStanding | null = down ?? (info.official ? ('official' as const) : null);
  // registry capabilities always apply; whatever the indexer reports for the template is added, never subtracted
  const powers = unionPowers(info.capabilities, powersOf(index));
  return {
    covenantId: info.covenantId, ticker: info.ticker, name: info.name, source: 'registry', info, index, standing, powers, issuerControl: hasIssuerControl(powers),
    templateId: index?.template_id ?? info.templateId, genesis: info.genesisVerified === undefined ? null : info.genesisVerified === true ? 'verified' : 'unverified', labelState: labelState(info, standing), customHash: info.customRegistry ?? null, badges: tokenBadges(info, standing),
    tradable: info.tradable, reason: info.untradableReason, lookalike: null,
    indexerProblems: check ? check.problems.map((p) => p.code) : [],
    openAsks: index ? index.open_asks : null, openBids: index ? index.open_bids : null, decimals: info.decimals, scale: 10n ** BigInt(Math.min(info.decimals, 9)), tick: info.tick,
  };
}

/** A token the registry does not know (found through the indexer or pasted): unverified, not tradable, with lookalike protection. */
export function unknownRow(
  reg: TokenRegistry, view: IndexerTokenView, source: TokenSource = 'indexer', others: readonly IndexerTokenView[] = [], held: readonly string[] = [],
): TokenRow {
  const ticker = sanitizeUntrusted(view.ticker ?? '', 16);
  // a token this build's registry does not list is never official or verified, whatever the indexer claims
  const standing: TokenStanding = view.standing === 'delisted' ? 'delisted' : 'unverified';
  const kind: BadgeKind = standing === 'delisted' ? 'delisted' : 'unverified';
  // the template's capabilities in the shipped registry (found by template hash / id) always apply; the indexer's powers are added
  const tpl = reg.templates.find((t) => (view.template_hash && t.template_hash === view.template_hash) || (view.template_id && t.id === view.template_id));
  const powers = unionPowers(tpl?.capabilities ?? [], powersOf(view));
  return {
    covenantId: view.covenant_id, ticker, name: null, source, info: null, index: view, standing, powers, issuerControl: hasIssuerControl(powers), templateId: view.template_id ?? null, genesis: null,
    labelState: standing, customHash: null, badges: [{ kind, tone: kind === 'delisted' ? 'bad' : 'warn', reason: null }],
    // this build's own registry does not list the token: it cannot trade it whatever its standing
    tradable: false, reason: standing === 'delisted' ? 'delisted' : 'unverified',
    // an indexer that calls an unregistered token official is contradicting the registry: recorded as a problem
    // other unregistered tokens with the same short id or ticker are named too (`collision`): only the full covenant id tells them apart
    lookalike: lookalikeReport(reg, view.ticker, view.covenant_id, undefined, [
      ...others.map((o) => ({ covenantId: o.covenant_id, ticker: o.ticker ?? '' })),
      // the tokens this wallet holds (its own record, not the indexer's): a token copying the short id of one of them is named even when
      // the indexer leaves the held token out of its list
      ...held.filter((id) => !others.some((o) => o.covenant_id === id)).map((id) => ({ covenantId: id, ticker: '' })),
    ]), indexerProblems: view.standing === 'official' ? ['standing-official-unregistered'] : [], openAsks: view.open_asks, openBids: view.open_bids,
    decimals: view.decimals,
    scale: view.scale != null ? BigInt(view.scale) : view.decimals != null ? 10n ** BigInt(Math.min(view.decimals, 9)) : null,
    tick: null,
  };
}

/**
 * Registry tokens first (registry order), then indexer-only tokens (those with open orders first, then by ticker). Duplicates by covenant id are
 * merged. `held`: covenant ids of the tokens the connected wallet holds (its local token tracker), compared with every unregistered token for a
 * shared short id besides the registry and the indexer's list.
 */
export function buildTokenRows(
  reg: TokenRegistry, indexerTokens: readonly IndexerTokenView[] | null, pasted: readonly IndexerTokenView[] = [], held: readonly string[] = [],
): TokenRow[] {
  const byId = new Map((indexerTokens ?? []).map((t) => [t.covenant_id, t]));
  const rows: TokenRow[] = reg.tokens.map((t) => registryRow(t, byId.get(t.covenantId) ?? null));
  const seen = new Set(rows.map((r) => r.covenantId));
  const extra: TokenRow[] = [];
  const unregistered = [...(indexerTokens ?? []), ...pasted].filter((v) => !reg.byCovenantId.has(v.covenant_id));
  for (const v of indexerTokens ?? []) {
    if (seen.has(v.covenant_id)) continue;
    seen.add(v.covenant_id);
    extra.push(unknownRow(reg, v, 'indexer', unregistered, held));
  }
  for (const v of pasted) {
    if (seen.has(v.covenant_id)) continue;
    seen.add(v.covenant_id);
    extra.push(unknownRow(reg, v, 'pasted', unregistered, held));
  }
  extra.sort((a, b) => (b.openAsks ?? 0) + (b.openBids ?? 0) - ((a.openAsks ?? 0) + (a.openBids ?? 0)) || (a.ticker < b.ticker ? -1 : a.ticker > b.ticker ? 1 : 0));
  return [...rows, ...extra];
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
