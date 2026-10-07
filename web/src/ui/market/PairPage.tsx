// `#/market/<base>/<quote>` (alias `#/pair/<base>/<quote>`): a token/token pair A/B (BASE/QUOTE, e.g. BTC/USDT). The page shows the pair book of
// `GET /v1/pairs/{base}/{quote}/book` (resting pair orders = direct levels, if-done pair entries resting at their limit = entry levels, quotes
// through the two KAS books = route levels, "via KAS") and the unified order ticket with B as its quote (every order type of the KAS ticket, prices
// in B per whole A, tips KAS), and the same panels as a token market in the same places: 24 h strip, chart, depth chart (of the pair book), the
// pair's recent fills and the facts of both tokens (pair-market.ts).
//
// The founder price rule: prices come only from KAS-book fills. The chart is the indexer's pair candles (derived from the two KAS series) or the
// client-side ratio of the two KAS candle series; last / change / high / low are KAS-derived; pair fills are VOLUME only (the volume tiles and the
// fills list, which has no price column). Flip = swap of base and quote (another pair): every panel follows. Live: `book:<base>` and `book:<quote>`
// trigger a debounced refetch (5 s polling without the socket). An indexer without the pair routes (404 / 405 / 501) shows "pair book unavailable"
// and still lets the user place orders.
import { useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { navigate, pairRoute } from '../../app/router';
import { t } from '../../i18n';
import type { PairToken } from '../../kob/pair';
import type { TokenInfo } from '../../kob/registry';
import { Badge, Banner, Button, ErrorBanner, scaleOfDecimals, Section, useAsync } from '../kit';
import { OrderTicket } from '../ticket/OrderTicket';
import { requestPairFlip } from '../ticket/pair-flip';
import { DepthChart } from './DepthChart';
import { PriceChart, type UsdChartSpec } from './PriceChart';
import { useLiveRefresh } from './live';
import { FlipButton, MarketShell, PairSelector } from './MarketBar';
import { openPair } from './market-nav';
import { displayedPair } from './market-select';
import { MarketStats } from './MarketStats';
import { basisOf, INTERVAL_MS, parseCandles } from './market-model';
import { PairBook } from './PairBook';
import { useCoalescedReload, useStaleSince } from './book-stale';
import { StaleSince } from './OrderBook';
import { buildPairBook, pickOf, type PairLevelRow } from './pair-model';
import { pairDepthSeries, pairFillsOf, pairRatioCandles, pairStatsModel, pairVolumeOf } from './pair-market';
import { PairTrades } from './PairTrades';
import { tokenTitle } from './TokenBadges';
import { PairDetails } from './TokenHeader';
import { buildTokenRows } from './token-model';
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
  const { registry, indexer } = services;
  const baseInfo = registry.byCovenantId.get(props.base) ?? null;
  const quoteInfo = registry.byCovenantId.get(props.quote) ?? null;
  const [liveKey, setLiveKey] = useState(0);
  const [prefill, setPrefill] = useState<{ side: 'buy' | 'sell'; price: bigint } | undefined>(undefined);
  const tokenList = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer]);
  const rows = useMemo(() => buildTokenRows(registry, tokenList.data ?? null), [registry, tokenList.data]);
  const baseRow = rows.find((r) => r.covenantId === props.base);
  const quoteRow = rows.find((r) => r.covenantId === props.quote);

  const book = useAsync((signal) => (indexer ? indexer.pairBook(props.base, props.quote, { depth: DEPTH * 2 }, { signal }) : Promise.resolve(null)), [indexer, props.base, props.quote]);
  const reloadBook = useCoalescedReload(book);
  const bookStaleSince = useStaleSince(book);
  const mode = useLiveRefresh(
    services,
    [`book:${props.base}`, `book:${props.quote}`],
    (e) => !e.token || e.token === props.base || e.token === props.quote,
    () => {
      reloadBook();
      setLiveKey((k) => k + 1);
    },
  );
  const base = useMemo(() => (baseInfo ? pairToken(baseInfo) : null), [baseInfo]);
  const quote = useMemo(() => (quoteInfo ? pairToken(quoteInfo) : null), [quoteInfo]);
  const model = useMemo(() => (book.data && base && quote ? buildPairBook(book.data, base, quote, DEPTH) : null), [book.data, base, quote]);

  // last price and 24 h change: the two tokens' KAS stats (the pair price is implied through the two KAS markets)
  const key = `${props.base}/${props.quote}`;
  const stats = useAsync(
    async (signal) => {
      if (!indexer || !baseInfo || !quoteInfo) return null;
      const [b, q] = await Promise.all([indexer.stats(props.base, { signal }), indexer.stats(props.quote, { signal })]);
      // the end of the 24 h window: the newest chain time the indexer reports (the wall clock when it reports none)
      const ts = Math.max(b?.ts ?? 0, q?.ts ?? 0);
      return { key, ...usdStats({ stats: b, decimals: baseInfo.decimals }, { stats: q, decimals: quoteInfo.decimals }), refMs: ts > 0 ? ts : Date.now() };
    },
    [indexer, props.base, props.quote, baseInfo?.covenantId, quoteInfo?.covenantId, liveKey],
  );
  // 24 h high / low: the indexer's pair candles (derived from the two KAS series), else the client-side ratio of the two KAS candle series
  const candles = useAsync(
    async (signal) => {
      if (!indexer || !base || !quote) return null;
      const pc = await indexer.pairCandles(props.base, props.quote, { interval: CANDLE_INTERVAL, limit: CANDLE_LIMIT }, { signal });
      const fromIndexer = pc ? pairRatioCandles(pc, base, quote) : null;
      if (fromIndexer) return { key, pair: true, candles: fromIndexer };
      const [b, q] = await Promise.all([
        indexer.candles(props.base, { interval: CANDLE_INTERVAL, limit: CANDLE_LIMIT }, { signal }),
        indexer.candles(props.quote, { interval: CANDLE_INTERVAL, limit: CANDLE_LIMIT }, { signal }),
      ]);
      const bb = b ? basisOf(b.price_basis) : null;
      const qb = q ? basisOf(q.price_basis) : null;
      if (!b || !q || !bb || !qb) return { key, pair: false, candles: [] };
      const rc = ratioCandles(
        { candles: parseCandles(b.items), decimals: base.decimals, basis: bb },
        { candles: parseCandles(q.items), decimals: quote.decimals, basis: qb },
        { intervalMs: INTERVAL_MS[CANDLE_INTERVAL], until: Date.now(), maxBars: CANDLE_LIMIT },
      );
      return { key, pair: false, candles: rc };
    },
    [indexer, props.base, props.quote, base, quote, liveKey],
  );
  // the pair's fills (volume only: amounts of A and B, counterparty, time) and the indexer's 24 h pair volume
  const fills = useAsync((signal) => (indexer ? indexer.pairFills(props.base, props.quote, { limit: FILLS }, { signal }) : Promise.resolve(null)), [indexer, props.base, props.quote, liveKey]);

  // a result of the other orientation (right after a flip) is never used
  const cur = stats.data && stats.data.key === key ? stats.data : undefined;
  const cc = candles.data && candles.data.key === key ? candles.data : undefined;
  const last = cur?.last ?? null;
  const change = cur?.changeBps ?? null;
  const dp = usdDp(last);
  const pairFills = useMemo(() => (fills.data && base && quote ? pairFillsOf(fills.data, base, quote) : fills.data === null ? [] : undefined), [fills.data, base, quote]);
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
  const depthSeries = useMemo(() => (book.data && base && quote ? pairDepthSeries(book.data, base, quote) : null), [book.data, base, quote]);
  const powers = useMemo(() => [...new Set([...(baseRow?.powers ?? []), ...(quoteRow?.powers ?? [])])], [baseRow, quoteRow]);

  if (!baseInfo || !quoteInfo || !base || !quote || !pairStats) {
    return (
      <div class="stack" data-testid="pair-not-found">
        <Banner tone="warn" title={t('pair.notFound')}>{t('pair.notFoundBody')}</Banner>
        <div><a href="#/market">{t('market.token.back')}</a></div>
      </div>
    );
  }
  const spec: UsdChartSpec = { base: { token: props.base, decimals: baseInfo.decimals }, quote: { token: props.quote, decimals: quoteInfo.decimals }, pairCandles: true };
  const statsModel = pairStats.model;
  const src = pairStats.source;
  const volumeNote = src === 'fills' ? t('pair.stats.srcFills', { n: pairStats.fills }) : src === 'candles' ? t('pair.stats.srcCandles', { n: pairStats.fills }) : t('pair.stats.srcNone');
  const rangeNote = statsModel.high !== null ? t('pair.stats.srcRange') : null;
  const statsLoading = !!indexer && ((stats.data === undefined && stats.loading) || (candles.data === undefined && candles.loading) || (fills.data === undefined && fills.loading));
  const unavailable = !!indexer && book.data === null && !book.loading && !book.error;
  const onPick = (row: PairLevelRow) => setPrefill(pickOf(row, base));
  return (
    <MarketShell
      data-testid="pair-page"
      attrs={{ 'data-base': props.base, 'data-quote': props.quote }}
      header={
        <div class="mkt-top">
          <div class="mkt-title">
            <FlipButton
              pair={`${quoteInfo.ticker}/${baseInfo.ticker}`}
              onClick={() => {
                // the ticket of the other pair takes the order across: the side turns over, amounts and prices are converted (pair-flip.ts)
                requestPairFlip(props.base, props.quote);
                navigate(pairRoute(props.quote, props.base));
              }}
            />
            <PairSelector pair={displayedPair(props.base, props.quote, false)} rows={rows} onChange={openPair} />
            <span class="muted small" data-testid="pair-subtitle" title={t('pair.explainer', { base: baseInfo.ticker, quote: quoteInfo.ticker })}>{t('pair.subtitle', { base: baseRow ? tokenTitle(baseRow) : baseInfo.ticker, quote: quoteRow ? tokenTitle(quoteRow) : quoteInfo.ticker })}</span>
            <Badge tone={mode === 'live' ? 'ok' : mode === 'polling' ? 'info' : 'neutral'} data-testid="pair-live" title={t(`market.live.${mode}Hint`)}>
              {t(`market.live.${mode}`)}
            </Badge>
          </div>
        </div>
      }
      stats={
        <MarketStats
          model={statsModel}
          ticker={baseInfo.ticker}
          quoteTicker={quoteInfo.ticker}
          loading={statsLoading}
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
          {indexer && book.loading && !book.data ? <div data-testid="pair-book-loading">{Array.from({ length: 8 }, (_, i) => <div key={i} class="skeleton skeleton-line" />)}</div> : null}
          {model && model.empty ? <p class="muted center" data-testid="pair-book-empty">{t('pair.book.empty')}</p> : null}
          {model && !model.empty ? <PairBook model={model} base={baseInfo.ticker} quote={quoteInfo.ticker} onPick={onPick} /> : null}
          <div class="pair-legend">
            <span>{t('pair.book.legendDirect')}</span>
            <span>{t('pair.book.legendEntry')}</span>
            <span>{t('pair.book.legendRoute')}</span>
          </div>
        </Section>
      }
      chart={
        <Section title={t('market.chart.title')} data-testid="chart-section" class="mkt-area-chart mkt-card">
          <PriceChart indexer={indexer} token={`${props.base}/${props.quote}`} decimals={baseInfo.decimals} ticker={baseInfo.ticker} quoteTicker={quoteInfo.ticker} dp={dp} refreshKey={liveKey} usd={spec} />
          <p class="small muted" style="margin:6px 0 0" data-testid="pair-chart-note">{t('pair.chart.note', { base: baseInfo.ticker, quote: quoteInfo.ticker })}</p>
        </Section>
      }
      depth={
        <Section title={t('market.depth.title')} data-testid="depth-section" class="mkt-area-depth mkt-card">
          <DepthChart
            series={depthSeries}
            loading={!!indexer && book.data === undefined}
            ticker={baseInfo.ticker}
            priceUnit={quoteInfo.ticker}
            dp={dp}
            amountDp={Math.min(base.decimals, 4)}
            source="book"
          />
        </Section>
      }
      tape={
        <div class="mkt-area-tape">
          <PairTrades fills={pairFills} loading={fills.loading} error={fills.error} onRetry={fills.reload} base={base} quote={quote} />
        </div>
      }
      ticket={
        <div class="stack mkt-area-ticket">
          <OrderTicket token={baseInfo} quote={quoteInfo} prefill={prefill} powers={powers} />
          <p class="small muted">
            <a href="#/orders" data-testid="pair-my-orders">{t('pair.myOrders')}</a>
          </p>
        </div>
      }
      details={<PairDetails rows={[baseRow, quoteRow].filter((r): r is NonNullable<typeof r> => !!r)} services={services} />}
    />
  );
}
