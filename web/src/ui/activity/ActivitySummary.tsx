// The summary at the top of the Activity page: all time, a time window (1D / 7D / 1M / 1Y) with its volume chart, and the books now (summary-model.ts).
// Read from the indexer every minute: the token list, the pairs and the KAS candles of each token (daily for all time and the long windows, hourly for
// 1D and 7D, only for tokens that traded in the last two weeks) and the store's activity totals (all time, and the chosen window: orders, traders, what the
// executors earned, what they paid the miners, the KAS locked). Figures an indexer does not report (an older one has no `/v1/stats`; executors are recorded by none) show "—". Nothing
// shows without an indexer or in standalone mode (the node alone has no history).
import { useEffect, useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { kasTitleOf, sompiToUsdCompact, type Fiat } from '../../app/fiat';
import { useFiat } from '../../app/use-fiat';
import { formatKas } from '../../kob/units';
import type { IndexerApi } from '../../data/indexer';
import type { ActivityStatsView, CandleView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { formatRowTime, Segmented, useAsync } from '../kit';
import { useStandalone } from '../shell/standalone';
import {
  axisTop,
  barPct,
  booksNow,
  changePct,
  compactKas,
  DAILY_LIMIT,
  HOURLY_LIMIT,
  SUMMARY_WINDOWS,
  summarizeAllTime,
  summarizeWindow,
  WINDOWS,
  type SummaryWindow,
  activityFigures,
  activitySince,
  fineKas,
} from './summary-model';

const REFRESH_MS = 60_000;
const PARALLEL = 4;
/** The log's first rows go first: the summary's reads start a little later and use a few connections at a time. */
const START_DELAY_MS = 400;
const RECENT_MS = 15 * 24 * 3_600_000;
const DASH = '—';

interface Loaded {
  now: number;
  tokens: Awaited<ReturnType<IndexerApi['tokens']>>;
  pairs: Awaited<ReturnType<IndexerApi['pairs']>>;
  daily: CandleView[][];
  hourly: CandleView[][];
  /** the all-time totals (null: this indexer has no `/v1/stats`) */
  activity: ActivityStatsView | null;
}

/** `f` over `items`, at most `PARALLEL` at a time, in order. */
async function pool<A, B>(items: readonly A[], f: (a: A) => Promise<B>): Promise<B[]> {
  const out: B[] = new Array(items.length);
  let next = 0;
  const run = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await f(items[i]!);
    }
  };
  await Promise.all(Array.from({ length: Math.min(PARALLEL, items.length) }, run));
  return out;
}

async function load(ix: IndexerApi, signal: AbortSignal): Promise<Loaded> {
  await new Promise<void>((ok) => setTimeout(ok, START_DELAY_MS));
  signal.throwIfAborted();
  const [tokens, pairs, activity] = await Promise.all([
    ix.tokens({ signal }),
    ix.pairs(undefined, { signal }).catch(() => null),
    ix.activity({}, { signal }).catch(() => null),
  ]);
  const now = Date.now();
  const daily = await pool(tokens, async (tk) => (await ix.candles(tk.covenant_id, { interval: '1d', limit: DAILY_LIMIT }, { signal }).catch(() => null))?.items ?? []);
  const recent = tokens.map((tk, i) => ({ tk, recent: (daily[i] ?? []).some((c) => c.t >= now - RECENT_MS) })).filter((x) => x.recent);
  const hourly = await pool(recent, async (x) => (await ix.candles(x.tk.covenant_id, { interval: '1h', limit: HOURLY_LIMIT }, { signal }).catch(() => null))?.items ?? []);
  signal.throwIfAborted();
  return { now, tokens, pairs, daily, hourly, activity };
}

const UNKNOWN = t('activity.summary.unknown');
/** A line's place while its text loads (a plain space collapses and the line takes no height). */
const NBSP = '\u00a0';

/** The totals of a window: asked once the page has loaded, again for each window chosen, and kept with the window they belong to. */
interface WindowActivity {
  win: SummaryWindow;
  activity: ActivityStatsView | null;
}

/**
 * A KAS value of the summary as it is shown: KAS (compact for a volume, or to the cent when small), or dollars; the tooltip has the exact KAS figure (and the rate in dollars), so a
 * compact figure never hides its digits.
 */
function money(sompi: bigint, fiat: Fiat | null, fine = false): { text: string; title?: string } {
  if (!fiat) return { text: fine ? fineKas(sompi) : compactKas(sompi), title: `${formatKas(sompi, { group: ',' })} KAS` };
  return { text: sompiToUsdCompact(sompi, fiat), title: kasTitleOf(formatKas(sompi, { group: ',' }), fiat) };
}

function Stat(props: { label: string; value: string | null; sub?: string | null; kas?: boolean; usd?: boolean; tip?: string | undefined; help?: string | undefined; testid: string }) {
  const known = props.value !== null;
  // a figure with a help text: the exact value, then what the figure is (the label carries the help text alone)
  const title = known ? (props.help && props.tip ? `${props.tip}\n${props.help}` : (props.tip ?? props.help)) : UNKNOWN;
  return (
    <div class="sum-stat" data-testid={props.testid}>
      <span class="sum-k" title={props.help}>
        {props.label}
      </span>
      <span class="sum-v" title={title}>
        {known ? props.value : DASH}
        {known && props.kas && !props.usd ? <span class="sum-unit">KAS</span> : null}
      </span>
      {/* a sub line that comes with the figures (null while they load) keeps its place meanwhile: the tile does not grow when they arrive */}
      {props.sub !== undefined ? <span class="sum-s">{props.sub ?? NBSP}</span> : null}
    </div>
  );
}

function changeText(pct: number | null): { text: string; tone: string } {
  if (pct === null) return { text: t('activity.summary.noPrev'), tone: '' };
  if (pct === 0) return { text: t('activity.summary.noChange'), tone: '' };
  const s = `${pct > 0 ? '+' : ''}${pct}%`;
  return { text: t('activity.summary.vsPrev', { pct: s }), tone: pct > 0 ? 'sum-up' : 'sum-down' };
}

/** The dated form of the kit's row time (`YYYY-MM-DD HH:MM`, 24-hour): a clock one day later makes `formatRowTime` print the date, whatever day it is now. */
const DAY_MS = 86_400_000;
const stamp = (ms: number): string => formatRowTime(ms, ms + DAY_MS);
const dateFmt = (ms: number): string => stamp(ms).slice(0, 10);
function barWhen(ms: number, w: SummaryWindow): string {
  if (w === '1D' || w === '7D') return stamp(ms);
  const day = dateFmt(ms);
  return w === '1Y' ? t('activity.summary.weekOf', { day }) : day;
}

/** "1 trade", "2,481 trades": one / other keys chosen here (no "(s)"). */
const tradesText = (n: number): string => t(n === 1 ? 'activity.summary.tradesN.one' : 'activity.summary.tradesN.other', { n: n.toLocaleString('en-US') });
const countText = (key: 'tokens' | 'pairs', n: number): string => t(`activity.summary.${key}.${n === 1 ? 'one' : 'other'}`, { n: n.toLocaleString('en-US') });

export function ActivitySummary() {
  const { indexer, standalone } = useServices();
  const sa = useStandalone(standalone);
  // the USD display (Settings): every KAS figure of the summary in dollars; null = KAS
  const fiat = useFiat();
  const off = !indexer || !!sa?.active;
  const data = useAsync((signal) => (off ? Promise.resolve(null) : load(indexer!, signal)), [indexer, off]);
  const [win, setWin] = useState<SummaryWindow>('1D');
  const [sel, setSel] = useState(-1);
  const winData = useAsync<WindowActivity | null>(
    async (signal) => {
      if (off) return null;
      await new Promise<void>((ok) => setTimeout(ok, START_DELAY_MS));
      signal.throwIfAborted();
      return { win, activity: (await indexer!.activity({ since: activitySince(win, Date.now()) }, { signal }).catch(() => null)) ?? null };
    },
    [indexer, off, win],
  );
  useEffect(() => {
    if (off) return;
    const id = setInterval(() => {
      data.refresh();
      winData.refresh();
    }, REFRESH_MS);
    return () => clearInterval(id);
  }, [off, data.refresh, winData.refresh]);
  const d = data.data;
  const view = useMemo(() => {
    if (!d) return null;
    const def = WINDOWS[win];
    const series = def.interval === '1h' ? d.hourly : d.daily;
    return { all: summarizeAllTime(d.daily), w: summarizeWindow(win, series, d.now), books: booksNow(d.tokens, d.pairs ?? []), allAct: activityFigures(d.activity) };
  }, [d, win]);
  // the window's totals belong to the window they were asked for: another window's are never shown under this one
  const wd = winData.data && winData.data.win === win ? winData.data : null;
  const winAct = wd ? activityFigures(wd.activity) : null;
  const num = (n: number | null | undefined): string | null => (n === null || n === undefined ? null : n.toLocaleString('en-US'));
  const kas = (v: bigint | null | undefined): string | null => (v === null || v === undefined ? null : money(v, fiat, true).text);
  const kasTip = (v: bigint | null | undefined): string | undefined => (v === null || v === undefined ? undefined : money(v, fiat, true).title);
  // what a tile of the executors' side is, and how many transactions it left out
  const notCounted = (n: number | null | undefined): string => (n ? ` ${t(n === 1 ? 'activity.summary.notCounted.one' : 'activity.summary.notCounted.other', { n: n.toLocaleString('en-US') })}` : '');
  const incomeHelp = (a: { executorIncomeUnknown: number | null }): string => t('activity.summary.paidHelp') + notCounted(a.executorIncomeUnknown);
  const minersHelp = (a: { minerFeesUnknown: number | null }): string => t('activity.summary.minersHelp') + notCounted(a.minerFeesUnknown);
  if (off || (data.error && !d)) return null;
  const loading = !view;
  const w = view?.w;
  const top = w ? axisTop(w.bars) : 0n;
  const pick = w && sel >= 0 && sel < w.bars.length ? sel : (w?.bars.length ?? 1) - 1;
  const bar = w?.bars[pick];
  const vol = changeText(w ? changePct(w.quote, w.prevQuote) : null);
  const trades = changeText(w ? changePct(w.trades, w.prevTrades) : null);
  const books = view?.books;
  const totalOrders = books ? books.asks + books.bids + books.pairOrders : 0;
  return (
    <section class={`sum${loading ? ' sum-loading' : ''}`} aria-label={t('activity.summary.label')} aria-busy={loading ? 'true' : undefined} data-testid="activity-summary">
      <div class="sum-band">
        <h2 class="sum-h">{t('activity.summary.allTime')}</h2>
        <span class="sum-hint" data-testid="activity-summary-since">
          {view ? (view.all.since === null ? t('activity.summary.sinceNone') : t('activity.summary.since', { date: dateFmt(view.all.since) })) : NBSP}
        </span>
      </div>
      <div class="sum-strip sum-strip-7">
        <Stat label={t('activity.summary.volume')} value={view ? money(view.all.quote, fiat).text : ''} tip={view ? money(view.all.quote, fiat).title : undefined} kas usd={!!fiat} testid="summary-all-volume" />
        <Stat label={t('activity.summary.trades')} value={view ? view.all.trades.toLocaleString('en-US') : ''} testid="summary-all-trades" />
        <Stat label={t('activity.summary.orders')} value={view ? num(view.allAct.orders) : ''} testid="summary-all-orders" />
        <Stat label={t('activity.summary.traders')} value={view ? num(view.allAct.traders) : ''} testid="summary-all-traders" />
        <Stat label={t('activity.summary.executors')} value={view ? num(view.allAct.executors) : ''} testid="summary-all-executors" />
        <Stat label={t('activity.summary.paid')} value={view ? kas(view.allAct.executorIncome) : ''} tip={view ? kasTip(view.allAct.executorIncome) : undefined} help={view ? incomeHelp(view.allAct) : undefined} kas usd={!!fiat} testid="summary-all-paid" />
        <Stat label={t('activity.summary.miners')} value={view ? kas(view.allAct.minerFees) : ''} tip={view ? kasTip(view.allAct.minerFees) : undefined} help={view ? minersHelp(view.allAct) : undefined} kas usd={!!fiat} testid="summary-all-miners" />
      </div>

      <div class="sum-band">
        <h2 class="sum-h">{t(`activity.summary.window.${win}`)}</h2>
        <Segmented
          aria-label={t('activity.summary.windowGroup')}
          value={win}
          onChange={(v) => {
            setWin(v);
            setSel(-1);
          }}
          options={SUMMARY_WINDOWS.map((v) => ({ value: v, label: v, 'data-testid': `summary-window-${v}` }))}
          data-testid="summary-window"
        />
      </div>
      <div class="sum-win">
        <div class="sum-card sum-chart-card">
          <div class="sum-chart-head">
            <div class="sum-stat">
              <span class="sum-k">{t(fiat ? 'activity.summary.volumeUsd' : 'activity.summary.volumeKas')}</span>
              <span class="sum-v sum-v-big" data-testid="summary-window-volume" title={w ? money(w.quote, fiat).title : undefined}>
                {w ? money(w.quote, fiat).text : ''}
                {fiat ? null : <span class="sum-unit">KAS</span>}
              </span>
              <span class={`sum-s ${vol.tone}`}>{w ? vol.text : ' '}</span>
            </div>
            <p class="sum-readout" aria-live="polite" data-testid="summary-readout">
              {bar && w ? (
                <>
                  <strong>{barWhen(bar.t, win)}</strong> <span title={money(bar.quote, fiat).title}>{money(bar.quote, fiat).text}{fiat ? '' : ' KAS'}</span> · {tradesText(bar.trades)}
                </>
              ) : (
                ' '
              )}
            </p>
          </div>
          <div class="sum-chart">
            <div class="sum-y" aria-hidden="true">
              <span>{top > 0n ? money(top, fiat).text : ''}</span>
              <span>{top > 0n ? money(top / 2n, fiat).text : ''}</span>
              <span>0</span>
            </div>
            <div class="sum-plot">
              <div class="sum-grid" aria-hidden="true" />
              <div class="sum-bars" role="group" aria-label={t('activity.summary.chart', { unit: t(`activity.summary.unit.${WINDOWS[win].unit}`) })} data-testid="summary-bars">
                {(w?.bars ?? []).map((b, i) => (
                  <button
                    key={b.t}
                    type="button"
                    class="sum-bar"
                    aria-pressed={i === pick ? 'true' : 'false'}
                    aria-label={t(fiat ? 'activity.summary.barUsd' : 'activity.summary.bar', { when: barWhen(b.t, win), kas: money(b.quote, fiat).text, trades: tradesText(b.trades) })}
                    title={money(b.quote, fiat).title}
                    onMouseEnter={() => setSel(i)}
                    onFocus={() => setSel(i)}
                    onClick={() => setSel(i)}
                  >
                    <span style={{ height: barPct(b.quote, top) }} />
                  </button>
                ))}
              </div>
            </div>
          </div>
          <div class="sum-x" aria-hidden="true">
            <span>{t(`activity.summary.ago.${win}`)}</span>
            <span>{t('activity.summary.agoNow')}</span>
          </div>
        </div>
        <div class="sum-tiles">
          <Stat label={t('activity.summary.trades')} value={w ? w.trades.toLocaleString('en-US') : ''} sub={w ? trades.text : null} testid="summary-window-trades" />
          <Stat label={t('activity.summary.active')} value={w ? String(w.activeTokens) : ''} sub={t('activity.summary.activeSub')} testid="summary-window-active" />
          <Stat label={t('activity.summary.executors')} value={winAct ? num(winAct.executors) : ''} testid="summary-window-executors" />
          <Stat label={t('activity.summary.orders')} value={winAct ? num(winAct.orders) : ''} testid="summary-window-orders" />
          <Stat label={t('activity.summary.traders')} value={winAct ? num(winAct.traders) : ''} testid="summary-window-traders" />
          <Stat label={t('activity.summary.paid')} value={winAct ? kas(winAct.executorIncome) : ''} tip={winAct ? kasTip(winAct.executorIncome) : undefined} help={winAct ? incomeHelp(winAct) : undefined} kas usd={!!fiat} testid="summary-window-paid" />
          <Stat label={t('activity.summary.miners')} value={winAct ? kas(winAct.minerFees) : ''} tip={winAct ? kasTip(winAct.minerFees) : undefined} help={winAct ? minersHelp(winAct) : undefined} kas usd={!!fiat} testid="summary-window-miners" />
        </div>
      </div>

      <div class="sum-band">
        <h2 class="sum-h">{t('activity.summary.now')}</h2>
      </div>
      <div class="sum-strip sum-strip-now">
        <div class="sum-stat sum-orders" data-testid="summary-now-orders">
          <span class="sum-k">{t('activity.summary.openOrders')}</span>
          <span class="sum-v">{books ? totalOrders.toLocaleString('en-US') : ''}</span>
          <span class="sum-split" aria-hidden="true">
            {books && totalOrders > 0 ? (
              <>
                <i class="sum-bid" style={{ flexGrow: books.bids }} />
                <i class="sum-ask" style={{ flexGrow: books.asks }} />
                <i class="sum-pair" style={{ flexGrow: books.pairOrders }} />
              </>
            ) : null}
          </span>
          <span class="sum-s">{books ? t('activity.summary.ordersSplit', { bids: books.bids, asks: books.asks, pairs: books.pairOrders }) : ' '}</span>
        </div>
        <Stat label={t('activity.summary.locked')} value={view ? kas(view.allAct.locked) : ''} tip={view ? kasTip(view.allAct.locked) : undefined} kas usd={!!fiat} testid="summary-now-locked" />
        <Stat
          label={t('activity.summary.withOrders')}
          value={books ? String(books.tokensWithOrders + books.pairs) : ''}
          sub={books ? t('activity.summary.withOrdersSub', { tokens: countText('tokens', books.tokensWithOrders), pairs: countText('pairs', books.pairs) }) : null}
          testid="summary-now-markets"
        />
      </div>
      <p class="sum-note">{t('activity.summary.note')}</p>
    </section>
  );
}
