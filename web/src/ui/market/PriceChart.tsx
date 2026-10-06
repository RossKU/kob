// Candlestick chart with a volume histogram (TradingView Lightweight Charts, Apache-2.0: the library's attribution logo stays on the chart
// and the footer carries its NOTICE with a link to TradingView). The library is loaded on demand (its own chunk) so the rest of the app
// does not pay for it; the panel keeps its size while it loads (skeleton) and when there is nothing to draw (empty state).
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import type { CandlestickData, HistogramData, IChartApi, ISeriesApi, MouseEventParams, Time, UTCTimestamp } from 'lightweight-charts';
import type { CandleInterval } from '../../data/indexer-types';
import type { IndexerApi } from '../../data/indexer';
import { CANDLE_INTERVALS } from '../../data/indexer';
import { t } from '../../i18n';
import { ErrorBanner, useAsync } from '../kit';
import { useTheme } from '../shell/theme';
import { basisOf, chartSeries, fillCandleGaps, INTERVAL_MS, parseCandles, priceText, type ChartBar, type VolumeBar } from './market-model';
import { flippedChartSeries } from './orientation';
import { ratioCandles, ratioSeries, usdText } from './usd-model';
import { pairRatioCandles } from './pair-market';

type Lw = typeof import('lightweight-charts');
let lwPromise: Promise<Lw> | null = null;
const loadLw = (): Promise<Lw> => (lwPromise ??= import('lightweight-charts'));

const INTERVAL_KEY = 'kob.chart.interval';
const MAX_BARS = 1500;

function storedInterval(): CandleInterval {
  try {
    const v = localStorage.getItem(INTERVAL_KEY);
    if (v && (CANDLE_INTERVALS as readonly string[]).includes(v)) return v as CandleInterval;
  } catch {
    /* no storage */
  }
  return '5m';
}

const cssVar = (name: string, fallback: string): string => {
  if (typeof document === 'undefined') return fallback;
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
};

/** `#rrggbb` -> `rgba(r,g,b,a)`; other colour syntaxes come back unchanged. */
function alpha(color: string, a: number): string {
  const m = /^#([0-9a-f]{6})$/i.exec(color);
  if (!m) return color;
  const n = parseInt(m[1]!, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}

function palette() {
  return {
    bg: cssVar('--surface', '#12161d'),
    text: cssVar('--muted', '#8b94a3'),
    grid: cssVar('--grid', 'rgba(139,148,163,0.1)'),
    border: cssVar('--border', '#252c38'),
    up: cssVar('--buy', '#2ebd85'),
    down: cssVar('--sell', '#f6465d'),
    font: cssVar('--font', 'system-ui, sans-serif'),
  };
}

const localeOf = () => 'en-US';

function timeText(sec: number, interval: CandleInterval, withDate: boolean): string {
  const d = new Date(sec * 1000);
  const utc = interval === '1d';
  const o: Intl.DateTimeFormatOptions = utc ? { timeZone: 'UTC', year: 'numeric', month: 'short', day: 'numeric' } : withDate ? { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' } : { hour: '2-digit', minute: '2-digit' };
  return new Intl.DateTimeFormat(localeOf(), o).format(d);
}

/**
 * A pair chart: BASE over QUOTE (two tokens; the USD reference token as QUOTE reads as USD). Prices come only from the KAS series: with
 * `pairCandles` the indexer's pair candles (`/v1/pairs/{base}/{quote}/candles`, derived from the two KAS series; the volume bars are the pair
 * volume), else, or when the indexer does not serve them, the client-side ratio of the two tokens' KAS candles (usd-model.ts).
 */
export interface UsdChartSpec {
  base: { token: string; decimals: number };
  quote: { token: string; decimals: number };
  pairCandles?: boolean;
}

export interface PriceChartProps {
  indexer: IndexerApi | null;
  /** the market's token (with `usd`: the chart's identity, `<base>/<quote>`) */
  token: string;
  decimals: number;
  ticker: string;
  /** price decimals of this market (tabular, same as the stats) */
  dp: number;
  /** bumps on every live refresh */
  refreshKey: number;
  /** draw the pair price (BASE over QUOTE through the two KAS books) instead of the token's KAS book */
  usd?: UsdChartSpec;
  /** the quote asset's name of a `usd` ratio chart (a token/token pair chart; `USD` for the USD reference token) */
  quoteTicker?: string;
  /** draw a KAS book the other way round (orientation.ts): 1/price, high and low swap, volume in the other asset. Display only. */
  inverted?: boolean;
}

interface Series {
  bars: ChartBar[];
  volumes: VolumeBar[];
  /** OHLC texts of bar i */
  text: (i: number) => { o: string; h: string; l: string; c: string } | null;
}
const NO_SERIES: Series = { bars: [], volumes: [], text: () => null };

interface Legend { o: string; h: string; l: string; c: string; v: string; up: boolean }

/** Price panel: interval switch (1m / 5m / 1h / 1d, remembered), candles with the previous close carried across empty buckets, volume below. */
export function PriceChart(props: PriceChartProps) {
  const [interval, setIntervalState] = useState<CandleInterval>(storedInterval);
  const setInterval = (v: CandleInterval) => {
    setIntervalState(v);
    try {
      localStorage.setItem(INTERVAL_KEY, v);
    } catch {
      /* no storage */
    }
  };
  const { indexer, token, usd } = props;
  const inverted = !!props.inverted;
  const usdKey = usd ? `${usd.base.token}/${usd.quote.token}` : '';
  const candles = useAsync(
    async (signal) => {
      if (!indexer) return null;
      if (usd) {
        if (usd.pairCandles) {
          const pc = await indexer.pairCandles(usd.base.token, usd.quote.token, { interval, limit: MAX_BARS }, { signal });
          if (pc) return { interval, view: { kind: 'pair' as const, pc } };
        }
        // both legs on the same interval and window
        const [q, b] = await Promise.all([
          indexer.candles(usd.quote.token, { interval, limit: MAX_BARS }, { signal }),
          indexer.candles(usd.base.token, { interval, limit: MAX_BARS }, { signal }),
        ]);
        return { interval, view: q && b ? { kind: 'usd' as const, q, b } : null };
      }
      const v = await indexer.candles(token, { interval, limit: MAX_BARS }, { signal });
      return { interval, view: v ? { kind: 'kas' as const, v } : null };
    },
    [indexer, token, usdKey, interval, props.refreshKey],
  );
  // an answer for another interval is not shown while the new one loads
  const current = candles.data && candles.data.interval === interval ? candles.data : undefined;
  const series: Series = useMemo(() => {
    const view = current?.view;
    if (!view) return NO_SERIES;
    const until = Date.now();
    if (view.kind === 'kas') {
      const basis = basisOf(view.v.price_basis);
      if (!basis) return NO_SERIES;
      const parsed = parseCandles(view.v.items);
      const filled = fillCandleGaps(parsed, INTERVAL_MS[interval], { until: Math.max(until, parsed.at(-1)?.t ?? 0), maxBars: MAX_BARS });
      if (inverted) return flippedChartSeries(filled, props.decimals, basis, props.dp);
      const p = (x: bigint) => priceText(x, props.decimals, basis, props.dp);
      return { ...chartSeries(filled, props.decimals, basis), text: (i) => (filled[i] ? { o: p(filled[i]!.o), h: p(filled[i]!.h), l: p(filled[i]!.l), c: p(filled[i]!.c) } : null) };
    }
    if (!usd) return NO_SERIES;
    if (view.kind === 'pair') {
      const tok = (x: { token: string; decimals: number }) => ({ covenantId: x.token, ticker: '', decimals: x.decimals, scale: 10n ** BigInt(Math.min(x.decimals, 9)) });
      const rc = pairRatioCandles(view.pc, tok(usd.base), tok(usd.quote)) ?? [];
      const p = (r: (typeof rc)[number]['o']) => usdText(r, props.dp);
      return { ...ratioSeries(rc), text: (i) => (rc[i] ? { o: p(rc[i]!.o), h: p(rc[i]!.h), l: p(rc[i]!.l), c: p(rc[i]!.c) } : null) };
    }
    const qBasis = basisOf(view.q.price_basis);
    const bBasis = basisOf(view.b.price_basis);
    if (!qBasis || !bBasis) return NO_SERIES;
    const quote = { candles: parseCandles(view.q.items), decimals: usd.quote.decimals, basis: qBasis };
    const base = { candles: parseCandles(view.b.items), decimals: usd.base.decimals, basis: bBasis };
    const rc = ratioCandles(base, quote, { intervalMs: INTERVAL_MS[interval], until, maxBars: MAX_BARS });
    const p = (r: (typeof rc)[number]['o']) => usdText(r, props.dp);
    return { ...ratioSeries(rc), text: (i) => (rc[i] ? { o: p(rc[i]!.o), h: p(rc[i]!.h), l: p(rc[i]!.l), c: p(rc[i]!.c) } : null) };
  }, [current, interval, props.decimals, props.dp, usdKey, inverted]);

  const state: 'loading' | 'unsupported' | 'empty' | 'ready' | 'off' = !indexer
    ? 'off'
    : current === undefined
      ? 'loading'
      : current.view === null
        ? 'unsupported'
        : series.bars.length === 0
          ? 'empty'
          : 'ready';

  const boxRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<{ chart: IChartApi; candles: ISeriesApi<'Candlestick'>; volume: ISeriesApi<'Histogram'> } | null>(null);
  const [libReady, setLibReady] = useState(false);
  const [libError, setLibError] = useState<Error | null>(null);
  const [theme] = useTheme();
  const [legend, setLegend] = useState<Legend | null>(null);
  const fitKey = useRef('');
  const legendOf = useRef<(i: number) => Legend | null>(() => null);

  legendOf.current = (i: number) => {
    const x = series.text(i);
    const b = series.bars[i];
    if (!x || !b) return null;
    return { ...x, v: series.volumes[i] ? series.volumes[i]!.value.toLocaleString(localeOf(), { maximumFractionDigits: 2 }) : '0', up: b.close >= b.open };
  };

  // create the chart once the library is there and the panel has something to draw
  useEffect(() => {
    if (state !== 'ready' || chartRef.current || !boxRef.current) return;
    let disposed = false;
    loadLw().then(
      (lw) => {
        if (disposed || !boxRef.current || chartRef.current) return;
        const pal = palette();
        const chart = lw.createChart(boxRef.current, {
          autoSize: true,
          layout: { background: { type: lw.ColorType.Solid, color: pal.bg }, textColor: pal.text, fontFamily: pal.font, fontSize: 11, attributionLogo: true },
          grid: { vertLines: { color: pal.grid }, horzLines: { color: pal.grid } },
          rightPriceScale: { borderColor: pal.border, scaleMargins: { top: 0.08, bottom: 0.26 } },
          timeScale: { borderColor: pal.border, timeVisible: true, secondsVisible: false, rightOffset: 4, barSpacing: 7 },
          crosshair: { mode: lw.CrosshairMode.Normal },
        });
        const candlesSeries = chart.addSeries(lw.CandlestickSeries, {
          upColor: pal.up, downColor: pal.down, wickUpColor: pal.up, wickDownColor: pal.down, borderVisible: false, priceLineVisible: true,
        });
        const volume = chart.addSeries(lw.HistogramSeries, { priceFormat: { type: 'volume' }, priceScaleId: '', lastValueVisible: false, priceLineVisible: false });
        volume.priceScale().applyOptions({ scaleMargins: { top: 0.8, bottom: 0 } });
        chart.subscribeCrosshairMove((param: MouseEventParams<Time>) => {
          const i = param.logical;
          setLegend(i === undefined || i === null ? null : legendOf.current(Math.round(i)));
        });
        chartRef.current = { chart, candles: candlesSeries, volume };
        setLibReady(true);
      },
      (e) => setLibError(e instanceof Error ? e : new Error(String(e))),
    );
    return () => {
      disposed = true;
    };
  }, [state]);

  useEffect(
    () => () => {
      chartRef.current?.chart.remove();
      chartRef.current = null;
    },
    [],
  );

  // data, formats and colours
  useEffect(() => {
    const c = chartRef.current;
    if (!c || !libReady) return;
    const pal = palette();
    const minMove = 10 ** -props.dp;
    c.chart.applyOptions({
      layout: { background: { color: pal.bg }, textColor: pal.text },
      grid: { vertLines: { color: pal.grid }, horzLines: { color: pal.grid } },
      rightPriceScale: { borderColor: pal.border },
      timeScale: { borderColor: pal.border, tickMarkFormatter: (time: Time) => timeText(Number(time), interval, false) },
      localization: { locale: localeOf(), timeFormatter: (time: Time) => timeText(Number(time), interval, true) },
    });
    c.candles.applyOptions({
      upColor: pal.up, downColor: pal.down, wickUpColor: pal.up, wickDownColor: pal.down, priceFormat: { type: 'price', precision: props.dp, minMove },
    });
    const muted = alpha(pal.text.startsWith('#') ? pal.text : '#8b94a3', 0.35);
    const bars: CandlestickData<Time>[] = series.bars.map(({ filled, ...b }) =>
      filled ? { ...b, time: b.time as UTCTimestamp, color: muted, wickColor: muted, borderColor: muted } : { ...b, time: b.time as UTCTimestamp },
    );
    const vols: HistogramData<Time>[] = series.volumes.map((v) => ({ time: v.time as UTCTimestamp, value: v.value, color: alpha(v.up ? pal.up : pal.down, 0.45) }));
    c.candles.setData(bars);
    c.volume.setData(vols);
    // a new interval (or token) starts at the newest ~120 bars; a live refresh keeps the user's zoom
    const key = `${token}:${usdKey}:${interval}:${inverted ? 'i' : 'n'}`;
    if (fitKey.current !== key && bars.length) {
      fitKey.current = key;
      c.chart.timeScale().setVisibleLogicalRange({ from: Math.max(0, bars.length - 120), to: bars.length + 3 });
    }
  }, [libReady, series, props.dp, theme, interval, token, inverted]);

  const shown = legend ?? legendOf.current(series.bars.length - 1);

  return (
    <div data-testid="price-chart" data-quote={usd ? 'usd' : 'kas'} data-inverted={inverted ? '1' : '0'} data-state={state} data-interval={interval} data-bars={series.bars.length}>
      <div class="chart-head">
        <div class="chart-intervals" role="group" aria-label={t('market.chart.interval')} data-testid="chart-intervals">
          {CANDLE_INTERVALS.map((iv) => (
            <button key={iv} type="button" aria-pressed={iv === interval ? 'true' : 'false'} data-testid={`chart-interval-${iv}`} onClick={() => setInterval(iv)}>
              {t(`market.chart.iv.${iv}`)}
            </button>
          ))}
        </div>
        <span class="small muted" data-testid="chart-unit">{t('market.chart.unitOf', inverted ? { quote: props.ticker, base: usd ? (props.quoteTicker ?? 'USD') : 'KAS' } : { quote: usd ? (props.quoteTicker ?? 'USD') : 'KAS', base: props.ticker })}</span>
      </div>
      <ErrorBanner error={candles.error ?? libError} title={t('market.chart.error')} onRetry={candles.reload} />
      <div class="chart-box">
        <div class="chart-canvas" ref={boxRef} aria-hidden="true" />
        {state === 'ready' && shown ? (
          <div class="chart-legend" data-testid="chart-legend">
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.o')}<b>{shown.o}</b></span>
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.h')}<b>{shown.h}</b></span>
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.l')}<b>{shown.l}</b></span>
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.c')}<b>{shown.c}</b></span>
            <span>{t('market.chart.v')}<b>{shown.v}</b></span>
          </div>
        ) : null}
        {state === 'loading' || (state === 'ready' && !libReady && !libError) ? <div class="chart-overlay skeleton" data-testid="chart-loading" /> : null}
        {state === 'empty' || state === 'unsupported' || state === 'off' ? (
          <div class="chart-overlay mkt-empty" data-testid="chart-empty">
            <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
              <path d="M4 19V5M4 19h16M8 15v-4M12 15V8M16 15v-6" stroke-linecap="round" />
            </svg>
            <span>{t(state === 'empty' ? 'market.chart.empty' : state === 'unsupported' ? 'market.chart.unsupported' : 'market.chart.off')}</span>
          </div>
        ) : null}
      </div>
    </div>
  );
}
