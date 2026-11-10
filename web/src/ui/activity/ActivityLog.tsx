// The activity log (kaspa.stream's transaction list, for KOB): one row per transaction, newest first, live. The SAME component on the Activity page
// (#/activity, every market) and on the token and pair pages (that market; "All activity →" opens the Activity page).
//
// Every row is a real link to the transaction on the network's explorer (new tab, `noopener noreferrer`), so a row must never move under the pointer:
// while the list is paused, pointed at or focused, new rows are held (`heldView`) and counted in a "+N new" pill that releases them.
//
// A page never shows more than the chosen number of rows, and the pages reach ACTIVITY_DEPTH rows back: a new row pushes the oldest off. An older page is held too, so
// its rows do not slide into the next one; the pill leads back to the first page.
import { memo } from 'preact/compat';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { displayToken } from '../../app/token-display';
import { useDeclarationLookup } from '../../app/use-declaration';
import { explorerTxUrl } from '../../config';
import { pubkeyToAddress } from '../../data/kaspa-sdk';
import type { PairFillsView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { longId } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { Badge, ErrorBanner, Section, SortButton, SortControl, ariaSortOf, scaledUnits, sortBy, useSortState, type SortColumn, type SortValue } from '../kit';
import { ListPager, RowsControl, showListStart } from '../market/ListPaging';
import { pageWindow, type ListPageSize } from '../market/market-list';
import { useHeldUntil, useListPage, useListPageSize, useRising } from '../market/use-market-list';
import { Popover } from '../shell/Popover';
import { clockText } from '../shell/standalone';
import {
  ACTIVITY_DEPTH, ALL_KINDS, confirmationsAt, AVAILABLE_KINDS, PAGE_ROWS, logHasNext, logPage, rowHref, SETTLE_DAA, filterRows, heldView, marketKeyOf, readPrefs, rowMakers, shownKinds, writePrefs,
  type ActivityKind, type ActivityPrefs, type ActivityRow, type ActorFilter, type MarketFilter, type MarketRef, type PrefsScope,
} from './activity-model';
import { KindGlyph, KindIcon, kindLabel } from './ActivityIcons';
import { useFiat } from '../../app/use-fiat';
import { ActivityRowView, RowsSkeleton, type RowFormat } from './ActivityRows';
import { FIRST_LIMIT } from './activity-store';
import { useActivity } from './use-activity';
import '../market/market.css';
import './activity.css';

const addressCache = new Map<string, string | null>();

const ACTIVITY_SORT_KEYS = ['kind', 'market', 'tx', 'wallet', 'amount', 'price', 'conf', 'time'] as const;
type ActivitySortKey = (typeof ACTIVITY_SORT_KEYS)[number];

/** The columns of the log, in its order; `time` is its own order (newest first). */
function activityColumns(usd: boolean): SortColumn<ActivitySortKey>[] {
  return [
    { key: 'kind', label: t('activity.col.kind'), kind: 'text' },
    { key: 'market', label: t('activity.col.market'), kind: 'text' },
    { key: 'tx', label: t('activity.col.tx'), kind: 'text' },
    { key: 'wallet', label: t('activity.col.wallet'), kind: 'text' },
    { key: 'amount', label: t('activity.col.amount'), kind: 'number' },
    { key: 'price', label: t(usd ? 'activity.col.priceUsd' : 'activity.col.price'), kind: 'number' },
    { key: 'conf', label: t('activity.col.conf'), kind: 'number' },
    { key: 'time', label: t('activity.col.time'), kind: 'time' },
  ];
}

/** Reads further back for one page at most: rows the filters hide must not read the whole history. */
const OLDER_READS = 4;

/** The placeholder rows while the first rows are held. */
const SKELETON_ROWS = 8;

export interface ActivityLogProps {
  /** the market shown: every market on the Activity page, the page's own on a token or pair page */
  market: MarketFilter;
  /** which stored filters apply: the Activity page's, or the one record of the token and pair pages */
  prefsScope: PrefsScope;
  /** the pair page's own copy of the pair's fills */
  pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null;
  /** a link to the Activity page (on the token and pair pages) */
  allLink?: boolean;
  /** the rows of a page until the viewer picks another size (default PAGE_ROWS) */
  pageRows?: ListPageSize;
  /** read the first rows at once, also below the screen (the Activity page, whose summary can push the log below a phone's screen) */
  eager?: boolean;
  'data-testid'?: string;
}

function ActivityLogView(props: ActivityLogProps) {
  const services = useServices();
  const { config, registry, sdk } = services;
  const wallet = useWallet();
  const me = wallet.info?.pubkey?.toLowerCase() ?? null;
  const [prefs, setPrefsState] = useState<ActivityPrefs>(() => readPrefs(props.prefsScope));
  const setPrefs = (p: ActivityPrefs) => {
    setPrefsState(p);
    writePrefs(props.prefsScope, p);
  };
  const kinds = useMemo(() => shownKinds(prefs), [prefs]);
  const listRef = useRef<HTMLDivElement>(null);
  const [onScreen, setOnScreen] = useState(() => typeof IntersectionObserver === 'undefined');
  useEffect(() => {
    const el = listRef.current;
    if (!el || typeof IntersectionObserver === 'undefined') return;
    const io = new IntersectionObserver((entries) => setOnScreen(entries.some((e) => e.isIntersecting)), { rootMargin: '200px 0px' });
    io.observe(el);
    return () => io.disconnect();
  }, []);
  // the reads of the first rows and their owners: while the list is on screen, or at once with `eager`
  const reading = onScreen || !!props.eager;
  const data = useActivity({ market: props.market, kinds, pairFills: props.pairFills ?? null, onScreen: reading });

  // the names and decimals of tokens the registry does not have arrive with the token list: rows read them again then
  const declared = useDeclarationLookup();
  const { nameOf, decimalsOf } = useMemo(
    () => ({
      nameOf: (token: Hex): string => displayToken(registry, config.network, token)?.ticker || longId(token),
      decimalsOf: (token: Hex): number => displayToken(registry, config.network, token)?.decimals ?? 8,
    }),
    [registry, config.network, declared],
  );
  const addressOf = (pk: Hex): string | null => {
    const k = `${config.network}:${pk}`;
    if (!addressCache.has(k)) {
      try {
        addressCache.set(k, pubkeyToAddress(sdk, pk, config.network));
      } catch {
        addressCache.set(k, null);
      }
    }
    return addressCache.get(k)!;
  };

  // the USD display (Settings): the KAS value line and the price of a KAS-market row in dollars; null = KAS
  const fiat = useFiat();
  // how the rows name things: one object per registry and network, so the memoized rows see equal props between renders
  const fmt = useMemo<RowFormat>(() => {
    let explorerHost = 'the explorer';
    try {
      explorerHost = new URL(explorerTxUrl(config, '0'.repeat(64)) ?? '').host;
    } catch {
      /* no explorer configured */
    }
    return {
      nameOf,
      decimalsOf,
      marketText: (m: MarketRef) => (m.kind === 'token' ? t('activity.market.kas', { ticker: nameOf(m.token) }) : t('activity.market.pair', { base: nameOf(m.base), quote: nameOf(m.quote) })),
      explorerHost,
      fiat,
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [registry, config, declared, fiat]);

  /** What a column compares for a row (the amounts as tokens, the price at its low end, the time in milliseconds). */
  const activityValue = (r: ActivityRow, key: ActivitySortKey): SortValue => {
    switch (key) {
      case 'kind':
        return kindLabel(r.kind, r.side);
      case 'market':
        return fmt.marketText(r.market);
      case 'tx':
        return r.txid;
      case 'wallet': {
        const first = rowMakers(r, data)[0];
        return first ? (addressOf(first) ?? longId(first)) : null;
      }
      case 'amount':
        return r.amount ? scaledUnits(r.amount.units, decimalsOf(r.amount.token)) : null;
      case 'price':
        // KAS per whole token, the low end of a ladder's range; a pair row has none
        return r.price && r.price.scale > 0n && r.market.kind === 'token' ? (r.price.low * 10n ** BigInt(decimalsOf(r.market.token) + 8)) / r.price.scale : null;
      case 'conf':
        return confirmationsAt(r, data.chain?.daa ?? null).n;
      case 'time':
        return r.timeMs;
    }
  };
  const all = filterRows(data.rows, { prefs, market: props.market, mine: me, makersOf: (r) => rowMakers(r, data) });

  // ---- the hold: paused, or the pointer or the focus in the list
  const [paused, setPaused] = useState(false);
  const [hover, setHover] = useState(false);
  const [focusIn, setFocusIn] = useState(false);
  const [size, setSize] = useListPageSize(`kob.activity.rows.${props.prefsScope}`, props.pageRows ?? PAGE_ROWS);
  // the viewer's sort of the rows within reach (none: newest first), kept per log in this browser
  const [sort, setSort] = useSortState<ActivitySortKey>(`activity.${props.prefsScope}`, ACTIVITY_SORT_KEYS);
  const filterKey = `${marketKeyOf(props.market)}|${[...kinds].join(',')}|${prefs.mine ? 'mine' : ''}`;
  const sortKey = sort ? `${sort.key}:${sort.dir}` : '-';
  // other filters or another size start at the first page
  const [asked, setAsked] = useListPage(`${filterKey}|${size}|${sortKey}`);
  const held = paused || hover || focusIn || asked > 0;
  const [, setReleased] = useState(0);
  // what the hold keeps in place is every row within reach, not one page: the rows of the other pages are no new rows
  const snapshot = useRef<Set<string> | null>(null);
  const keptBefore = useRef<ActivityRow[]>([]);
  const lastFilter = useRef(filterKey);
  if (!held) snapshot.current = null;
  else if (snapshot.current === null || lastFilter.current !== filterKey) snapshot.current = new Set((lastFilter.current === filterKey ? keptBefore.current : all.slice(0, ACTIVITY_DEPTH)).map((r) => r.key));
  lastFilter.current = filterKey;
  const view = heldView(all, snapshot.current);
  const byTime = logPage(view.rows, size, asked);
  // the hold is about the newest rows, in whatever order they are shown; a sorted log pages the same rows by the viewer's column (a row that arrives takes its place in it)
  keptBefore.current = byTime.kept;
  const cols = activityColumns(!!fiat);
  const ownCol = cols[cols.length - 1]!;
  const kept = sort ? sortBy(byTime.kept, sort, (r, key) => activityValue(r, key)) : byTime.kept;
  const pager = pageWindow(kept.length, size, asked);
  const shown = kept.slice(pager.from, pager.to);
  const release = () => {
    setPaused(false);
    setAsked(0);
    snapshot.current = hover || focusIn ? new Set(all.slice(0, ACTIVITY_DEPTH).map((r) => r.key)) : null;
    setReleased((n) => n + 1);
  };
  const turnTo = (page: number) => {
    setAsked(page);
    showListStart(listRef.current);
  };

  // the rows the asked page needs, read further back while they are not there (the first read gives about 50)
  const want = Math.min((asked + 1) * size, ACTIVITY_DEPTH);
  const olderReads = useRef({ want, n: 0 });
  useEffect(() => {
    const r = olderReads.current;
    if (r.want !== want) olderReads.current = { want, n: 0 };
    // a market page's first page shows what the first read gave: its ticket's reads come first, older rows wait until more rows are asked for
    if (props.prefsScope === 'market' && want <= FIRST_LIMIT) return;
    if (!onScreen || !data.hasOlder || data.loading || data.loadingOlder || view.rows.length >= want || olderReads.current.n >= OLDER_READS) return;
    olderReads.current.n++;
    data.loadOlder();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [want, onScreen, data.hasOlder, data.loading, data.loadingOlder, view.rows.length]);

  // rows that arrive after the first rows flash once; a page turned to, or older rows read in below the known ones, do not
  const seen = useRef<Set<string> | null>(null);
  const fresh = new Set<string>();
  if (byTime.kept.length) {
    const had = seen.current;
    if (had) {
      const lastKnown = byTime.kept.reduce((last, r, i) => (had.has(r.key) ? i : last), -1);
      for (const r of byTime.kept.slice(0, lastKnown)) if (!had.has(r.key)) fresh.add(r.key);
    }
    seen.current = new Set([...(had ?? []), ...byTime.kept.map((r) => r.key)]);
  }

  // the Wallet column: the owners of the rows' orders are looked up only while the list is read (on screen, or `eager`; one at a time: the order ticket's reads of
  // a slow indexer must not queue behind them)
  const wanted = reading ? shown.flatMap((r) => r.orders).join(',') : '';
  useEffect(() => {
    if (wanted) data.want(wanted.split(',') as Hex[]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wanted]);

  // the first rows come together, with their owners: the first read (a pair: the page's fills too) and the lookups of the page, at most LIST_HOLD_MS after the list is on
  // screen. From then on a row is live and comes at once
  const pairPending = !!props.pairFills && props.pairFills.view === undefined;
  const firstIn = !data.firstRead && !pairPending && shown.every((r) => r.orders.every((id) => data.order(id) !== undefined));
  const open = useHeldUntil(firstIn, reading);
  const rising = useRising(open);
  const rowsShown = open && shown.length > 0;

  const today = new Date().toDateString();
  const liveKind = data.mode === 'standalone' ? 'standalone' : data.mode === 'off' ? 'off' : held ? 'held' : data.mode;

  // ---- the header's tools
  const toggleKind = (k: ActivityKind) => setPrefs({ ...prefs, kinds: { ...prefs.kinds, [k]: !prefs.kinds[k] } });
  const setActor = (a: ActorFilter) => setPrefs({ ...prefs, actor: a });

  const kindToggles = AVAILABLE_KINDS.map((k) => {
    const what = t(`activity.toggle.${k}`);
    const on = prefs.kinds[k];
    return (
      <button
        key={k}
        type="button"
        class={`act-ib act-ib-${k}`}
        aria-pressed={on ? 'true' : 'false'}
        title={t(on ? 'activity.toggle.hide' : 'activity.toggle.show', { what })}
        aria-label={what}
        data-testid={`activity-kind-${k}`}
        onClick={() => toggleKind(k)}
      >
        <KindGlyph kind={k} label={null} />
      </button>
    );
  });

  const tools = (
    <div class="act-tools">
      <div class="act-seg" role="group" aria-label={t('activity.actor.label')} data-testid="activity-actor">
        {(['all', 'orders', 'executions'] as const).map((a) => (
          <button
            key={a}
            type="button"
            aria-pressed={prefs.actor === a ? 'true' : 'false'}
            data-testid={`activity-actor-${a}`}
            {...(a !== 'all' ? { title: t(`activity.actor.${a}Title`) } : {})}
            onClick={() => setActor(a)}
          >
            {t(`activity.actor.${a}`)}
          </button>
        ))}
      </div>
      <span class="act-kinds">{kindToggles}</span>
      <button
        type="button"
        class="act-ib act-mine"
        aria-pressed={prefs.mine && !!me ? 'true' : 'false'}
        disabled={!me}
        title={me ? t('activity.mineTitle') : t('activity.mineNoWallet')}
        aria-label={t('activity.mine')}
        data-testid="activity-mine"
        onClick={() => setPrefs({ ...prefs, mine: !prefs.mine })}
      >
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
          <circle cx="12" cy="8" r="4" />
          <path d="M4.5 20.5a7.5 7.5 0 0 1 15 0" />
        </svg>
      </button>
      <Popover label={t('activity.vis.label')} trigger={<span class="act-vis">{t('activity.vis')} ▾</span>} triggerAttrs={{ 'data-testid': 'activity-vis' }} panelTestId="activity-vis-panel">
        <div class="act-vis-panel">
          <h4>{t('activity.vis.kinds')}</h4>
          {AVAILABLE_KINDS.map((k) => (
            <label key={k} class="act-vis-row">
              <input type="checkbox" checked={prefs.kinds[k]} onChange={() => toggleKind(k)} data-testid={`activity-vis-${k}`} />
              <KindIcon kind={k} small />
              {t(`activity.toggle.${k}`)}
            </label>
          ))}
          <h4>{t('activity.vis.who')}</h4>
          <label class="act-vis-row">
            <input type="checkbox" checked={prefs.mine && !!me} disabled={!me} onChange={() => setPrefs({ ...prefs, mine: !prefs.mine })} data-testid="activity-vis-mine" />
            {me ? t('activity.mine') : t('activity.mineNoWallet')}
          </label>
          {/* PHASE 2: cancels, refunds / kills / expiries and stop arms join the list (and these toggles) once the indexer lists them across orders */}
          <p class="small muted act-vis-note">{t('activity.vis.phase2')}</p>
        </div>
      </Popover>
      <RowsControl id="activity" size={size} onSize={setSize} />
      {/* always in the row (hidden while nothing is held), so the tools never move when it comes and goes */}
      <button
        type="button"
        class={`act-new${view.held > 0 ? '' : ' act-new-off'}`}
        onClick={release}
        title={t('activity.newRowsTitle')}
        tabIndex={view.held > 0 ? 0 : -1}
        aria-hidden={view.held > 0 ? undefined : 'true'}
        {...(view.held > 0 ? { 'data-testid': 'activity-new' } : {})}
        {...(paused ? { 'aria-live': 'polite' } : {})}
      >
        {t('activity.newRows', { n: view.held })}
      </button>
      <button
        type="button"
        class="act-ib"
        aria-pressed={paused ? 'true' : 'false'}
        aria-label={t(paused ? 'activity.resume' : 'activity.pause')}
        title={t(paused ? 'activity.resume' : 'activity.pause')}
        data-testid="activity-pause"
        onClick={() => (paused ? release() : setPaused(true))}
      >
        {paused ? (
          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="currentColor"><path d="M7 4.5v15l12-7.5z" /></svg>
        ) : (
          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="currentColor"><rect x="6" y="4.5" width="4" height="15" rx="1" /><rect x="14" y="4.5" width="4" height="15" rx="1" /></svg>
        )}
      </button>
    </div>
  );

  const title = (
    <span class="act-title">
      <span class={`act-dot act-dot-${liveKind}`} title={t(`activity.live.${liveKind}Hint`)} data-testid="activity-live" data-mode={liveKind} role="img" aria-label={t(`activity.live.${liveKind}`)} />
      {t('activity.title')}
      <span class="act-scope muted">{props.market.kind === 'all' ? t('activity.allMarkets') : fmt.marketText(props.market)}</span>
      {data.sinceMs !== null ? (
        data.keptUntilMs !== null ? (
          <Badge tone="info" title={t('activity.keptHint')} data-testid="activity-since" data-kept="1">
            {t('activity.kept', { until: clockText(data.keptUntilMs), since: clockText(data.sinceMs) })}
          </Badge>
        ) : (
          <Badge tone="info" title={t('activity.sinceHint')} data-testid="activity-since">
            {t('activity.since', { time: clockText(data.sinceMs) })}
          </Badge>
        )
      ) : null}
    </span>
  );

  const legendKinds = ALL_KINDS.filter((k) => AVAILABLE_KINDS.includes(k));
  return (
    <Section title={title} actions={tools} class="act" data-testid={props['data-testid'] ?? 'activity'}>
      <ErrorBanner error={data.rows.length ? null : data.error} title={t('activity.error')} onRetry={data.retry} data-testid="activity-error" />
      <div class="act-log" ref={listRef} role="table" aria-label={t('activity.tableLabel')} data-testid="activity-list" data-count={rowsShown ? shown.length : 0} data-held={held ? 'true' : 'false'}>
        {/* where the log is two lines per row (no header), the same sort is a control above the rows */}
        {rowsShown && kept.length > 1 ? <SortControl id="activity" class="act-sortctl" columns={cols} state={sort} own={ownCol} onSort={setSort} /> : null}
        <div class="act-head" role="row">
          {cols.map((c) => (
            <span key={c.key} role="columnheader" aria-sort={ariaSortOf(sort, c.key, ownCol)} class={c.key === 'amount' || c.key === 'price' || c.key === 'conf' || c.key === 'time' ? 'act-num' : undefined}>
              <SortButton id="activity" col={c} state={sort} own={ownCol} onSort={setSort}>{c.key === 'kind' ? <span aria-hidden="true">⇅</span> : undefined}</SortButton>
            </span>
          ))}
        </div>
        <div
          class={`act-body${rowsShown ? ' act-rows' : ''}`}
          style={rowsShown ? `--n:${shown.length}` : undefined}
          role="rowgroup"
          // a pointer that MOVES in the list holds it (a list that slid under a resting pointer does not)
          onPointerMove={(e) => rowsShown && !hover && (e as PointerEvent).pointerType !== 'touch' && setHover(true)}
          onPointerLeave={() => setHover(false)}
          onFocusIn={() => setFocusIn(true)}
          onFocusOut={(e) => {
            const next = (e as FocusEvent).relatedTarget as Node | null;
            if (!next || !(e.currentTarget as HTMLElement).contains(next)) setFocusIn(false);
          }}
        >
          {rowsShown ? (
            shown.map((r, i) => {
              const makers = rowMakers(r, data);
              return (
                <ActivityRowView
                  key={r.key}
                  row={r}
                  i={i}
                  fresh={fresh.has(r.key)}
                  rise={rising && !fresh.has(r.key)}
                  href={rowHref(config, r)}
                  addrs={makers.map((pk) => addressOf(pk) ?? longId(pk)).join('\n')}
                  pending={r.orders.some((id) => data.order(id) === undefined)}
                  chainDaa={data.chain?.daa ?? null}
                  chainAt={data.chain?.atMs ?? null}
                  today={today}
                  fmt={fmt}
                />
              );
            })
          ) : !open || data.loading || pairPending ? (
            <RowsSkeleton rows={SKELETON_ROWS} />
          ) : data.error ? null : (
            <div class="act-empty muted" data-testid="activity-empty">{data.rows.length ? t('activity.emptyFiltered') : t('activity.empty')}</div>
          )}
        </div>
      </div>
      <div class="act-foot">
        {open ? <ListPager id="activity" window={pager} total={kept.length} hasNext={logHasNext(pager, size, asked, data.hasOlder)} loading={data.loadingOlder} onPage={turnTo} /> : null}
        <span class="act-foot-r">
          {props.allLink ? (
            <a href="#/activity" class="small" title={t('activity.allActivityTitle')} data-testid="activity-all-link">
              {t('activity.allActivity')}
            </a>
          ) : null}
          {data.sinceMs !== null ? (
            <span class="small muted" data-testid="activity-nothing-before">{data.keptUntilMs !== null ? t('activity.nothingBeforeKept') : t('activity.nothingBefore', { time: clockText(data.sinceMs) })}</span>
          ) : null}
        </span>
      </div>
      <div class="act-legend" aria-label={t('activity.legend')} data-testid="activity-legend">
        {legendKinds.flatMap((k) =>
          k === 'fill'
            ? [
                <span key="fb"><KindIcon kind="fill" side="buy" small />{t('activity.legend.fillBuy')}</span>,
                <span key="fs"><KindIcon kind="fill" side="sell" small />{t('activity.legend.fillSell')}</span>,
              ]
            : [<span key={k}><KindIcon kind={k} small />{t(`activity.legend.${k}`)}</span>],
        )}
        <span><i class="act-pie" style="--p:40" aria-hidden="true" />{t('activity.legend.confirming', { total: SETTLE_DAA })}</span>
        <span><span class="act-ok" aria-hidden="true">{'✓'}</span>{t('activity.legend.settled')}</span>
      </div>
    </Section>
  );
}

/**
 * The log, rendered again only when its own inputs change: a token or pair page re-renders on every book notice, and the log above has its own
 * live data (a page's market object is a new one each time; its key is what counts).
 */
export const ActivityLog = memo(
  ActivityLogView,
  (a, b) =>
    marketKeyOf(a.market) === marketKeyOf(b.market) &&
    a.prefsScope === b.prefsScope &&
    a.pairFills?.view === b.pairFills?.view &&
    a.allLink === b.allLink &&
    a.pageRows === b.pageRows &&
    a['data-testid'] === b['data-testid'],
);
