// One token of a market page, whichever way it is known. It may be a listed row, a row only
// kascov.io names, the wallet's node-verified holdings of it, or token outputs of it the node
// confirmed. For a token without a registry entry it is the TokenInfo synthesised from what the
// chain and the indexer show (use-open-token.ts). The token page and the two sides of a pair page
// share this, so a token trades on a pair exactly when it trades on its own page.
import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { displayToken } from '../../app/token-display';
import { useDeclaration } from '../../app/use-declaration';
import type { DeclaredToken } from '../../data/token-declarations';
import { longId, sanitizeUntrusted, type TokenInfo } from '../../kob/registry';
import { scaleOfDecimals } from '../kit';
import { applyKascov, kascovRowOf } from './kascov-merge';
import type { TokenRow } from './token-model';
import { useHeldToken, type HeldTokenState } from './use-held-token';
import { useKascov, type KascovState } from './use-kascov';
import { useOpenToken, type OpenTokenState } from './use-open-token';
import { usePublicToken, type PublicTokenState } from './use-public-token';

export interface MarketTokenOptions {
  /** The rows of the registry and the indexer's token list. */
  rows: readonly TokenRow[];
  /** The indexer's token list is still being read. */
  rowsLoading: boolean;
  /** Changes when the book moved, so an open-list token's scales are read again. */
  refreshKey: number;
  /**
   * Decimals entered for the token's first order. They are used only while no order tells the
   * scale.
   */
  decimals: number | null;
  /** The verified declarations known so far (`useDeclarationLookup`). */
  declared: (covenantId: string) => DeclaredToken | null | undefined;
}

export interface MarketToken {
  /**
   * The token as the page trades it: the row with its synthesised TokenInfo where it has one,
   * named.
   */
  row: TokenRow | null;
  /**
   * The row with the creator's name, ticker and icon from kascov.io where KOB has none of its own.
   * The facts stay KOB's.
   */
  headerRow: TokenRow | null;
  /**
   * What an order ticket trades, named as the page names it. That is the list's ticker, else the
   * creator's on kascov.io. It is a label, and the id stays the identity.
   */
  ticketInfo: TokenInfo | null;
  open: OpenTokenState;
  held: HeldTokenState;
  pub: PublicTokenState;
  shown: TokenInfo | undefined;
  /**
   * The token's facts are its declaration's, because there is no registry entry or one that
   * disagrees with the chain.
   */
  onChain: boolean;
  /**
   * An answer that can still change the row is on its way: the lists, the wallet's holdings, the
   * token outputs or kascov.io.
   */
  resolving: boolean;
  /** The state of kascov.io's token list (use-kascov.ts). The chart's kascov toggle follows it. */
  kascovStatus: KascovState['status'];
}

export function useMarketToken(id: string, o: MarketTokenOptions): MarketToken {
  const services = useServices();
  const { registry, kob } = services;
  const { rows, rowsLoading, refreshKey, decimals, declared: declaredOf } = o;
  const listedRow = useMemo(() => rows.find((r) => r.covenantId === id) ?? null, [rows, id]);
  // The creator's name, ticker and icon from kascov.io, where KOB has none of its own
  // (kascov-merge.ts). The facts stay KOB's.
  const kascov = useKascov();
  // A token in neither list, with no order of it yet, that kascov.io names and whose program this
  // build pins. It is the same row the market list shows for it.
  const kascovRow = useMemo(
    () => (!listedRow && kascov.status === 'ok' ? kascovRowOf(id, kascov.tokens, { network: services.config.network, templates: kob.templates(), registry, declared: declaredOf }) : null),
    [listedRow, kascov, id, services.config.network, kob, registry, declaredOf],
  );
  // A KCC-20 token in neither list, with no order of it yet. Its market opens from the wallet's
  // node-verified holdings of it, else from token outputs of it the indexer names and the node
  // confirms, whoever holds them (kob/open-token.ts). The kascov.io row is only what the page
  // shows while it checks, or when nothing verifies. There is no ticket then, and the page says
  // which fact is missing.
  // A listed row that names no program is resolved the same way. That happens when several order
  // placements claim different programs (`program_ambiguous`), which anyone can place. The program
  // a token output of the wallet or of any holder shows, confirmed on the node, wins. With none
  // the page says so.
  const ambiguous = listedRow?.index?.program_ambiguous === true;
  const notListed = (!listedRow || ambiguous) && !rowsLoading;
  const held = useHeldToken(id, notListed);
  const pub = usePublicToken(id, notListed && !held.row && !held.loading);
  const baseRow = ambiguous ? (held.row ?? pub.row ?? listedRow) : (listedRow ?? held.row ?? pub.row ?? kascovRow);
  // The token's KCC-23 declaration, verified against its covenant id. Its decimals decide an
  // open-list token's scale, and its facts are shown. A token only the wallet's holdings show is
  // asked for its declaration too. The indexer lists it to its owner and not in its token list,
  // because no order names it yet. So a fresh browser of the issuing key names it and knows its
  // decimals instead of asking for them.
  const declared = useDeclaration(id, listedRow?.index?.declaration === true || !!held.row || !!pub.row);
  // An open-list token has a program on the strict list and no registry entry. It is tradable with
  // a TokenInfo synthesised from its orders, with its decimals taken from the scale its orders
  // share.
  const open = useOpenToken(baseRow, refreshKey, decimals, declared ?? null);
  // A token the indexer lists without a ticker ("") is named like one without any
  // (app/token-display.ts). That is the open-list token's name, this browser's issuance ticker or
  // else its covenant id, else the display name. There is never an empty name in a title, a unit
  // or a hint.
  const shown = useMemo(() => displayToken(registry, services.config.network, id), [registry, services.config.network, id, declared]);
  // Where the token's facts are its declaration's, there is no registry entry or it disagrees with
  // the chain. Its name is then as declared, with the marker the wallet views show ("declared,
  // bound by ancestry", "facts disagree"). It never uses one of this browser's issuance or of
  // assumed decimals here. The decimals are the market's, and the banner under the title says
  // where they come from.
  const onChain = !!shown && (shown.decimalsSource === 'declared' || !!shown.declarationConflicts?.length);
  const row = useMemo(() => {
    if (!baseRow) return baseRow;
    const named = { ...baseRow, ticker: baseRow.ticker || open.info?.ticker || shown?.ticker || '', name: onChain && shown!.name ? sanitizeUntrusted(shown!.name, 64) : baseRow.name };
    return open.info
      ? { ...named, info: open.info, decimals: open.info.decimals, scale: scaleOfDecimals(open.info.decimals), tick: 1n, tradable: true, reason: null }
      : named;
  }, [baseRow, open.info, shown, onChain]);
  // The title bar's row has the creator's name, ticker and icon from kascov.io where KOB has none
  // of its own. The facts below stay KOB's.
  const headerRow = useMemo(() => (row && kascov.status === 'ok' ? applyKascov(row, kascov.tokens, longId(row.covenantId)) : row), [row, kascov]);
  const ticketInfo = useMemo(() => {
    const i = row?.info ?? null;
    return i && i.openList && headerRow && headerRow.ticker && headerRow.ticker !== i.ticker ? { ...i, ticker: headerRow.ticker } : i;
  }, [row?.info, headerRow?.ticker]);
  // The token outputs are asked for last and always answer, with a row or a reason. Until then a
  // token in neither list, or a listed row with no program, is undecided.
  const undecided = (!listedRow || ambiguous) && !held.row && !pub.row && pub.reason === null;
  const resolving = rowsLoading || held.loading || pub.loading || undecided || (!baseRow && kascov.status === 'loading');
  return { row, headerRow, ticketInfo, open, held, pub, shown, onChain, resolving, kascovStatus: kascov.status };
}
