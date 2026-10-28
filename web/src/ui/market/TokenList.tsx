import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { useDeclarationLookup } from '../../app/use-declaration';
import { navigate, pairRoute, routeToHash, tokenRoute } from '../../app/router';
import type { IndexerTokenView, StatsView } from '../../data/indexer-types';
import type { PairToken } from '../../kob/pair';
import { t } from '../../i18n';
import { Banner, Button, ErrorBanner, Field, Loading, Section, Table, TableMessage, toError, useAsync } from '../kit';
import { StatusBadges, TokenFacts } from './TokenBadges';
import { IdentityBlock } from './Fingerprint';
import { IdCopy } from './IdCopy';
import { ListLegend } from './ListLegend';
import { KascovNote } from './KascovNote';
import { mergeKascov } from './kascov-merge';
import { TokenIcon } from './TokenIcon';
import { TokenNote } from './TokenNote';
import './market.css';
import {
  ariaSort, DEFAULT_SORT, filterPairs, kascovEntry, listedPairs, LOADING, nextSort, NONE, pairEntry, pairLabelOf, sortEntries, tokenEntry,
  type Cell, type ListedPair, type ListEntry, type Slot, type SortKey, type SortState,
} from './market-list';
import { pairVolumeOf, type PairVolume } from './pair-market';
import { buildTokenRows, filterTokens, isNameless, parseCovenantId, tradableHere, tradableRows, type TokenRow } from './token-model';
import { useKascov } from './use-kascov';
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
  { key: 'last', label: (kas) => (kas ? t('market.col.lastIn', { quote: 'KAS' }) : t('market.col.last')), hint: 'market.col.lastHint', cls: 'ml-last' },
  { key: 'change', label: () => t('market.col.change'), hint: 'market.col.changeHint', cls: 'ml-change' },
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

function TokenTableRow({ row, entry }: { row: TokenRow; entry: ListEntry }) {
  // line 1: the ticker (bold) and the name; line 2: the covenant id shortened, with a copy button, and the badges. A token without a ticker is led by its name
  const headline = row.ticker || row.name || t('market.list.unnamed');
  const sub = row.ticker ? row.name : '';
  return (
    <tr data-testid={`token-row-${row.covenantId}`} data-source={row.source} data-tradable={row.tradable ? '1' : '0'} data-state={rowState(entry)}>
      <td>
        <div class="ml-name">
          <IdentityBlock row={row} />
          <div class="ml-name-text">
            <div class="ml-line1">
              <a href={`#/market/${row.covenantId}`} class="token-name" data-testid={`token-link-${row.covenantId}`}>
                {headline}
              </a>
              {sub || row.declared || row.kascov || row.source === 'kascov' ? (
                <span class="small muted ml-sub" data-testid={`token-name-${row.covenantId}`}>
                  {sub}
                  {row.declared && row.decimals !== null ? <TokenNote token={{ decimalsSource: 'declared', decimals: row.decimals }} /> : null}
                  <KascovNote row={row} compact />
                  {/* the orders column is hidden on a phone: the row says it here */}
                  {row.source === 'kascov' ? <span class="ml-only-narrow" data-testid={`token-no-orders-narrow-${row.covenantId}`}> · {t('market.list.noOrders')}</span> : null}
                </span>
              ) : null}
            </div>
            <div class="ml-line2">
              <IdCopy value={row.covenantId} data-testid={`token-hash-${row.covenantId}`} />
              <span class="ml-badges">
                <StatusBadges row={row} />
                <TokenFacts rows={[row]} inline data-testid={`token-facts-${row.covenantId}`} />
              </span>
            </div>
          </div>
        </div>
      </td>
      {FIGURES.map((f) => <Figure key={f.key} cell={entry.cells[f.key]} entry={entry} testid={`token-${f.key}-${row.covenantId}`} cls={f.cls} />)}
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
  const { registry, indexer, kob, config } = useServices();
  const tokens = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer]);
  const pairs = useAsync((signal) => (indexer ? indexer.pairs(undefined, { signal }) : Promise.resolve(null)), [indexer]);
  const [query, setQuery] = useState('');
  const [showNameless, setShowNameless] = useState(false);
  const [pasted, setPasted] = useState<IndexerTokenView[]>([]);
  const [sort, setSort] = useState<SortState>(DEFAULT_SORT);
  const [generation, setGeneration] = useState(0);

  // a token this build cannot trade is not listed (a direct link to it shows a short "not tradable" page)
  // a token the registry does not list is named by its verified KCC-23 declaration (its ticker, name and decimals)
  const declared = useDeclarationLookup();
  // the names, icons and tokens kascov.io adds (kascov-merge.ts): only tokens of a program this build trades on this network; none of its facts
  const kascov = useKascov();
  const kascovTokens = kascov.status === 'ok' ? kascov.tokens : null;
  const rows = useMemo(() => {
    const built = buildTokenRows(registry, tokens.data ?? null, pasted, declared);
    const templates = kob.templates();
    const known = new Set(built.map((r) => r.covenantId.toLowerCase()));
    return mergeKascov(tradableRows(built, templates), kascovTokens, { network: config.network, templates, registry, declared, known });
  }, [registry, tokens.data, pasted, kob, declared, kascovTokens, config.network]);
  const listed = useMemo(() => listedPairs(pairs.data, rows), [pairs.data, rows]);

  // the 24 h figures: one `/v1/stats` per listed token and one 24 h pair volume per listed pair, once the token list is in (a few calls at a time)
  const ready = !indexer || tokens.data !== undefined || tokens.error !== null;
  const statKeys = useMemo(() => (ready ? rows.filter((r) => r.source !== 'kascov').map((r) => r.covenantId) : null), [ready, rows]);
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
    const entries = filterTokens(visible, query).map((r) => (r.source === 'kascov' ? kascovEntry(r) : tokenEntry(r, slotOf(r.covenantId))));
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
  const cols = FIGURES.length + 1;

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
        <ListLegend />
        <ErrorBanner error={tokens.error} title={t('market.list.indexerError')} onRetry={tokens.reload} data-testid="token-list-error" />
        <Table dense class="ml-table ml-tokens" data-testid="token-table">
          <thead>
            <tr>
              <SortHeader k="market" label={t('market.col.token')} sort={sort} onSort={onSort} />
              {FIGURES.map((f) => <SortHeader key={f.key} k={f.key} label={f.label(true)} hint={f.hint ? t(f.hint) : undefined} sort={sort} onSort={onSort} right cls={f.cls} />)}
            </tr>
          </thead>
          <tbody>
            {tokens.loading && !tokens.data && indexer && rows.length === 0 ? (
              <TableMessage colSpan={cols}><Loading /></TableMessage>
            ) : shownRows.length === 0 ? (
              <TableMessage colSpan={cols} data-testid="token-list-empty">{query ? t('market.list.noMatch') : t('market.list.empty')}</TableMessage>
            ) : (
              shownRows.map(({ row, entry }) => <TokenTableRow key={row.covenantId} row={row} entry={entry} />)
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
        {kascov.status === 'unavailable' ? <p class="small muted" data-testid="kascov-unavailable">{t('market.list.kascovDown')}</p> : null}
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
