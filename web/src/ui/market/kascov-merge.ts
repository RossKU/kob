// The market list completed from kascov.io (data/kascov.ts): which of its tokens the list may show, and how its names and icons join the rows KOB already
// has. Pure.
//
// The rule, in one place: KOB decides WHAT is listed and every fact about it; kascov only supplies words and a picture.
//
//   * WHICH TOKENS. A token kascov knows that KOB's own sources do not (no registry entry, nothing on the KOB indexer: no order names it yet) is added only
//     if the program hash kascov names for it is one of the programs this build pins (`kob.templates()`, from crates/kob-protocol/data/programs.json) and
//     trades on this network (`programAllowed`: the profile's `networks` and, on mainnet, its review). kascov's `review` and `alive` are not
//     read. Such a token is a row with no orders; opening it goes through the same checks as any token (the token page re-derives everything from the
//     indexer or the wallet's node-verified holdings, kob/open-token.ts). A token kascov names without a program hash (the 46-byte KRON family) is never
//     added from kascov: it appears when the registry or the indexer lists it, as before.
//   * WHICH WORDS. A row KOB already names keeps its ticker and name. Where it has none, kascov's claim fills the gap and the row says where it came from
//     (`kascov.usedTicker` / `usedName`). If kascov's ticker or declared decimals differ from what KOB derived, KOB's stand and the row carries a note.
//     A row whose program hash KOB knows and kascov contradicts is not touched at all: kascov is describing another program.
//   * WHICH REMARK. kascov's `status: "verified"` is shown as a small chip "Verified by kascov" in place of "Genesis not verified" (third party, labelled: kascov
//     checked the token's history, KOB has not yet; a token page runs KOB's own check and replaces it). It is no fact and changes no warning.
//   * WHICH ICON. Every matched row may carry the hash and sources of an icon; whether it is shown is decided by the bytes (data/kascov.ts).
import type { IndexerTokenView } from '../../data/indexer-types';
import type { KascovToken } from '../../data/kascov';
import type { DeclaredToken } from '../../data/token-declarations';
import { programAllowed } from '../../kob/open-token';
import type { TokenRegistry } from '../../kob/registry';
import { TEMPLATE_ID_PROGRAM } from '../../kob/token-market';
import type { TemplateInfo } from '../../kob/types';
import { unknownRow, withKascovRemark, type KascovMeta, type TokenRow } from './token-model';

/**
 * Whether a token of program hash `hash` is one this build can trade on `network`: the hash is a pinned token program (not an order template) and the
 * program is enabled for the network. No hash (kascov does not name the program of the token) is not enough.
 */
export function kascovTradable(hash: string | null | undefined, network: string | null | undefined, templates: readonly TemplateInfo[]): boolean {
  if (!hash) return false;
  const tpl = templates.find((x) => x.hash === hash && x.tokenSlots);
  return !!tpl && programAllowed(tpl.name, network);
}

export interface KascovMergeContext {
  network: string | null | undefined;
  /** `kob.templates()` */
  templates: readonly TemplateInfo[];
  registry: TokenRegistry;
  /** verified KCC-23 declarations known so far (they name a kascov-only row before kascov does) */
  declared?: ((covenantId: string) => DeclaredToken | null | undefined) | undefined;
  /** covenant ids KOB's own sources know but the list does not show (an indexer token of a program this build does not trade): never added from kascov */
  known?: ReadonlySet<string> | undefined;
}

const rowHash = (r: Pick<TokenRow, 'info' | 'index'>): string | null => r.info?.templateHash || r.index?.template_hash || null;

/**
 * `row` with kascov's words and icon laid under KOB's own. `noTicker`: the row's ticker is a stand-in (the covenant id a page shows for a token without
 * one), so kascov's may take its place.
 */
function enrich(row: TokenRow, k: KascovToken, noTicker = false): TokenRow {
  const usedTicker = (row.ticker === '' || noTicker) && k.ticker !== null;
  const usedName = !row.name && k.name !== null;
  const meta: KascovMeta = {
    ticker: k.ticker, name: k.name, usedTicker, usedName, verified: k.verified,
    tickerDiffers: row.ticker !== '' && !noTicker && k.ticker !== null && row.ticker.toUpperCase() !== k.ticker.toUpperCase(),
    decimalsDiffers: row.decimals !== null && k.decimals !== null && row.decimals !== k.decimals,
    decimals: k.decimals, imageHash: k.imageHash, imageUrl: k.imageUrl,
  };
  return withKascovRemark({ ...row, ticker: usedTicker ? k.ticker! : row.ticker, name: usedName ? k.name : row.name, kascov: meta }, k.verified);
}

/**
 * One row (a token page's) with kascov's words and icon, when kascov lists it and does not name another program than the row's. `standIn`: the row's
 * ticker when it is only the covenant id shown for a token without a ticker.
 */
export function applyKascov(row: TokenRow, tokens: readonly KascovToken[] | null | undefined, standIn?: string): TokenRow {
  const k = tokens?.find((x) => x.covenantId === row.covenantId.toLowerCase());
  if (!k) return row;
  const mine = rowHash(row);
  return mine && k.templateHash && mine !== k.templateHash ? row : enrich(row, k, standIn !== undefined && row.ticker === standIn);
}

/** A row for a token only kascov names: no registry entry, no indexer view, no orders (`openAsks` / `openBids` unknown, never 0). */
function kascovOnlyRow(k: KascovToken, ctx: KascovMergeContext): TokenRow {
  const tpl = ctx.templates.find((x) => x.hash === k.templateHash && x.tokenSlots)!;
  const view: IndexerTokenView = {
    ticker: '', covenant_id: k.covenantId, template_hash: k.templateHash, extension_commitment: null, decimals: null, open_asks: 0, open_bids: 0,
    template_id: Object.entries(TEMPLATE_ID_PROGRAM).find(([, p]) => p === tpl.name)?.[0] ?? null,
  };
  const base = unknownRow(ctx.registry, view, 'kascov', ctx.declared?.(k.covenantId));
  return enrich({ ...base, openAsks: null, openBids: null }, k);
}

/**
 * The row of one token only kascov names (`source: 'kascov'`), for the token's own page: the same row the list shows, built only when kascov names the
 * token and the program hash it names is one this build pins and trades on `network` (`kascovTradable`). Null otherwise (kascov does not know the token,
 * names no program, or a program this build does not trade). Its facts are KOB's: the page re-derives them from the indexer and the node.
 */
export function kascovRowOf(covenantId: string, tokens: readonly KascovToken[] | null | undefined, ctx: KascovMergeContext): TokenRow | null {
  const k = tokens?.find((x) => x.covenantId === covenantId.toLowerCase());
  return k && kascovTradable(k.templateHash, ctx.network, ctx.templates) ? kascovOnlyRow(k, ctx) : null;
}

/**
 * The rows KOB has, named and pictured from kascov where it has them, plus the kascov-only tokens this build can trade (appended, by name). `tokens`
 * null/empty (kascov down, not loaded yet, a network it does not serve) returns `rows` unchanged. Nothing is removed.
 */
export function mergeKascov(rows: readonly TokenRow[], tokens: readonly KascovToken[] | null | undefined, ctx: KascovMergeContext): TokenRow[] {
  if (!tokens || tokens.length === 0) return [...rows];
  const byId = new Map(tokens.map((k) => [k.covenantId, k]));
  const have = new Set([...rows.map((r) => r.covenantId.toLowerCase()), ...(ctx.known ?? [])]);
  const merged = rows.map((r) => {
    const k = byId.get(r.covenantId.toLowerCase());
    if (!k) return r;
    const mine = rowHash(r);
    // both sides name a program and they differ: the entry is about something else
    return mine && k.templateHash && mine !== k.templateHash ? r : enrich(r, k);
  });
  const added = tokens
    .filter((k) => !have.has(k.covenantId) && kascovTradable(k.templateHash, ctx.network, ctx.templates))
    .map((k) => kascovOnlyRow(k, ctx))
    .sort((a, b) => (a.ticker.toLowerCase() < b.ticker.toLowerCase() ? -1 : a.ticker.toLowerCase() > b.ticker.toLowerCase() ? 1 : a.covenantId < b.covenantId ? -1 : 1));
  return [...merged, ...added];
}
