import { useMemo, useState } from 'preact/hooks';
import { displayToken } from '../../app/token-display';
import { useDeclaration } from '../../app/use-declaration';
import { useServices, useWallet } from '../../app/context';
import type { TicketPreset } from '../../app/router';
import { t } from '../../i18n';
import { Badge, Banner, Button, ErrorBanner, Loading, scaleOfDecimals, Section, tickKasFraction, tickPriceFraction, useAsync } from '../kit';
import { OrderTicket } from '../ticket/OrderTicket';
import { CrossCheckBanner } from './CrossCheckBanner';
import { useSystemStatus } from '../shell/StatusProvider';
import { buildBookModel, groupSizesAt, inferTick, levelsFromView } from './book-model';
import { useStaleSince } from './book-stale';
import { useOpenToken } from './use-open-token';
import { useHeldToken } from './use-held-token';
import { FirstOrderDecimals } from './FirstOrderDecimals';
import { issuedDecimals } from '../../data/issued-decimals';
import { DepthChart } from './DepthChart';
import { useLiveRefresh } from './live';
import { basisToTokenSompi, depthFromBook, depthFromView, priceDecimals, priceText, statsModel, tapeFromFills, tapeFromTrades } from './market-model';
import {
  INVERTED_GROUP_MULTIPLIERS, defaultInverted, depthFromBookFlipped, depthFromViewFlipped, flipDp, flippedBookView, flippedPriceText, invertedRegrouper, marketKey, nativeBookView, oppositeSide, pairLabel, ratioText,
  statsModelFlipped, tokensPerKas,
  type BookViewCtx,
} from './orientation';
import { useInverted } from './use-orientation';
import { MarketStats } from './MarketStats';
import { BookGrouping, OrderBook, StaleSince } from './OrderBook';
import { FlipButton, MarketShell, PairSelector } from './MarketBar';
import { openPair } from './market-nav';
import { displayedPair, marketFor, type DisplayedPair } from './market-select';
import { PriceChart } from './PriceChart';
import { TokenDetails, TokenHeader } from './TokenHeader';
import { NotTradablePage } from './NotTradablePage';
import { buildTokenRows, sharedTickerIds, tradableRows } from './token-model';
import { TradeTape } from './TradesPanel';
import { tradeRows } from './trades-model';
import './market.css';

const BOOK_DEPTH = 12;
const TAPE_LIMIT = 50;

/**
 * `#/market/<covenantId>`: an exchange layout. Title bar + 24 h stats; order book | price chart, depth and trade tape | order ticket;
 * token facts below. Live: the WebSocket feed (`book:` / `fills:`) triggers a debounced refetch, 5 s polling without it.
 */
export function TokenPage(props: { covenantId: string; ticket?: TicketPreset; amount?: string }) {
  const services = useServices();
  const wallet = useWallet();
  const { indexer: health } = useSystemStatus();
  const { registry, indexer, kob } = services;
  const id = props.covenantId;
  // the close of a position (My orders): `?ticket=sell&amount=N` (buy first) opens the ticket on a market SELL of the N base units the position
  // bought and has not sold yet, `?ticket=cover&amount=N` (sell first) on a market BUY of the N it sold; `?ticket=close` on "close (sell all)"
  const [prefill, setPrefill] = useState<{ side?: 'buy' | 'sell'; price?: bigint; amount?: bigint; type?: 'close' | 'market' } | undefined>(
    props.ticket === 'close'
      ? { side: 'sell', type: 'close' }
      : props.ticket === 'cover' || props.ticket === 'sell'
        ? { side: props.ticket === 'cover' ? 'buy' : 'sell', type: 'market', ...(props.amount ? { amount: BigInt(props.amount) } : {}) }
        : undefined,
  );
  const [liveKey, setLiveKey] = useState(0);
  // bumps on a reorg / resync (history may have changed, not only grown): the chart reloads in full instead of fetching only its newest bars
  const [resyncKey, setResyncKey] = useState(0);
  // the chosen price grouping per orientation (steps of KAS per token natively, of tokens per KAS inverted)
  const [groupIdxNative, setGroupIdxNative] = useState(0);
  const [groupIdxInv, setGroupIdxInv] = useState(0);

  const tokens = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer]);
  const allRows = useMemo(() => buildTokenRows(registry, tokens.data ?? null, []), [registry, tokens.data]);
  const listedRow = useMemo(() => allRows.find((r) => r.covenantId === id) ?? null, [allRows, id]);
  // a KCC-20 token in neither list (no order of it yet): its market only from the wallet's node-verified holdings of it (kob/open-token.ts)
  const held = useHeldToken(id, !listedRow && !tokens.loading);
  const baseRow = listedRow ?? held.row;
  // the pair selector offers only tokens this build can trade
  const choiceRows = useMemo(() => tradableRows(allRows, kob.templates()), [allRows, kob]);
  // another listed token uses this ticker: a short note at the title and the ticket
  const sharedTicker = useMemo(() => sharedTickerIds(choiceRows).has(id), [choiceRows, id]);
  // an open-list token (program on the strict list, no registry entry) is tradable with a TokenInfo synthesised from its orders: its decimals taken
  // from the scale its orders share
  // the decimals entered for the token's first order (used only while no order tells the scale); per token
  const [entered, setEntered] = useState<{ id: string; decimals: number } | null>(null);
  const enteredDecimals = entered && entered.id === id ? entered.decimals : null;
  // the token's KCC-23 declaration (verified against its covenant id): its decimals decide an open-list token's scale, its facts are shown
  const declared = useDeclaration(id, listedRow?.index?.declaration === true);
  const open = useOpenToken(baseRow, liveKey, enteredDecimals, declared ?? null);
  // a token the indexer lists without a ticker ("") is named like one without any (app/token-display.ts): the open-list token's name (this
  // browser's issuance ticker, else its covenant id), else the display name; never an empty name in a title, a unit or a hint
  const shown = useMemo(() => displayToken(registry, services.config.network, id), [registry, services.config.network, id, declared]);
  const row = useMemo(() => {
    if (!baseRow) return baseRow;
    const named = { ...baseRow, ticker: baseRow.ticker || open.info?.ticker || shown?.ticker || '' };
    return open.info
      ? { ...named, info: open.info, decimals: open.info.decimals, scale: scaleOfDecimals(open.info.decimals), tick: 1n, tradable: true, reason: null }
      : named;
  }, [baseRow, open.info, shown]);

  const book = useAsync((signal) => (indexer ? indexer.book(id, { depth: 60, aggregate: true }, { signal }) : Promise.resolve(null)), [indexer, id]);
  // the trade tape: `/v1/trades`, or the fill events of an indexer without it
  const trades = useAsync(
    async (signal) => {
      if (!indexer) return null;
      // the tape, the stats and the depth decorate the page: low fetch priority, the order ticket's reads get a connection first
      const v = await indexer.trades(id, { limit: TAPE_LIMIT }, { signal, priority: 'low' });
      if (v) return { kind: 'trades' as const, v };
      return { kind: 'fills' as const, v: await indexer.fills({ token: id, limit: TAPE_LIMIT }, { signal, priority: 'low' }) };
    },
    [indexer, id],
  );
  const stats = useAsync((signal) => (indexer ? indexer.stats(id, { signal, priority: 'low' }) : Promise.resolve(null)), [indexer, id]);
  const depth = useAsync((signal) => (indexer ? indexer.depth(id, { levels: 60 }, { signal, priority: 'low' }) : Promise.resolve(null)), [indexer, id]);

  // a live notice or a poll never aborts a pull in flight: it is folded into one pull right after it (`refresh`), so a slow indexer still gets its
  // answers through and is never asked twice for the same thing; only the manual button aborts and restarts
  const refreshAll = (manual = false, full = false) => {
    for (const r of [book, trades, stats, depth]) {
      if (manual) r.reload();
      else r.refresh();
    }
    setLiveKey((k) => k + 1);
    if (full) setResyncKey((k) => k + 1);
  };
  const mode = useLiveRefresh(services, [`book:${id}`, `fills:${id}`], (e) => !e.token || e.token === id, (full) => refreshAll(false, full));

  // every book row carries its own scale; the market's is the token's standard scale (`10^decimals`, at most 10^9): prices per whole token. The
  // decimals: the registry's, else the scale its orders set, else this browser's issuance of it, else 8 (the same rule as everywhere: never base units)
  const decimals = row?.decimals ?? shown?.decimals ?? 8;
  const scale = row?.scale ?? null;
  const bookScale = scale ?? 1n;

  // orientation (orientation.ts): TOKEN/KAS natively (the default of every market, a USD reference token included), KAS/TOKEN inverted. Display only. Needs the scale.
  const usdRef = services.config.quoteTokens[id.toLowerCase()] === 'USD';
  const [invertedChoice, toggleInverted, setInverted] = useInverted(marketKey('token', id), defaultInverted(usdRef));
  const canFlip = scale !== null;
  const inverted = invertedChoice && canFlip;
  const name = row?.ticker || shown?.ticker || '';

  // the book without grouping: the reference price of the page when there is neither a 24 h last price nor a trade
  const refMid = useMemo(() => {
    const m = book.data ? buildBookModel(book.data, { scale: bookScale, depth: 1 }) : null;
    return m ? m.mid : null;
  }, [book.data]);
  // price grouping: the registry tick, or the step the visible prices sit on, then steps scaled to the price (its decade only, so they stay put)
  const midDecade = refMid !== null && refMid > 0n ? refMid.toString().length : 0;
  const baseTick = useMemo(() => {
    if (row?.tick && row.tick > 0n) return row.tick;
    return book.data ? inferTick([...levelsFromView(book.data.asks, bookScale), ...levelsFromView(book.data.bids, bookScale)].map((l) => l.price)) : 1n;
  }, [row?.tick, book.data]);
  const groups = useMemo(() => groupSizesAt(baseTick, midDecade ? 10n ** BigInt(midDecade - 1) * 2n : null), [baseTick, midDecade]);
  const groupIdx = Math.min(inverted ? groupIdxInv : groupIdxNative, groups.length - 1);
  const group = groups[groupIdx]!;

  const tape = useMemo(() => {
    const d = trades.data;
    if (!d) return undefined;
    return d.kind === 'trades' ? tapeFromTrades(d.v, TAPE_LIMIT) : tapeFromFills(tradeRows(d.v.items, bookScale, TAPE_LIMIT), scale);
  }, [trades.data, scale]);

  // one price precision for the whole page: from the last trade, else the book (native: KAS per token; inverted: tokens per KAS)
  const refPrice = useMemo((): { price: bigint; basis: bigint } | null => {
    const s = stats.data;
    if (s && s.last && /^\d+$/.test(s.last) && /^[1-9]\d*$/.test(s.price_basis)) return { price: BigInt(s.last), basis: BigInt(s.price_basis) };
    const r = tape?.rows.find((x) => x.price !== null);
    if (r && tape?.basis) return { price: r.price!, basis: tape.basis };
    if (refMid && scale) return { price: refMid, basis: scale };
    return null;
  }, [stats.data, tape, refMid, scale]);
  // native price decimals: from the market's tick when the registry has one (stable per market), else from the price magnitude (chosen from one
  // reference price). Every row of every price column of the page shows exactly this many decimals.
  const tickDp = useMemo(() => (row?.tick && row.tick > 0n ? tickPriceFraction(row.tick, decimals, scale) : null), [row?.tick, decimals, scale]);
  const dpNative = useMemo(() => tickDp ?? priceDecimals(refPrice ? basisToTokenSompi(refPrice.price, decimals, refPrice.basis) : null), [tickDp, refPrice, decimals]);
  const dpFlipped = useMemo(() => flipDp(refPrice ? tokensPerKas(refPrice.price, decimals, refPrice.basis) : null), [refPrice, decimals]);
  const dp = inverted ? dpFlipped : dpNative;

  // the grouped book (inverted: the steps are multiples of the displayed price step, `orientation.ts`)
  const invMult = INVERTED_GROUP_MULTIPLIERS[Math.min(groupIdxInv, INVERTED_GROUP_MULTIPLIERS.length - 1)]!;
  const regroup = useMemo(() => (inverted && scale !== null ? invertedRegrouper({ decimals, scale, dp, mult: invMult }) : undefined), [inverted, scale, decimals, dp, invMult]);
  const freshModel = useMemo(
    () => (book.data ? buildBookModel(book.data, { scale: bookScale, depth: BOOK_DEPTH, group: inverted || groupIdx === 0 ? 0n : group, ...(regroup ? { regroup } : {}) }) : null),
    [book.data, bookScale, group, groupIdx, inverted, regroup],
  );
  // the live book as it is (a crossed book, matchers a moment behind, is drawn with a negative spread like any other)
  const model = freshModel;
  // a failed or late pull: the last good book stays, "stale since" says how old it is
  const bookStaleSince = useStaleSince(book);
  const groupOptions = useMemo(
    () =>
      inverted
        ? INVERTED_GROUP_MULTIPLIERS.map((m, i) => ({ value: String(i), label: `${ratioText({ num: m, den: 10n ** BigInt(dp) }, dp, '').replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '')}` }))
        : groups.map((g, i) => ({ value: String(i), label: `${priceText(g, decimals, bookScale, 10).replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '')}` })),
    [inverted, groups, decimals, bookScale, dp],
  );

  const sm = useMemo(() => (inverted ? statsModelFlipped(stats.data ?? null, decimals, dp) : statsModel(stats.data ?? null, decimals, dp)), [stats.data, decimals, dp, inverted]);
  const last = useMemo(() => {
    if (sm.last) return { text: sm.last, side: sm.lastSide };
    const r = tape?.rows.find((x) => x.price !== null);
    if (!r) return null;
    if (inverted) return tape!.basis ? { text: flippedPriceText(r.price!, decimals, tape!.basis, dp), side: r.side ? oppositeSide(r.side) : null } : null;
    return { text: priceText(r.price!, decimals, tape!.basis ?? 0n, dp), side: r.side };
  }, [sm, tape, decimals, dp, inverted]);
  const statsForView = sm.last || !last ? sm : { ...sm, last: last.text, lastSide: last.side };

  const freshDepth = useMemo(() => {
    if (depth.data) return inverted ? depthFromViewFlipped(depth.data, decimals) : depthFromView(depth.data, decimals);
    // no depth endpoint, or it failed: the depth of the book on screen
    if ((depth.data === null || (depth.data === undefined && depth.error)) && freshModel && scale) {
      const full = buildBookModel(book.data!, { scale, depth: 60 });
      return inverted ? depthFromBookFlipped(full, decimals, scale) : depthFromBook(full, decimals, scale);
    }
    return null;
  }, [depth.data, freshModel, book.data, decimals, scale, inverted]);
  const depthSeries = freshDepth;

  const bookCtx: BookViewCtx = useMemo(
    () => ({
      name,
      decimals,
      scale: bookScale,
      dp,
      tick: baseTick,
      labels: inverted
        ? { price: t('market.book.priceIn', { unit: t('market.stats.unitOf', { base: 'KAS', quote: name }) }), size: t('market.book.sizeIn', { unit: 'KAS' }), total: t('market.book.totalIn', { unit: name }) }
        : { price: t('market.book.priceIn', { unit: t('market.stats.unitOf', { base: name, quote: 'KAS' }) }), size: t('market.book.sizeIn', { unit: name }), total: t('market.book.totalIn', { unit: 'KAS' }) },
    }),
    [name, decimals, bookScale, dp, baseTick, inverted],
  );
  const bookView = useMemo(
    () => (model && !model.empty ? (inverted && scale !== null ? flippedBookView(model, bookCtx) : nativeBookView(model, bookCtx)) : null),
    [model, bookCtx, inverted, scale],
  );

  if (!row) {
    if (tokens.loading || held.loading) return <Loading />;
    return (
      <div class="stack" data-testid="token-not-found">
        <Banner tone="warn" title={t('market.token.notFound')}>
          {t('market.token.notFoundBody', { id })}
          {held.reason && held.reason !== 'not-held' ? <div data-testid="token-held-reason">{t(`market.held.${held.reason}`)}</div> : null}
          {!wallet.info ? <div data-testid="token-held-connect">{t('market.held.connect')}</div> : null}
        </Banner>
        <div><a href="#/market">{t('market.token.back')}</a></div>
      </div>
    );
  }

  // a registry token this build cannot trade (or an open-list token whose program it does not embed, or does not trade on this network) has
  // no market page here
  if ((row.source === 'registry' && !row.tradable) || open.reason === 'program-unknown' || open.reason === 'program-unreviewed') {
    const reason = open.reason === 'program-unknown' || open.reason === 'program-unreviewed' ? t(`market.open.${open.reason}`) : row.reason ? t(`market.reason.${row.reason}`) : null;
    return <NotTradablePage ticker={row.ticker} covenantId={row.covenantId} reason={reason} />;
  }

  const info = row.info;
  const stale = health.assessment.level === 'warn' || health.assessment.level === 'bad';
  const liveBadge = (
    <Badge tone={mode === 'live' ? 'ok' : mode === 'polling' ? 'info' : 'neutral'} data-testid="live-indicator" title={t(`market.live.${mode}Hint`)}>
      {t(`market.live.${mode}`)}
    </Badge>
  );
  // the pair selector: a change of side leads to another market; the same token with the other order is only a flip
  const onPair = (next: DisplayedPair) => {
    const m = marketFor(next);
    if (m && m.route.name === 'token' && m.route.covenantId === id && m.inverted !== null) {
      if (canFlip) setInverted(m.inverted);
      return;
    }
    openPair(next);
  };

  return (
    <MarketShell
      data-testid="token-page"
      attrs={{ 'data-token': id }}
      header={
        <TokenHeader
          row={row}
          sharedTicker={sharedTicker}
          badges={liveBadge}
          pair={
            <>
              {canFlip ? <FlipButton pair={pairLabel(name, !inverted)} pressed={inverted} onClick={toggleInverted} /> : null}
              <PairSelector pair={displayedPair(id, null, inverted)} rows={row ? choiceRows.map((r) => (r.covenantId === id ? { ...r, ticker: row.ticker } : r)) : choiceRows} onChange={onPair} />
            </>
          }
        />
      }
      notices={
        <>
          {shown?.declarationConflicts?.length ? (
            <Banner tone="warn" title={t('market.declaration.conflictTitle')} data-testid="declaration-conflict">
              {t('market.declaration.conflict', { fields: shown.declarationConflicts.map((f) => t(`token.note.field.${f}`)).join(', ') })}
            </Banner>
          ) : null}
          <CrossCheckBanner token={id} refreshKey={liveKey} />
        </>
      }
      stats={<MarketStats model={statsForView} ticker={name} inverted={inverted} loading={!!indexer && stats.data === undefined && stats.loading} unsupported={stats.data === null && !!indexer} />}
      book={
        <Section
          title={t('market.book.title')}
          data-testid="book-section"
          class="mkt-area-book mkt-card"
          actions={
            <>
              {stale ? <Badge tone={health.assessment.level === 'bad' ? 'bad' : 'warn'} data-testid="book-stale">{t('market.book.stale')}</Badge> : null}
              {/* the steps come from the book's tick: no book on screen yet (loading, indexer unreachable), no grouping to offer */}
              {book.data ? <BookGrouping options={groupOptions} value={String(groupIdx)} onChange={(v) => (inverted ? setGroupIdxInv : setGroupIdxNative)(Math.max(0, Number(v) || 0))} /> : null}
              <Button small variant="ghost" onClick={() => refreshAll(true)} loading={book.loading && !!book.data} title={t('common.refresh')} aria-label={t('common.refresh')} data-testid="book-refresh">
                {'↻'}
              </Button>
            </>
          }
        >
          {!indexer ? <Banner tone="info">{t('market.book.noIndexer')}</Banner> : null}
          {/* with a book on screen a failed pull is only the subtle marker below; the banner is for a page that has no book to show */}
          <ErrorBanner error={book.data ? null : book.error} title={t('market.book.error')} onRetry={book.reload} data-testid="book-error" />
          <StaleSince since={bookStaleSince} />
          {indexer && !model && book.loading ? (
            <div data-testid="book-loading">
              {Array.from({ length: BOOK_DEPTH * 2 + 2 }, (_, i) => <div key={i} class="skeleton skeleton-line" />)}
            </div>
          ) : null}
          {model && model.empty ? <p class="muted center" data-testid="book-empty">{t('market.book.empty')}</p> : null}
          {bookView ? <OrderBook view={bookView} rows={BOOK_DEPTH} last={last} inverted={inverted} token={name} onPick={(side, price) => setPrefill({ side, price })} /> : null}
        </Section>
      }
      chart={
        <Section title={t('market.chart.title')} data-testid="chart-section" class="mkt-area-chart mkt-card">
          <PriceChart indexer={indexer} token={id} decimals={decimals} ticker={name} dp={dp} refreshKey={liveKey} resyncKey={resyncKey} inverted={inverted} />
        </Section>
      }
      depth={
        <Section title={t('market.depth.title')} data-testid="depth-section" class="mkt-area-depth mkt-card">
          <DepthChart series={depthSeries} loading={!!indexer && depth.data === undefined && !depth.error} unavailable={!!depth.error && !!book.error && !depthSeries} ticker={inverted ? 'KAS' : name} priceUnit={inverted ? t('market.stats.unitOf', { base: 'KAS', quote: name }) : t('market.stats.unitOf', { base: name, quote: 'KAS' })} inverted={inverted} dp={dp} amountDp={inverted ? Math.min(tickKasFraction(baseTick), 4) : Math.min(decimals, 4)} source={depth.data ? 'depth' : 'book'} />
        </Section>
      }
      tape={
        <div class="mkt-area-tape">
          <TradeTape tape={tape} loading={trades.loading} error={trades.error} onRetry={trades.reload} decimals={decimals} ticker={name} dp={dp} tick={baseTick} inverted={inverted} />
        </div>
      }
      ticket={
        <div class="stack mkt-area-ticket">
          {info && row.tradable ? (
            <div data-testid="ticket-container">
              {info.openScale === 'entered' ? (
                <Banner tone="info" data-testid="first-order-entered">
                  {t('market.first.entered', { decimals: info.decimals })}{' '}
                  <Button small variant="ghost" onClick={() => setEntered(null)} data-testid="first-order-change">{t('market.first.change')}</Button>
                </Banner>
              ) : null}
              {wallet.networkMismatch ? <Banner tone="error" data-testid="ticket-network-blocked">{t('market.ticket.networkBlocked')}</Banner> : null}
              <OrderTicket token={info} prefill={prefill} inverted={inverted} />
            </div>
          ) : open.reason === 'no-scale' ? (
            <FirstOrderDecimals key={id} known={issuedDecimals(services.config.network, id)} onSet={(d) => setEntered({ id, decimals: d })} />
          ) : (
            <Section title={t('market.ticket.title')} data-testid="ticket-unavailable" class="mkt-card">
              <Banner tone="warn" data-testid="ticket-untradable">
                {open.reason
                  ? t('market.ticket.untradable', { reason: t(`market.open.${open.reason}`) })
                  : open.loading
                    ? t('market.open.loading')
                    : row.reason
                      ? t('market.ticket.untradable', { reason: t(`market.reason.${row.reason}`) })
                      : t('market.ticket.untradableUnknown')}
                {/* decimals entered for a first order, but the market has a scale now (an order came first): drop them, never trade at both */}
                {open.reason === 'scale-conflict' ? (
                  <div>
                    <Button small variant="ghost" onClick={() => setEntered(null)} data-testid="first-order-clear">{t('market.first.clear')}</Button>
                  </div>
                ) : null}
              </Banner>
            </Section>
          )}
        </div>
      }
      details={<TokenDetails row={row} services={services} />}
    />
  );
}
