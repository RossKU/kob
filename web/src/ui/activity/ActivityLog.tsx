// The activity log (kaspa.stream's transaction list, for KOB): one row per transaction, newest first, live. The SAME component on the Activity page
// (#/activity, every market or the one in its URL) and on the token and pair pages (preset to that market; the viewer can widen it, and "All
// activity →" opens the Activity page with the same filter).
//
// Every row is a real link to the transaction on the network's explorer (new tab, `noopener noreferrer`), so a row must never move under the pointer:
// while the list is paused, pointed at or focused, new rows are held (`heldView`) and counted in a "+N new" pill that releases them.
import type { ComponentChildren } from 'preact';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { displayToken } from '../../app/token-display';
import { activityMarketParam } from '../../app/router';
import { explorerTxUrl } from '../../config';
import { pubkeyToAddress } from '../../data/kaspa-sdk';
import type { PairFillsView } from '../../data/indexer-types';
import { formatDateTime, t } from '../../i18n';
import { longId } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { pow10 } from '../../kob/units';
import { Badge, Button, ErrorBanner, fitAmountColumn, Section, unitsFraction, useInterval } from '../kit';
import { priceDecimals, priceText } from '../market/market-model';
import { TokenIcon } from '../market/TokenIcon';
import { Popover } from '../shell/Popover';
import { clockText } from '../shell/standalone';
import {
  ALL_KINDS, AVAILABLE_KINDS, PAGE_ROWS, rowHref, SETTLE_DAA, confirmationsAt, daaNow, filterRows, heldView, marketKeyOf, readPrefs, rowMakers, rowTime, sameMarket, shortWallet,
  shownKinds, writePrefs,
  type ActivityKind, type ActivityPrefs, type ActivityRow, type ActorFilter, type MarketFilter, type MarketRef, type PrefsScope,
} from './activity-model';
import { KindGlyph, KindIcon, kindLabel } from './ActivityIcons';
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
  /** a link to the Activity page with this filter (on the token and pair pages) */
  allLink?: boolean;
  /** rows shown at first and added by each "Load older" (default PAGE_ROWS) */
  pageRows?: number;
  'data-testid'?: string;
}

export function ActivityLog(props: ActivityLogProps) {
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

  const nameOf = (token: Hex): string => displayToken(registry, config.network, token)?.ticker || longId(token);
  const decimalsOf = (token: Hex): number => displayToken(registry, config.network, token)?.decimals ?? 8;
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

  // the confirmation pies move while a row on screen is not settled
  const [now, setNow] = useState(Date.now());
  const chainDaa = daaNow(data.chain, now);
  const unsettled = shown.some((r) => !confirmationsAt(r, chainDaa).settled);
  useInterval(() => setNow(Date.now()), 1000, unsettled);

  const explorerHost = (() => {
    try {
      return new URL(explorerTxUrl(config, '0'.repeat(64)) ?? '').host;
    } catch {
      return 'the explorer';
    }
  })();

  const marketText = (m: MarketRef): string => (m.kind === 'token' ? t('activity.market.kas', { ticker: nameOf(m.token) }) : t('activity.market.pair', { base: nameOf(m.base), quote: nameOf(m.quote) }));
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
      {view.held > 0 ? (
        <button type="button" class="act-new" onClick={release} title={t('activity.newRowsTitle')} data-testid="activity-new" {...(paused ? { 'aria-live': 'polite' } : {})}>
          {t('activity.newRows', { n: view.held })}
        </button>
      ) : null}
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

  // ---- rows
  const rowEl = (r: ActivityRow) => {
    const href = rowHref(config, r);
    const conf = confirmationsAt(r, chainDaa);
    const makers = rowMakers(r, data);
    const pendingLookup = r.orders.some((id) => data.order(id) === undefined);
    const addrs = makers.map((pk) => addressOf(pk) ?? longId(pk));
    const amountToken = r.amount?.token ?? null;
    const amount = r.amount && amountToken ? cellText(r.amount.units, decimalsOf(amountToken)) : null;
    const amountText = amount && amountToken ? `${amount.text} ${nameOf(amountToken)}` : '—';
    let sub: string = '';
    if (r.value?.kind === 'kas') sub = `${fitAmountColumn([r.value.sompi], 8, { fraction: 2, width: 16 })[0]!.text} KAS`;
    else if (r.value?.kind === 'token') sub = `${cellText(r.value.units, decimalsOf(r.value.token)).text} ${nameOf(r.value.token)}`;
    else if (r.pairSide) sub = t(r.pairSide === 'ask' ? 'activity.pairAsk' : 'activity.pairBid');
    const priceTokenDecimals = r.market.kind === 'token' ? decimalsOf(r.market.token) : 0;
    const price = r.price ? priceRange(r.price, priceTokenDecimals) : '—';
    const kindText = r.kinds.length > 1 ? r.kinds.map((k) => kindLabel(k, r.side)).join(' + ') : kindLabel(r.kind, r.side);
    const confText = conf.settled ? t('activity.conf.settled') : conf.n === null ? t('activity.conf.unknown') : t('activity.rowConf', { n: conf.n, total: SETTLE_DAA });
    const time = rowTime(r.timeMs, now);
    const fullTime = r.timeMs !== null ? `${formatDateTime(Math.floor(r.timeMs / 1000), { timeStyle: 'medium' })}${r.timeEstimated ? ` (${t('activity.timeEstimated')})` : ''}` : `DAA ${r.daa}`;
    const label = t('activity.rowLabel', { kind: kindText, market: marketText(r.market), amount: amountText, conf: confText, time: fullTime, explorer: explorerHost });
    const cells: ComponentChildren = (
      <>
        <span class="act-c-ty" role="cell">
          <KindIcon kind={r.kind} side={r.side} />
        </span>
        <span class="act-c-mk" role="cell" title={marketText(r.market)}>
          <MarketIcons market={r.market} nameOf={nameOf} />
          {r.moreMarkets > 0 ? <span class="act-more" title={t('activity.moreMarketsTitle', { n: r.moreMarkets })}>{t('activity.moreMarkets', { n: r.moreMarkets })}</span> : null}
          <span class="act-txm">{r.txid.slice(0, 8)}</span>
        </span>
        <span class="act-c-tx" role="cell">{`${r.txid.slice(0, 8)}…${r.txid.slice(-7)}`}</span>
        <span class="act-c-who" role="cell" title={addrs.length ? t('activity.wallet.title', { list: addrs.join(', ') }) : undefined} data-testid="activity-wallet">
          {addrs.length ? (
            addrs.slice(0, 2).flatMap((a, i) => [
              ...(i > 0 ? [<i key={`s`} class="act-sep" aria-hidden="true">{' ⇄ '}</i>] : []),
              <span key={a}>{shortWallet(a)}</span>,
            ])
          ) : (
            <span class="muted">{pendingLookup ? t('activity.wallet.pending') : t('activity.wallet.unknown')}</span>
          )}
          {addrs.length > 2 ? <i class="act-sep">{` +${addrs.length - 2}`}</i> : null}
          {r.placed > 1 ? <i class="act-tag" title={t('activity.wallet.ladderTitle', { n: r.placed })}>{t('activity.wallet.ladder', { n: r.placed })}</i> : null}
          {r.counterparty ? <i class="act-tag">{t(`activity.cp.${r.counterparty}`)}</i> : null}
        </span>
        <span class={`act-c-amt act-num${r.side ? ` act-${r.side}` : ''}`} role="cell" title={amount?.exact ? `${amount.exact} ${amountToken ? nameOf(amountToken) : ''}` : undefined}>
          <b>{amountText}</b>
          {sub ? <span class="act-sub">{sub}</span> : null}
        </span>
        <span class="act-c-px act-num" role="cell">{price}</span>
        <span class="act-c-conf act-num" role="cell" title={conf.settled ? t('activity.conf.settledTitle', { total: SETTLE_DAA }) : conf.n !== null ? t('activity.conf.title', { n: conf.n, total: SETTLE_DAA, s: Math.max(0, Math.ceil((SETTLE_DAA - conf.n) / 10)) }) : undefined} data-settled={conf.settled ? 'true' : 'false'}>
          {conf.settled ? (
            <span class="act-ok" aria-hidden="true">{'✓'}</span>
          ) : (
            <>
              <span>{conf.n ?? '…'}</span>
              <i class="act-pie" style={`--p:${Math.min(100, Math.round(((conf.n ?? 0) / SETTLE_DAA) * 100))}`} aria-hidden="true" />
            </>
          )}
        </span>
        <span class="act-c-time act-num" role="cell" title={fullTime}>{time}</span>
      </>
    );
    const attrs = {
      key: r.key,
      role: 'row' as const,
      class: `act-row${fresh.has(r.key) ? ' fresh' : ''}`,
      'data-testid': 'activity-row',
      'data-kind': r.kind,
      'data-side': r.side ?? '',
      'data-txid': r.txid,
      'data-market': marketKeyOf(r.market),
      'aria-label': label,
      title: `${kindText} · ${t('activity.openTx', { explorer: explorerHost })}`,
    };
    return href ? (
      <a {...attrs} href={href} target="_blank" rel="noopener noreferrer">
        {cells}
      </a>
    ) : (
      <div {...attrs}>{cells}</div>
    );
  };

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
          class="act-body"
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
            shown.map(rowEl)
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
            <a href={`#/activity${props.market.kind === 'all' ? '' : `?market=${activityMarketParam(props.market)}`}`} class="small" title={t('activity.allActivityTitle')} data-testid="activity-all-link">
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

/** One token icon for a KAS book, `A / B` for a pair (the tickers in the cell's title and the row's label). */
function MarketIcons(props: { market: MarketRef; nameOf: (t: Hex) => string }) {
  const m = props.market;
  const icon = (id: Hex) => <TokenIcon row={{ covenantId: id, ticker: props.nameOf(id), kascov: null }} size={22} anonymous />;
  return m.kind === 'token' ? (
    <span class="act-mk">{icon(m.token)}</span>
  ) : (
    <span class="act-mk">
      {icon(m.base)}
      <span class="act-sl" aria-hidden="true">/</span>
      {icon(m.quote)}
    </span>
  );
}

/** An amount in one cell: the exact amount when it fits, else fewer decimals or K / M / B (the exact amount in `exact`). */
function cellText(units: bigint, decimals: number): { text: string; exact: string | null } {
  return fitAmountColumn([units], decimals, { fraction: Math.min(unitsFraction(units, decimals), 6), width: 12 })[0]!;
}

/** KAS per whole token (the row's own precision); a ladder shows its range. */
function priceRange(p: { low: bigint; high: bigint; scale: bigint }, decimals: number): string {
  const perToken = (x: bigint) => (p.scale > 0n ? (x * pow10(decimals)) / p.scale : x);
  const dp = priceDecimals(perToken(p.high));
  const one = (x: bigint) => priceText(x, decimals, p.scale, dp);
  return p.low === p.high ? one(p.low) : `${one(p.low)}–${one(p.high)}`;
}
