// Drawings on the price chart (a horizontal line, a trend line, an extended line, a box): the model, the browser storage and the pixel geometry the chart's
// tools use. No chart library here, so that it is unit-tested in node; the drawing itself is chart-drawings-primitive.ts, the tools are PriceChart.tsx.
//
// What a drawing is anchored to: a price (the series' own number, the one on the price axis) and, for the two points of a trend line, an extended line or a
// box (its two opposite corners), a time in unix seconds.
// A time is not a bar index and not tied to an interval: the chart maps it to its x position through the bar grid on screen (`TimeGrid`: the time of the
// first bar and the bar width), so a drawing keeps its place on a 5m chart, a 1H chart and a 1D chart, and on a point past the newest bar.
//
// Where they are kept: this browser's localStorage, one entry per network and market (`kob.chart.drawings.<network>.<market>`), nothing leaves the
// browser. The market key is the token's covenant id (a pair: `<base>/<quote>`), with `:inv` after it for the inverted KAS chart (1/price: another
// price axis, so the lines of one view are not the other's). Every access is guarded: no storage (a private window, blocked site data, a full disk)
// means the drawings last until the page is left, never a broken chart. What is read is checked field by field: anything that is no drawing is dropped.
//
// Colour: every drawing carries one of a few swatches, stored as the swatch's name (never a CSS colour: nothing stored is ever used as markup or style). The
// name maps to a theme token (`COLOR_TOKEN`) that is read when the chart paints, so a drawing reads on both themes and follows a theme change. A drawing stored
// before colours existed has none and reads as the default swatch, the subtle grey it always had.

export interface Anchor {
  /** unix seconds */
  time: number;
  /** the series' price (what the price axis shows) */
  price: number;
}

/** The swatches, in the order the toolbar shows them. */
export const DRAWING_COLORS = ['muted', 'buy', 'sell', 'info', 'warn', 'accent'] as const;
export type DrawingColor = (typeof DRAWING_COLORS)[number];
/** What a drawing without a colour (a stored one from before colours) and a fresh toolbar use: the subtle line the tools always drew. */
export const DEFAULT_COLOR: DrawingColor = 'muted';
/** The theme token each swatch is read from (the accent is the chart's own violet, declared in market.css: the theme has no second hue that parts from buy green). */
export const COLOR_TOKEN: Record<DrawingColor, string> = { muted: '--muted', buy: '--buy', sell: '--sell', info: '--info', warn: '--warn', accent: '--draw-accent' };

export const isDrawingColor = (v: unknown): v is DrawingColor => (DRAWING_COLORS as readonly unknown[]).includes(v);

/**
 * hline: a price level across the pane. trend: the segment between two points. xline: the line through two points, extended past both to the plot's edges.
 * box: the rectangle with the two points as opposite corners (a translucent fill and an outline).
 */
export type Drawing = { id: string; color: DrawingColor } & (
  | { kind: 'hline'; price: number }
  | { kind: 'trend'; a: Anchor; b: Anchor }
  | { kind: 'xline'; a: Anchor; b: Anchor }
  | { kind: 'box'; a: Anchor; b: Anchor }
);

/** At most this many drawings per market and network: the entry stays small and the chart quick, whatever a page does. */
export const MAX_DRAWINGS = 40;

const STORE_VERSION = 1;

export const drawingsKey = (network: string, market: string): string => `kob.chart.drawings.${network}.${market}`;

/** The market key of a chart: its token id (pair: `base/quote`), told apart per orientation (an inverted chart has the reciprocal price axis). */
export const drawingsMarket = (token: string, inverted: boolean): string => (inverted ? `${token}:inv` : token);

const finite = (n: unknown): n is number => typeof n === 'number' && Number.isFinite(n);
const anchorOf = (v: unknown): Anchor | null => {
  const o = v as { time?: unknown; price?: unknown } | null;
  return o && finite(o.time) && finite(o.price) ? { time: o.time, price: o.price } : null;
};

/** The drawings of a stored text; anything that is not a well-formed drawing (or a repeated id, or the 41st) is dropped. */
export function parseDrawings(raw: string | null): Drawing[] {
  if (!raw) return [];
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return [];
  }
  const items = (v as { v?: unknown; items?: unknown } | null)?.items;
  if ((v as { v?: unknown } | null)?.v !== STORE_VERSION || !Array.isArray(items)) return [];
  const out: Drawing[] = [];
  const seen = new Set<string>();
  for (const it of items) {
    if (out.length >= MAX_DRAWINGS) break;
    const o = it as { id?: unknown; kind?: unknown; price?: unknown; a?: unknown; b?: unknown; color?: unknown } | null;
    if (!o || typeof o.id !== 'string' || o.id.length === 0 || o.id.length > 40 || seen.has(o.id)) continue;
    const color = isDrawingColor(o.color) ? o.color : DEFAULT_COLOR;
    if (o.kind === 'hline' && finite(o.price)) {
      out.push({ id: o.id, color, kind: 'hline', price: o.price });
    } else if (o.kind === 'trend' || o.kind === 'xline' || o.kind === 'box') {
      const a = anchorOf(o.a);
      const b = anchorOf(o.b);
      if (!a || !b) continue;
      // two equal points give an extended line no direction
      if (o.kind === 'xline' && a.time === b.time && a.price === b.price) continue;
      out.push({ id: o.id, color, kind: o.kind, a, b });
    } else continue;
    seen.add(o.id);
  }
  return out;
}

export const serializeDrawings = (list: readonly Drawing[]): string => JSON.stringify({ v: STORE_VERSION, items: list });

function storage(): Storage | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

export function loadDrawings(network: string, market: string): Drawing[] {
  try {
    return parseDrawings(storage()?.getItem(drawingsKey(network, market)) ?? null);
  } catch {
    return [];
  }
}

/** Keeps `list` for the market (an empty list removes the entry). False when the browser would not store it: the drawings then last until the page is left. */
export function saveDrawings(network: string, market: string, list: readonly Drawing[]): boolean {
  try {
    const s = storage();
    if (!s) return false;
    if (list.length === 0) s.removeItem(drawingsKey(network, market));
    else s.setItem(drawingsKey(network, market), serializeDrawings(list));
    return true;
  } catch {
    return false;
  }
}

/** A drawing id that is none of `taken`. */
export function newDrawingId(taken: Iterable<string>): string {
  const used = new Set(taken);
  for (;;) {
    const id = `d${Date.now().toString(36)}${Math.floor(Math.random() * 36 ** 4).toString(36)}`;
    if (!used.has(id)) return id;
  }
}

// ------------------------------------------------------------------------------------------------ time <-> bar position

/** The bar grid on screen: the time (unix seconds) of the first bar and the bar width in seconds. Bar i (the chart's logical index) starts at `t0 + i * step`. */
export interface TimeGrid {
  t0: number;
  step: number;
}

/** The chart's logical (fractional bar) index of a time. */
export const logicalAt = (g: TimeGrid, time: number): number => (time - g.t0) / g.step;
/** The time of a logical index. */
export const timeAt = (g: TimeGrid, logical: number): number => g.t0 + logical * g.step;

/**
 * The pane x of a fractional logical index. The library's `timeScale().logicalToCoordinate` answers 0 (the left edge) for any index that is not a whole number, so a time that is
 * not on a bar boundary of the interval on screen (a point drawn on 5m, looked at on 1H or 1D; a time between two bars) would be placed at the left edge, not at its time. `at` is the library's
 * mapping of a WHOLE index: the answer is the straight line through the two whole indexes around `logical` (bars are evenly spaced). Null when the library has no x (no data).
 */
export function logicalX(logical: number, at: (whole: number) => number | null): number | null {
  const lo = Math.floor(logical);
  const x0 = at(lo);
  if (x0 === null) return null;
  if (lo === logical) return x0;
  const x1 = at(lo + 1);
  return x1 === null ? null : x0 + (logical - lo) * (x1 - x0);
}

// ------------------------------------------------------------------------------------------------ pixel shapes (hit test)

/**
 * A drawing in chart pixels (what is on screen now). A horizontal line spans the pane, so it has only its y; an extended line is given by its two points
 * (it is the whole line through them: the pane clips it); a box by two opposite corners.
 */
export type Shape =
  | { id: string; kind: 'hline'; y: number }
  | { id: string; kind: 'trend' | 'xline' | 'box'; x1: number; y1: number; x2: number; y2: number };

/** Distance from point p to the segment (a, b). */
export function segmentDistance(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const len2 = dx * dx + dy * dy;
  const t = len2 === 0 ? 0 : Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / len2));
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

/** Distance from point p to the whole line through (a, b), past both points; to the point a when the two are one. */
export function lineDistance(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const len = Math.hypot(dx, dy);
  return len === 0 ? Math.hypot(px - ax, py - ay) : Math.abs((px - ax) * dy - (py - ay) * dx) / len;
}

/** Distance from point p to the outline of the rectangle with opposite corners (x1, y1) and (x2, y2), whichever way round they are given; 0 on the outline. */
export function boxOutlineDistance(px: number, py: number, x1: number, y1: number, x2: number, y2: number): number {
  const l = Math.min(x1, x2);
  const r = Math.max(x1, x2);
  const t = Math.min(y1, y2);
  const b = Math.max(y1, y2);
  return Math.min(segmentDistance(px, py, l, t, r, t), segmentDistance(px, py, r, t, r, b), segmentDistance(px, py, r, b, l, b), segmentDistance(px, py, l, b, l, t));
}

export const insideBox = (px: number, py: number, x1: number, y1: number, x2: number, y2: number): boolean =>
  px >= Math.min(x1, x2) && px <= Math.max(x1, x2) && py >= Math.min(y1, y2) && py <= Math.max(y1, y2);

/**
 * The part of the line through (a, b) that lies in the pane (0, 0)-(w, h): its two ends on the pane's edges, in the direction a -> b; null when the line
 * misses the pane or has no direction. Liang-Barsky with an unbounded parameter: the clip a segment would get, with the line running on past both points.
 */
export function extendToPane(ax: number, ay: number, bx: number, by: number, w: number, h: number): { x1: number; y1: number; x2: number; y2: number } | null {
  const dx = bx - ax;
  const dy = by - ay;
  if (dx === 0 && dy === 0) return null;
  let t0 = -Infinity;
  let t1 = Infinity;
  // p * t <= q for each of the four edges
  for (const [p, q] of [[-dx, ax], [dx, w - ax], [-dy, ay], [dy, h - ay]] as const) {
    if (p === 0) {
      if (q < 0) return null; // parallel to this edge and outside it
    } else if (p < 0) t0 = Math.max(t0, q / p);
    else t1 = Math.min(t1, q / p);
  }
  if (t0 > t1) return null;
  return { x1: ax + t0 * dx, y1: ay + t0 * dy, x2: ax + t1 * dx, y2: ay + t1 * dy };
}

/**
 * The drawing nearest to (x, y) within `tolerance` pixels (a finger needs more than a mouse), or null. A box is picked on its outline, and inside it as a last
 * resort: a line or an edge nearer than the tolerance wins over the box's area, so a box does not hide what is drawn on it.
 */
export function pickShape(shapes: readonly Shape[], x: number, y: number, tolerance: number): string | null {
  let best: string | null = null;
  let bestD = tolerance;
  for (const s of shapes) {
    let d: number;
    if (s.kind === 'hline') d = Math.abs(y - s.y);
    else if (s.kind === 'trend') d = segmentDistance(x, y, s.x1, s.y1, s.x2, s.y2);
    else if (s.kind === 'xline') d = lineDistance(x, y, s.x1, s.y1, s.x2, s.y2);
    else {
      const e = boxOutlineDistance(x, y, s.x1, s.y1, s.x2, s.y2);
      d = insideBox(x, y, s.x1, s.y1, s.x2, s.y2) ? Math.min(e, tolerance) : e;
    }
    if (d <= bestD) {
      best = s.id;
      bestD = d;
    }
  }
  return best;
}
