// Drawings on the price chart (a horizontal line, a trend line): the model, the browser storage and the pixel geometry the chart's tools use. No chart
// library here, so that it is unit-tested in node; the drawing itself is chart-drawings-primitive.ts, the tools are PriceChart.tsx.
//
// What a drawing is anchored to: a price (the series' own number, the one on the price axis) and, for a trend line's two points, a time in unix seconds.
// A time is not a bar index and not tied to an interval: the chart maps it to its x position through the bar grid on screen (`TimeGrid`: the time of the
// first bar and the bar width), so a drawing keeps its place on a 5m chart, a 1H chart and a 1D chart, and on a point past the newest bar.
//
// Where they are kept: this browser's localStorage, one entry per network and market (`kob.chart.drawings.<network>.<market>`), nothing leaves the
// browser. The market key is the token's covenant id (a pair: `<base>/<quote>`), with `:inv` after it for the inverted KAS chart (1/price: another
// price axis, so the lines of one view are not the other's). Every access is guarded: no storage (a private window, blocked site data, a full disk)
// means the drawings last until the page is left, never a broken chart. What is read is checked field by field: anything that is no drawing is dropped.

export interface Anchor {
  /** unix seconds */
  time: number;
  /** the series' price (what the price axis shows) */
  price: number;
}

export type Drawing =
  | { id: string; kind: 'hline'; price: number }
  | { id: string; kind: 'trend'; a: Anchor; b: Anchor };

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
    const o = it as { id?: unknown; kind?: unknown; price?: unknown; a?: unknown; b?: unknown } | null;
    if (!o || typeof o.id !== 'string' || o.id.length === 0 || o.id.length > 40 || seen.has(o.id)) continue;
    if (o.kind === 'hline' && finite(o.price)) {
      out.push({ id: o.id, kind: 'hline', price: o.price });
    } else if (o.kind === 'trend') {
      const a = anchorOf(o.a);
      const b = anchorOf(o.b);
      if (!a || !b) continue;
      out.push({ id: o.id, kind: 'trend', a, b });
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

// ------------------------------------------------------------------------------------------------ pixel shapes (hit test)

/** A drawing in chart pixels (what is on screen now). A horizontal line spans the pane, so it has only its y. */
export type Shape = { id: string; kind: 'hline'; y: number } | { id: string; kind: 'trend'; x1: number; y1: number; x2: number; y2: number };

/** Distance from point p to the segment (a, b). */
export function segmentDistance(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const len2 = dx * dx + dy * dy;
  const t = len2 === 0 ? 0 : Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / len2));
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

/** The drawing nearest to (x, y) within `tolerance` pixels (a finger needs more than a mouse), or null. */
export function pickShape(shapes: readonly Shape[], x: number, y: number, tolerance: number): string | null {
  let best: string | null = null;
  let bestD = tolerance;
  for (const s of shapes) {
    const d = s.kind === 'hline' ? Math.abs(y - s.y) : segmentDistance(x, y, s.x1, s.y1, s.x2, s.y2);
    if (d <= bestD) {
      best = s.id;
      bestD = d;
    }
  }
  return best;
}
