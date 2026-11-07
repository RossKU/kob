// One token of a market page, whichever way it is known: a listed row, a row only kascov.io names, the wallet's node-verified holdings of it, token
// outputs of it the node confirmed, and for a token without a registry entry the TokenInfo synthesised from what the chain and the indexer show
// (use-open-token.ts). The token page and the two sides of a pair page share this, so a token trades on a pair exactly when it trades on its own page.
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
import { useKascov } from './use-kascov';
import { useOpenToken, type OpenTokenState } from './use-open-token';
import { usePublicToken, type PublicTokenState } from './use-public-token';

export interface MarketTokenOptions {
  /** the rows of the registry and the indexer's token list */
  rows: readonly TokenRow[];
  /** the indexer's token list is still being read */
  rowsLoading: boolean;
  /** changes when the book moved: an open-list token's scales are read again */
  refreshKey: number;
  /** decimals entered for the token's first order (used only while no order tells the scale) */
  decimals: number | null;
  /** the verified declarations known so far (`useDeclarationLookup`) */
  declared: (covenantId: string) => DeclaredToken | null | undefined;
}

export interface MarketToken {
  /** the token as the page trades it: the row with its synthesised TokenInfo where it has one, named */
  row: TokenRow | null;
  /** the row with the creator's name, ticker and icon from kascov.io where KOB has none of its own (the facts stay KOB's) */
  headerRow: TokenRow | null;
  /** what an order ticket trades: the creator's ticker (kascov.io) where KOB has no name of its own; a label, the id stays the identity */
  ticketInfo: TokenInfo | null;
  open: OpenTokenState;
  held: HeldTokenState;
  pub: PublicTokenState;
  shown: TokenInfo | undefined;
  /** the token's facts are its declaration's: no registry entry, or one that disagrees with the chain */
  onChain: boolean;
  /** an answer that can still change the row is on its way (the lists, the wallet's holdings, the token outputs, kascov.io) */
  resolving: boolean;
}

export function useMarketToken(id: string, o: MarketTokenOptions): MarketToken {
  const services = useServices();
  const { registry, kob } = services;
  const { rows, rowsLoading, refreshKey, decimals, declared: declaredOf } = o;
  const listedRow = useMemo(() => rows.find((r) => r.covenantId === id) ?? null, [rows, id]);
  // the creator's name, ticker and icon from kascov.io, where KOB has none of its own (kascov-merge.ts; the facts stay KOB's)
  const kascov = useKascov();
  // a token in neither list (no order of it yet) that kascov.io names and whose program this build pins: the same row the market list shows for it
  const kascovRow = useMemo(
    () => (!listedRow && kascov.status === 'ok' ? kascovRowOf(id, kascov.tokens, { network: services.config.network, templates: kob.templates(), registry, declared: declaredOf }) : null),
    [listedRow, kascov, id, services.config.network, kob, registry, declaredOf],
  );
  // a KCC-20 token in neither list (no order of it yet): its market opens from the wallet's node-verified holdings of it, else from token outputs of it
  // the indexer names and the node confirms, whoever holds them (kob/open-token.ts); the kascov.io row is only what the page shows while it checks, or
  // when nothing verifies (no ticket then: the page says which fact is missing)
  // A listed row that names no program because several order placements claim different ones (`program_ambiguous`; anyone can place one) is
  // resolved the same way: the program a token output of the wallet or of any holder shows, confirmed on the node, wins; with none the page says so
  // (audit 2026-10-30 V1)
  const ambiguous = listedRow?.index?.program_ambiguous === true;
  const notListed = (!listedRow || ambiguous) && !rowsLoading;
  const held = useHeldToken(id, notListed);
  const pub = usePublicToken(id, notListed && !held.row && !held.loading);
  const baseRow = ambiguous ? (held.row ?? pub.row ?? listedRow) : (listedRow ?? held.row ?? pub.row ?? kascovRow);
  // the token's KCC-23 declaration (verified against its covenant id): its decimals decide an open-list token's scale, its facts are shown
  // a token only the wallet's holdings show (the indexer lists it to its owner, not in its token list: no order names it yet) is asked for its declaration
  // too, so a fresh browser of the issuing key names it and knows its decimals instead of asking for them
  const declared = useDeclaration(id, listedRow?.index?.declaration === true || !!held.row || !!pub.row);
  // an open-list token (program on the strict list, no registry entry) is tradable with a TokenInfo synthesised from its orders: its decimals taken
  // from the scale its orders share
  const open = useOpenToken(baseRow, refreshKey, decimals, declared ?? null);
  // a token the indexer lists without a ticker ("") is named like one without any (app/token-display.ts): the open-list token's name (this
  // browser's issuance ticker, else its covenant id), else the display name; never an empty name in a title, a unit or a hint
  const shown = useMemo(() => displayToken(registry, services.config.network, id), [registry, services.config.network, id, declared]);
  // where the token's facts are its declaration's (no registry entry, or one that disagrees with the chain): its name as declared, and the marker
  // the wallet views show ("declared, bound by ancestry", "facts disagree"); never one of this browser's issuance or of assumed decimals here, where the
  // decimals are the market's (the banner under the title says where they come from)
  const onChain = !!shown && (shown.decimalsSource === 'declared' || !!shown.declarationConflicts?.length);
  const row = useMemo(() => {
    if (!baseRow) return baseRow;
    const named = { ...baseRow, ticker: baseRow.ticker || open.info?.ticker || shown?.ticker || '', name: onChain && shown!.name ? sanitizeUntrusted(shown!.name, 64) : baseRow.name };
    return open.info
      ? { ...named, info: open.info, decimals: open.info.decimals, scale: scaleOfDecimals(open.info.decimals), tick: 1n, tradable: true, reason: null }
      : named;
  }, [baseRow, open.info, shown, onChain]);
  // the title bar's row: the creator's name, ticker and icon from kascov.io where KOB has none of its own (the facts below stay KOB's)
  const headerRow = useMemo(() => (row && kascov.status === 'ok' ? applyKascov(row, kascov.tokens, longId(row.covenantId)) : row), [row, kascov]);
  const ticketInfo = useMemo(() => {
    const i = row?.info ?? null;
    return i && i.openList && headerRow && headerRow.ticker && headerRow.ticker !== i.ticker && headerRow.kascov?.usedTicker ? { ...i, ticker: headerRow.ticker } : i;
  }, [row?.info, headerRow]);
  // the token outputs are asked for last and always answer (a row or a reason): until then a token in neither list, or a listed row with no program, is undecided
  const undecided = (!listedRow || ambiguous) && !held.row && !pub.row && pub.reason === null;
  const resolving = rowsLoading || held.loading || pub.loading || undecided || (!baseRow && kascov.status === 'loading');
  return { row, headerRow, ticketInfo, open, held, pub, shown, onChain, resolving };
}
