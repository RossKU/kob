import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  boxOutlineDistance, COLOR_TOKEN, DEFAULT_COLOR, DRAWING_COLORS, drawingsKey, drawingsMarket, extendToPane, insideBox, isDrawingColor, lineDistance, loadDrawings, logicalAt, logicalX,
  MAX_DRAWINGS, newDrawingId, parseDrawings, pickShape, saveDrawings, segmentDistance, serializeDrawings, timeAt, type Drawing, type Shape,
} from './chart-drawings';

/** A Storage in memory: vitest runs in node, which has none. */
function fakeStorage(): Storage {
  const m = new Map<string, string>();
  return {
    get length() { return m.size; },
    clear: () => m.clear(),
    getItem: (k) => m.get(k) ?? null,
    key: (i) => [...m.keys()][i] ?? null,
    removeItem: (k) => void m.delete(k),
    setItem: (k, v) => void m.set(k, String(v)),
  };
}

const H: Drawing = { id: 'h1', color: 'muted', kind: 'hline', price: 0.025 };
const T: Drawing = { id: 't1', color: 'buy', kind: 'trend', a: { time: 1_700_000_000, price: 0.024 }, b: { time: 1_700_003_600, price: 0.026 } };
const X: Drawing = { id: 'x1', color: 'info', kind: 'xline', a: { time: 1_700_000_000, price: 0.024 }, b: { time: 1_700_003_600, price: 0.026 } };
const B: Drawing = { id: 'b1', color: 'accent', kind: 'box', a: { time: 1_700_000_000, price: 0.024 }, b: { time: 1_700_003_600, price: 0.026 } };
const TOKEN = 'a'.repeat(64);

describe('chart drawings storage', () => {
  const g = globalThis as { localStorage?: Storage };
  beforeEach(() => {
    g.localStorage = fakeStorage();
  });
  afterEach(() => {
    delete g.localStorage;
  });

  it('keeps a market per network and per orientation', () => {
    expect(drawingsKey('mainnet', TOKEN)).toBe(`kob.chart.drawings.mainnet.${TOKEN}`);
    expect(drawingsMarket(TOKEN, false)).toBe(TOKEN);
    expect(drawingsMarket(TOKEN, true)).toBe(`${TOKEN}:inv`);
    expect(drawingsMarket(`${TOKEN}/${'b'.repeat(64)}`, false)).toContain('/');
  });

  it('round-trips every kind with its colour and removes the entry when the list is emptied', () => {
    expect(loadDrawings('mainnet', TOKEN)).toEqual([]);
    expect(saveDrawings('mainnet', TOKEN, [H, T, X, B])).toBe(true);
    expect(loadDrawings('mainnet', TOKEN)).toEqual([H, T, X, B]);
    expect(saveDrawings('mainnet', TOKEN, [])).toBe(true);
    expect(g.localStorage!.getItem(drawingsKey('mainnet', TOKEN))).toBeNull();
    expect(loadDrawings('mainnet', TOKEN)).toEqual([]);
  });

  it('separates networks, markets and orientations', () => {
    saveDrawings('mainnet', TOKEN, [H]);
    saveDrawings('testnet-10', TOKEN, [T]);
    saveDrawings('mainnet', drawingsMarket(TOKEN, true), [{ ...H, id: 'h2', price: 40, color: 'sell' }]);
    saveDrawings('mainnet', 'c'.repeat(64), []);
    expect(loadDrawings('mainnet', TOKEN)).toEqual([H]);
    expect(loadDrawings('testnet-10', TOKEN)).toEqual([T]);
    expect(loadDrawings('mainnet', drawingsMarket(TOKEN, true))).toEqual([{ id: 'h2', color: 'sell', kind: 'hline', price: 40 }]);
    expect(loadDrawings('mainnet', 'c'.repeat(64))).toEqual([]);
  });

  it('works without storage: nothing is kept, nothing throws', () => {
    delete g.localStorage;
    expect(loadDrawings('mainnet', TOKEN)).toEqual([]);
    expect(saveDrawings('mainnet', TOKEN, [H])).toBe(false);
    // a storage that throws on every access (blocked site data, full disk)
    const boom = () => {
      throw new Error('denied');
    };
    g.localStorage = { getItem: boom, setItem: boom, removeItem: boom, clear: boom, key: boom, length: 0 } as unknown as Storage;
    expect(loadDrawings('mainnet', TOKEN)).toEqual([]);
    expect(saveDrawings('mainnet', TOKEN, [H])).toBe(false);
    expect(saveDrawings('mainnet', TOKEN, [])).toBe(false);
  });

  it('a stored text that is no drawings reads as none', () => {
    for (const raw of [null, '', 'not json', '[]', '{}', '{"v":2,"items":[]}', '{"v":1}', '{"v":1,"items":"x"}', 'null', '5']) expect(parseDrawings(raw)).toEqual([]);
  });

  it('drops what is not a well-formed drawing and keeps the rest', () => {
    const items = [
      H,
      { id: 'x1', kind: 'hline', price: 'high' },
      { id: 'x2', kind: 'hline', price: null },
      { id: 'x3', kind: 'hline', price: Infinity },
      { id: 'x4', kind: 'trend', a: { time: 1, price: 2 } },
      { id: 'x5', kind: 'trend', a: { time: 1, price: 2 }, b: { time: 'late', price: 3 } },
      { id: 'x6', kind: 'ray', price: 1 },
      { id: 'x7', kind: 'xline', a: { time: 5, price: 5 }, b: { time: 5, price: 5 } }, // an extended line through one point has no direction
      { id: 'x8', kind: 'box', a: { time: 1, price: 2 } },
      { id: '', kind: 'hline', price: 1 },
      { kind: 'hline', price: 1 },
      { id: 'h1', kind: 'hline', price: 9 }, // a repeated id
      { id: 'y'.repeat(41), kind: 'hline', price: 1 },
      null,
      7,
      T,
    ];
    expect(parseDrawings(JSON.stringify({ v: 1, items }))).toEqual([H, T]);
    // NaN is no JSON number: it comes back as null and is dropped
    expect(parseDrawings(serializeDrawings([{ id: 'n', color: 'muted', kind: 'hline', price: NaN }]))).toEqual([]);
  });

  it('keeps at most MAX_DRAWINGS of a stored list', () => {
    const many: Drawing[] = Array.from({ length: MAX_DRAWINGS + 10 }, (_, i) => ({ id: `d${i}`, color: 'muted', kind: 'hline', price: i + 1 }));
    expect(parseDrawings(serializeDrawings(many))).toHaveLength(MAX_DRAWINGS);
  });

  it('extra fields of a stored drawing do not come back', () => {
    const raw = JSON.stringify({ v: 1, items: [{ id: 'h1', kind: 'hline', price: 1, html: '<img src=x onerror=1>', style: 'color:red' }] });
    expect(parseDrawings(raw)).toEqual([{ id: 'h1', color: DEFAULT_COLOR, kind: 'hline', price: 1 }]);
  });

  it('a drawing stored before colours existed reads in the default colour, and a colour that is no swatch does too', () => {
    const a = { time: 1_700_000_000, price: 0.024 };
    const b = { time: 1_700_003_600, price: 0.026 };
    // the entries the first tools wrote: same version, no colour field
    g.localStorage!.setItem(
      drawingsKey('mainnet', TOKEN),
      JSON.stringify({ v: 1, items: [{ id: 'h1', kind: 'hline', price: 0.025 }, { id: 't1', kind: 'trend', a, b }] }),
    );
    expect(loadDrawings('mainnet', TOKEN)).toEqual([
      { id: 'h1', color: 'muted', kind: 'hline', price: 0.025 },
      { id: 't1', color: 'muted', kind: 'trend', a, b },
    ]);
    // a colour is a swatch's name: a CSS colour, markup or a number is not taken
    const odd = ['#ff0000', 'red', 'url(javascript:1)', '<b>', 7, null, {}, ['buy']].map((color, i) => ({ id: `o${i}`, kind: 'hline', price: 1, color }));
    expect(parseDrawings(JSON.stringify({ v: 1, items: odd })).map((d) => d.color)).toEqual(odd.map(() => DEFAULT_COLOR));
    // a recolour survives the round trip, and an old drawing keeps its place in the list while it is recoloured
    saveDrawings('mainnet', TOKEN, [{ ...T, color: 'warn' }]);
    expect(loadDrawings('mainnet', TOKEN)[0]!.color).toBe('warn');
  });

  it('keeps the box and the extended line with their two points; one without a colour gets the default', () => {
    const raw = JSON.stringify({ v: 1, items: [{ id: B.id, kind: B.kind, a: B.a, b: B.b }, X] });
    expect(parseDrawings(raw)).toEqual([{ ...B, color: DEFAULT_COLOR }, X]);
  });
});

describe('the swatches', () => {
  it('are about six, each on its own theme token, the default among them', () => {
    expect(DRAWING_COLORS.length).toBe(6);
    expect(DRAWING_COLORS).toContain(DEFAULT_COLOR);
    for (const c of DRAWING_COLORS) {
      expect(isDrawingColor(c)).toBe(true);
      expect(COLOR_TOKEN[c]).toMatch(/^--[a-z-]+$/);
    }
    expect(new Set(Object.values(COLOR_TOKEN)).size).toBe(DRAWING_COLORS.length);
    expect(isDrawingColor('purple')).toBe(false);
  });
});

describe('drawing ids', () => {
  it('are none of the taken ones and differ from each other', () => {
    const taken = new Set<string>();
    for (let i = 0; i < 500; i++) {
      const id = newDrawingId(taken);
      expect(taken.has(id)).toBe(false);
      expect(id.length).toBeLessThanOrEqual(40);
      taken.add(id);
    }
  });
});

describe('the time grid', () => {
  const g5m = { t0: 1_700_000_000, step: 300 };
  it('maps times and bar positions both ways, also before the first bar and past the newest', () => {
    for (const l of [-12, 0, 0.5, 119, 123.25]) expect(logicalAt(g5m, timeAt(g5m, l))).toBeCloseTo(l, 9);
    expect(logicalAt(g5m, g5m.t0 + 3 * 300)).toBe(3);
  });
  it('keeps one moment on another interval: the same time lands on the same instant of the new grid', () => {
    const t = g5m.t0 + 7 * 300 + 150;
    const g1h = { t0: g5m.t0 - 5 * 3600, step: 3600 };
    expect(timeAt(g1h, logicalAt(g1h, t))).toBeCloseTo(t, 6);
    expect(logicalAt(g1h, t)).toBeCloseTo(5 + (7 * 300 + 150) / 3600, 9);
  });
});

describe('picking a drawing', () => {
  it('measures the distance to a segment, also past its ends', () => {
    expect(segmentDistance(5, 3, 0, 0, 10, 0)).toBe(3);
    expect(segmentDistance(-3, 4, 0, 0, 10, 0)).toBe(5);
    expect(segmentDistance(13, 4, 0, 0, 10, 0)).toBe(5);
    expect(segmentDistance(3, 4, 0, 0, 0, 0)).toBe(5); // a point
  });

  const shapes: Shape[] = [
    { id: 'h', kind: 'hline', y: 100 },
    { id: 't', kind: 'trend', x1: 0, y1: 0, x2: 200, y2: 200 },
  ];
  it('takes the nearest drawing within the tolerance, a horizontal line along its whole width', () => {
    expect(pickShape(shapes, 500, 104, 8)).toBe('h');
    expect(pickShape(shapes, 500, 111, 8)).toBeNull();
    expect(pickShape(shapes, 50, 54, 8)).toBe('t');
    expect(pickShape(shapes, 300, 300, 8)).toBeNull(); // past the segment's end
    // near the crossing at (100, 100): the closer one wins
    expect(pickShape(shapes, 100, 102, 8)).toBe('t');
    expect(pickShape(shapes, 103, 99, 8)).toBe('h');
  });
  it('a larger tolerance (a finger) reaches farther', () => {
    expect(pickShape(shapes, 500, 112, 8)).toBeNull();
    expect(pickShape(shapes, 500, 112, 14)).toBe('h');
    expect(pickShape([], 1, 1, 14)).toBeNull();
  });
});

describe('the extended line', () => {
  const W = 400;
  const PH = 200;
  it('runs on past both points to the plot edges, in the direction of the points', () => {
    // a rising line through (100, 150) and (200, 100): slope -0.5
    const e = extendToPane(100, 150, 200, 100, W, PH)!;
    expect(e.x1).toBeCloseTo(0, 9);
    expect(e.y1).toBeCloseTo(200, 9);
    expect(e.x2).toBeCloseTo(400, 9);
    expect(e.y2).toBeCloseTo(0, 9);
    // given the other way round it is the same line, the ends swap
    const r = extendToPane(200, 100, 100, 150, W, PH)!;
    expect([r.x1, r.y1, r.x2, r.y2].map(Math.round)).toEqual([400, 0, 0, 200]);
  });
  it('a horizontal and a vertical line span the pane; one that misses the pane is null; two equal points have no line', () => {
    expect(extendToPane(50, 80, 60, 80, W, PH)).toEqual({ x1: 0, y1: 80, x2: 400, y2: 80 });
    expect(extendToPane(120, 10, 120, 20, W, PH)).toEqual({ x1: 120, y1: 0, x2: 120, y2: 200 });
    expect(extendToPane(0, 300, 10, 300, W, PH)).toBeNull();
    expect(extendToPane(-50, 0, -50, 10, W, PH)).toBeNull();
    expect(extendToPane(5, 5, 5, 5, W, PH)).toBeNull();
  });
  it('also reaches a line whose two points are far off the pane', () => {
    const e = extendToPane(-1000, -1000, -900, -900, W, PH)!; // the diagonal y = x
    expect(e.x1).toBeCloseTo(0, 9);
    expect(e.y1).toBeCloseTo(0, 9);
    expect(e.x2).toBeCloseTo(200, 9);
    expect(e.y2).toBeCloseTo(200, 9);
  });
  it('is picked along its whole line, past the two points, and not off it', () => {
    expect(lineDistance(300, 100, 0, 0, 100, 100)).toBeCloseTo(141.42, 1);
    expect(lineDistance(5, 3, 0, 0, 10, 0)).toBe(3);
    expect(lineDistance(30, 40, 7, 7, 7, 7)).toBe(Math.hypot(23, 33));
    const line: Shape = { id: 'x', kind: 'xline', x1: 100, y1: 150, x2: 200, y2: 100 };
    expect(pickShape([line], 100, 150, 8)).toBe('x'); // on a point
    expect(pickShape([line], 350, 28, 8)).toBe('x'); // far past the second point (the line is at y = 25 there), near it
    expect(pickShape([line], 10, 198, 8)).toBe('x'); // far before the first (y = 195)
    expect(pickShape([line], 350, 65, 8)).toBeNull();
    // the same two points as a trend line (a segment) are no hit past its end
    expect(pickShape([{ ...line, kind: 'trend' }], 350, 28, 8)).toBeNull();
  });
});

describe('the box', () => {
  const box: Shape = { id: 'b', kind: 'box', x1: 300, y1: 200, x2: 100, y2: 100 }; // corners given the other way round
  it('measures the distance to its outline whichever way the corners are given, and knows its inside', () => {
    expect(boxOutlineDistance(200, 100, 100, 100, 300, 200)).toBe(0);
    expect(boxOutlineDistance(200, 90, 300, 200, 100, 100)).toBe(10);
    expect(boxOutlineDistance(310, 210, 100, 100, 300, 200)).toBeCloseTo(Math.hypot(10, 10), 9);
    expect(boxOutlineDistance(200, 150, 100, 100, 300, 200)).toBe(50);
    expect(insideBox(200, 150, 300, 200, 100, 100)).toBe(true);
    expect(insideBox(99, 150, 100, 100, 300, 200)).toBe(false);
  });
  it('is picked on its outline and inside it, not outside', () => {
    expect(pickShape([box], 200, 103, 8)).toBe('b'); // near the top edge
    expect(pickShape([box], 296, 150, 8)).toBe('b'); // near the right edge
    expect(pickShape([box], 200, 150, 8)).toBe('b'); // deep inside
    expect(pickShape([box], 200, 96, 8)).toBe('b'); // just outside, within the tolerance
    expect(pickShape([box], 200, 80, 8)).toBeNull();
    expect(pickShape([box], 50, 150, 8)).toBeNull();
  });
  it('does not hide a line drawn across it: the nearer line wins over the area, the area over nothing', () => {
    const line: Shape = { id: 'h', kind: 'hline', y: 150 };
    expect(pickShape([box, line], 200, 152, 8)).toBe('h');
    expect(pickShape([line, box], 200, 152, 8)).toBe('h'); // the order of the list does not matter
    expect(pickShape([box, line], 200, 175, 8)).toBe('b'); // away from the line, in the box
    // an edge near the pointer wins over the area of a larger box around it
    const inner: Shape = { id: 'i', kind: 'box', x1: 150, y1: 120, x2: 250, y2: 180 };
    expect(pickShape([box, inner], 200, 121, 8)).toBe('i');
  });
});

describe('logicalX (the library answers x = 0 for a fractional index)', () => {
  /** The library's mapping: bar i at 10 + 7 * i, and 0 for anything that is not a whole number (timeScale.indexToCoordinate). */
  const lib = (i: number): number => (Number.isInteger(i) ? 10 + 7 * i : 0);

  it('places a whole index as the library does', () => {
    expect(logicalX(5, lib)).toBe(45);
    expect(logicalX(-3, lib)).toBe(-11);
  });

  it('places a fractional index on the line between its two neighbours, not at the left edge', () => {
    expect(logicalX(5.5, lib)).toBeCloseTo(48.5, 9);
    expect(logicalX(5.25, lib)).toBeCloseTo(46.75, 9);
    expect(logicalX(-0.5, lib)).toBeCloseTo(6.5, 9);
    expect(logicalX(-2.75, lib)).toBeCloseTo(10 + 7 * -2.75, 9);
  });

  it('a time that is not on a bar boundary of another interval keeps its order and its place', () => {
    const g1h = { t0: 1_700_000_000, step: 3600 };
    const t1 = g1h.t0 + 40 * 3600 + 1200; // 20 minutes into bar 40
    const t2 = g1h.t0 + 42 * 3600 + 2400; // 40 minutes into bar 42
    const x1 = logicalX(logicalAt(g1h, t1), lib)!;
    const x2 = logicalX(logicalAt(g1h, t2), lib)!;
    expect(x1).toBeCloseTo(10 + 7 * (40 + 1 / 3), 9);
    expect(x2).toBeCloseTo(10 + 7 * (42 + 2 / 3), 9);
    expect(x2).toBeGreaterThan(x1);
    // two times inside one (daily) bar are apart, not both at 0
    const g1d = { t0: 1_700_006_400, step: 86400 };
    const a = logicalX(logicalAt(g1d, g1d.t0 + 86400 + 5 * 3600), lib)!;
    const b = logicalX(logicalAt(g1d, g1d.t0 + 86400 + 6 * 3600), lib)!;
    expect(a).toBeGreaterThan(10);
    expect(b - a).toBeCloseTo(7 / 24, 9);
  });

  it('no x from the library (no data): none', () => {
    expect(logicalX(5.5, () => null)).toBeNull();
    expect(logicalX(5, () => null)).toBeNull();
    expect(logicalX(5.5, (i) => (i === 6 ? null : 1))).toBeNull();
  });
});
