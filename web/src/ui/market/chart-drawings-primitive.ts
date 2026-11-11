// The drawings of chart-drawings.ts as a series primitive of Lightweight Charts v5 (the library's plugin API, the way its official plugin examples draw:
// a primitive attached to the candle series, one pane view with a renderer, an axis view for the price tag). The library calls `updateAllViews` before
// every paint (pan, zoom, resize, new data), so a drawing follows the chart without any bookkeeping here; the colours are read from the theme's tokens at
// paint time (`colors`), so a theme change needs only a repaint. Only types are imported from the library: its code stays in its own lazy chunk.
import type { IChartApiBase, Logical, ISeriesApi, ISeriesPrimitive, ISeriesPrimitiveAxisView, IPrimitivePaneRenderer, IPrimitivePaneView, SeriesAttachedParameter, SeriesType, Time } from 'lightweight-charts';
import type { CanvasRenderingTarget2D } from 'fancy-canvas';
import { logicalAt, pickShape, timeAt, type Anchor, type Drawing, type Shape, type TimeGrid } from './chart-drawings';

export interface DrawingColors {
  /** the default line: subtle */
  line: string;
  /** the drawing the delete tool is over */
  hot: string;
  /** the price tag's text and the point dots' fill */
  surface: string;
}

/** What the tools show besides the stored drawings. */
export interface DrawingOverlay {
  /** the first point of a trend line being drawn, and where the pointer is now: a dashed preview between them */
  pending: Anchor | null;
  cursor: Anchor | null;
  /** the drawing under the delete tool's pointer */
  hover: string | null;
}

/** A pixel of tolerance for picking a drawing: 8 for a mouse, 14 for a finger. */
export const PICK_MOUSE = 8;
export const PICK_TOUCH = 14;

type Api = SeriesAttachedParameter<Time, SeriesType>;

export class DrawingsPrimitive implements ISeriesPrimitive<Time> {
  private api: Api | null = null;
  private drawings: readonly Drawing[] = [];
  private grid: TimeGrid | null = null;
  private overlay: DrawingOverlay = { pending: null, cursor: null, hover: null };
  private shapes: Shape[] = [];
  private preview: { x1: number; y1: number; x2: number; y2: number } | null = null;
  private tags: ISeriesPrimitiveAxisView[] = [];
  private readonly view: IPrimitivePaneView;

  constructor(private readonly colors: () => DrawingColors) {
    this.view = { zOrder: () => 'top', renderer: () => this.renderer };
  }

  attached(param: Api): void {
    this.api = param;
  }
  detached(): void {
    this.api = null;
  }

  set(next: { drawings?: readonly Drawing[]; grid?: TimeGrid | null; overlay?: Partial<DrawingOverlay> }): void {
    if (next.drawings) this.drawings = next.drawings;
    if (next.grid !== undefined) this.grid = next.grid;
    if (next.overlay) this.overlay = { ...this.overlay, ...next.overlay };
    this.api?.requestUpdate();
  }

  /** A repaint (a theme change: the colours are read at paint time). */
  refresh(): void {
    this.api?.requestUpdate();
  }

  // -------------------------------------------------------------------------------- pointer <-> chart

  private chart(): IChartApiBase<Time> | null {
    return this.api?.chart ?? null;
  }
  private series(): ISeriesApi<SeriesType, Time> | null {
    return this.api?.series ?? null;
  }

  /** The chart point under a pane pixel (x, y): its time (the bar grid carries it past the first and the newest bar) and price. Null while the chart has no data. */
  anchorAt(x: number, y: number): Anchor | null {
    const chart = this.chart();
    const series = this.series();
    if (!chart || !series || !this.grid) return null;
    const logical = chart.timeScale().coordinateToLogical(x);
    const price = series.coordinateToPrice(y);
    return logical === null || price === null ? null : { time: timeAt(this.grid, logical), price };
  }

  /** The drawing within `tolerance` pixels of (x, y). */
  pick(x: number, y: number, tolerance: number): string | null {
    return pickShape(this.computeShapes(), x, y, tolerance);
  }

  /** The pane pixel of a chart point; null while the chart has no data. */
  px(a: Anchor): { x: number; y: number } | null {
    const chart = this.chart();
    const series = this.series();
    if (!chart || !series || !this.grid) return null;
    const x = chart.timeScale().logicalToCoordinate(logicalAt(this.grid, a.time) as Logical);
    const y = series.priceToCoordinate(a.price);
    return x === null || y === null ? null : { x, y };
  }

  private computeShapes(): Shape[] {
    const series = this.series();
    const out: Shape[] = [];
    if (!series) return out;
    for (const d of this.drawings) {
      if (d.kind === 'hline') {
        const y = series.priceToCoordinate(d.price);
        if (y !== null) out.push({ id: d.id, kind: 'hline', y });
      } else {
        const a = this.px(d.a);
        const b = this.px(d.b);
        if (a && b) out.push({ id: d.id, kind: 'trend', x1: a.x, y1: a.y, x2: b.x, y2: b.y });
      }
    }
    return out;
  }

  // -------------------------------------------------------------------------------- library hooks

  updateAllViews(): void {
    this.shapes = this.computeShapes();
    const { pending, cursor } = this.overlay;
    const a = pending ? this.px(pending) : null;
    const b = cursor ? this.px(cursor) : null;
    this.preview = a && b ? { x1: a.x, y1: a.y, x2: b.x, y2: b.y } : null;
    // the price tag of a horizontal line on the price axis
    const series = this.series();
    const h = this.chart()?.paneSize().height ?? 0;
    this.tags = [];
    if (!series) return;
    for (const d of this.drawings) {
      if (d.kind !== 'hline') continue;
      const y = series.priceToCoordinate(d.price);
      if (y === null || y < 0 || y > h) continue;
      const text = series.priceFormatter().format(d.price);
      const hot = this.overlay.hover === d.id;
      this.tags.push({
        coordinate: () => y,
        text: () => text,
        textColor: () => this.colors().surface,
        backColor: () => (hot ? this.colors().hot : this.colors().line),
        visible: () => true,
        tickVisible: () => true,
      });
    }
  }

  paneViews(): readonly IPrimitivePaneView[] {
    return [this.view];
  }

  priceAxisViews(): readonly ISeriesPrimitiveAxisView[] {
    return this.tags;
  }

  private readonly renderer: IPrimitivePaneRenderer = {
    draw: (target: CanvasRenderingTarget2D) => {
      target.useMediaCoordinateSpace(({ context: ctx, mediaSize }) => {
        const c = this.colors();
        ctx.lineCap = 'round';
        const dot = (x: number, y: number, color: string) => {
          ctx.beginPath();
          ctx.arc(x, y, 3.5, 0, Math.PI * 2);
          ctx.fillStyle = c.surface;
          ctx.fill();
          ctx.lineWidth = 1.5;
          ctx.strokeStyle = color;
          ctx.stroke();
        };
        for (const s of this.shapes) {
          const color = this.overlay.hover === s.id ? c.hot : c.line;
          ctx.setLineDash([]);
          ctx.strokeStyle = color;
          ctx.lineWidth = this.overlay.hover === s.id ? 2 : 1.5;
          ctx.beginPath();
          if (s.kind === 'hline') {
            ctx.moveTo(0, s.y);
            ctx.lineTo(mediaSize.width, s.y);
            ctx.stroke();
          } else {
            ctx.moveTo(s.x1, s.y1);
            ctx.lineTo(s.x2, s.y2);
            ctx.stroke();
            dot(s.x1, s.y1, color);
            dot(s.x2, s.y2, color);
          }
        }
        const pv = this.preview;
        if (pv) {
          ctx.setLineDash([5, 4]);
          ctx.strokeStyle = c.line;
          ctx.lineWidth = 1.5;
          ctx.beginPath();
          ctx.moveTo(pv.x1, pv.y1);
          ctx.lineTo(pv.x2, pv.y2);
          ctx.stroke();
          ctx.setLineDash([]);
          dot(pv.x1, pv.y1, c.line);
        } else if (this.overlay.pending) {
          const a = this.px(this.overlay.pending);
          if (a) dot(a.x, a.y, c.line);
        }
      });
    },
  };
}
