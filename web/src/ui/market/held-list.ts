// The connected wallet's own tokens in the market list. The indexer's token list names only
// registry tokens and tokens that have had an order placed, so a token just created, or one sitting
// in a wallet with no order ever placed, has no row there and its holder could not find its market.
// The wallet's holdings (`TokenTracker.heldTokens`, the same node-verified source the Balances
// panel reads) add a row for each such token, built as its market page builds it
// (`kob/open-token.ts` `heldTokenView`), and mark every token the wallet holds. The list's filters
// (favorites, hidden, held, the tokens without a name) are decided here too. Pure.
import type { HeldTokenUtxo } from '../../data/token-tracker';
import { heldTokenView } from '../../kob/open-token';
import type { TokenRegistry } from '../../kob/registry';
import { isKeyOwned } from '../../kob/token-state';
import type { TokenProgram, TokenUtxo } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';
import { filterTokens, isNameless, unknownRow, type DeclarationLookup, type TokenRow } from './token-model';

/**
 * What the list reads of one held token output: its token, the program its script verified under,
 * and its state.
 */
export type Holding = Pick<HeldTokenUtxo, 'covenantId' | 'program'> & Pick<TokenUtxo, 'state'> & Partial<TokenUtxo>;

const amountOf = (h: Holding): bigint => {
  try {
    return BigInt(h.state.amount);
  } catch {
    return 0n;
  }
};

/**
 * The covenant ids (lower case) of every token the wallet holds: any output of it that the node
 * confirmed, a KRON output KOB cannot spend included, as the Balances panel counts it. An output of
 * no tokens holds nothing.
 */
export function heldIds(holdings: readonly Holding[] | null | undefined): Set<string> {
  return new Set((holdings ?? []).filter((h) => amountOf(h) > 0n).map((h) => h.covenantId.toLowerCase()));
}

/**
 * A row for each token the wallet holds that `listed` (lower-case covenant ids) does not name. Each
 * one is built from the wallet's key-owned outputs of it, as its market page builds it
 * (`heldTokenView`: one pinned program that this network allows, one extension commitment), and
 * named by its verified KCC-23 declaration where one is known. A token whose holdings do not make
 * it tradable here gets no row, as its page would show no ticket. Sorted by covenant id, so the
 * rows do not move when the holdings arrive in another order.
 */
export function heldRows(
  kob: KobWasm, reg: TokenRegistry, holdings: readonly Holding[] | null | undefined, listed: ReadonlySet<string>, network: string | null | undefined, declared?: DeclarationLookup,
): TokenRow[] {
  const byId = new Map<string, Holding[]>();
  for (const h of holdings ?? []) {
    const id = h.covenantId.toLowerCase();
    // Only what the key can spend is offered for a first order, as `findHeldToken` decides for the
    // page.
    if (listed.has(id) || amountOf(h) <= 0n || !isKeyOwned(h.state, h.program)) continue;
    byId.set(id, [...(byId.get(id) ?? []), h]);
  }
  const out: TokenRow[] = [];
  for (const [id, mine] of [...byId].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
    const programs = [...new Set(mine.map((h) => h.program))] as TokenProgram[];
    const r = heldTokenView(kob, id, programs, mine.map((h) => ({ ...h, covenantId: id }) as TokenUtxo), network);
    if (r.ok) out.push(unknownRow(reg, r.view, 'held', declared?.(id)));
  }
  return out;
}

/**
 * The list's rows with the held rows a row of the same token does not already stand for added after
 * them.
 */
export function withHeldRows<R extends Pick<TokenRow, 'covenantId'>>(rows: readonly R[], held: readonly R[]): R[] {
  const have = new Set(rows.map((r) => r.covenantId.toLowerCase()));
  const out = [...rows];
  for (const h of held) {
    const id = h.covenantId.toLowerCase();
    if (have.has(id)) continue;
    have.add(id);
    out.push(h);
  }
  return out;
}

// ------------------------------------------------------------------------------------------------
// the filters

/**
 * The list's filters. `favorites` and `hidden` are views of their own and exclude each other;
 * `held` narrows whichever view is shown to the wallet's tokens; `nameless` lets the tokens without
 * a name into the main list and the Held view.
 */
export interface ListFilters {
  favorites: boolean;
  hidden: boolean;
  held: boolean;
  nameless: boolean;
}

export type FilterKey = keyof ListFilters;

export const NO_FILTERS: ListFilters = { favorites: false, hidden: false, held: false, nameless: false };

/**
 * One filter switched; the Favorites and the Hidden views exclude each other, as their buttons did
 * before they were one menu.
 */
export function toggleFilter(f: ListFilters, key: FilterKey): ListFilters {
  const next = { ...f, [key]: !f[key] };
  if (key === 'favorites' && next.favorites) next.hidden = false;
  if (key === 'hidden' && next.hidden) next.favorites = false;
  return next;
}

/** How many filters are on: the number the menu's button shows. */
export const activeFilters = (f: ListFilters): number => (Object.keys(f) as FilterKey[]).filter((k) => f[k]).length;

export interface FilterInput<R extends TokenRow> {
  /** the list's rows (registry, indexer, held, kascov.io) */
  rows: readonly R[];
  /** the Favorites view's rows (a favourite no list names keeps a row by its id) */
  favoriteRows: readonly R[];
  /** the Hidden view's rows, the same way */
  hiddenRows: readonly R[];
  /** the hidden tokens (lower-case covenant ids) */
  hiddenIds: ReadonlySet<string>;
  /** the tokens the wallet holds (lower-case covenant ids, `heldIds`) */
  heldIds: ReadonlySet<string>;
  filters: ListFilters;
  query: string;
}

/**
 * The rows a view shows, before the sort. The main list leaves out the tokens without a name unless
 * asked for and the hidden tokens; a search looks through all of them. The Favorites and the Hidden
 * views list their own tokens. The Held filter keeps the wallet's tokens of whichever view is
 * shown.
 */
export function filterRows<R extends TokenRow>(o: FilterInput<R>): R[] {
  const { filters: f } = o;
  const searching = o.query.trim() !== '';
  const named = searching || f.nameless ? o.rows : o.rows.filter((r) => !isNameless(r));
  const inList = searching ? named : named.filter((r) => !o.hiddenIds.has(r.covenantId.toLowerCase()));
  const view = f.favorites ? o.favoriteRows : f.hidden ? o.hiddenRows : inList;
  const kept = f.held ? view.filter((r) => o.heldIds.has(r.covenantId.toLowerCase())) : view;
  return filterTokens(kept, o.query) as R[];
}
