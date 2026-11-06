// The activity log (kaspa.stream's transaction list, for KOB): one row per transaction, newest first, live. The SAME component on the Activity page
// (#/activity, every market, no market filter) and on the token and pair pages (preset to that market; the viewer can widen it, and "All activity →"
// opens the Activity page).
//
// Every row is a real link to the transaction on the network's explorer (new tab, `noopener noreferrer`), so a row must never move under the pointer:
// while the list is paused, pointed at or focused, new rows are held (`heldView`) and counted in a "+N new" pill that releases them.
import { memo } from 'preact/compat';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { displayToken } from '../../app/token-display';
import { explorerTxUrl } from '../../config';
import { pubkeyToAddress } from '../../data/kaspa-sdk';
import type { PairFillsView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { longId } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { Badge, Button, ErrorBanner, Section } from '../kit';
import { Popover } from '../shell/Popover';
import { clockText } from '../shell/standalone';
import {
  ALL_KINDS, AVAILABLE_KINDS, PAGE_ROWS, rowHref, SETTLE_DAA, filterRows, heldView, marketKeyOf, readPrefs, rowMakers, sameMarket, shownKinds, writePrefs,
  type ActivityKind, type ActivityPrefs, type ActivityRow, type ActorFilter, type MarketFilter, type MarketRef, type PrefsScope,
} from './activity-model';
import { KindGlyph, KindIcon } from './ActivityIcons';
import { ActivityRowView, type RowFormat } from './ActivityRows';
import { useActivity } from './use-activity';
import '../market/market.css';
import './activity.css';

const addressCache = new Map<string, string | null>();

export interface ActivityLogProps {
  /** the market filter now (the Activity page: from its URL) */
  market: MarketFilter;
  onMarketChange(m: MarketFilter): void;
  /** the token or pair page this log sits on: offered as "This market" */
  pageMarket?: MarketRef;
  /** which stored filters apply: the Activity page's, or the one record of the token and pair pages */
  prefsScope: PrefsScope;
  /** the pair page's own copy of the pair's fills */
  pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null;
  /** a link to the Activity page (on the token and pair pages) */
  allLink?: boolean;
  /** rows shown at first and added by each "Load older" (default PAGE_ROWS) */
  pageRows?: number;
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
  const data = useActivity({ market: props.market, kinds, pairFills: props.pairFills ?? null, onScreen });

  const { nameOf, decimalsOf } = useMemo(
    () => ({
      nameOf: (token: Hex): string => displayToken(registry, config.network, token)?.ticker || longId(token),
      decimalsOf: (token: Hex): number => displayToken(registry, config.network, token)?.decimals ?? 8,
    }),
    [registry, config.network],
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

  const all = filterRows(data.rows, { prefs, market: props.market, mine: me, makersOf: (r) => rowMakers(r, data) });

  // ---- the hold: paused, or the pointer or the focus in the list
  const [paused, setPaused] = useState(false);
  const [hover, setHover] = useState(false);
  const [focusIn, setFocusIn] = useState(false);
  const held = paused || hover || focusIn;
  const pageRows = props.pageRows ?? PAGE_ROWS;
  const [limit, setLimit] = useState(pageRows);
  const [, setReleased] = useState(0);
  const snapshot = useRef<Set<string> | null>(null);
  const shownBefore = useRef<ActivityRow[]>([]);
  const filterKey = `${marketKeyOf(props.market)}|${[...kinds].join(',')}|${prefs.mine ? 'mine' : ''}`;
  const lastFilter = useRef(filterKey);
  if (!held) snapshot.current = null;
  else if (snapshot.current === null || lastFilter.current !== filterKey) snapshot.current = new Set((lastFilter.current === filterKey ? shownBefore.current : all.slice(0, limit)).map((r) => r.key));
  lastFilter.current = filterKey;
  const view = heldView(all, snapshot.current);
  const shown = view.rows.slice(0, limit);
  shownBefore.current = shown;
  const release = () => {
    setPaused(false);
    snapshot.current = hover || focusIn ? new Set(all.slice(0, limit).map((r) => r.key)) : null;
    setReleased((n) => n + 1);
  };

  // rows that arrive after the first rows flash once
  const seen = useRef<Set<string> | null>(null);
  const fresh = new Set<string>();
  if (shown.length) {
    if (seen.current) for (const r of shown) if (!seen.current.has(r.key)) fresh.add(r.key);
    seen.current = new Set([...(seen.current ?? []), ...shown.map((r) => r.key)]);
  }

  // the Wallet column: the owners of the rows' orders are looked up only while the list is on screen (one at a time: the order ticket's reads of
  // a slow indexer must not queue behind them)
  const wanted = onScreen ? shown.flatMap((r) => r.orders).join(',') : '';
  useEffect(() => {
    if (wanted) data.want(wanted.split(',') as Hex[]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wanted]);

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
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [registry, config]);
  const marketText = fmt.marketText;
  const today = new Date().toDateString();
  const liveKind = data.mode === 'standalone' ? 'standalone' : data.mode === 'off' ? 'off' : held ? 'held' : data.mode;

  // ---- the header's tools
  const toggleKind = (k: ActivityKind) => setPrefs({ ...prefs, kinds: { ...prefs.kinds, [k]: !prefs.kinds[k] } });
  const setActor = (a: ActorFilter) => setPrefs({ ...prefs, actor: a });
  const marketOptions = useMemo(() => {
    const opts: { value: string; label: string; m: MarketFilter }[] = [{ value: 'all', label: t('activity.market.all'), m: { kind: 'all' } }];
    const add = (m: MarketRef, label?: string) => {
      const value = marketKeyOf(m);
      if (!opts.some((o) => o.value === value)) opts.push({ value, label: label ?? marketText(m), m });
    };
    if (props.pageMarket) add(props.pageMarket, t('activity.market.this', { name: marketText(props.pageMarket) }));
    if (props.market.kind !== 'all') add(props.market);
    const tokens = [...registry.tokens].filter((x) => x.tradable).sort((a, b) => a.ticker.localeCompare(b.ticker));
    for (const x of tokens) add({ kind: 'token', token: x.covenantId.toLowerCase() });
    return opts;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [registry, props.pageMarket && marketKeyOf(props.pageMarket), marketKeyOf(props.market)]);

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
      {/* the market filter belongs to the token and pair pages; the Activity page shows every market */}
      {props.pageMarket ? (
        <select
          class="act-sel"
          aria-label={t('activity.market.label')}
          data-testid="activity-market"
          value={marketKeyOf(props.market)}
          onChange={(e) => {
            const o = marketOptions.find((x) => x.value === (e.target as HTMLSelectElement).value);
            if (o && !sameMarket(o.m, props.market)) props.onMarketChange(o.m);
          }}
        >
          {marketOptions.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      ) : null}
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
      <span class="act-scope muted">{props.market.kind === 'all' ? t('activity.allMarkets') : marketText(props.market)}</span>
      {data.sinceMs !== null ? (
        <Badge tone="info" title={t('activity.sinceHint')} data-testid="activity-since">
          {t('activity.since', { time: clockText(data.sinceMs) })}
        </Badge>
      ) : null}
    </span>
  );

  const legendKinds = ALL_KINDS.filter((k) => AVAILABLE_KINDS.includes(k));
  return (
    <Section title={title} actions={tools} class="act" data-testid={props['data-testid'] ?? 'activity'}>
      <ErrorBanner error={data.rows.length ? null : data.error} title={t('activity.error')} onRetry={data.retry} data-testid="activity-error" />
      <div class="act-log" ref={listRef} role="table" aria-label={t('activity.tableLabel')} data-testid="activity-list" data-count={shown.length} data-held={held ? 'true' : 'false'}>
        <div class="act-head" role="row">
          <span role="columnheader" aria-label={t('activity.col.kind')} />
          <span role="columnheader">{t('activity.col.market')}</span>
          <span role="columnheader">{t('activity.col.tx')}</span>
          <span role="columnheader">{t('activity.col.wallet')}</span>
          <span role="columnheader" class="act-num">{t('activity.col.amount')}</span>
          <span role="columnheader" class="act-num">{t('activity.col.price')}</span>
          <span role="columnheader" class="act-num">{t('activity.col.conf')}</span>
          <span role="columnheader" class="act-num">{t('activity.col.time')}</span>
        </div>
        <div
          class={`act-body${shown.length ? ' act-rows' : ''}`}
          style={shown.length ? `--n:${shown.length}` : undefined}
          role="rowgroup"
          // a pointer that MOVES in the list holds it (a list that slid under a resting pointer does not)
          onPointerMove={(e) => !hover && (e as PointerEvent).pointerType !== 'touch' && setHover(true)}
          onPointerLeave={() => setHover(false)}
          onFocusIn={() => setFocusIn(true)}
          onFocusOut={(e) => {
            const next = (e as FocusEvent).relatedTarget as Node | null;
            if (!next || !(e.currentTarget as HTMLElement).contains(next)) setFocusIn(false);
          }}
        >
          {shown.length ? (
            shown.map((r, i) => {
              const makers = rowMakers(r, data);
              return (
                <ActivityRowView
                  key={r.key}
                  row={r}
                  i={i}
                  fresh={fresh.has(r.key)}
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
          ) : data.loading ? (
            <div class="act-empty" data-testid="activity-loading">
              {Array.from({ length: 6 }, (_, i) => <div key={i} class="skeleton skeleton-line" style={`margin:12px 8px;opacity:${1 - i * 0.12}`} />)}
            </div>
          ) : data.error ? null : (
            <div class="act-empty muted" data-testid="activity-empty">{data.rows.length ? t('activity.emptyFiltered') : t('activity.empty')}</div>
          )}
        </div>
      </div>
      <div class="act-foot">
        <span class="small muted" data-testid="activity-showing">{t('activity.showing', { shown: shown.length, kept: view.rows.length })}</span>
        <span class="act-foot-r">
          {props.allLink ? (
            <a href="#/activity" class="small" title={t('activity.allActivityTitle')} data-testid="activity-all-link">
              {t('activity.allActivity')}
            </a>
          ) : null}
          {data.sinceMs !== null ? (
            <span class="small muted" data-testid="activity-nothing-before">{t('activity.nothingBefore', { time: clockText(data.sinceMs) })}</span>
          ) : view.rows.length > limit || data.hasOlder ? (
            <Button
              small
              variant="secondary"
              loading={data.loadingOlder}
              data-testid="activity-older"
              onClick={() => {
                if (view.rows.length <= limit) data.loadOlder();
                setLimit((l) => l + pageRows);
              }}
            >
              {t('activity.loadOlder')}
            </Button>
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
    (a.pageMarket ? marketKeyOf(a.pageMarket) : '') === (b.pageMarket ? marketKeyOf(b.pageMarket) : '') &&
    a.onMarketChange === b.onMarketChange &&
    a.prefsScope === b.prefsScope &&
    a.pairFills?.view === b.pairFills?.view &&
    a.allLink === b.allLink &&
    a.pageRows === b.pageRows &&
    a['data-testid'] === b['data-testid'],
);
