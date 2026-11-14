import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useMedia } from '../shell/narrow';
import { useServices } from '../../app/context';
import { FiatNote, useFiat, useKasRate } from '../../app/use-fiat';
import { useDeclarationLookup } from '../../app/use-declaration';
import { navigate, pairRoute, routeToHash, tokenRoute } from '../../app/router';
import type { IndexerTokenView, PairBookView, PairFillsView, StatsView } from '../../data/indexer-types';
import type { PairToken } from '../../kob/pair';
import { t } from '../../i18n';
import { Badge, Banner, Button, ErrorBanner, Field, Section, Table, TableMessage, toError, useAsync } from '../kit';
import { FavoriteStar, HideToggle } from './FavoriteStar';
import { isFavoritePair, isFavoriteToken, togglePair, toggleToken, useFavorites } from './favorites';
import { useHiddenTokens } from './hidden-tokens';
import { ListPager, RowsControl, showListStart } from './ListPaging';
import { PairPicker } from './MarketBar';
import { StatusBadges } from './TokenBadges';
import { IdentityBlock } from './Fingerprint';
import { IdCopy } from './IdCopy';
import { ListLegend } from './ListLegend';
import { hasKascovMismatch, KascovMismatch, KascovNote } from './KascovNote';
import { mergeKascov } from './kascov-merge';
import { TokenIcon } from './TokenIcon';
import { TokenNote } from './TokenNote';
import './market.css';
import {
  ariaSort, DEFAULT_SORT, filterPairs, heldEntry, kascovEntry, keepOrder, listedPairs, LOADING, nextSort, NONE, pageWindow, pairEntry, pairHasDecimals, pairLabelOf, sortEntries, tokenEntry,
  type Cell, type ListedPair, type ListEntry, type Slot, type SortKey, type SortState,
} from './market-list';
import { mergeOrientations } from './pair-model';
import { mergePairFills } from '../activity/activity-model';
import { PAIR_DEPTH_LEVELS, pairDepthOf, pairVolumeOf, type PairDepth, type PairVolume } from './pair-market';
import { buildTokenRows, idOnlyRow, isNameless, parseCovenantId, tokenLabel, tradableHere, tradableRows, type TokenRow } from './token-model';
import { filterRows, heldIds, heldRows, NO_FILTERS, toggleFilter, withHeldRows, type FilterKey, type ListFilters } from './held-list';
import { ListFilterMenu, type FilterItem } from './ListFilterMenu';
import { useHeldDeclarations, useWalletHoldings } from './use-wallet-holdings';
import { useKascov } from './use-kascov';
import { useHeldUntil, useListPage, useListPageSize, usePooledSlots } from './use-market-list';
import { isAbort } from '../kit/hooks';
import { useStandalone } from '../shell/standalone';

function AddUnknownToken(props: { onFound: (v: IndexerTokenView) => void }) {
  const { indexer, registry, kob } = useServices();
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  // `page`: the token's page, where a wallet that holds a token the indexer does not list yet can open its market
  const [message, setMessage] = useState<{ tone: 'error' | 'info'; text: string; page?: string } | null>(null);
  const id = parseCovenantId(text);

  const lookup = async () => {
    if (!id) return;
    if (registry.byCovenantId.has(id)) {
      navigate(tokenRoute(id));
      return;
    }
    if (!indexer) {
      setMessage({ tone: 'error', text: t('market.add.noIndexer') });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const found = (await indexer.tokens()).find((x) => x.covenant_id === id);
      if (!found) setMessage({ tone: 'error', text: t('market.add.notFound'), page: routeToHash(tokenRoute(id)) });
      else if (!tradableHere({ source: 'indexer', info: null, index: found }, kob.templates())) setMessage({ tone: 'error', text: t('market.add.untradable') });
      else {
        props.onFound(found);
        setMessage({ tone: 'info', text: t('market.add.found') });
        setText('');
      }
    } catch (e) {
      setMessage({ tone: 'error', text: t('common.errorDetail', { message: toError(e).message }) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title={t('market.add.title')} collapsible defaultOpen={false} data-testid="add-token">
      <p class="small muted">{t('market.add.hint')}</p>
      <div class="row" style="align-items:flex-start">
        <div class="grow">
          <Field
            label={t('market.add.label')}
            value={text}
            onValue={(v) => {
              setText(v);
              setMessage(null);
            }}
            error={text.trim() !== '' && !id ? t('market.add.invalid') : null}
            placeholder="64 hex"
            data-testid="token-add-input"
          />
        </div>
        <div style="padding-top:24px">
          <Button variant="primary" disabled={!id} loading={busy} onClick={() => void lookup()} data-testid="token-add-button">
            {t('market.add.button')}
          </Button>
        </div>
      </div>
      {message ? (
        <Banner tone={message.tone} data-testid="token-add-result">
          {message.text}
          {message.page ? <> <a href={message.page} data-testid="token-add-page">{t('market.add.openPage')}</a></> : null}
        </Banner>
      ) : null}
    </Section>
  );
}

/** A figure of a row: a skeleton while its row loads, `—` when there is none (the tooltip says why when the indexer could not answer). */
function Figure(props: { cell: Cell; entry: ListEntry; testid: string; cls?: string | undefined }) {
  const c = props.cell;
  const title = c.title ?? (c.text === null && props.entry.unavailable ? t('market.list.unavailable') : null);
  return (
    <td class={`right num nowrap${props.cls ? ` ${props.cls}` : ''}`} data-testid={props.testid} title={title ?? undefined}>
      {c.text !== null ? (
        <span class={c.tone ? `tone-${c.tone}` : undefined} data-thin={c.thin ? 'true' : undefined} data-thin-book={c.thinBook ? 'true' : undefined}>{c.text}</span>
      ) : props.entry.loading ? (
        <span class="skeleton skeleton-line ml-skel" role="status" aria-label={t('common.loading')} />
      ) : (
        '—'
      )}
    </td>
  );
}

/**
 * The figure columns, in order: key, header, the class that hides it on a narrow screen (a phone keeps the price and the change: the market cap and the depth
 * would not fit a card beside the name, the covenant id and the badges). The market cap is a token's (a pair has no supply).
 */
const FIGURES: { key: Exclude<SortKey, 'market'>; label: (quote: string | null) => string; hint?: string; cls?: string; tokensOnly?: true }[] = [
  { key: 'last', label: (quote) => (quote ? t('market.col.lastIn', { quote }) : t('market.col.last')), hint: 'market.col.lastHint', cls: 'ml-last' },
  { key: 'change', label: () => t('market.col.change'), hint: 'market.col.changeHint', cls: 'ml-change' },
  { key: 'volume', label: (quote) => (quote ? t('market.col.volumeIn', { quote }) : t('market.col.volume')), cls: 'ml-md' },
  { key: 'mcap', label: (quote) => t('market.col.mcap', { quote: quote ?? 'KAS' }), hint: 'market.col.mcapHint', cls: 'ml-md', tokensOnly: true },
  { key: 'spread', label: () => t('market.col.spread'), cls: 'ml-md' },
  { key: 'depth', label: (quote) => (quote ? t('market.col.depthIn', { quote }) : t('market.col.depth')), hint: 'market.col.depthHint', cls: 'ml-md' },
];
const TOKEN_FIGURES = FIGURES;
const PAIR_FIGURES = FIGURES.filter((f) => !f.tokensOnly);

/** A sortable column header: a button (keyboard and screen reader: `aria-sort` on the header cell). */
function SortHeader(props: { k: SortKey; label: string; hint?: string | undefined; sort: SortState; onSort: (k: SortKey) => void; right?: boolean; cls?: string | undefined }) {
  const state = ariaSort(props.sort, props.k);
  const cls = `${props.right ? 'right ' : ''}${props.cls ?? ''}`.trim();
  return (
    <th class={cls || undefined} aria-sort={state} title={props.hint}>
      <button type="button" class={`th-sort${state !== 'none' ? ' active' : ''}`} onClick={() => props.onSort(props.k)} data-testid={`sort-${props.k}`}>
        {props.label}
        <span class="th-sort-mark" aria-hidden="true">{state === 'ascending' ? '▲' : state === 'descending' ? '▼' : '↕'}</span>
      </button>
    </th>
  );
}

const rowState = (e: ListEntry): string => (e.loading ? 'loading' : e.unavailable ? 'unavailable' : 'ready');

/** The most rows of a batch that wait for the one above: a long batch is not slower than a short one. */
const REVEAL_STEPS = 14;
/** The placeholder rows while the list is held (the first figures are not in yet). */
const SKELETON_ROWS = 8;

/** A row's place while the list is held: as tall as a token's row, so nothing moves when the rows come. */
function SkeletonRow() {
  return (
    <tr class="ml-skel-row" data-testid="token-row-skeleton" aria-hidden="true">
      <td>
        <div class="ml-name">
          <span class="skeleton ml-skel-icon" />
          <div class="ml-name-text ml-skel-text">
            <span class="skeleton skeleton-line" />
            <span class="skeleton skeleton-line" />
          </div>
        </div>
      </td>
      {TOKEN_FIGURES.map((f) => (
        <td key={f.key} class={`right${f.cls ? ` ${f.cls}` : ''}`}>
          <span class="skeleton skeleton-line ml-skel" />
        </td>
      ))}
    </tr>
  );
}

/** How long a hidden row takes to leave (`.ml-out` in market.css). */
const ROW_LEAVE_MS = 360;
const reducedMotion = (): boolean => typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

/** A phone: the list is cards (market.css), and a card's star and eye are one "⋯" menu. */
const PHONE_LIST_QUERY = '(max-width: 759px)';

function TokenTableRow({ row, entry, step, favorite, onFavorite, hidden, onHide, held, leaving, again }: { row: TokenRow; entry: ListEntry; step: number; favorite: boolean; onFavorite: (id: string) => void; hidden: boolean; onHide: (id: string) => void; held: boolean; leaving: boolean; again: '' | 'a' | 'b' }) {
  // every row has the same three lines, so every row is as tall and the icons keep one pitch: line 1 the ticker (bold) and the name (one line, an ellipsis, the
  // whole name in its title); line 2 the covenant id fragment with its copy button; line 3 the badges (one line, never wrapped). A token without a ticker is led by its name
  const headline = row.ticker || row.name || t('market.list.unnamed');
  const sub = row.ticker ? row.name : '';
  // a phone: the star and the eye are behind one "⋯" (they took a third of a card's line), and the covenant id is short, the whole of it one tap away
  const phone = useMedia(PHONE_LIST_QUERY);
  const [menu, setMenu] = useState(false);
  const [idOpen, setIdOpen] = useState(false);
  useEffect(() => {
    if (!menu) return;
    const away = (e: Event) => { if (!(e.target as Element | null)?.closest?.(`[data-testid="token-row-${row.covenantId}"] .ml-actions, [data-testid="token-more-${row.covenantId}"]`)) setMenu(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === 'Escape') setMenu(false); };
    document.addEventListener('pointerdown', away);
    document.addEventListener('keydown', esc);
    return () => { document.removeEventListener('pointerdown', away); document.removeEventListener('keydown', esc); };
  }, [menu, row.covenantId]);
  useEffect(() => { if (!phone) { setMenu(false); setIdOpen(false); } }, [phone]);
  return (
    <tr class={`ml-in${again ? ` ml-again-${again}` : ''}${leaving ? ' ml-out' : ''}`} style={`--i:${step}`} data-testid={`token-row-${row.covenantId}`} data-source={row.source} data-tradable={row.tradable ? '1' : '0'} data-held={held ? '1' : undefined} data-state={rowState(entry)}>
      <td>
        <div class="ml-name">
          <span class={`ml-actions${menu ? ' is-open' : ''}`} role={phone ? 'menu' : undefined}>
            <FavoriteStar on={favorite} name={tokenLabel(row)} onToggle={() => { onFavorite(row.covenantId); setMenu(false); }} data-testid={`token-favorite-${row.covenantId}`} />
            <HideToggle hidden={hidden} name={tokenLabel(row)} onToggle={() => { onHide(row.covenantId); setMenu(false); }} data-testid={`token-hide-${row.covenantId}`} />
          </span>
          <IdentityBlock row={row} />
          <div class="ml-name-text">
            <div class="ml-line1">
              <a href={`#/market/${row.covenantId}`} class="token-name" title={headline} data-testid={`token-link-${row.covenantId}`}>
                {headline}
              </a>
              {sub || row.declared || row.kascov || row.source === 'kascov' ? (
                <span class="small muted ml-sub" title={sub || undefined} data-testid={`token-name-${row.covenantId}`}>
                  {sub}
                  {row.declared && row.decimals !== null ? <TokenNote token={{ decimalsSource: 'declared', decimals: row.decimals }} /> : null}
                  <KascovNote row={row} compact />
                  {/* the depth column is hidden on a phone: the row says it here */}
                  {row.source === 'kascov' ? <span class="ml-only-narrow" data-testid={`token-no-orders-narrow-${row.covenantId}`}> · {t('market.list.noOrders')}</span> : null}
                </span>
              ) : null}
            </div>
            <div class={`ml-line2${phone && idOpen ? ' ml-line2-open' : ''}`}>
              <IdCopy value={row.covenantId} data-testid={`token-hash-${row.covenantId}`} compact={phone} expanded={idOpen} onToggle={() => setIdOpen(!idOpen)} />
            </div>
            <div class={hasKascovMismatch(row) ? 'ml-line3 ml-line3-wrap' : 'ml-line3'} data-testid={`token-badges-${row.covenantId}`}>
              <span class="ml-badges">
                {/* The wallet holds the token. It is said first, so a phone, which hides the family chip,
                    still shows it. */}
                {held ? <Badge tone="info" class="ml-held" title={t('market.held.title')} data-testid={`token-held-${row.covenantId}`}>{t('market.held.badge')}</Badge> : null}
                <StatusBadges row={row} />
                <KascovMismatch row={row} />
              </span>
            </div>
          </div>
        </div>
      </td>
      {TOKEN_FIGURES.map((f) => <Figure key={f.key} cell={entry.cells[f.key]} entry={entry} testid={`token-${f.key}-${row.covenantId}`} cls={f.cls} />)}
      <td class="ml-more-cell">
        <button type="button" class="ml-more" aria-haspopup="menu" aria-expanded={menu ? 'true' : 'false'} aria-label={t('market.row.more', { name: tokenLabel(row) })} title={t('market.row.more', { name: tokenLabel(row) })} onClick={() => setMenu(!menu)} data-testid={`token-more-${row.covenantId}`}>
          <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor" aria-hidden="true" focusable="false"><circle cx="5" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="19" cy="12" r="1.8" /></svg>
        </button>
      </td>
    </tr>
  );
}

function PairTableRow({ pair, entry, onFavorite }: { pair: ListedPair; entry: ListEntry; onFavorite: (p: ListedPair) => void }) {
  const id = `${pair.base.covenantId}-${pair.quote.covenantId}`;
  return (
    <tr data-testid={`pair-row-${id}`} data-state={rowState(entry)}>
      <td>
        <div class="ml-pair-name">
          <FavoriteStar on name={pairLabelOf(pair)} onToggle={() => onFavorite(pair)} data-testid={`pair-favorite-${id}`} />
          <a href={routeToHash(pairRoute(pair.base.covenantId, pair.quote.covenantId))} class="token-name" data-testid={`pair-link-${id}`}>
            {pairLabelOf(pair)}
          </a>
        </div>
      </td>
      {PAIR_FIGURES.map((f) => <Figure key={f.key} cell={entry.cells[f.key]} entry={entry} testid={`pair-${f.key}-${id}`} cls={f.cls} />)}
    </tr>
  );
}

/** What the list reads of a pair besides its two KAS markets: the 24 h pair volume and the depth within 2 % of the mid (either may be missing). */
interface PairFigures { volume: PairVolume | null; depth: PairDepth | null }

/** One figure of the pair slots as its own slot: a pair whose figures are none leaves it unavailable. */
function sliceOf<T>(slots: Map<string, Slot<PairFigures | null>>, pick: (f: PairFigures) => T | null): Map<string, Slot<T | null>> {
  const out = new Map<string, Slot<T | null>>();
  for (const [k, s] of slots) out.set(k, s.state === 'ok' ? { state: 'ok', value: pick(s.value!) } : s);
  return out;
}

/**
 * A row whose market the indexer has figures for. A row only kascov.io or the wallet's holdings
 * name has no order the indexer knows, so nothing is asked for it.
 */
const hasStats = (r: TokenRow): boolean => r.source !== 'kascov' && r.source !== 'held';

const pairToken = (r: TokenRow): PairToken => ({ covenantId: r.covenantId, ticker: r.ticker, decimals: r.decimals ?? 0, scale: r.scale ?? 1n });

/**
 * Market list: every token this build can trade as its KAS market (TOKEN/KAS: last price, 24 h change, 24 h KAS volume, market cap, spread, depth within
 * 2 % of the mid; the token's badges and its full token hash), sortable by every column (24 h volume first), searchable, add by covenant id. A phone shows the
 * name, the price and the change. A token pair is reached by the picker; the Favorites view lists the favourite tokens and pairs (BASE/QUOTE, the same columns without
 * the market cap), and only then is anything read for a pair.
 *
 * The rows arrive together: the list is held on placeholders until the first figures are in (at most LIST_HOLD_MS), then the first rows rise in, sorted, and
 * stay in their places while later figures arrive; a sort, a search or the end of the hold's late figures places them anew. One page shows the number of rows the
 * viewer picked, so only those rows load their icons; the search looks through every page, and a new sort, search or size starts at the first page.
 */
export function TokenList(props: { invalidToken?: string }) {
  const { registry, indexer, kob, config, standalone } = useServices();
  // USD display (Settings): the price, volume, market cap and depth of the token rows in dollars; null = KAS
  const fiat = useFiat();
  // the KAS price whatever the display currency: the thin-book marker of the 24 h change ("under $100 within 2 %")
  const rate = useKasRate();
  // standalone mode (data/standalone.ts): the list and its figures are what this page saw on the node since the mode started, read again as it sees more
  const sa = useStandalone(standalone);
  const saKey = sa?.active ? `standalone:${sa.version}` : 'indexer';
  const tokens = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer, saKey]);
  const { favorites, change: changeFavorites } = useFavorites(config.network);
  // the tokens the viewer hid from the list (hidden-tokens.ts)
  const { hidden, toggle: toggleHiddenToken } = useHiddenTokens(config.network);
  const hiddenSet = useMemo(() => new Set(hidden), [hidden]);
  // The list's filters are one menu (held-list.ts): the Favorites and the Hidden views, the
  // wallet's tokens and the tokens without a name.
  const [filters, setFilters] = useState<ListFilters>(NO_FILTERS);
  const { favorites: favoritesOnly, hidden: hiddenOnly, nameless: showNameless } = filters;
  // a row the eye was pressed on leaves as it came in, reversed (`.ml-out`), then the list drops it
  const [leaving, setLeaving] = useState<ReadonlySet<string>>(() => new Set());
  // once it is gone, the rows that stood below it rise in again from its place, one after the other, as the page's rows first did (`.ml-again-a` / `-b` in
  // market.css: another animation name restarts the animation without remounting the row), instead of jumping up a place
  const [again, setAgain] = useState<{ from: number; n: number }>({ from: Number.POSITIVE_INFINITY, n: 0 });
  // the live order counts of the favourite pairs: nothing is read for a pair until they are shown
  const readPairs = favoritesOnly && favorites.pairs.length > 0;
  const pairs = useAsync((signal) => (indexer && readPairs ? indexer.pairs(undefined, { signal }) : Promise.resolve(null)), [indexer, !!sa?.active, readPairs]);
  const [query, setQuery] = useState('');
  const [pasted, setPasted] = useState<IndexerTokenView[]>([]);
  const [sort, setSort] = useState<SortState>(DEFAULT_SORT);
  const [generation, setGeneration] = useState(0);
  const firstKey = useRef(saKey);
  useEffect(() => {
    if (firstKey.current === saKey) return;
    firstKey.current = saKey;
    setGeneration((g) => g + 1);
  }, [saKey]);

  // a token this build cannot trade is not listed (a direct link to it shows a short "not tradable" page)
  // a token the registry does not list is named by its verified KCC-23 declaration (its ticker, name and decimals)
  const declared = useDeclarationLookup();
  // the names, icons and tokens kascov.io adds (kascov-merge.ts): only tokens of a program this build trades on this network; none of its facts
  const kascov = useKascov();
  const kascovTokens = kascov.status === 'ok' ? kascov.tokens : null;
  // The connected wallet's tokens (use-wallet-holdings.ts). Each one the lists do not name gets a
  // row, so its holder finds its market and places its first order there, and every one the
  // wallet holds is marked. Nothing of this reaches anyone else's list.
  const { holdings, connected } = useWalletHoldings(`${generation}|${saKey}`);
  const heldSet = useMemo(() => heldIds(holdings), [holdings]);
  const built = useMemo(() => buildTokenRows(registry, tokens.data ?? null, pasted, declared), [registry, tokens.data, pasted, declared]);
  const mine = useMemo(
    () => heldRows(kob, registry, holdings, new Set(built.map((r) => r.covenantId.toLowerCase())), config.network, declared),
    [kob, registry, holdings, built, config.network, declared],
  );
  useHeldDeclarations(useMemo(() => mine.map((r) => r.covenantId), [mine]));
  const rows = useMemo(() => {
    const all = withHeldRows(built, mine);
    const templates = kob.templates();
    const known = new Set(all.map((r) => r.covenantId.toLowerCase()));
    return mergeKascov(tradableRows(all, templates), kascovTokens, { network: config.network, templates, registry, declared, known });
  }, [built, mine, kob, kascovTokens, config.network, registry, declared]);
  // A wallet that is gone has no tokens to show: its filter goes with it.
  useEffect(() => {
    if (!connected && filters.held) setFilters((f) => ({ ...f, held: false }));
  }, [connected, filters.held]);
  const listed = useMemo(() => (favoritesOnly ? listedPairs(favorites.pairs, rows, pairs.data) : []), [favoritesOnly, favorites.pairs, pairs.data, rows]);
  // the Favorites view's tokens: a favourite no list names now keeps a row by its id, so its star can be taken off
  const favoriteRows = useMemo(() => {
    if (!favoritesOnly) return [];
    const byId = new Map(rows.map((r) => [r.covenantId.toLowerCase(), r]));
    return favorites.tokens.map((id) => byId.get(id) ?? idOnlyRow(id));
  }, [favoritesOnly, favorites.tokens, rows]);
  // the Hidden view's tokens: as the Favorites view, a hidden token no list names now keeps a row by its id, so it can be shown again
  const hiddenRows = useMemo(() => {
    if (!hiddenOnly) return [];
    const byId = new Map(rows.map((r) => [r.covenantId.toLowerCase(), r]));
    return hidden.map((id) => byId.get(id) ?? idOnlyRow(id));
  }, [hiddenOnly, hidden, rows]);

  // the 24 h figures: one `/v1/stats` per listed token, and in the Favorites view one more for a token only a favourite names and one 24 h pair volume per pair, once
  // the token list is in (a few calls at a time)
  const ready = !indexer || tokens.data !== undefined || tokens.error !== null;
  const statKeys = useMemo(() => {
    if (!ready) return null;
    const own = rows.filter(hasStats).map((r) => r.covenantId);
    const more = [...favoriteRows.filter(hasStats).map((r) => r.covenantId), ...listed.flatMap((p) => [p.base.covenantId, p.quote.covenantId])];
    return [...new Set([...own, ...more])];
  }, [ready, rows, favoriteRows, listed]);
  const loadStats = useMemo(() => (indexer ? (id: string, signal: AbortSignal) => indexer.stats(id, { signal }) : null), [indexer]);
  const stats = usePooledSlots(statKeys, loadStats, generation);
  const pairKeys = useMemo(() => (pairs.data !== undefined || pairs.error ? listed.map((p) => p.key) : null), [pairs.data, pairs.error, listed]);
  const loadPairFigures = useMemo(() => {
    if (!indexer) return null;
    const byKey = new Map(listed.map((p) => [p.key, p]));
    return async (key: string, signal: AbortSignal): Promise<PairFigures | null> => {
      const p = byKey.get(key);
      if (!p || !pairHasDecimals(p)) return null;
      const [base, quote] = [pairToken(p.base), pairToken(p.quote)];
      // the 24 h volume and the depth near the mid (the pair book), both orientations of the pair together (the pair page's figures): a book that cannot be read
      // leaves only its own cell empty, and an orientation that cannot be read leaves the other's figure
      const soft = <T,>(e: unknown): T | null => {
        if (isAbort(e)) throw e;
        return null;
      };
      const [view, viewBack, book, bookBack] = await Promise.all([
        indexer.pairFills(p.base.covenantId, p.quote.covenantId, { limit: 1 }, { signal }),
        indexer.pairFills(p.quote.covenantId, p.base.covenantId, { limit: 1 }, { signal }).catch(soft<PairFillsView>),
        indexer.pairBook(p.base.covenantId, p.quote.covenantId, { depth: PAIR_DEPTH_LEVELS }, { signal }).catch(soft<PairBookView>),
        indexer.pairBook(p.quote.covenantId, p.base.covenantId, { depth: PAIR_DEPTH_LEVELS }, { signal }).catch(soft<PairBookView>),
      ]);
      const volume = pairVolumeOf(view ? mergePairFills(view, viewBack) : view, base, quote);
      const depth = pairDepthOf(book ? mergeOrientations(book, bookBack) : book, base, quote);
      return volume === null && depth === null ? null : { volume, depth };
    };
  }, [indexer, listed]);
  const pairFigures = usePooledSlots(pairKeys, loadPairFigures, generation);
  const volumes = useMemo(() => sliceOf(pairFigures, (f) => f.volume), [pairFigures]);
  const depths = useMemo(() => sliceOf(pairFigures, (f) => f.depth), [pairFigures]);

  // the first rows wait for the figures they are sorted by: the indexer's list, its stats and kascov's names, each at its own pace, would move them
  const figuresIn = ready && statKeys !== null && statKeys.every((k) => (stats.get(k) ?? LOADING).state !== 'loading') && kascov.status !== 'loading';
  const open = useHeldUntil(figuresIn);

  const shownRows = useMemo(() => {
    const slotOf = (id: string): Slot<StatsView> => stats.get(id) ?? (indexer ? LOADING : NONE);
    // tokens without metadata only on a search or on request, or as a favourite; a hidden token only on a search, as a favourite or in the Hidden view
    const visible = filterRows({ rows, favoriteRows, hiddenRows, hiddenIds: hiddenSet, heldIds: heldSet, filters, query });
    const byId = new Map(visible.map((r) => [r.covenantId, r]));
    const entries = visible.map((r) => (r.source === 'kascov' ? kascovEntry(r) : r.source === 'held' ? heldEntry(r) : tokenEntry(r, slotOf(r.covenantId), fiat, rate)));
    return sortEntries(entries, sort).map((e) => ({ entry: e, row: byId.get(e.key)! }));
  }, [rows, favoriteRows, hiddenRows, hiddenSet, heldSet, filters, query, sort, stats, indexer, fiat, rate]);
  const namelessCount = useMemo(() => rows.filter(isNameless).length, [rows]);
  const shownPairs = useMemo(() => {
    const slotOf = (id: string): Slot<StatsView> => stats.get(id) ?? (indexer ? LOADING : NONE);
    const byKey = new Map(listed.map((p) => [p.key, p]));
    const entries = filterPairs(listed, query).map((p) =>
      pairEntry(p, slotOf(p.base.covenantId), slotOf(p.quote.covenantId), volumes.get(p.key) ?? (indexer ? LOADING : NONE), depths.get(p.key) ?? (indexer ? LOADING : NONE)),
    );
    return sortEntries(entries, sort).map((e) => ({ entry: e, pair: byKey.get(e.key)! }));
  }, [listed, query, sort, stats, volumes, depths, indexer]);

  // the rows' places: taken from the sort when something the viewer sees as a new list happens, kept while figures arrive
  const [pageSize, setPageSize] = useListPageSize();
  const viewKey = `${sort.key}:${sort.dir}|${query}|${showNameless ? 1 : 0}|${favoritesOnly ? 1 : 0}|${hiddenOnly ? 1 : 0}|${filters.held ? 1 : 0}`;
  const placedBy = `${viewKey}|${generation}|${figuresIn ? 1 : 0}|${open ? 1 : 0}`;
  const placed = useRef<{ by: string; ids: string[] }>({ by: '', ids: [] });
  const sortedIds = shownRows.map((r) => r.row.covenantId);
  placed.current = { by: placedBy, ids: placed.current.by === placedBy ? keepOrder(placed.current.ids, sortedIds) : sortedIds };
  const rowOf = new Map(shownRows.map((r) => [r.row.covenantId, r]));
  // another sort, search or size starts at the first page
  const [turned, setTurned] = useListPage(`${viewKey}|${pageSize}`);
  const pager = pageWindow(placed.current.ids.length, pageSize, turned);
  const windowRows = placed.current.ids.slice(pager.from, pager.to).map((id) => rowOf.get(id)!);
  const listTop = useRef<HTMLDivElement>(null);
  // the placeholders are as tall as a page of rows: a page left scrolled down would otherwise open the list in its middle
  useEffect(() => window.scrollTo(0, 0), []);

  const onSort = (k: SortKey) => setSort((s) => nextSort(s, k));
  const turnTo = (page: number) => {
    setTurned(page);
    showListStart(listTop.current);
  };
  const onFavoriteToken = (id: string) => changeFavorites((f) => toggleToken(f, id));
  const onHideToken = (id: string) => {
    if (leaving.has(id)) return;
    const at = windowRows.findIndex((r) => r.row.covenantId === id);
    setLeaving((s) => new Set(s).add(id));
    window.setTimeout(() => {
      toggleHiddenToken(id);
      if (at >= 0) setAgain((a) => ({ from: at, n: a.n + 1 }));
      setLeaving((s) => {
        const next = new Set(s);
        next.delete(id);
        return next;
      });
    }, reducedMotion() ? 0 : ROW_LEAVE_MS);
  };
  const onFavoritePair = (p: ListedPair) => changeFavorites((f) => togglePair(f, p.base.covenantId, p.quote.covenantId));
  const refresh = () => {
    tokens.reload();
    pairs.reload();
    setGeneration((g) => g + 1);
  };
  const cols = TOKEN_FIGURES.length + 1;
  // The menu offers a filter only where it can change something: the wallet's tokens with a
  // wallet, the Hidden view with hidden tokens, the tokens without a name when there are some.
  // One that is on stays, so it can be switched off.
  const filterItems: FilterItem[] = [
    { key: 'favorites', label: t('market.favorites'), testid: 'token-list-favorites' },
    ...(connected || filters.held ? [{ key: 'held' as FilterKey, label: t('market.filter.held'), testid: 'token-list-held' }] : []),
    ...(hidden.length > 0 || hiddenOnly ? [{ key: 'hidden' as FilterKey, label: t('market.hidden.show', { n: hidden.length }), testid: 'token-list-hidden' }] : []),
    ...(namelessCount > 0 || showNameless ? [{ key: 'nameless' as FilterKey, label: t('market.filter.nameless', { n: namelessCount }), testid: 'token-list-nameless' }] : []),
  ];
  // the Favorites view with favourite pairs only has no empty token table above them
  const noTokenTable = favoritesOnly && shownRows.length === 0 && shownPairs.length > 0;

  return (
    <div class="stack" data-testid="token-list-view">
      {props.invalidToken ? <Banner tone="warn" data-testid="banner-bad-token">{t('market.badTokenId', { id: props.invalidToken })}</Banner> : null}
      <Section
        title={t('market.title')}
        actions={<Button small onClick={refresh} loading={tokens.loading && !!tokens.data}>{t('common.refresh')}</Button>}
        data-testid="token-list"
      >
        <div class="ml-tools" ref={listTop}>
          <div class="ml-search">
            <Field label={t('market.search.label')} type="search" value={query} onValue={setQuery} placeholder={t('market.search.placeholder')} data-testid="token-search" />
          </div>
          <ListFilterMenu filters={filters} items={filterItems} onToggle={(k) => setFilters((f) => toggleFilter(f, k))} onClear={() => setFilters(NO_FILTERS)} />
          <PairPicker rows={rows} />
          <RowsControl id="token-list" size={pageSize} onSize={setPageSize} />
        </div>
        <ListLegend names={kascov.status} />
        <FiatNote />
        <ErrorBanner error={tokens.error} title={t('market.list.indexerError')} onRetry={tokens.reload} data-testid="token-list-error" />
        {noTokenTable ? null : (
          <Table dense class="ml-table ml-tokens" data-testid="token-table">
            <thead>
              <tr>
                <SortHeader k="market" label={t('market.col.token')} sort={sort} onSort={onSort} />
                {TOKEN_FIGURES.map((f) => <SortHeader key={f.key} k={f.key} label={f.label(fiat ? 'USD' : 'KAS')} hint={f.hint ? t(f.hint) : undefined} sort={sort} onSort={onSort} right cls={f.cls} />)}
              </tr>
            </thead>
            <tbody>
              {!open ? (
                Array.from({ length: SKELETON_ROWS }, (_, i) => <SkeletonRow key={i} />)
              ) : shownRows.length === 0 ? (
                <TableMessage colSpan={cols} data-testid="token-list-empty">{query ? t('market.list.noMatch') : filters.held ? t('market.held.empty') : favoritesOnly ? t('market.favorites.empty') : hiddenOnly ? t('market.hidden.empty') : t('market.list.empty')}</TableMessage>
              ) : (
                windowRows.map(({ row, entry }, i) => (
                  <TokenTableRow
                    key={row.covenantId}
                    row={row}
                    entry={entry}
                    step={Math.min(i >= again.from ? i - again.from : i, REVEAL_STEPS)}
                    favorite={isFavoriteToken(favorites, row.covenantId)}
                    onFavorite={onFavoriteToken}
                    hidden={hiddenSet.has(row.covenantId.toLowerCase())}
                    onHide={onHideToken}
                    held={heldSet.has(row.covenantId.toLowerCase())}
                    leaving={leaving.has(row.covenantId)}
                    again={i >= again.from ? (again.n % 2 ? 'a' : 'b') : ''}
                  />
                ))
              )}
            </tbody>
          </Table>
        )}
        {open ? <ListPager id="token-list" window={pager} total={shownRows.length} hasNext={pager.page + 1 < pager.pages} onPage={turnTo} /> : null}
        {!indexer ? <p class="small muted">{t('market.list.noIndexer')}</p> : null}
        {kascov.status === 'unavailable' ? <p class="small muted" data-testid="kascov-unavailable">{t('market.list.kascovDown')}</p> : null}
        {open && shownPairs.length > 0 ? (
          <div class="ml-pairs" data-testid="favorite-pairs">
            <Table dense class="ml-table" data-testid="pair-table">
              <thead>
                <tr>
                  <SortHeader k="market" label={t('market.col.pair')} sort={sort} onSort={onSort} />
                  {PAIR_FIGURES.map((f) => <SortHeader key={f.key} k={f.key} label={f.label(null)} hint={f.hint ? t(f.hint) : undefined} sort={sort} onSort={onSort} right cls={f.cls} />)}
                </tr>
              </thead>
              <tbody>
                {shownPairs.map(({ pair, entry }) => <PairTableRow key={pair.key} pair={pair} entry={entry} onFavorite={onFavoritePair} />)}
              </tbody>
            </Table>
          </div>
        ) : null}
      </Section>
      <AddUnknownToken onFound={(v) => setPasted((p) => (p.some((x) => x.covenant_id === v.covenant_id) ? p : [...p, v]))} />
    </div>
  );
}
