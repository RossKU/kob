import type { RefObject } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import { formatNumber, t } from '../../i18n';
import { depthGeometry, type DepthSeries } from './market-model';

const HEIGHT = 240;
const AXIS = 18; // room for the price labels under the plot

/** Width of an element, following resizes (0 until mounted). */
function useWidth(): [RefObject<HTMLDivElement>, number] {
  const ref = useRef<HTMLDivElement>(null);
  const [w, setW] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    setW(el.clientWidth);
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(() => setW(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, w];
}

const fmtPrice = (v: number, dp: number) => formatNumber(v, { minimumFractionDigits: dp, maximumFractionDigits: dp });
/** The tooltip's cumulative amount: a fixed number of decimals per market (not per hover position), thousands grouped, never compact. */
const fmtFixed = (v: number, dp: number) => formatNumber(v, { minimumFractionDigits: dp, maximumFractionDigits: dp });
/** Axis / summary amounts: whole above 100, two decimals above 0.01, three significant digits below it (a pair of a high-priced token trades fractions of a token). */
const fmtAmount = (v: number) =>
  v > 0 && v < 0.01 ? formatNumber(v, { maximumSignificantDigits: 3 }) : formatNumber(v, { notation: v >= 100_000 ? 'compact' : 'standard', maximumFractionDigits: v >= 100 ? 0 : 2 });

/**
 * Depth chart: cumulative bids (left, green) and asks (right, red) around the mid price, step areas with a soft gradient, a subtle grid,
 * price and amount axes, a hover read-out. Drawn in SVG at the panel's pixel width (crisp text at any size).
 */
export function DepthChart(props: { series: DepthSeries | null; loading: boolean; ticker: string; /** the unit of the price axis: KAS natively, the token when inverted */ priceUnit?: string; /** shown as KAS/TOKEN */ inverted?: boolean; dp: number; /** fixed decimals of the cumulative amount in the hover read-out (token decimals / tick of the market); default 2 */ amountDp?: number; source: 'depth' | 'book' }) {
  const [ref, width] = useWidth();
  const [hover, setHover] = useState<{ x: number; y: number; side: 'bid' | 'ask'; price: number; cum: number } | null>(null);
  const s = props.series;
  const plotH = HEIGHT - AXIS;
  const g = s && width > 0 ? depthGeometry(s, width, plotH) : null;
  const summary = s ? t('market.depth.summary', { bids: fmtAmount(s.bids.at(-1)?.cum ?? 0), asks: fmtAmount(s.asks.at(-1)?.cum ?? 0), ticker: props.ticker }) : t('market.depth.title');

  const onMove = (e: MouseEvent) => {
    if (!g || !s || !g.hasData) return;
    const box = (e.currentTarget as SVGElement).getBoundingClientRect();
    const x = e.clientX - box.left;
    const price = g.xMin + (x / g.width) * (g.xMax - g.xMin);
    const mid = s.mid ?? price;
    const side: 'bid' | 'ask' = price <= mid ? 'bid' : 'ask';
    const pts = side === 'bid' ? s.bids : s.asks;
    // the cumulative amount available at this price: every level at least as good as it
    let cum = 0;
    for (const p of pts) if (side === 'bid' ? p.price >= price : p.price <= price) cum = p.cum;
    const y = plotH - (cum / g.yMax) * plotH;
    setHover({ x, y, side, price, cum });
  };

  return (
    <figure style="margin:0" data-testid="depth-chart" data-source={props.source} data-state={g?.hasData ? 'ready' : props.loading ? 'loading' : 'empty'}>
      <div class="depth-box" ref={ref}>
        {g && g.hasData ? (
          <svg class="depth-svg" width={width} height={HEIGHT} role="img" aria-label={summary} onMouseMove={onMove} onMouseLeave={() => setHover(null)}>
            <title>{t('market.depth.title')}</title>
            <defs>
              <linearGradient id="depth-bid-fill" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stop-color="var(--buy)" stop-opacity="0.35" />
                <stop offset="100%" stop-color="var(--buy)" stop-opacity="0.04" />
              </linearGradient>
              <linearGradient id="depth-ask-fill" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stop-color="var(--sell)" stop-opacity="0.35" />
                <stop offset="100%" stop-color="var(--sell)" stop-opacity="0.04" />
              </linearGradient>
            </defs>
            {g.yTicks.map((tk) => (
              <g key={`y${tk.value}`}>
                <line class="grid" x1="0" x2={width} y1={tk.y} y2={tk.y} />
                <text class="axis-label" x={width - 2} y={tk.y - 3} text-anchor="end">{fmtAmount(tk.value)}</text>
              </g>
            ))}
            {g.xTicks.map((tk) => (
              <g key={`x${tk.value}`}>
                <line class="grid" x1={tk.x} x2={tk.x} y1="0" y2={plotH} />
                <text class="axis-label" x={tk.x} y={HEIGHT - 4} text-anchor="middle">{fmtPrice(tk.value, Math.min(props.dp, 8))}</text>
              </g>
            ))}
            <line class="grid" x1="0" x2={width} y1={plotH} y2={plotH} />
            {g.bidArea ? <path class="bid-area" d={g.bidArea} /> : null}
            {g.askArea ? <path class="ask-area" d={g.askArea} /> : null}
            {g.bidLine ? <path class="bid-line" d={g.bidLine} /> : null}
            {g.askLine ? <path class="ask-line" d={g.askLine} /> : null}
            {g.midX !== null ? <line class="mid-line" x1={g.midX} x2={g.midX} y1="0" y2={plotH} /> : null}
            {hover ? (
              <>
                <line class="cross" x1={hover.x} x2={hover.x} y1="0" y2={plotH} />
                <circle cx={hover.x} cy={hover.y} r="3.5" fill={hover.side === 'bid' ? 'var(--buy)' : 'var(--sell)'} />
              </>
            ) : null}
          </svg>
        ) : props.loading ? (
          <div class="skeleton" style={`height:${HEIGHT}px`} data-testid="depth-loading" />
        ) : (
          <div class="mkt-empty" style={`height:${HEIGHT}px`} data-testid="depth-empty">
            {t('market.depth.empty')}
          </div>
        )}
        {hover ? (
          <div class="depth-tip" style={{ left: `${Math.min(Math.max(hover.x + 10, 0), Math.max(0, width - 170))}px`, top: `${Math.max(0, hover.y - 44)}px` }}>
            <div class={hover.side === 'bid' ? 'buy' : 'sell'}>{t(hover.side === 'bid' ? 'market.depth.bids' : 'market.depth.asks')}</div>
            <div>{t('market.depth.tip', { price: fmtPrice(hover.price, props.dp), priceUnit: props.priceUnit ?? 'KAS', amount: fmtFixed(hover.cum, props.amountDp ?? 2), ticker: props.ticker })}</div>
          </div>
        ) : null}
      </div>
      <figcaption class="depth-legend">
        <span><i style="background:var(--buy)" />{t('market.depth.bids')}</span>
        <span><i style="background:var(--sell)" />{t('market.depth.asks')}</span>
        <span class="grow" />
        <span>{summary}</span>
        {s?.estimated ? <span class="est" title={t(props.inverted ? 'market.depth.estimatedInv' : 'market.depth.estimated')}>~</span> : null}
      </figcaption>
    </figure>
  );
}
