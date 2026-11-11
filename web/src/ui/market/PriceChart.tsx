// Candlestick chart with a volume histogram (TradingView Lightweight Charts, Apache-2.0: the library's attribution logo stays on the chart
// and the footer carries its NOTICE with a link to TradingView). The library is loaded on demand (its own chunk) so the rest of the app
// does not pay for it; the panel keeps its size while it loads (skeleton) and when there is nothing to draw (empty state).
//
// Drawing tools (horizontal line, trend line, extended line, box, recolour, delete): the library has none, so they are a series primitive of its plugin API
// (chart-drawings-primitive.ts) with the model and the browser storage in chart-drawings.ts. While a tool is on, a transparent layer over the plot area (the
// price and time axes stay free) takes the clicks and taps; with no tool on, there is no layer and the chart pans and zooms as before. Drawings stay in this
// browser, per market. The extended line is a tool of its own rather than a toggle: every tool is one pressed button that ends with the drawing it places, a
// toggle would be a second kind of control (a state to read off the toolbar) and would let a drawing change kind. Colours: one button beside the tools shows the current colour
// and opens a small palette of six swatches that sets the colour of the NEXT drawing (remembered); the recolour tool, which works like the delete tool, gives the clicked drawing the selected colour. A click on a
// drawing with no tool on cannot select it: there is no layer then, and the plot belongs to the chart's pan and zoom. The header is ONE row: the interval switch on the left, the tools and the colour button on the right (what does not fit scrolls sideways inside the tools' row on
// a phone; the colour button stays outside the scroller so its palette is not clipped). On a touch screen (`pointer: coarse`) the drawing tools and the colours sit behind ONE "Draw" button that opens a small
// sheet (they do not scroll away behind an edge any more); the intervals and Expand stay in the row. The legend is a row of its own above the plot, never over it (the OHLC read-out with the volume and its unit, then
// the unit of the price). Its height never depends on the live values: one line with an ellipsis on the read-out (the unit does not shrink; a phone drops the price unit, which the market bar names); a row
// that grew and shrank with every tick would move everything below it. Prices are shown with the market's display decimals (the kit's price rule), the axis, the crosshair and the legend alike.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { kascovBucketFor } from '../../data/kascov-candles';
import type { CandlestickData, HistogramData, IChartApi, ISeriesApi, MouseEventParams, Time, UTCTimestamp, WhitespaceData } from 'lightweight-charts';
import type { CandleInterval } from '../../data/indexer-types';
import type { IndexerApi } from '../../data/indexer';
import { CANDLE_INTERVALS } from '../../data/indexer';
import { t } from '../../i18n';
import { ErrorBanner, useAsync } from '../kit';
import { loadCandles, MAX_BARS, type ChartSource, type ChartView } from './candle-feed';
import { useTheme } from '../shell/theme';
import { useMedia } from '../shell/narrow';
import { Popover } from '../shell/Popover';
import { basisOf, chartSeries, compactText, fillCandleGaps, INTERVAL_MS, parseCandles, priceText, type ChartBar, type VolumeBar } from './market-model';
import { flippedChartSeries } from './orientation';
import { ratioCandles, ratioSeries, usdText } from './usd-model';
import { pairRatioCandles } from './pair-market';
import { setChartExpanded, useChartExpanded } from './chart-expand';
import { crosshairLabel, tickLabel } from './chart-time';
import { laidOnBars, lineData, overlayPriceText, pairPoints, tokenPoints } from './kascov-overlay';
import { useKascovCandles } from './use-kascov-candles';
import { DrawingsPrimitive, PICK_MOUSE, PICK_TOUCH, type DrawingColors } from './chart-drawings-primitive';
import { COLOR_TOKEN, DEFAULT_COLOR, DRAWING_COLORS, drawingsMarket, isDrawingColor, loadDrawings, MAX_DRAWINGS, newDrawingId, saveDrawings, type Anchor, type Drawing, type DrawingColor } from './chart-drawings';

type Lw = typeof import('lightweight-charts');
let lwPromise: Promise<Lw> | null = null;
const loadLw = (): Promise<Lw> => (lwPromise ??= import('lightweight-charts'));

/** Below this width of the chart's panel the drawing tools are behind the Draw button (the full row needs about 465 px beside the intervals). */
const NARROW_PANEL_PX = 480;
const INTERVAL_KEY = 'kob.chart.interval';
const COLOR_KEY = 'kob.chart.draw.color';
const KASCOV_KEY = 'kob.chart.kascov';
/** The kascov logo, a static file of this app (public/kascov-logo.svg), never loaded from kascov.io. */
const KASCOV_LOGO_URL = `${import.meta.env.BASE_URL}kascov-logo.svg`;

/** The viewer's choice of the optional kascov line (off unless they turned it on), kept in this browser. */
function storedKascov(): boolean {
  try {
    return localStorage.getItem(KASCOV_KEY) === '1';
  } catch {
    return false;
  }
}

function storedColor(): DrawingColor {
  try {
    const v = localStorage.getItem(COLOR_KEY);
    if (isDrawingColor(v)) return v;
  } catch {
    /* no storage */
  }
  return DEFAULT_COLOR;
}

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
    bg: cssVar('--surface', '#141414'),
    text: cssVar('--muted', '#b2b2b2'),
    grid: cssVar('--grid', 'rgba(139,148,163,0.1)'),
    border: cssVar('--border', '#2f2f2f'),
    up: cssVar('--buy', '#2ebd85'),
    down: cssVar('--sell', '#f6465d'),
    font: cssVar('--font', 'system-ui, sans-serif'),
  };
}

const localeOf = () => 'en-US';

/** Last-resort values of the swatches' tokens (as in styles.css, dark theme), for a page where the tokens are not there. */
const SWATCH_FALLBACK: Record<DrawingColor, string> = { muted: '#b2b2b2', buy: '#2ebd85', sell: '#f6465d', info: '#79b4ff', warn: '#f2b350', accent: '#b48cf0' };

/** The drawings' colours, from the theme's tokens: the swatches, the delete tool's hot colour, and the surface for the price tag's text and the dots. */
function drawingColors(): DrawingColors {
  const paint = Object.fromEntries(DRAWING_COLORS.map((c) => [c, cssVar(COLOR_TOKEN[c], SWATCH_FALLBACK[c])])) as Record<DrawingColor, string>;
  // the default grey stays a little soft, as the tools always drew it
  if (paint.muted.startsWith('#')) paint.muted = alpha(paint.muted, 0.85);
  return { swatch: (c) => paint[c], hot: cssVar('--sell', '#f6465d'), surface: cssVar('--surface', '#141414') };
}

type Tool = 'hline' | 'trend' | 'xline' | 'box' | 'paint' | 'erase';
/** The tools that place a drawing with two clicks (the first point, then the second). */
type TwoPoint = 'trend' | 'xline' | 'box';
const isTwoPoint = (t: Tool | null): t is TwoPoint => t === 'trend' || t === 'xline' || t === 'box';
const TOOLS: readonly Tool[] = ['hline', 'trend', 'xline', 'box', 'paint', 'erase'];

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
  /** bumps on every live refresh: only the bars since the newest one are read (candle-feed.ts) */
  refreshKey: number;
  /** bumps on a reorg / resync: the next refresh reads the full series again */
  resyncKey?: number;
  /** draw the pair price (BASE over QUOTE through the two KAS books) instead of the token's KAS book */
  usd?: UsdChartSpec;
  /** the quote asset's name of a `usd` ratio chart (a token/token pair chart; `USD` for the USD reference token) */
  quoteTicker?: string;
  /** draw a KAS book the other way round (orientation.ts): 1/price, high and low swap, volume in the other asset. Display only. */
  inverted?: boolean;
  /**
   * The tokens kascov.io may price for this chart, with their decimals: ONE for a token's market, BASE then QUOTE for a pair. Given only when kascov prices every one of them
   * (data/kascov.ts `priced`); without it the chart has no kascov toggle. Third party and optional: kascov-overlay.ts.
   */
  kascov?: readonly { token: string; decimals: number }[];
}

interface Series {
  bars: ChartBar[];
  volumes: VolumeBar[];
  /** OHLC texts of bar i */
  text: (i: number) => { o: string; h: string; l: string; c: string } | null;
}
const NO_SERIES: Series = { bars: [], volumes: [], text: () => null };

interface Legend { o: string; h: string; l: string; c: string; v: string; /** the whole volume, for its title */ vExact: string; up: boolean; /** the kascov price at this bar, when the line is on and has one */ k: string | null }

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
  // the series on screen and what it was loaded for: a live refresh reads only the bars since its newest one; a change of source, interval
  // or a reorg / resync (`resyncKey`) reads the full series
  const cache = useRef<{ key: string; resync: number; view: ChartView | null } | null>(null);
  const resync = props.resyncKey ?? 0;
  const candles = useAsync(
    async (signal) => {
      if (!indexer) return null;
      const src: ChartSource = usd ? { base: usd.base.token, quote: usd.quote.token, ...(usd.pairCandles ? { pairCandles: true } : {}) } : { token };
      const key = `${token}|${usdKey}|${interval}`;
      const c = cache.current;
      const prev = c && c.key === key && c.resync === resync ? c.view : null;
      const view = await loadCandles(indexer, src, interval, prev, signal);
      cache.current = { key, resync, view };
      return { interval, view };
    },
    [indexer, token, usdKey, interval],
    { refreshKey: props.refreshKey },
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

  const state: 'loading' | 'error' | 'unsupported' | 'empty' | 'ready' | 'off' = !indexer
    ? 'off'
    : current === undefined
      ? candles.error
        ? 'error' // the banner above says it; no endless skeleton
        : 'loading'
      : current.view === null
        ? 'unsupported'
        : series.bars.length === 0
          ? 'empty'
          : 'ready';

  // the optional kascov.io line (third party, off by default): asked for only while it is on and the market has a kascov price (kascov-overlay.ts, use-kascov-candles.ts)
  const kascovLegs = props.kascov ?? [];
  const kascovKey = kascovLegs.map((l) => `${l.token}:${l.decimals}`).join('/');
  const kascovAvail = kascovLegs.length > 0 && kascovLegs.length === (usd ? 2 : 1);
  const [kascovOn, setKascovOnState] = useState<boolean>(storedKascov);
  const setKascovOn = (v: boolean) => {
    setKascovOnState(v);
    try {
      localStorage.setItem(KASCOV_KEY, v ? '1' : '0');
    } catch {
      /* no storage */
    }
  };
  const kascovShown = kascovAvail && kascovOn;
  const kascovIds = useMemo(() => kascovLegs.map((l) => l.token), [kascovKey]);
  const kascovLine = useKascovCandles({ enabled: kascovShown, ids: kascovIds, bucket: kascovBucketFor(interval) });
  // the line on KOB's own bars: one price (or none) per bar
  const overlayValues = useMemo(() => {
    if (!kascovShown || kascovLine.status !== 'ok' || kascovLine.legs.length !== kascovLegs.length) return null;
    const legs = kascovLine.legs.map((candles, i) => ({ candles, decimals: kascovLegs[i]!.decimals }));
    const points = usd ? pairPoints(legs[0]!, legs[1]!) : tokenPoints(legs[0]!, inverted);
    return laidOnBars(points, series.bars.map((b) => b.time), INTERVAL_MS[interval]);
  }, [kascovShown, kascovLine, kascovKey, series, interval, usd, inverted]);

  const boxRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const axisSeen = useRef(new Set<string>());
  const chartRef = useRef<{ chart: IChartApi; candles: ISeriesApi<'Candlestick'>; kascov: ISeriesApi<'Line'>; volume: ISeriesApi<'Histogram'>; drawings: DrawingsPrimitive } | null>(null);
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
    const vol = series.volumes[i]?.value ?? 0;
    const k = overlayValues?.[i];
    return { ...x, v: compactText(vol), vExact: vol.toLocaleString(localeOf(), { maximumFractionDigits: 8 }), up: b.close >= b.open, k: k ? overlayPriceText(k) : null };
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
        // the optional kascov line goes in FIRST: a series added later is drawn over it, so KOB's candles and volume are always in front. It never takes part in the price scale's
        // auto-fit (a far-off third-party price must not squash KOB's candles), and it has no price tag, price line or crosshair dot.
        const kascovSeries = chart.addSeries(lw.LineSeries, {
          color: pal.text, lineWidth: 1, lastValueVisible: false, priceLineVisible: false, crosshairMarkerVisible: false, autoscaleInfoProvider: () => null,
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
        const drawingsPrim = new DrawingsPrimitive(drawingColors);
        candlesSeries.attachPrimitive(drawingsPrim);
        chartRef.current = { chart, candles: candlesSeries, kascov: kascovSeries, volume, drawings: drawingsPrim };
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
    axisSeen.current = new Set();
    panelRef.current?.removeAttribute('data-axis-ticks');
    const noteTick = (label: string): string => {
      if (!axisSeen.current.has(label)) {
        axisSeen.current.add(label);
        panelRef.current?.setAttribute('data-axis-ticks', [...axisSeen.current].join('|'));
      }
      return label;
    };
    c.chart.applyOptions({
      layout: { background: { color: pal.bg }, textColor: pal.text },
      grid: { vertLines: { color: pal.grid }, horzLines: { color: pal.grid } },
      rightPriceScale: { borderColor: pal.border },
      // the axis reads dates on day-level ticks and 24-hour times on intraday ones (chart-time.ts); the labels the library asks for are kept in `data-axis-ticks` (what the axis says, for the tests)
      timeScale: { borderColor: pal.border, tickMarkFormatter: (time: Time, type: number) => noteTick(tickLabel(Number(time), interval, type, localeOf())) },
      localization: { locale: localeOf(), timeFormatter: (time: Time) => crosshairLabel(Number(time), interval, localeOf()) },
    });
    c.candles.applyOptions({
      upColor: pal.up, downColor: pal.down, wickUpColor: pal.up, wickDownColor: pal.down, priceFormat: { type: 'price', precision: props.dp, minMove },
    });
    const muted = alpha(pal.text.startsWith('#') ? pal.text : '#8b94a3', 0.35);
    // a token pair draws no candle for a bucket where neither token traded: a pair has no price of its own to carry; a token's market carries its close, muted
    const bars: (CandlestickData<Time> | WhitespaceData<Time>)[] = series.bars.map(({ filled, ...b }) =>
      filled
        ? props.usd
          ? { time: b.time as UTCTimestamp }
          : { ...b, time: b.time as UTCTimestamp, color: muted, wickColor: muted, borderColor: muted }
        : { ...b, time: b.time as UTCTimestamp },
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
    c.drawings.refresh();
  }, [libReady, series, props.dp, theme, interval, token, inverted]);

  // the kascov line: its prices on KOB's bars (none while it is off or loading), in the muted text colour of the theme
  useEffect(() => {
    const c = chartRef.current;
    if (!c || !libReady) return;
    c.kascov.applyOptions({ color: palette().text, priceFormat: { type: 'price', precision: props.dp, minMove: 10 ** -props.dp } });
    c.kascov.setData(overlayValues ? lineData(overlayValues, series.bars.map((b) => b.time)).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })) : []);
  }, [libReady, overlayValues, series, props.dp, theme]);

  // ---- drawings: stored per network and market (and orientation), shown by the primitive, edited by the tools
  const network = useServices().config.network;
  const market = drawingsMarket(token, inverted);
  const storeKey = `${network}|${market}`;
  const [drawings, setDrawings] = useState<Drawing[]>(() => loadDrawings(network, market));
  const drawingsRef = useRef(drawings);
  const loadedFor = useRef(storeKey);
  const [tool, setTool] = useState<Tool | null>(null);
  const [pending, setPending] = useState<Anchor | null>(null);
  const [color, setColorState] = useState<DrawingColor>(storedColor);
  const setColor = (c: DrawingColor) => {
    setColorState(c);
    try {
      localStorage.setItem(COLOR_KEY, c);
    } catch {
      /* no storage */
    }
  };
  const [inset, setInset] = useState({ r: 0, b: 0 });
  const pointer = useRef('mouse');
  // another market (or network, or orientation): its own drawings
  useEffect(() => {
    if (loadedFor.current === storeKey) return;
    loadedFor.current = storeKey;
    const list = loadDrawings(network, market);
    drawingsRef.current = list;
    setDrawings(list);
    setTool(null);
    setPending(null);
  }, [storeKey, network, market]);
  const commit = (next: Drawing[]) => {
    if (loadedFor.current !== storeKey) return;
    drawingsRef.current = next;
    setDrawings(next);
    saveDrawings(network, market, next);
  };
  const ready = state === 'ready' && libReady;
  const toolOn = ready ? tool : null;
  useEffect(() => {
    chartRef.current?.drawings.set({
      drawings,
      overlay: { pending: isTwoPoint(toolOn) ? pending : null, pendingKind: isTwoPoint(toolOn) ? toolOn : 'trend', cursor: null, color, hover: null, hoverColor: toolOn === 'paint' ? color : null },
    });
  }, [libReady, drawings, pending, toolOn, color]);
  // the bar grid the drawings' times are placed on: the first bar's time and the interval
  useEffect(() => {
    const b = series.bars;
    chartRef.current?.drawings.set({ grid: b.length ? { t0: b[0]!.time, step: INTERVAL_MS[interval] / 1000 } : null });
  }, [libReady, series, interval]);
  // Esc: first drops the first point of a two-point drawing, then leaves the tool
  useEffect(() => {
    if (!toolOn) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (pending) setPending(null);
      else setTool(null);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [toolOn, pending]);
  // the layer takes the plot area only: the axes beside and under it keep their own pointer handling
  useEffect(() => {
    const c = chartRef.current;
    if (!toolOn || !c) return;
    const measure = () => setInset({ r: c.chart.priceScale('right').width(), b: c.chart.timeScale().height() });
    measure();
    if (typeof ResizeObserver === 'undefined' || !boxRef.current) return;
    const ro = new ResizeObserver(measure);
    ro.observe(boxRef.current);
    return () => ro.disconnect();
  }, [toolOn, series]);
  // Expand chart (the page's grid follows: chart-expand.ts, market.css). Esc restores it, unless it already means something else: a drawing tool is on (it leaves the tool),
  // a pop-up is open (it closes), or another handler took the key
  const expanded = useChartExpanded();
  useEffect(() => {
    if (!expanded) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented || toolOn) return;
      if (document.querySelector('[aria-expanded="true"], [role="dialog"]')) return;
      setChartExpanded(false);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [expanded, toolOn]);
  const pickTool = (next: Tool) => {
    setTool(tool === next ? null : next);
    setPending(null);
  };
  const full = drawings.length >= MAX_DRAWINGS;
  const layerPoint = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const onLayerClick = (e: MouseEvent) => {
    const prim = chartRef.current?.drawings;
    if (!prim || !toolOn) return;
    const { x, y } = layerPoint(e);
    const list = drawingsRef.current;
    if (toolOn === 'erase' || toolOn === 'paint') {
      const id = prim.pick(x, y, pointer.current === 'mouse' ? PICK_MOUSE : PICK_TOUCH);
      if (id && toolOn === 'erase') {
        commit(list.filter((d) => d.id !== id));
        prim.set({ overlay: { hover: null } });
      } else if (id) {
        commit(list.map((d) => (d.id === id && d.color !== color ? { ...d, color } : d)));
      }
      return;
    }
    const a = prim.anchorAt(x, y);
    if (!a || list.length >= MAX_DRAWINGS) return;
    if (toolOn === 'hline') {
      commit([...list, { id: newDrawingId(list.map((d) => d.id)), color, kind: 'hline', price: a.price }]);
      setTool(null);
    } else if (!pending) {
      setPending(a);
    } else {
      const p0 = prim.px(pending);
      if (p0 && Math.hypot(p0.x - x, p0.y - y) < 4) return; // two points on one spot make no line (and no box)
      commit([...list, { id: newDrawingId(list.map((d) => d.id)), color, kind: toolOn, a: pending, b: a }]);
      setPending(null);
      setTool(null);
    }
  };
  const onLayerMove = (e: PointerEvent) => {
    pointer.current = e.pointerType;
    const prim = chartRef.current?.drawings;
    if (!prim || e.pointerType !== 'mouse') return;
    const { x, y } = layerPoint(e);
    if (toolOn === 'erase' || toolOn === 'paint') prim.set({ overlay: { hover: prim.pick(x, y, PICK_MOUSE) } });
    else if (isTwoPoint(toolOn) && pending) prim.set({ overlay: { cursor: prim.anchorAt(x, y) } });
  };
  const onLayerLeave = () => chartRef.current?.drawings.set({ overlay: { cursor: null, hover: null } });
  const hint = !toolOn ? '' : full && toolOn !== 'erase' ? t('market.chart.draw.full', { max: MAX_DRAWINGS }) : t(isTwoPoint(toolOn) ? `market.chart.draw.hint.${toolOn}${pending ? 2 : 1}` : `market.chart.draw.hint.${toolOn}`);
  const priceTxt = (n: number) => n.toFixed(props.dp);
  const listText = (d: Drawing): string => {
    const colorName = t(`market.chart.draw.color.${d.color}`);
    if (d.kind === 'hline') return t('market.chart.draw.listHline', { price: priceTxt(d.price), color: colorName });
    return t(d.kind === 'trend' ? 'market.chart.draw.listTrend' : d.kind === 'xline' ? 'market.chart.draw.listXline' : 'market.chart.draw.listBox', { from: priceTxt(d.a.price), to: priceTxt(d.b.price), color: colorName });
  };

  const shown = legend ?? legendOf.current(series.bars.length - 1);
  // the volume's unit: the token (a flipped market's volume bars are KAS); a pair chart's volume is in its base token
  const volUnit = inverted ? 'KAS' : props.ticker;
  // a touch screen: the drawing tools and the colours are behind one "Draw" button (a sheet), so no row of them scrolls away behind an edge
  // The same where the chart's panel is narrow (a narrow window with a mouse): the row of tools would not fit beside the intervals, so the one Draw button stands in for it whatever the pointer.
  // Measured on the panel (not the window: the panel is 500 px on 1280 and 330 on a phone), before the first paint, so the row never flashes.
  const touch = useMedia('(pointer: coarse)');
  const [panelWidth, setPanelWidth] = useState(0);
  useLayoutEffect(() => {
    const el = panelRef.current;
    if (!el) return;
    const read = () => setPanelWidth(el.clientWidth);
    read();
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(read);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const coarse = touch || (panelWidth > 0 && panelWidth < NARROW_PANEL_PX);
  /** The drawing tools (and the clear-all button): the same buttons in the desktop row and in the touch sheet, where a pick closes the sheet so the chart is free to draw on. */
  const toolButtons = (pick: boolean) => (
    <>
      {TOOLS.map((k) => (
        <button key={k} type="button" disabled={!ready} aria-pressed={toolOn === k ? 'true' : 'false'} aria-label={t(`market.chart.draw.${k}`)} title={`${t(`market.chart.draw.${k}`)}. ${t('market.chart.draw.local')}`} data-testid={`draw-${k}`} {...(pick ? { 'data-pick': '' } : {})} onClick={() => pickTool(k)}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            {k === 'hline' ? (
              <path d="M3 12h18" />
            ) : k === 'trend' ? (
              <><path d="M5 18L19 6" /><circle cx="5" cy="18" r="1.6" /><circle cx="19" cy="6" r="1.6" /></>
            ) : k === 'xline' ? (
              <><path d="M2 20L22 4" /><circle cx="8" cy="15.2" r="1.6" /><circle cx="16" cy="8.8" r="1.6" /></>
            ) : k === 'box' ? (
              <rect x="4.5" y="7" width="15" height="10" rx="1" fill="currentColor" fill-opacity="0.16" />
            ) : k === 'paint' ? (
              <path d="M12 3.5s6 6.2 6 10.3a6 6 0 0 1-12 0C6 9.7 12 3.5 12 3.5z" />
            ) : (
              <path d="M5 7h14M10 7V5h4v2M7 7l1 12h8l1-12" />
            )}
          </svg>
        </button>
      ))}
      <button type="button" disabled={!ready || drawings.length === 0} aria-label={t('market.chart.draw.clear')} title={`${t('market.chart.draw.clear')}. ${t('market.chart.draw.local')}`} data-testid="draw-clear" {...(pick ? { 'data-pick': '' } : {})} onClick={() => { setPending(null); commit([]); }}>
        {/* a bin with a cross in it: every drawing goes (the delete tool's bin is empty) */}
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M4 7h16M10 7V5h4v2M6 7l1 12h10l1-12M10.2 11l3.6 5M13.8 11l-3.6 5" />
        </svg>
      </button>
    </>
  );
  /** The six swatches of the drawing colour: in the desktop palette a pick closes it; in the touch sheet it stays open (a colour, then a tool). */
  const swatches = (pick: boolean) => (
    <div class="chart-swatches" role="group" aria-label={t('market.chart.draw.colors')} data-testid="chart-swatches">
      {DRAWING_COLORS.map((c) => (
        <button key={c} type="button" class="chart-sw" data-color={c} {...(pick ? { 'data-pick': '' } : {})} aria-pressed={color === c ? 'true' : 'false'} aria-label={t(`market.chart.draw.color.${c}`)} title={t(`market.chart.draw.color.${c}`)} data-testid={`draw-color-${c}`} onClick={() => setColor(c)}>
          <i aria-hidden="true" />
        </button>
      ))}
    </div>
  );

  return (
    <div class="chart-panel" ref={panelRef} data-testid="price-chart" data-quote={usd ? 'usd' : 'kas'} data-inverted={inverted ? '1' : '0'} data-state={state} data-interval={interval} data-bars={series.bars.length} data-drawings={drawings.length} data-expanded={expanded ? '1' : '0'} data-kascov={kascovShown ? '1' : '0'}>
      <div class="chart-head">
        <div class="chart-intervals" role="group" aria-label={t('market.chart.interval')} data-testid="chart-intervals">
          {CANDLE_INTERVALS.map((iv) => (
            <button key={iv} type="button" aria-pressed={iv === interval ? 'true' : 'false'} data-testid={`chart-interval-${iv}`} onClick={() => setInterval(iv)}>
              {t(`market.chart.iv.${iv}`)}
            </button>
          ))}
        </div>
        <div class="chart-actions">
          {coarse ? (
            <div class="chart-colors">
              <Popover
                label={t('market.chart.draw.sheet')}
                trigger={
                  <>
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 20l1-4L16.5 4.5a2 2 0 0 1 3 3L8 19z" /><path d="M14.5 6.5l3 3" /></svg>
                    <span>{t('market.chart.draw.button')}</span>
                    {toolOn ? <i class="chart-dot" data-color={color} aria-hidden="true" /> : null}
                  </>
                }
                triggerClass="chart-draw-btn"
                triggerAttrs={{ 'data-testid': 'chart-draw-sheet', 'aria-haspopup': 'true', 'data-active': toolOn ? '1' : '0' }}
                panelTestId="chart-draw-panel"
                panelClass="chart-draw-sheet"
                closeOnPick
              >
                <div class="chart-tools" role="group" aria-label={t('market.chart.draw.tools')} title={t('market.chart.draw.local')} data-testid="chart-tools">
                  {toolButtons(true)}
                </div>
                {swatches(false)}
              </Popover>
            </div>
          ) : (
            <>
              <div class="chart-row">
                <div class="chart-tools" role="group" aria-label={t('market.chart.draw.tools')} title={t('market.chart.draw.local')} data-testid="chart-tools">
                  {toolButtons(false)}
                </div>
              </div>
              <div class="chart-colors">
                <Popover
                  label={t('market.chart.draw.colorNow', { color: t(`market.chart.draw.color.${color}`) })}
                  trigger={<><i class="chart-dot" data-color={color} aria-hidden="true" /><span class="chart-caret" aria-hidden="true">▾</span></>}
                  triggerClass="chart-color-btn"
                  triggerAttrs={{ 'data-testid': 'chart-color', 'aria-haspopup': 'true', title: t('market.chart.draw.colorHint') }}
                  panelTestId="chart-color-panel"
                  closeOnPick
                >
                  {swatches(true)}
                </Popover>
              </div>
            </>
          )}
          {/* the size of the chart: outside the tools' scroller, so a phone never scrolls it away */}
          <div class="chart-tools chart-size">
            {kascovAvail ? (
              <button type="button" aria-pressed={kascovOn ? 'true' : 'false'} aria-label={t('market.chart.kascov')} title={t('market.chart.kascov')} data-testid="chart-kascov-toggle" onClick={() => setKascovOn(!kascovOn)}>
                {/* kascov.io logo, used with the KOB founder's approval; source: https://kascov.io/ (its <link rel=icon> SVG, public/kascov-logo.svg, served from our own origin) */}
                <img class="chart-kascov-logo" src={KASCOV_LOGO_URL} alt="" width="16" height="16" draggable={false} />
              </button>
            ) : null}
            <button type="button" aria-pressed={expanded ? 'true' : 'false'} aria-label={expanded ? t('market.chart.restore') : t('market.chart.expand')} title={expanded ? t('market.chart.restore') : t('market.chart.expand')} data-testid="chart-expand" onClick={() => setChartExpanded(!expanded)}>
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                {expanded ? <path d="M10 4v6H4M10 10L3.5 3.5M14 20v-6h6M14 14l6.5 6.5" /> : <path d="M14 4h6v6M20 4l-6.5 6.5M10 20H4v-6M4 20l6.5-6.5" />}
              </svg>
            </button>
          </div>
        </div>
      </div>
      <div class="chart-legend" data-testid="chart-legend">
        {state === 'ready' && shown ? (
          <span class="chart-ohlc">
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.o')}<b>{shown.o}</b></span>
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.h')}<b>{shown.h}</b></span>
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.l')}<b>{shown.l}</b></span>
            <span class={shown.up ? 'tone-up' : 'tone-down'}>{t('market.chart.c')}<b>{shown.c}</b></span>
            <span title={`${shown.vExact} ${volUnit}`}>{t('market.chart.v')}<b>{shown.v}</b><span class="chart-vol-unit">{volUnit}</span></span>
          </span>
        ) : null}
        {/* the optional third-party price: muted, with what it is in its tooltip; "unavailable" when kascov did not answer, and nothing else breaks */}
        {state === 'ready' && kascovShown && (kascovLine.status === 'unavailable' || shown?.k) ? (
          <span class="chart-kascov" title={t('market.chart.kascov.title')} data-testid="chart-kascov" data-state={kascovLine.status === 'ok' ? 'ok' : 'unavailable'}>
            {kascovLine.status === 'ok' ? t('market.chart.kascov.legend', { price: shown!.k! }) : t('market.chart.kascov.unavailable')}
          </span>
        ) : null}
        {/* the unit stays when there is nothing to read out (a market with no trades yet) */}
        <span class="chart-unit" data-testid="chart-unit">{t('market.chart.unitOf', inverted ? { base: usd ? (props.quoteTicker ?? 'USD') : 'KAS', quote: props.ticker } : { base: props.ticker, quote: usd ? (props.quoteTicker ?? 'USD') : 'KAS' })}</span>
      </div>
      <ErrorBanner error={candles.error ?? libError} title={t('market.chart.error')} onRetry={candles.reload} />
      <div class="chart-box">
        <div class="chart-canvas" ref={boxRef} aria-hidden="true" />
        {drawings.length ? (
          <ul class="sr-only" aria-label={t('market.chart.draw.list')} data-testid="chart-draw-list">
            {drawings.map((d) => (
              <li key={d.id} data-kind={d.kind} data-color={d.color}>{listText(d)}</li>
            ))}
          </ul>
        ) : null}
        {toolOn ? (
          <>
            <div
              class={`chart-draw-layer tool-${toolOn}`}
              style={{ right: `${inset.r}px`, bottom: `${inset.b}px` }}
              data-testid="chart-draw-layer"
              data-tool={toolOn}
              onPointerDown={(e) => (pointer.current = e.pointerType)}
              onPointerMove={onLayerMove}
              onPointerLeave={onLayerLeave}
              onClick={onLayerClick}
            />
            <div class="chart-draw-hint" role="status" data-testid="chart-draw-hint">{hint}</div>
          </>
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
