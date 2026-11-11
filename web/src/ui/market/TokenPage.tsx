import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { marketPageTitle, pageTitle, useDocumentTitle } from '../../app/page-title';
import { useDeclarationLookup } from '../../app/use-declaration';
import { useFiat } from '../../app/use-fiat';
import { useServices, useWallet } from '../../app/context';
import { OrdersView } from '../orders/OrdersView';
import type { TicketPreset } from '../../app/router';
import { t } from '../../i18n';
import { Badge, Banner, Button, displayPriceDp, ErrorBanner, PRICE_DP_CAP, Section, tickKasFraction, tickPriceFraction, useAsync } from '../kit';
import { OrderTicket } from '../ticket/OrderTicket';
import { CrossCheckBanner } from './CrossCheckBanner';
import { NodeDownNote } from '../shell/NodeDownNote';
import { useSystemStatus } from '../shell/StatusProvider';
import { clockText, useStandalone } from '../shell/standalone';
import { buildBookModel, groupSizesAt, inferTick, levelsFromView } from './book-model';
import { useStaleSince } from './book-stale';
import { BOOK_FETCH_DEPTH, useBookRows } from './book-rows';
import { useMarketToken } from './use-market-token';
import { FirstOrderDecimals } from './FirstOrderDecimals';
import { issuedDecimals } from '../../data/issued-decimals';
import { DepthChart } from './DepthChart';
import { useLiveRefresh } from './live';
import { basisToTokenSompi, depthFromBook, depthFromView, priceText, statsModel, tapeFromFills, tapeFromTrades } from './market-model';
import {
  INVERTED_GROUP_MULTIPLIERS, defaultInverted, depthFromBookFlipped, depthFromViewFlipped, flipDp, flippedBookView, flippedPriceText, invertedRegrouper, marketKey, nativeBookView, oppositeSide, pairLabel, ratioText,
  statsModelFlipped, tokensPerKas,
  type BookViewCtx,
} from './orientation';
import { useInverted } from './use-orientation';
import { MarketStats } from './MarketStats';
import { usdFlippedStatsModel, usdStatsModel } from './stats-usd';
import { BookFrame, BookGrouping, OrderBook, StaleSince } from './OrderBook';
import { BookPanelBody, BookViewSwitch, bookPanelClass, useBookPanelView } from './BookTabs';
import { tokenDepthPick } from './depth-pick';
import { FavoriteStar } from './FavoriteStar';
import { isFavoriteToken, toggleToken, useFavorites } from './favorites';
import { FlipButton, MarketFrame, MarketShell, PairSelector } from './MarketBar';
import { ForgetMarket } from './ForgetMarket';
import { settled } from './market-list';
import { useHeldUntil } from './use-market-list';
import { openPair } from './market-nav';
import { displayedPair, marketFor, type DisplayedPair } from './market-select';
import { PriceChart } from './PriceChart';
import { TokenFacts, tokenTitle } from './TokenBadges';
import { TokenDetails, TokenHeader } from './TokenHeader';
import { NotTradablePage } from './NotTradablePage';
import { useGenesisCheck } from './use-genesis-check';
import { applyGenesisCheck, buildTokenRows, scaleNeedsIndexer, sharedTickerIds, tradableRows, withShownRows } from './token-model';
import { MarketActivity } from '../activity/ActivityPage';
import { tradeRows } from './trades-model';
import './market.css';

/** Standalone mode (data/standalone.ts): a figure built from what was seen since the mode started says so. */
function SinceBadge(props: { since: number; area: string; /** what the indexer gave before the mode is kept: when it was read */ until?: number | null }) {
  if (props.until != null) {
    return (
      <Badge tone="info" data-testid={`${props.area}-standalone`} data-kept="1" title={t('market.standalone.keptHint')}>
        {t('market.standalone.kept', { until: clockText(props.until), since: clockText(props.since) })}
      </Badge>
    );
  }
  return (
    <Badge tone="info" data-testid={`${props.area}-standalone`} title={t('market.standalone.sinceHint')}>
      {t('market.standalone.since', { time: clockText(props.since) })}
    </Badge>
  );
}
const TAPE_LIMIT = 50;

/**
 * `#/market/<covenantId>`: an exchange layout. Title bar + 24 h stats; order book | price chart and depth | order ticket; the activity log of the market (ui/activity, full width);
 * token facts below. Live: the WebSocket feed (`book:` / `fills:`) triggers a debounced refetch, 5 s polling without it.
 */
export function TokenPage(props: { covenantId: string; ticket?: TicketPreset; amount?: string }) {
  const services = useServices();
  const wallet = useWallet();
  const { indexer: health } = useSystemStatus();
  const { registry, indexer, kob, config } = services;
  const id = props.covenantId;
  const { favorites, change: changeFavorites } = useFavorites(config.network);
  // standalone mode: the book, the trades and the figures come from what this page saw on the node since `sa.sinceMs`
  const sa = useStandalone(services.standalone);
  const standalone = sa?.active ? sa : null;
  // what the indexer gave this page before the mode (data/stream-carry.ts): the token's facts (the token list) and its market figures, with when they were read
  const carry = standalone ? services.standalone?.carry ?? null : null;
  const keptTokenAt = carry?.tokenAt(id) ?? null;
  const keptMarketAt = carry?.marketAt(id) ?? null;
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

  const tokens = useAsync((signal) => (indexer ? indexer.tokens({ signal }) : Promise.resolve(null)), [indexer, !!standalone]);
  // a token the registry does not list is named by its verified KCC-23 declaration (the selector, the title, the name)
  const declaredOf = useDeclarationLookup();
  const allRows = useMemo(() => buildTokenRows(registry, tokens.data ?? null, [], declaredOf), [registry, tokens.data, declaredOf]);
  // the pair selector offers only tokens this build can trade
  const choiceRows = useMemo(() => tradableRows(allRows, kob.templates()), [allRows, kob]);
  // another listed token uses this ticker: a short note at the title and the ticket
  const sharedTicker = useMemo(() => sharedTickerIds(choiceRows).has(id), [choiceRows, id]);
  // the decimals entered for the token's first order (used only while no order tells the scale); per token
  const [entered, setEntered] = useState<{ id: string; decimals: number } | null>(null);
  const enteredDecimals = entered && entered.id === id ? entered.decimals : null;
  // the token however it is known, its synthesised TokenInfo where it has no registry entry, and how it is named (use-market-token.ts)
  const { row, headerRow, ticketInfo, open, held, pub, shown, onChain } = useMarketToken(id, { rows: allRows, rowsLoading: tokens.loading, refreshKey: liveKey, decimals: enteredDecimals, declared: declaredOf });

  // KOB's own genesis check of the token (once per token and session, data/genesis-check.ts): "Genesis verified", "Genesis check failed", or "Genesis not verified" with the
  // reason, replacing kascov's remark; a token the registry verified is not checked. The result gates nothing.
  const genesisCheck = useGenesisCheck(headerRow ?? row ?? null);
  const checkedRow = useMemo(() => (headerRow ? applyGenesisCheck(headerRow, genesisCheck) : headerRow), [headerRow, genesisCheck]);

  // the pair selector's rows: the tradable tokens, this token named as the page names it (a token no list names has no row of its own: it is added)
  const selectorRows = useMemo(() => withShownRows(choiceRows, [headerRow ?? row]), [choiceRows, headerRow, row]);

  const book = useAsync((signal) => (indexer ? indexer.book(id, { depth: BOOK_FETCH_DEPTH, aggregate: true }, { signal }) : Promise.resolve(null)), [indexer, id]);
  // the latest trades (the last price and the page's price precision; the list of trades is the activity log below): `/v1/trades`, or the fill events of an indexer without it
  const trades = useAsync(
    async (signal) => {
      if (!indexer) return null;
      // the trades, the stats and the depth decorate the page: low fetch priority, the order ticket's reads get a connection first
      const v = await indexer.trades(id, { limit: TAPE_LIMIT }, { signal, priority: 'low' });
      if (v) return { kind: 'trades' as const, v };
      return { kind: 'fills' as const, v: await indexer.fills({ token: id, limit: TAPE_LIMIT }, { signal, priority: 'low' }) };
    },
    [indexer, id],
  );
  const stats = useAsync((signal) => (indexer ? indexer.stats(id, { signal, priority: 'low' }) : Promise.resolve(null)), [indexer, id]);
  const depth = useAsync((signal) => (indexer ? indexer.depth(id, { levels: 60 }, { signal, priority: 'low' }) : Promise.resolve(null)), [indexer, id]);
  // the book and the 24 h strip show together, once for the page: the price precision and the last price of both come from the stats and the trades (the ticket waits for none
  // of this, and a refresh or a notice after that is shown as it comes)
  const panelsOpen = useHeldUntil(settled(book) && settled(trades) && settled(stats));

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
  // entering or leaving standalone mode: everything is read again from the other source (in full); in the mode, every change of what was seen too
  const saActive = !!standalone;
  const firstMode = useRef(true);
  useEffect(() => {
    if (firstMode.current) {
      firstMode.current = false;
      return;
    }
    refreshAll(true, true);
  }, [saActive]);
  useEffect(() => {
    if (standalone) refreshAll(false);
  }, [standalone?.version]);

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
  // the unit of every price and amount on the page: KOB's name of the token, else the creator's ticker (the same label rule as the list), else its covenant id
  const name = headerRow?.ticker || shown?.ticker || '';
  // the tab: `EXKCC/KAS · KOB` (a token still being looked up, or with no name, is named by its short id, never a whole covenant id), `Token not found` when nothing names it
  useDocumentTitle(row || tokens.loading || held.loading || pub.loading ? marketPageTitle({ name: row ? name : null, id }, { name: 'KAS', id: 'kas' }) : pageTitle(t('market.token.notFound')));

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
  // price decimals: the kit's one display rule (about 5 significant digits of the reference price, at most PRICE_DP_CAP decimals; not the token's tick, which can need 8 at 33 KAS), chosen
  // once per market and shared by the book, the stats, the chart and the depth chart, so one price reads the same everywhere; the whole price is in the title where a cell shows less
  const refPerToken = useMemo(() => (refPrice ? basisToTokenSompi(refPrice.price, decimals, refPrice.basis) : null), [refPrice, decimals]);
  const dpNative = useMemo(() => displayPriceDp(refPerToken), [refPerToken]);
  const dpFlipped = useMemo(() => flipDp(refPrice ? tokensPerKas(refPrice.price, decimals, refPrice.basis) : null), [refPrice, decimals]);
  const dp = inverted ? dpFlipped : dpNative;

  // the levels per side the card's height holds (book-rows.ts)
  const { rows: bookRows, fitRef: bookFit } = useBookRows();
  // the grouped book (inverted: the steps are multiples of the displayed price step, `orientation.ts`)
  const invMult = INVERTED_GROUP_MULTIPLIERS[Math.min(groupIdxInv, INVERTED_GROUP_MULTIPLIERS.length - 1)]!;
  const regroup = useMemo(() => (inverted && scale !== null ? invertedRegrouper({ decimals, scale, dp, mult: invMult }) : undefined), [inverted, scale, decimals, dp, invMult]);
  const freshModel = useMemo(
    () => (book.data ? buildBookModel(book.data, { scale: bookScale, depth: bookRows, group: inverted || groupIdx === 0 ? 0n : group, ...(regroup ? { regroup } : {}) }) : null),
    [book.data, bookScale, group, groupIdx, inverted, regroup, bookRows],
  );
  // the live book as it is (a crossed book, matchers a moment behind, is drawn with a negative spread like any other)
  const model = freshModel;
  // the book's own decimals: the page's, or fewer for a coarser grouping (the selector is also the display precision: a step of 0.01 reads two decimals). A flipped book keeps the page's: its
  // steps are multiples of the displayed price step, so its rows show the same decimals at every grouping
  const groupDp = groupIdx === 0 ? null : tickPriceFraction(group, decimals, scale, 0, PRICE_DP_CAP);
  const bookDp = inverted ? dp : displayPriceDp(refPerToken, groupDp);
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
  // the USD display (Settings): the KAS tiles of the strip in dollars (a flipped market's prices are tokens per KAS and stay; its KAS volume tile is dollars)
  const fiat = useFiat();
  // the depth chart is a view of the order book's panel at every width (an "Order book | Depth" switch)
  const bookPanel = useBookPanelView();
  const tapeLast = useMemo(() => {
    const r = tape?.rows.find((x) => x.price !== null);
    return r && tape?.basis ? { price: r.price!, basis: tape.basis } : null;
  }, [tape]);
  const usdStrip = useMemo(
    () => (fiat ? (inverted ? usdFlippedStatsModel(statsForView, stats.data ?? null, fiat) : usdStatsModel(statsForView, stats.data ?? null, decimals, fiat, name, tapeLast)) : null),
    [fiat, inverted, statsForView, stats.data, decimals, name, tapeLast],
  );

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
      dp: bookDp,
      tick: baseTick,
      labels: inverted
        ? { price: t('market.book.priceIn', { unit: t('market.stats.unitOf', { base: 'KAS', quote: name }) }), size: t('market.book.sizeIn', { unit: 'KAS' }), total: t('market.book.totalIn', { unit: name }) }
        : { price: t('market.book.priceIn', { unit: t('market.stats.unitOf', { base: name, quote: 'KAS' }) }), size: t('market.book.sizeIn', { unit: name }), total: t('market.book.totalIn', { unit: 'KAS' }) },
    }),
    [name, decimals, bookScale, bookDp, baseTick, inverted],
  );
  const bookView = useMemo(
    () => (model && !model.empty ? (inverted && scale !== null ? flippedBookView(model, bookCtx) : nativeBookView(model, bookCtx)) : null),
    [model, bookCtx, inverted, scale],
  );

  if (!row) {
    if (tokens.loading || held.loading || pub.loading) return <MarketFrame bookTitle={t('market.book.title')} data-testid="token-loading" />;
    return (
      <div class="stack" data-testid="token-not-found">
        {/* in standalone mode a token no list names is only not known there: the Trade tab keeps leading to it */}
        {standalone ? null : <ForgetMarket />}
        <Banner tone="warn" title={standalone ? t('market.token.standaloneUnknown') : t('market.token.notFound')}>
          {standalone ? t('market.token.standaloneUnknownBody', { id, time: clockText(standalone.sinceMs ?? Date.now()) }) : t('market.token.notFoundBody', { id })}
          {pub.reason ? <div data-testid="token-public-reason">{t(`market.public.${pub.reason}`)}</div> : null}
          {held.reason && held.reason !== 'not-held' ? <div data-testid="token-held-reason">{t(`market.held.${held.reason}`)}</div> : null}
          {!wallet.info ? <div data-testid="token-held-connect">{t('market.held.connect')}</div> : null}
        </Banner>
        <div><a href="#/market">{t('market.token.back')}</a></div>
      </div>
    );
  }

  // a registry token this build cannot trade (or an open-list token whose program it does not embed, or does not trade on this network) has
  // no market page here
  if ((row.source === 'registry' && !row.tradable) || open.reason === 'program-unknown' || open.reason === 'program-ambiguous' || open.reason === 'program-unreviewed') {
    const reason = open.reason === 'program-unknown' || open.reason === 'program-ambiguous' || open.reason === 'program-unreviewed' ? t(`market.open.${open.reason}`) : row.reason ? t(`market.reason.${row.reason}`) : null;
    return <NotTradablePage ticker={row.ticker} covenantId={row.covenantId} reason={reason} />;
  }

  const info = row.info;
  // the standalone book is the node's, not an indexer's that fell behind
  const stale = !standalone && (health.assessment.level === 'warn' || health.assessment.level === 'bad');
  const liveKind = standalone ? 'standalone' : mode;
  const liveBadge = (
    <Badge tone={mode === 'live' ? 'ok' : mode === 'polling' ? 'info' : 'neutral'} data-testid="live-indicator" title={t(`market.live.${liveKind}Hint`)}>
      {t(`market.live.${liveKind}`)}
    </Badge>
  );
  const since = standalone?.sinceMs ?? null;
  // an open-list token is traded in standalone mode only with decimals known without the indexer (its declaration or its program's default): the
  // scale its orders seen since the mode started use is no evidence of the market's
  const standaloneScaleBlocked = !!standalone && scaleNeedsIndexer(row, open.reason, carry?.scaleOf(id) != null);
  // the pair selector: a change of side leads to another market; the same token with the other order is only a flip
  const onPair = (next: DisplayedPair) => {
    const m = marketFor(next);
    if (m && m.route.name === 'token' && m.route.covenantId === id && m.inverted !== null) {
      if (canFlip) setInverted(m.inverted);
      return;
    }
    openPair(next);
  };

  // a click on the depth chart prefills the ticket like a click on a book row: the book's rule (an ask a buy, a bid a sell; on a flipped market the chart's bid side
  // is the native ask side, so there the other way round) at the price under the pointer, on the market's price step
  const onPickDepth = (side: 'bid' | 'ask', price: number) => {
    if (scale === null) return;
    const p = tokenDepthPick(side, price, { inverted, decimals, scale: bookScale, tick: baseTick });
    if (p) setPrefill(p);
  };
  const depthChart = (
    <DepthChart onPick={onPickDepth} fiat={inverted ? fiat : null} series={depthSeries} loading={!!indexer && depth.data === undefined && !depth.error} unavailable={!!depth.error && !!book.error && !depthSeries} ticker={inverted ? 'KAS' : name} priceUnit={inverted ? t('market.stats.unitOf', { base: 'KAS', quote: name }) : t('market.stats.unitOf', { base: name, quote: 'KAS' })} inverted={inverted} dp={dp} amountDp={inverted ? Math.min(tickKasFraction(baseTick), 4) : Math.min(decimals, 4)} source={depth.data ? 'depth' : 'book'} />
  );

  // the 24 h figures, in the market bar (the book and the strip show together, once for the page)
  const statsStrip = panelsOpen ? (
    <MarketStats open model={usdStrip?.model ?? statsForView} ticker={name} inverted={inverted} loading={!!indexer && stats.data === undefined && stats.loading} unsupported={stats.data === null && !!indexer}
      {...(usdStrip ? { usd: true, lastTitle: usdStrip.lastTitle, titles: usdStrip.titles } : {})} />
  ) : (
    <MarketStats model={statsModel(null, decimals, dp)} ticker={name} inverted={inverted} loading unsupported={false} />
  );

  return (
    <MarketShell
      data-testid="token-page"
      attrs={{ 'data-token': id }}
      header={
        <TokenHeader
          row={checkedRow ?? row}
          sharedTicker={sharedTicker}
          stats={statsStrip}
          badges={liveBadge}
          pair={
            <>
              <PairSelector pair={displayedPair(id, null, inverted)} rows={selectorRows} onChange={onPair} />
              {canFlip ? <FlipButton pair={pairLabel(name, !inverted)} pressed={inverted} onClick={toggleInverted} /> : null}
            </>
          }
          tools={<FavoriteStar on={isFavoriteToken(favorites, id)} name={tokenTitle(checkedRow ?? row)} onToggle={() => changeFavorites((f) => toggleToken(f, id))} data-testid="token-page-favorite" />}
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
          {since !== null ? (
            <div class="row">
              <SinceBadge since={since} area="stats" until={keptMarketAt} />
              {keptTokenAt !== null ? (
                <Badge tone="info" data-testid="token-kept-standalone" title={t('market.standalone.tokenKeptHint')}>
                  {t('market.standalone.tokenKept', { time: clockText(keptTokenAt) })}
                </Badge>
              ) : null}
            </div>
          ) : null}
        </>
      }
      book={
        <Section
          title={t('market.book.title')}
          data-testid="book-section"
          class={bookPanelClass(bookPanel.view)}
          actions={
            <>
              <BookViewSwitch view={bookPanel.view} onChange={bookPanel.setView} />
              {stale ? <Badge tone={health.assessment.level === 'bad' ? 'bad' : 'warn'} data-testid="book-stale">{t('market.book.stale')}</Badge> : null}
              {/* the book's own controls keep their place while the depth chart is shown (market.css hides them, so the header does not change) */}
              <div class="ob-tools book-only">
                {/* the steps come from the book's tick: no book on screen yet (loading, indexer unreachable), no grouping to offer */}
                {book.data && panelsOpen ? <BookGrouping options={groupOptions} value={String(groupIdx)} onChange={(v) => (inverted ? setGroupIdxInv : setGroupIdxNative)(Math.max(0, Number(v) || 0))} /> : null}
                {/* the glyph itself spins while the book is pulled: no spinner is added to the header, so the grouping box beside it never moves */}
                <Button small variant="ghost" onClick={() => refreshAll(true)} title={t('common.refresh')} aria-label={t('common.refresh')} aria-busy={book.loading && !!book.data ? 'true' : undefined} data-testid="book-refresh">
                  <span class={`refresh-glyph${book.loading && !!book.data ? ' is-spinning' : ''}`} aria-hidden="true">{'↻'}</span>
                </Button>
              </div>
            </>
          }
        >
          <BookPanelBody view={bookPanel.view} depth={<>{since !== null ? <div class="book-depth-note"><SinceBadge since={since} area="depth" /></div> : null}{depthChart}</>}>
            {!indexer ? <Banner tone="info">{t('market.book.noIndexer')}</Banner> : null}
            {standalone && since !== null ? (
              <p class="small muted" data-testid="book-standalone">{t(standalone.reason === 'manual' ? 'market.book.standaloneManual' : 'market.book.standaloneAuto', { time: clockText(since) })}</p>
            ) : null}
            {/* with a book on screen a failed pull is only the subtle marker below; the banner is for a page that has no book to show */}
            <ErrorBanner error={book.data ? null : book.error} title={t('market.book.error')} onRetry={book.reload} data-testid="book-error" />
            <StaleSince since={bookStaleSince} />
            {indexer && (model ? !panelsOpen : book.loading) ? <BookFrame head={bookCtx.labels} rows={bookRows} fitRef={bookFit} data-testid="book-loading" /> : null}
            {panelsOpen && model && model.empty ? <p class="muted center" data-testid="book-empty">{t('market.book.empty')}</p> : null}
            {panelsOpen && bookView ? <OrderBook view={bookView} rows={bookRows} fitRef={bookFit} last={last} inverted={inverted} token={name} onPick={(side, price) => setPrefill({ side, price })} /> : null}
          </BookPanelBody>
        </Section>
      }
      chart={
        <Section title={t('market.chart.title')} data-testid="chart-section" class="mkt-area-chart mkt-card" actions={since !== null ? <SinceBadge since={since} area="chart" until={keptMarketAt} /> : undefined}>
          <PriceChart indexer={indexer} token={id} decimals={decimals} ticker={name} dp={dp} refreshKey={liveKey} resyncKey={resyncKey} inverted={inverted} />
        </Section>
      }
      ticket={
        <div class="stack mkt-area-ticket">
          {/* a dead Kaspa node: the one line (with its Retry) beside the ticket, whose own button is disabled meanwhile */}
          <NodeDownNote class="mkt-node-note" />
          {standaloneScaleBlocked ? (
            <Section title={t('market.ticket.title')} data-testid="ticket-unavailable" class="mkt-card">
              <Banner tone="warn" data-testid="ticket-standalone-scale">{t('market.ticket.standaloneScale')}</Banner>
            </Section>
          ) : info && row.tradable ? (
            <div data-testid="ticket-container">
              {info.openScale === 'entered' ? (
                <Banner tone="info" data-testid="first-order-entered">
                  {t('market.first.entered', { decimals: info.decimals })}{' '}
                  <Button small variant="ghost" onClick={() => setEntered(null)} data-testid="first-order-change">{t('market.first.change')}</Button>
                </Banner>
              ) : null}
              {wallet.networkMismatch ? <Banner tone="error" data-testid="ticket-network-blocked">{t('market.ticket.networkBlocked')}</Banner> : null}
              <OrderTicket token={ticketInfo ?? info} prefill={prefill} inverted={inverted} facts={<TokenFacts rows={[checkedRow ?? row]} compact data-testid="ticket-token-facts" />} />
            </div>
          ) : open.reason === 'no-scale' ? (
            <FirstOrderDecimals key={id} known={issuedDecimals(services.config.network, id)} claim={headerRow?.kascov?.decimals ?? null} onSet={(d) => setEntered({ id, decimals: d })} />
          ) : (
            <Section title={t('market.ticket.title')} data-testid="ticket-unavailable" class="mkt-card">
              <Banner tone="warn" data-testid="ticket-untradable">
                {row.source === 'kascov'
                  ? pub.loading || pub.reason === null
                    ? t('market.public.loading')
                    : t('market.public.unverified', { reason: t(`market.public.${pub.reason}`) })
                  : open.reason === 'book-unavailable'
                  ? t('market.ticket.untradableBook', { reason: t(`market.open.${open.reason}`) })
                  : open.reason
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
      mine={wallet.info ? <OrdersView embed={{ base: id, quote: null }} /> : undefined}
      activity={<MarketActivity market={{ kind: 'token', token: id }} />}
      details={<TokenDetails row={row} services={services} note={onChain ? shown : null} {...(checkedRow ? { checked: checkedRow } : {})} />}
    />
  );
}
