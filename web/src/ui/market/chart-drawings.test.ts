import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  drawingsKey, drawingsMarket, loadDrawings, logicalAt, MAX_DRAWINGS, newDrawingId, parseDrawings, pickShape, saveDrawings, segmentDistance, serializeDrawings, timeAt,
  type Drawing, type Shape,
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

const H: Drawing = { id: 'h1', kind: 'hline', price: 0.025 };
const T: Drawing = { id: 't1', kind: 'trend', a: { time: 1_700_000_000, price: 0.024 }, b: { time: 1_700_003_600, price: 0.026 } };
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

  it('round-trips both kinds and removes the entry when the list is emptied', () => {
    expect(loadDrawings('mainnet', TOKEN)).toEqual([]);
    expect(saveDrawings('mainnet', TOKEN, [H, T])).toBe(true);
    expect(loadDrawings('mainnet', TOKEN)).toEqual([H, T]);
    expect(saveDrawings('mainnet', TOKEN, [])).toBe(true);
    expect(g.localStorage!.getItem(drawingsKey('mainnet', TOKEN))).toBeNull();
    expect(loadDrawings('mainnet', TOKEN)).toEqual([]);
  });

  it('separates networks, markets and orientations', () => {
    saveDrawings('mainnet', TOKEN, [H]);
    saveDrawings('testnet-10', TOKEN, [T]);
    saveDrawings('mainnet', drawingsMarket(TOKEN, true), [{ ...H, id: 'h2', price: 40 }]);
    saveDrawings('mainnet', 'c'.repeat(64), []);
    expect(loadDrawings('mainnet', TOKEN)).toEqual([H]);
    expect(loadDrawings('testnet-10', TOKEN)).toEqual([T]);
    expect(loadDrawings('mainnet', drawingsMarket(TOKEN, true))).toEqual([{ id: 'h2', kind: 'hline', price: 40 }]);
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
    expect(parseDrawings(serializeDrawings([{ id: 'n', kind: 'hline', price: NaN }]))).toEqual([]);
  });

  it('keeps at most MAX_DRAWINGS of a stored list', () => {
    const many: Drawing[] = Array.from({ length: MAX_DRAWINGS + 10 }, (_, i) => ({ id: `d${i}`, kind: 'hline', price: i + 1 }));
    expect(parseDrawings(serializeDrawings(many))).toHaveLength(MAX_DRAWINGS);
  });

  it('extra fields of a stored drawing do not come back', () => {
    const raw = JSON.stringify({ v: 1, items: [{ id: 'h1', kind: 'hline', price: 1, html: '<img src=x onerror=1>', style: 'color:red' }] });
    expect(parseDrawings(raw)).toEqual([{ id: 'h1', kind: 'hline', price: 1 }]);
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
