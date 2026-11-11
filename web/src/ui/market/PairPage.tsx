// `#/market/<base>/<quote>`: a token/token pair A/B (BASE/QUOTE, e.g. BTC/USDT). The page shows the pair book of
// `GET /v1/pairs/{base}/{quote}/book` (resting pair orders = direct levels, if-done pair entries resting at their limit = entry levels, quotes
// through the two KAS books = route levels, "via KAS") and the unified order ticket with B as its quote (every order type of the KAS ticket, prices
// in B per whole A, tips KAS), and the same panels as a token market in the same places: 24 h strip, chart, depth chart (of the pair book), the
// pair's recent fills and the facts of both tokens (pair-market.ts). A token is resolved as on its own page (use-market-token.ts): no registry entry is needed,
// and a token the page cannot trade yet says why (pair-model.ts `pairSideOf`).
//
// The founder price rule: prices come only from KAS-book fills. The chart is the indexer's pair candles (derived from the two KAS series) or the
// client-side ratio of the two KAS candle series; last / change / high / low are KAS-derived; pair fills are VOLUME only (the volume tiles and the
// fills list, which has no price column). Flip = swap of base and quote (another pair): every panel follows. Live: `book:<base>` and `book:<quote>`
// trigger a debounced refetch (5 s polling without the socket). An indexer without the pair routes (404 / 405 / 501) shows "pair book unavailable"
// and still lets the user place orders.
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { OrdersView } from '../orders/OrdersView';
import { FiatNote } from '../../app/use-fiat';
import { useDeclarationLookup } from '../../app/use-declaration';
import { pageTitle, useDocumentTitle } from '../../app/page-title';
import { navigate, pairRoute, replaceRoute, routeToHash, tokenRoute } from '../../app/router';
import { t } from '../../i18n';
import type { PairToken } from '../../kob/pair';
import { longId, type TokenInfo } from '../../kob/registry';
import { Badge, Banner, Button, ErrorBanner, scaleOfDecimals, Section, useAsync } from '../kit';
import { OrderTicket } from '../ticket/OrderTicket';
import { requestPairFlip } from '../ticket/pair-flip';
import { DepthChart } from './DepthChart';
import { FavoriteStar } from './FavoriteStar';
import { isFavoritePair, togglePair, useFavorites } from './favorites';
import { PriceChart, type UsdChartSpec } from './PriceChart';
import { useLiveRefresh } from './live';
import { FlipButton, MarketFrame, MarketShell, PairSelector, useWideMarket } from './MarketBar';
import { ForgetMarket } from './ForgetMarket';
import { settled } from './market-list';
import { openPair } from './market-nav';
import { displayedPair } from './market-select';
import { MarketStats } from './MarketStats';
import { basisOf, INTERVAL_MS, parseCandles, statsModel as noStats } from './market-model';
import { PairBook } from './PairBook';
import { useStaleSince } from './book-stale';
import { loadCandles, type ChartView } from './candle-feed';
import { BookFrame, StaleSince } from './OrderBook';
import { buildPairBook, bridgeQuoteFlip, mergeOrientations, pairSideOf, pickOf, type PairLevelRow } from './pair-model';
import { useGenesisCheck } from './use-genesis-check';
import { idleLeg, pairDepthSeries, pairRatioCandles, pairStatsModel, pairVolumeOf } from './pair-market';
import { mergePairFills } from '../activity/activity-model';
import { MarketActivity } from '../activity/ActivityPage';
import { TokenFacts, tokenTitle } from './TokenBadges';
import { IdentityBlock } from './Fingerprint';
import { PairDetails, TokenLine } from './TokenHeader';
import { NotTradablePage } from './NotTradablePage';
import { buildTokenRows, scaleNeedsIndexer, sharedTickerIds, tokenLabel, tradableRows, withShownRows } from './token-model';
import { useHeldUntil } from './use-market-list';
import { useMarketToken } from './use-market-token';
import { useStandalone } from '../shell/standalone';
import { ratioCandles, usdDp, usdStats } from './usd-model';
import './market.css';
import '../ticket/ticket.css';

const DEPTH = 15;
const FILLS = 50;
const CANDLE_INTERVAL = '5m' as const;
const CANDLE_LIMIT = 600;
const pairToken = (x: TokenInfo): PairToken => ({ covenantId: x.covenantId, ticker: x.ticker, decimals: x.decimals, scale: scaleOfDecimals(x.decimals) });

export function PairPage(props: { base: string; quote: string }) {
  const services = useServices();
  const wallet = useWallet();
  const { registry, indexer, kob, config } = services;
  const { favorites, change: changeFavorites } = useFavorites(config.network);
  const standalone = useStandalone(services.standalone)?.active === true;
  const [liveKey, setLiveKey] = useState(0);
  // bumps on a reorg / resync: the chart reloads in full (TokenPage)
  const [resyncKey, setResyncKey] = useState(0);
  const [prefill, setPrefill] = useState<{ side: 'buy' | 'sell'; price: bigint } | undefined>(undefined);
  const tokenList = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer, standalone]);
  const declared = useDeclarationLookup();
  const rows = useMemo(() => buildTokenRows(registry, tokenList.data ?? null, [], declared), [registry, tokenList.data, declared]);
  const choiceRows = useMemo(() => tradableRows(rows, kob.templates()), [rows, kob]);
  // listed tokens that share a ticker with another listed token: a short note at each token's facts and at the ticket
  const shared = useMemo(() => sharedTickerIds(choiceRows), [choiceRows]);
  // each token as its own page resolves it; no decimals are entered here, a first order is placed on the token's page
  const sideOptions = { rows, rowsLoading: tokenList.loading, refreshKey: liveKey, decimals: null, declared };
  const baseToken = useMarketToken(props.base, sideOptions);
  const quoteToken = useMarketToken(props.quote, sideOptions);
  const baseSide = pairSideOf(baseToken, !!indexer);
  const quoteSide = pairSideOf(quoteToken, !!indexer);
  const baseInfo = baseSide.state === 'ready' ? baseSide.info : null;
  const quoteInfo = quoteSide.state === 'ready' ? quoteSide.info : null;
  const baseRow = baseToken.headerRow;
  const quoteRow = quoteToken.headerRow;
  // a token a federated bridge issues (its genesis holds the minter KOB pins, kUSDT's BridgeMint) is the quote: shown as the base, the page turns the
  // pair round once its check says so (the book is the same either way, mergeOrientations); the history entry is replaced, so Back does not loop
  const baseGenesis = useGenesisCheck(baseRow ?? null);
  const quoteGenesis = useGenesisCheck(quoteRow ?? null);
  useEffect(() => {
    if (bridgeQuoteFlip(baseGenesis, quoteGenesis)) replaceRoute(pairRoute(props.quote, props.base));
  }, [baseGenesis, quoteGenesis, props.base, props.quote]);
  // the two tokens are always among the choices, also when no list names them
  const selectorRows = useMemo(() => withShownRows(choiceRows, [baseRow, quoteRow]), [choiceRows, baseRow, quoteRow]);
  // the tab: `BASE / QUOTE` by the tickers the page shows (a token without one: its short id)
  useDocumentTitle(pageTitle(`${baseRow?.ticker || longId(props.base)} / ${quoteRow?.ticker || longId(props.quote)}`));

  // the book of BASE/QUOTE with the resting orders of QUOTE/BASE turned round (pair-model.ts mergeOrientations): one book whichever way the pair is shown
  const book = useAsync(
    (signal) =>
      indexer
        ? Promise.all([
            indexer.pairBook(props.base, props.quote, { depth: DEPTH * 2 }, { signal }),
            indexer.pairBook(props.quote, props.base, { depth: DEPTH * 2 }, { signal }).catch(() => null),
          ]).then(([ab, ba]) => (ab ? mergeOrientations(ab, ba) : ab))
        : Promise.resolve(null),
    [indexer, props.base, props.quote],
  );
  const bookStaleSince = useStaleSince(book);
  const mode = useLiveRefresh(
    services,
    [`book:${props.base}`, `book:${props.quote}`],
    (e) => !e.token || e.token === props.base || e.token === props.quote,
    (full) => {
      book.refresh();
      setLiveKey((k) => k + 1);
      if (full) setResyncKey((k) => k + 1);
    },
  );
  // keyed on what they read: an open-list token's TokenInfo is a new object after every live refresh, and the chart must not reload for that alone
  const base = useMemo(() => (baseInfo ? pairToken(baseInfo) : null), [baseInfo?.covenantId, baseInfo?.ticker, baseInfo?.decimals]);
  const quote = useMemo(() => (quoteInfo ? pairToken(quoteInfo) : null), [quoteInfo?.covenantId, quoteInfo?.ticker, quoteInfo?.decimals]);
  const model = useMemo(() => (book.data && base && quote ? buildPairBook(book.data, base, quote, DEPTH) : null), [book.data, base, quote]);

  // last price and 24 h change: the two tokens' KAS stats (the pair price is implied through the two KAS markets)
  const key = `${props.base}/${props.quote}`;
  const stats = useAsync(
    async (signal) => {
      if (!indexer || !baseInfo || !quoteInfo) return null;
      const [b, q] = await Promise.all([indexer.stats(props.base, { signal, priority: 'low' }), indexer.stats(props.quote, { signal, priority: 'low' })]);
      // the end of the 24 h window: the newest chain time the indexer reports (the wall clock when it reports none)
      const ts = Math.max(b?.ts ?? 0, q?.ts ?? 0);
      return { key, ...usdStats({ stats: b, decimals: baseInfo.decimals }, { stats: q, decimals: quoteInfo.decimals }), refMs: ts > 0 ? ts : Date.now(), baseLastTs: b?.last_ts ?? null, quoteLastTs: q?.last_ts ?? null };
    },
    [indexer, props.base, props.quote, baseInfo?.covenantId, quoteInfo?.covenantId, baseInfo?.decimals, quoteInfo?.decimals],
    { refreshKey: liveKey },
  );
  // 24 h high / low: the indexer's pair candles (derived from the two KAS series), else the client-side ratio of the two KAS candle series; a live
  // refresh reads only the bars since the newest one (candle-feed.ts), a reorg / resync the full window
  const pairCandles = useRef<{ key: string; resync: number; view: ChartView | null } | null>(null);
  const candles = useAsync(
    async (signal) => {
      if (!indexer || !base || !quote) return null;
      const c = pairCandles.current;
      const prev = c && c.key === key && c.resync === resyncKey ? c.view : null;
      let view = await loadCandles(indexer, { base: props.base, quote: props.quote, pairCandles: true }, CANDLE_INTERVAL, prev, signal, CANDLE_LIMIT);
      const fromIndexer = view?.kind === 'pair' ? pairRatioCandles(view.pc, base, quote) : null;
      // pair candles this build cannot read: the two KAS series
      if (view?.kind === 'pair' && !fromIndexer) view = await loadCandles(indexer, { base: props.base, quote: props.quote }, CANDLE_INTERVAL, null, signal, CANDLE_LIMIT);
      pairCandles.current = { key, resync: resyncKey, view };
      if (fromIndexer) return { key, pair: true, candles: fromIndexer };
      const bb = view?.kind === 'usd' ? basisOf(view.b.price_basis) : null;
      const qb = view?.kind === 'usd' ? basisOf(view.q.price_basis) : null;
      if (view?.kind !== 'usd' || !bb || !qb) return { key, pair: false, candles: [] };
      const rc = ratioCandles(
        { candles: parseCandles(view.b.items), decimals: base.decimals, basis: bb },
        { candles: parseCandles(view.q.items), decimals: quote.decimals, basis: qb },
        { intervalMs: INTERVAL_MS[CANDLE_INTERVAL], until: Date.now(), maxBars: CANDLE_LIMIT },
      );
      return { key, pair: false, candles: rc };
    },
    [indexer, props.base, props.quote, base, quote],
    { refreshKey: liveKey },
  );
  // the pair's fills (volume only: amounts of A and B, counterparty, time) and the indexer's 24 h pair volume
  // both orientations (activity-model.ts mergePairFills): the volume and the log count the orders of QUOTE/BASE too
  const fills = useAsync(
    (signal) =>
      indexer
        ? Promise.all([
            indexer.pairFills(props.base, props.quote, { limit: FILLS }, { signal, priority: 'low' }),
            indexer.pairFills(props.quote, props.base, { limit: FILLS }, { signal, priority: 'low' }).catch(() => null),
          ]).then(([ab, ba]) => (ab ? mergePairFills(ab, ba) : ab))
        : Promise.resolve(null),
    [indexer, props.base, props.quote],
    {
    refreshKey: liveKey,
  });

  // the book and the 24 h strip show together, once for the page (TokenPage); the ticket waits for none of this. Their reads wait for the two tokens
  const tokensIn = !!base && !!quote;
  const panelsOpen = useHeldUntil(tokensIn && settled(book) && settled(stats) && settled(fills), tokensIn);

  // a result of the other orientation (right after a flip) is never used
  const cur = stats.data && stats.data.key === key ? stats.data : undefined;
  const cc = candles.data && candles.data.key === key ? candles.data : undefined;
  const last = cur?.last ?? null;
  const change = cur?.changeBps ?? null;
  const dp = usdDp(last);
  const pairStats = useMemo(
    () =>
      base && quote
        ? pairStatsModel({
            last,
            changeBps: change,
            dp,
            candles: cc?.candles ?? [],
            pairCandles: cc?.pair ?? false,
            intervalMs: INTERVAL_MS[CANDLE_INTERVAL],
            refMs: cur?.refMs ?? Date.now(),
            volume: pairVolumeOf(fills.data, base, quote),
            base,
            quote,
          })
        : null,
    [base, quote, last, change, dp, cc, cur, fills.data],
  );
  // the activity log reads this pair's fills from here (the route is heavy: never read twice); a read that ended without an answer is `null`, so the log does not wait for it
  const fillsView = settled(fills) ? (fills.data ?? null) : undefined;
  const pairFillsSrc = useMemo(() => ({ base: props.base, quote: props.quote, view: fillsView }), [props.base, props.quote, fillsView]);
  const depthSeries = useMemo(() => (book.data && base && quote ? pairDepthSeries(book.data, base, quote) : null), [book.data, base, quote]);
  // three columns: the depth chart is a panel of its own; below that it is a tab of the price chart's panel
  const wide = useWideMarket();

  const sides = [baseSide, quoteSide];
  const blocked = sides.find((s) => s.state === 'blocked');
  if (blocked) return <NotTradablePage ticker={blocked.row.ticker} covenantId={blocked.row.covenantId} reason={blocked.reason ? t(blocked.reason) : null} />;
  if (sides.some((s) => s.state === 'loading')) return <MarketFrame bookTitle={t('pair.book.title')} data-testid="pair-loading" />;
  const back = <div><a href="#/market">{t('market.token.back')}</a></div>;
  if (sides.some((s) => s.state === 'unknown')) {
    return (
      <div class="stack" data-testid="pair-not-found">
        <ForgetMarket />
        <Banner tone="warn" title={t('pair.notFound')}>
          {t('pair.notFoundBody')}
          {sides.map((s, i) => (s.state === 'unknown' && s.reason ? <div key={i} data-testid="pair-public-reason">{t(s.reason)}</div> : null))}
        </Banner>
        {back}
      </div>
    );
  }
  const noScale = sides.flatMap((s) => (s.state === 'no-scale' ? [s.row] : []));
  if (noScale.length) {
    return (
      <div class="stack" data-testid="pair-no-scale">
        <Banner tone="info" title={t('pair.noScale.title')}>{t('pair.noScale.body')}</Banner>
        {noScale.map((r) => <div key={r.covenantId}><a href={routeToHash(tokenRoute(r.covenantId))} data-testid="pair-no-scale-link">{t('pair.openToken', { token: tokenLabel(r) })}</a></div>)}
        {back}
      </div>
    );
  }
  const stopped = sides.flatMap((s) => (s.state === 'unavailable' ? [s] : []));
  if (stopped.length) {
    return (
      <div class="stack" data-testid="pair-token-unavailable">
        <Banner tone="warn" title={t('pair.unavailable')}>
          {stopped.map((s) => <div key={s.row.covenantId} data-testid="pair-token-unavailable-reason" data-token={s.row.covenantId}>{`${tokenLabel(s.row)}: ${t(s.reason)}`}</div>)}
        </Banner>
        {stopped.map((s) => <div key={s.row.covenantId}><a href={routeToHash(tokenRoute(s.row.covenantId))}>{t('pair.openToken', { token: tokenLabel(s.row) })}</a></div>)}
        {back}
      </div>
    );
  }
  if (!baseInfo || !quoteInfo || !base || !quote || !pairStats) return <MarketFrame bookTitle={t('pair.book.title')} data-testid="pair-loading" />;
  // a leg that has not traded for a day only carries its last price: the chart then shows the other leg's moves, which the page says
  const idleBase = !!cur && idleLeg(cur.baseLastTs, cur.refMs);
  const idleQuote = !!cur && idleLeg(cur.quoteLastTs, cur.refMs);
  const idleNote = idleBase && idleQuote
    ? t('pair.chart.idleBoth')
    : idleBase || idleQuote
      ? t('pair.chart.idle', { ticker: (idleBase ? baseInfo : quoteInfo).ticker, other: (idleBase ? quoteInfo : baseInfo).ticker })
      : null;
  const spec: UsdChartSpec = { base: { token: props.base, decimals: baseInfo.decimals }, quote: { token: props.quote, decimals: quoteInfo.decimals }, pairCandles: true };
  const statsModel = pairStats.model;
  const src = pairStats.source;
  const volumeNote = src === 'fills' ? t('pair.stats.srcFills', { n: pairStats.fills }) : src === 'candles' ? t('pair.stats.srcCandles', { n: pairStats.fills }) : t('pair.stats.srcNone');
  const rangeNote = statsModel.high !== null ? t('pair.stats.srcRange') : null;
  const statsLoading = !!indexer && ((stats.data === undefined && stats.loading) || (candles.data === undefined && candles.loading) || (fills.data === undefined && fills.loading));
  const unavailable = !!indexer && book.data === null && !book.loading && !book.error;
  const onPick = (row: PairLevelRow) => setPrefill(pickOf(row, base));
  const depthChart = (
    <DepthChart
      series={depthSeries}
      loading={!!indexer && book.data === undefined && !book.error}
      unavailable={!!book.error && !depthSeries}
      ticker={baseInfo.ticker}
      priceUnit={t('market.stats.unitOf', { base: baseInfo.ticker, quote: quoteInfo.ticker })}
      dp={dp}
      amountDp={Math.min(base.decimals, 4)}
      source="book"
    />
  );
  return (
    <MarketShell
      data-testid="pair-page"
      attrs={{ 'data-base': props.base, 'data-quote': props.quote }}
      // the wallet's summary under the market shows its values in USD when that is chosen: the note says what the dollars are (the pair's own figures stay in their tokens)
      notices={<FiatNote />}
      header={
        <div class="stack-sm" data-testid="pair-header">
          {/* line 1, always one line: the two tokens' pictures (icon and fingerprint of each), the pair selector and the swap button, then Live (which alone may wrap
              on a very narrow phone); then one compact line per token (short id with copy, name, chips). The base token's block is where the token page has it. */}
          <div class="mkt-pairhd">
            <div class="mkt-pairhd-main">
              <IdentityBlock row={baseRow ?? { covenantId: props.base, ticker: baseInfo.ticker, kascov: null }} size="pair" data-testid="pair-base-identity" />
              <span class="pair-sep mkt-pairhd-sep" aria-hidden="true">{'/'}</span>
              <span data-testid="pair-identities">
                <IdentityBlock row={quoteRow ?? { covenantId: props.quote, ticker: quoteInfo.ticker, kascov: null }} size="pair" data-testid="pair-quote-identity" />
              </span>
              <span class="mkt-pairhd-tickers">
                <PairSelector pair={displayedPair(props.base, props.quote, false)} rows={selectorRows} onChange={openPair} />
                <span class="sr-only" data-testid="pair-subtitle">{t('pair.subtitle', { base: baseRow ? tokenTitle(baseRow) : baseInfo.ticker, quote: quoteRow ? tokenTitle(quoteRow) : quoteInfo.ticker })}</span>
              </span>
              <FlipButton
                pair={`${quoteInfo.ticker}/${baseInfo.ticker}`}
                onClick={() => {
                  // the ticket of the other pair takes the order across: the side turns over, amounts and prices are converted (pair-flip.ts)
                  requestPairFlip(props.base, props.quote);
                  navigate(pairRoute(props.quote, props.base));
                }}
              />
            </div>
            <FavoriteStar
              on={isFavoritePair(favorites, props.base, props.quote)}
              name={`${baseRow ? tokenTitle(baseRow) : baseInfo.ticker} / ${quoteRow ? tokenTitle(quoteRow) : quoteInfo.ticker}`}
              onToggle={() => changeFavorites((f) => togglePair(f, props.base, props.quote))}
              data-testid="pair-favorite"
            />
            <Badge tone={mode === 'live' ? 'ok' : mode === 'polling' ? 'info' : 'neutral'} data-testid="pair-live" title={t(`market.live.${mode}Hint`)}>
              {t(`market.live.${mode}`)}
            </Badge>
          </div>
          <div class="mkt-pairhd-tokens" data-testid="pair-token-lines">
            {[baseRow, quoteRow].map((r) => (r ? <TokenLine key={r.covenantId} row={r} ticker data-testid="pair-token-meta" /> : null))}
          </div>
        </div>
      }
      stats={
        <MarketStats
          open={panelsOpen}
          model={panelsOpen ? statsModel : noStats(null, 0)}
          ticker={baseInfo.ticker}
          quoteTicker={quoteInfo.ticker}
          loading={!panelsOpen || statsLoading}
          unsupported={false}
          volumeSource={src}
          titles={{ high: rangeNote, low: rangeNote, volume: volumeNote, quoteVolume: volumeNote, trades: volumeNote }}
        />
      }
      book={
        <Section
          title={t('pair.book.title')}
          data-testid="pair-book-section"
          class="mkt-area-book mkt-card"
          actions={
            <Button small variant="ghost" onClick={book.reload} loading={book.loading && !!book.data} title={t('common.refresh')} aria-label={t('common.refresh')} data-testid="pair-refresh">
              {'↻'}
            </Button>
          }
        >
          {!indexer ? <Banner tone="info">{t('market.book.noIndexer')}</Banner> : null}
          <ErrorBanner error={book.data ? null : book.error} title={t('pair.book.error')} onRetry={book.reload} data-testid="pair-book-error" />
          <StaleSince since={bookStaleSince} data-testid="pair-book-stale-since" />
          {unavailable ? <Banner tone="warn" data-testid="pair-book-unavailable" title={t('pair.book.unavailable')}>{t('pair.book.unavailableBody')}</Banner> : null}
          {book.data && !model ? <Banner tone="warn" data-testid="pair-book-mismatch">{t('pair.book.mismatch')}</Banner> : null}
          {indexer && (model ? !panelsOpen : book.loading && !book.data) ? (
            <BookFrame head={{ price: t('pair.book.price', { base: baseInfo.ticker, quote: quoteInfo.ticker }), size: t('pair.book.amount', { base: baseInfo.ticker }), total: t('pair.book.source') }} rows={12} data-testid="pair-book-loading" />
          ) : null}
          {panelsOpen && model && model.empty ? <p class="muted center" data-testid="pair-book-empty">{t('pair.book.empty')}</p> : null}
          {panelsOpen && model && !model.empty ? <PairBook model={model} base={baseInfo.ticker} quote={quoteInfo.ticker} onPick={onPick} /> : null}
          <div class="pair-legend">
            <span>{t('pair.book.legendDirect')}</span>
            <span>{t('pair.book.legendEntry')}</span>
            <span>{t('pair.book.legendRoute')}</span>
          </div>
        </Section>
      }
      chart={
        <Section title={t(wide ? 'market.chart.title' : 'market.chart.titleTabs')} data-testid="chart-section" class="mkt-area-chart mkt-card">
          <PriceChart indexer={indexer} token={`${props.base}/${props.quote}`} decimals={baseInfo.decimals} ticker={baseInfo.ticker} quoteTicker={quoteInfo.ticker} dp={dp} refreshKey={liveKey} resyncKey={resyncKey} usd={spec} {...(wide ? {} : { depth: depthChart })} />
          <p class="small muted" style="margin:6px 0 0" data-testid="pair-chart-note">{t('pair.chart.note', { base: baseInfo.ticker, quote: quoteInfo.ticker })}</p>
          {idleNote ? <p class="small muted" style="margin:4px 0 0" data-testid="pair-chart-idle">{idleNote}</p> : null}
        </Section>
      }
      depth={
        wide ? (
          <Section title={t('market.depth.title')} data-testid="depth-section" class="mkt-area-depth mkt-card">
            {depthChart}
          </Section>
        ) : undefined
      }
      ticket={
        <div class="stack mkt-area-ticket">
          {/* an open-list token in standalone mode: the rule of the token page, for either token */}
          {standalone && [baseToken, quoteToken].some((m) => !!m.row && scaleNeedsIndexer(m.row, m.open.reason, services.standalone?.carry.scaleOf(m.row.covenantId) != null)) ? (
            <Section title={t('market.ticket.title')} data-testid="ticket-unavailable" class="mkt-card">
              <Banner tone="warn" data-testid="ticket-standalone-scale">{t('market.ticket.standaloneScale')}</Banner>
            </Section>
          ) : (
            <OrderTicket token={baseInfo} quote={quoteInfo} prefill={prefill} facts={<TokenFacts rows={[baseRow, quoteRow].filter((r): r is NonNullable<typeof r> => !!r)} compact data-testid="ticket-token-facts" />} />
          )}
          <p class="small muted">
            <a href="#/portfolio/open" data-testid="pair-my-orders">{t('pair.myOrders')}</a>
          </p>
        </div>
      }
      mine={wallet.info ? <OrdersView embed={{ base: props.base, quote: props.quote }} /> : undefined}
      activity={<MarketActivity market={{ kind: 'pair', base: props.base, quote: props.quote }} pairFills={pairFillsSrc} />}
      details={<PairDetails shared={shared} rows={[baseRow, quoteRow].filter((r): r is NonNullable<typeof r> => !!r)} services={services} />}
    />
  );
}
