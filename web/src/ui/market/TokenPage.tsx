import { useMemo, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import type { TicketPreset } from '../../app/router';
import { t } from '../../i18n';
import { Badge, Banner, Button, ErrorBanner, Loading, scaleOfDecimals, Section, tickKasFraction, tickPriceFraction, useAsync, useInterval } from '../kit';
import { OrderTicket } from '../ticket/OrderTicket';
import { CrossCheckBanner } from './CrossCheckBanner';
import { useSystemStatus } from '../shell/StatusProvider';
import { buildBookModel, groupSizes, inferTick, levelsFromView } from './book-model';
import { useCoalescedReload, useStaleSince } from './book-stale';
import { useOpenToken } from './use-open-token';
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
import { buildTokenRows } from './token-model';
import { useHeldTokenIds } from './use-held-tokens';
import { TradeTape } from './TradesPanel';
import { tradeRows } from './trades-model';
import { WalletTokenBalances } from './WalletTokenBalances';
import './market.css';

const BOOK_DEPTH = 12;
const TAPE_LIMIT = 50;

/**
 * `#/market/<covenantId>`: an exchange layout. Title bar + 24 h stats; order book | price chart, depth and trade tape | order ticket and
 * balances; token facts below. Live: the WebSocket feed (`book:` / `fills:`) triggers a debounced refetch, 5 s polling without it.
 */
export function TokenPage(props: { covenantId: string; ticket?: TicketPreset; amount?: string }) {
  const services = useServices();
  const wallet = useWallet();
  const { indexer: health } = useSystemStatus();
  const { registry, indexer } = services;
  const id = props.covenantId;
  // a `?ticket=close` link (My orders, close position) opens the ticket on "close": sell the whole free balance at market; `?ticket=cover&amount=N`
  // (close of a sell-first position) on a market BUY of the N base units the position sold and has not bought back yet
  const [prefill, setPrefill] = useState<{ side?: 'buy' | 'sell'; price?: bigint; amount?: bigint; type?: 'close' | 'market' } | undefined>(
    props.ticket === 'close' ? { side: 'sell', type: 'close' } : props.ticket === 'cover' ? { side: 'buy', type: 'market', ...(props.amount ? { amount: BigInt(props.amount) } : {}) } : undefined,
  );
  const [balKey, setBalKey] = useState(0);
  const [liveKey, setLiveKey] = useState(0);
  // the chosen price grouping per orientation (steps of KAS per token natively, of tokens per KAS inverted)
  const [groupIdxNative, setGroupIdxNative] = useState(0);
  const [groupIdxInv, setGroupIdxInv] = useState(0);

  const tokens = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer]);
  const held = useHeldTokenIds();
  const allRows = useMemo(() => buildTokenRows(registry, tokens.data ?? null, [], held), [registry, tokens.data, held]);
  const baseRow = useMemo(() => allRows.find((r) => r.covenantId === id) ?? null, [allRows, id]);
  // an open-list token (program on the strict list, no registry entry) is tradable with a TokenInfo synthesised from its orders: unverified, its
  // decimals taken from the scale its orders share
  const open = useOpenToken(baseRow, liveKey);
  const row = useMemo(
    () =>
      baseRow && open.info
        ? { ...baseRow, info: open.info, decimals: open.info.decimals, scale: scaleOfDecimals(open.info.decimals), tick: 1n, tradable: true, reason: null }
        : baseRow,
    [baseRow, open.info],
  );

  const book = useAsync((signal) => (indexer ? indexer.book(id, { depth: 60, aggregate: true }, { signal }) : Promise.resolve(null)), [indexer, id]);
  // the trade tape: `/v1/trades`, or the fill events of an indexer without it
  const trades = useAsync(
    async (signal) => {
      if (!indexer) return null;
      const v = await indexer.trades(id, { limit: TAPE_LIMIT }, { signal });
      if (v) return { kind: 'trades' as const, v };
      return { kind: 'fills' as const, v: await indexer.fills({ token: id, limit: TAPE_LIMIT }, { signal }) };
    },
    [indexer, id],
  );
  const stats = useAsync((signal) => (indexer ? indexer.stats(id, { signal }) : Promise.resolve(null)), [indexer, id]);
  const depth = useAsync((signal) => (indexer ? indexer.depth(id, { levels: 60 }, { signal }) : Promise.resolve(null)), [indexer, id]);

  // a live notice or a poll never aborts a book pull in flight (a slow indexer would otherwise never get an answer through)
  const reloadBook = useCoalescedReload(book);
  const refreshAll = (manual = false) => {
    if (manual) book.reload();
    else reloadBook();
    trades.reload();
    stats.reload();
    depth.reload();
    setLiveKey((k) => k + 1);
  };
  const mode = useLiveRefresh(services, [`book:${id}`, `fills:${id}`], (e) => !e.token || e.token === id, () => refreshAll());
  useInterval(() => setBalKey((k) => k + 1), 15_000, !!wallet.info);

  // every book row carries its own scale; the market's is the token's standard scale (`10^decimals`, at most 10^9): prices per whole token
  const decimals = row?.decimals ?? 0;
  const scale = row?.scale ?? null;
  const bookScale = scale ?? 1n;

  // orientation (orientation.ts): TOKEN/KAS natively (the default of every market, a USD reference token included), KAS/TOKEN inverted. Display only. Needs the scale.
  const usdRef = services.config.quoteTokens[id.toLowerCase()] === 'USD';
  const [invertedChoice, toggleInverted, setInverted] = useInverted(marketKey('token', id), defaultInverted(usdRef));
  const canFlip = scale !== null;
  const inverted = invertedChoice && canFlip;
  const name = row?.ticker ?? '';

  // price grouping: multiples of the registry tick, or of the step the visible prices sit on
  const baseTick = useMemo(() => {
    if (row?.tick && row.tick > 0n) return row.tick;
    return book.data ? inferTick([...levelsFromView(book.data.asks, bookScale), ...levelsFromView(book.data.bids, bookScale)].map((l) => l.price)) : 1n;
  }, [row?.tick, book.data]);
  const groups = useMemo(() => groupSizes(baseTick), [baseTick]);
  const groupIdx = Math.min(inverted ? groupIdxInv : groupIdxNative, groups.length - 1);
  const group = groups[groupIdx]!;
  // the book without grouping: the reference price of the page when there is neither a 24 h last price nor a trade
  const refMid = useMemo(() => {
    const m = book.data ? buildBookModel(book.data, { scale: bookScale, depth: 1 }) : null;
    return m ? m.mid : null;
  }, [book.data]);

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
        ? INVERTED_GROUP_MULTIPLIERS.map((m, i) => ({ value: String(i), label: `${m}x · ${ratioText({ num: m, den: 10n ** BigInt(dp) }, dp, '').replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '')}` }))
        : groups.map((g, i) => ({ value: String(i), label: `${10 ** i}x · ${priceText(g, decimals, bookScale, 10).replace(/(\.\d*?)0+$/, '$1').replace(/\.$/, '')}` })),
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
    if (depth.data === null && freshModel && scale) {
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
        ? { price: t('market.book.priceIn', { unit: name }), size: t('market.book.sizeIn', { unit: 'KAS' }), total: t('market.book.totalIn', { unit: name }) }
        : { price: t('market.book.priceIn', { unit: 'KAS' }), size: t('market.book.sizeIn', { unit: name || t('common.baseUnits') }), total: t('market.book.totalIn', { unit: 'KAS' }) },
    }),
    [name, decimals, bookScale, dp, baseTick, inverted],
  );
  const bookView = useMemo(
    () => (model && !model.empty ? (inverted && scale !== null ? flippedBookView(model, bookCtx) : nativeBookView(model, bookCtx)) : null),
    [model, bookCtx, inverted, scale],
  );

  if (!row) {
    if (tokens.loading) return <Loading />;
    return (
      <div class="stack" data-testid="token-not-found">
        <Banner tone="warn" title={t('market.token.notFound')}>{t('market.token.notFoundBody', { id })}</Banner>
        <div><a href="#/market">{t('market.token.back')}</a></div>
      </div>
    );
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
          badges={liveBadge}
          pair={
            <>
              {canFlip ? <FlipButton pair={pairLabel(name, !inverted)} pressed={inverted} onClick={toggleInverted} /> : null}
              <PairSelector pair={displayedPair(id, null, inverted)} rows={allRows} onChange={onPair} />
            </>
          }
        />
      }
      notices={<CrossCheckBanner token={id} refreshKey={liveKey} />}
      stats={<MarketStats model={statsForView} ticker={name} inverted={inverted} loading={!!indexer && stats.data === undefined && stats.loading} unsupported={stats.data === null && !!indexer} />}
      book={
        <Section
          title={t('market.book.title')}
          data-testid="book-section"
          class="mkt-area-book mkt-card"
          actions={
            <>
              {stale ? <Badge tone={health.assessment.level === 'bad' ? 'bad' : 'warn'} data-testid="book-stale">{t('market.book.stale')}</Badge> : null}
              <BookGrouping options={groupOptions} value={String(groupIdx)} onChange={(v) => (inverted ? setGroupIdxInv : setGroupIdxNative)(Math.max(0, Number(v) || 0))} />
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
          <PriceChart indexer={indexer} token={id} decimals={decimals} ticker={name} dp={dp} refreshKey={liveKey} inverted={inverted} />
        </Section>
      }
      depth={
        <Section title={t('market.depth.title')} data-testid="depth-section" class="mkt-area-depth mkt-card">
          <DepthChart series={depthSeries} loading={!!indexer && depth.data === undefined} ticker={inverted ? 'KAS' : name} priceUnit={inverted ? name : 'KAS'} inverted={inverted} dp={dp} amountDp={inverted ? Math.min(tickKasFraction(baseTick), 4) : Math.min(decimals, 4)} source={depth.data ? 'depth' : 'book'} />
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
              {wallet.networkMismatch ? <Banner tone="error" data-testid="ticket-network-blocked">{t('market.ticket.networkBlocked')}</Banner> : null}
              <OrderTicket token={info} prefill={prefill} powers={row.powers} inverted={inverted} />
            </div>
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
              </Banner>
            </Section>
          )}
          {info ? <WalletTokenBalances token={info} refreshKey={balKey} /> : null}
        </div>
      }
      details={<TokenDetails row={row} services={services} />}
    />
  );
}
