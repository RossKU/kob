import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { navigate, pairRoute, routeToHash, tokenRoute } from '../../app/router';
import type { IndexerTokenView, StatsView } from '../../data/indexer-types';
import type { PairToken } from '../../kob/pair';
import { t } from '../../i18n';
import { Banner, Button, CopyText, ErrorBanner, Field, Loading, Section, Table, TableMessage, toError, useAsync } from '../kit';
import { SharedTickerNote, StatusBadges, tokenTitle } from './TokenBadges';
import './market.css';
import {
  ariaSort, DEFAULT_SORT, filterPairs, listedPairs, LOADING, nextSort, NONE, pairEntry, pairLabelOf, sortEntries, tokenEntry,
  type Cell, type ListedPair, type ListEntry, type Slot, type SortKey, type SortState,
} from './market-list';
import { pairVolumeOf, type PairVolume } from './pair-market';
import { buildTokenRows, filterTokens, isNameless, parseCovenantId, sharedTickerIds, tradableHere, tradableRows, type TokenRow } from './token-model';
import { usePooledSlots } from './use-market-list';

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
      else if (!tradableHere({ source: 'indexer', info: null, index: found, delisted: found.standing === 'delisted' }, kob.templates())) setMessage({ tone: 'error', text: t('market.add.untradable') });
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
        <span class={c.tone ? `tone-${c.tone}` : undefined}>{c.text}</span>
      ) : props.entry.loading ? (
        <span class="skeleton skeleton-line ml-skel" role="status" aria-label={t('common.loading')} />
      ) : (
        '—'
      )}
    </td>
  );
}

/** The figure columns of both tables, in order: key, header, the class that hides it on a narrow screen. */
const FIGURES: { key: Exclude<SortKey, 'market'>; label: (kas: boolean) => string; hint?: string; cls?: string }[] = [
  { key: 'last', label: (kas) => (kas ? t('market.col.lastIn', { quote: 'KAS' }) : t('market.col.last')), hint: 'market.col.lastHint' },
  { key: 'change', label: () => t('market.col.change'), hint: 'market.col.changeHint' },
  { key: 'volume', label: (kas) => (kas ? t('market.col.volumeIn', { quote: 'KAS' }) : t('market.col.volume')), cls: 'ml-md' },
  { key: 'spread', label: () => t('market.col.spread'), cls: 'ml-md' },
  { key: 'orders', label: () => t('market.col.orders'), cls: 'ml-md' },
];

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

function TokenTableRow({ row, entry, shared }: { row: TokenRow; entry: ListEntry; shared: boolean }) {
  return (
    <tr data-testid={`token-row-${row.covenantId}`} data-source={row.source} data-tradable={row.tradable ? '1' : '0'} data-state={rowState(entry)}>
      <td>
        <a href={`#/market/${row.covenantId}`} class="token-name" data-testid={`token-link-${row.covenantId}`}>
          {tokenTitle(row)}
        </a>
        {row.name ? <div class="small muted">{row.name}</div> : null}
        <div class="ml-badges"><StatusBadges row={row} /></div>
        <SharedTickerNote shared={shared} data-testid={`shared-ticker-${row.covenantId}`} />
      </td>
      {FIGURES.map((f) => <Figure key={f.key} cell={entry.cells[f.key]} entry={entry} testid={`token-${f.key}-${row.covenantId}`} cls={f.cls} />)}
      <td class="token-hash-cell ml-lg">
        <CopyText value={row.covenantId} short={false} class="token-hash" data-testid={`token-hash-${row.covenantId}`} />
      </td>
    </tr>
  );
}

function PairTableRow({ pair, entry }: { pair: ListedPair; entry: ListEntry }) {
  const id = `${pair.base.covenantId}-${pair.quote.covenantId}`;
  return (
    <tr data-testid={`pair-row-${id}`} data-state={rowState(entry)}>
      <td>
        <a href={routeToHash(pairRoute(pair.base.covenantId, pair.quote.covenantId))} class="token-name" data-testid={`pair-link-${id}`}>
          {pairLabelOf(pair)}
        </a>
      </td>
      {FIGURES.map((f) => <Figure key={f.key} cell={entry.cells[f.key]} entry={entry} testid={`pair-${f.key}-${id}`} cls={f.cls} />)}
    </tr>
  );
}

const pairToken = (r: TokenRow): PairToken => ({ covenantId: r.covenantId, ticker: r.ticker, decimals: r.decimals ?? 0, scale: r.scale ?? 1n });

/**
 * Market list: every token this build can trade as its KAS market (TOKEN/KAS: last price, 24 h change, 24 h KAS volume, spread, open orders; the
 * token's badges and its full token hash), and the token pairs with live pair orders (BASE/QUOTE, the same columns); sortable by every column (24 h
 * volume first), searchable, add by covenant id. A phone shows the name, the price and the change.
 */
export function TokenList(props: { invalidToken?: string }) {
  const { registry, indexer, kob } = useServices();
  const tokens = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer]);
  const pairs = useAsync((signal) => (indexer ? indexer.pairs(undefined, { signal }) : Promise.resolve(null)), [indexer]);
  const [query, setQuery] = useState('');
  const [showNameless, setShowNameless] = useState(false);
  const [pasted, setPasted] = useState<IndexerTokenView[]>([]);
  const [sort, setSort] = useState<SortState>(DEFAULT_SORT);
  const [generation, setGeneration] = useState(0);

  // a token this build cannot trade is not listed (a direct link to it shows a short "not tradable" page)
  const rows = useMemo(() => tradableRows(buildTokenRows(registry, tokens.data ?? null, pasted), kob.templates()), [registry, tokens.data, pasted, kob]);
  const shared = useMemo(() => sharedTickerIds(rows), [rows]);
  const listed = useMemo(() => listedPairs(pairs.data, rows), [pairs.data, rows]);

  // the 24 h figures: one `/v1/stats` per listed token and one 24 h pair volume per listed pair, once the token list is in (a few calls at a time)
  const ready = !indexer || tokens.data !== undefined || tokens.error !== null;
  const statKeys = useMemo(() => (ready ? rows.map((r) => r.covenantId) : null), [ready, rows]);
  const loadStats = useMemo(() => (indexer ? (id: string, signal: AbortSignal) => indexer.stats(id, { signal }) : null), [indexer]);
  const stats = usePooledSlots(statKeys, loadStats, generation);
  const pairKeys = useMemo(() => (pairs.data !== undefined || pairs.error ? listed.map((p) => p.key) : null), [pairs.data, pairs.error, listed]);
  const loadVolume = useMemo(() => {
    if (!indexer) return null;
    const byKey = new Map(listed.map((p) => [p.key, p]));
    return async (key: string, signal: AbortSignal): Promise<PairVolume | null> => {
      const p = byKey.get(key);
      if (!p) return null;
      const view = await indexer.pairFills(p.base.covenantId, p.quote.covenantId, { limit: 1 }, { signal });
      return pairVolumeOf(view, pairToken(p.base), pairToken(p.quote));
    };
  }, [indexer, listed]);
  const volumes = usePooledSlots(pairKeys, loadVolume, generation);

  const shownRows = useMemo(() => {
    const slotOf = (id: string): Slot<StatsView> => stats.get(id) ?? (indexer ? LOADING : NONE);
    const byId = new Map(rows.map((r) => [r.covenantId, r]));
    // tokens without metadata only on a search or on request
    const visible = query.trim() || showNameless ? rows : rows.filter((r) => !isNameless(r));
    const entries = filterTokens(visible, query).map((r) => tokenEntry(r, slotOf(r.covenantId)));
    return sortEntries(entries, sort).map((e) => ({ entry: e, row: byId.get(e.key)! }));
  }, [rows, query, sort, stats, indexer, showNameless]);
  const namelessCount = useMemo(() => rows.filter(isNameless).length, [rows]);
  const shownPairs = useMemo(() => {
    const slotOf = (id: string): Slot<StatsView> => stats.get(id) ?? (indexer ? LOADING : NONE);
    const byKey = new Map(listed.map((p) => [p.key, p]));
    const entries = filterPairs(listed, query).map((p) =>
      pairEntry(p, slotOf(p.base.covenantId), slotOf(p.quote.covenantId), volumes.get(p.key) ?? (indexer ? LOADING : NONE)),
    );
    return sortEntries(entries, sort).map((e) => ({ entry: e, pair: byKey.get(e.key)! }));
  }, [listed, query, sort, stats, volumes, indexer]);

  const onSort = (k: SortKey) => setSort((s) => nextSort(s, k));
  const refresh = () => {
    tokens.reload();
    pairs.reload();
    setGeneration((g) => g + 1);
  };
  const cols = FIGURES.length + 2;

  return (
    <div class="stack" data-testid="token-list-view">
      {props.invalidToken ? <Banner tone="warn" data-testid="banner-bad-token">{t('market.badTokenId', { id: props.invalidToken })}</Banner> : null}
      <Section
        title={t('market.title')}
        actions={<Button small onClick={refresh} loading={tokens.loading && !!tokens.data}>{t('common.refresh')}</Button>}
        data-testid="token-list"
      >
        <div style="max-width:420px;margin-bottom:12px">
          <Field label={t('market.search.label')} type="search" value={query} onValue={setQuery} placeholder={t('market.search.placeholder')} data-testid="token-search" />
        </div>
        <ErrorBanner error={tokens.error} title={t('market.list.indexerError')} onRetry={tokens.reload} data-testid="token-list-error" />
        <Table dense class="ml-table" data-testid="token-table">
          <thead>
            <tr>
              <SortHeader k="market" label={t('market.col.token')} sort={sort} onSort={onSort} />
              {FIGURES.map((f) => <SortHeader key={f.key} k={f.key} label={f.label(true)} hint={f.hint ? t(f.hint) : undefined} sort={sort} onSort={onSort} right cls={f.cls} />)}
              <th class="ml-lg">{t('market.col.hash')}</th>
            </tr>
          </thead>
          <tbody>
            {tokens.loading && !tokens.data && indexer && rows.length === 0 ? (
              <TableMessage colSpan={cols}><Loading /></TableMessage>
            ) : shownRows.length === 0 ? (
              <TableMessage colSpan={cols} data-testid="token-list-empty">{query ? t('market.list.noMatch') : t('market.list.empty')}</TableMessage>
            ) : (
              shownRows.map(({ row, entry }) => <TokenTableRow key={row.covenantId} row={row} entry={entry} shared={shared.has(row.covenantId)} />)
            )}
          </tbody>
        </Table>
        {namelessCount > 0 && !query.trim() ? (
          <p class="small muted">
            <Button small onClick={() => setShowNameless((v) => !v)} data-testid="token-list-nameless">
              {t(showNameless ? 'market.list.hideNameless' : 'market.list.showNameless', { n: namelessCount })}
            </Button>
          </p>
        ) : null}
        {!indexer ? <p class="small muted">{t('market.list.noIndexer')}</p> : null}
        {listed.length > 0 ? (
          <div class="ml-pairs" data-testid="pair-list">
            <h3 class="ml-subtitle">{t('market.pairs.title')}</h3>
            <Table dense class="ml-table" data-testid="pair-table">
              <thead>
                <tr>
                  <SortHeader k="market" label={t('market.col.pair')} sort={sort} onSort={onSort} />
                  {FIGURES.map((f) => <SortHeader key={f.key} k={f.key} label={f.label(false)} hint={f.hint ? t(f.hint) : undefined} sort={sort} onSort={onSort} right cls={f.cls} />)}
                </tr>
              </thead>
              <tbody>
                {shownPairs.length === 0 ? (
                  <TableMessage colSpan={FIGURES.length + 1} data-testid="pair-list-empty">{t('market.pairs.noMatch')}</TableMessage>
                ) : (
                  shownPairs.map(({ pair, entry }) => <PairTableRow key={pair.key} pair={pair} entry={entry} />)
                )}
              </tbody>
            </Table>
          </div>
        ) : null}
      </Section>
      <AddUnknownToken onFound={(v) => setPasted((p) => (p.some((x) => x.covenant_id === v.covenant_id) ? p : [...p, v]))} />
    </div>
  );
}
