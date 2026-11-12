// Drawings are anchored to a price and a unix time, so they keep their place when the chart's interval changes (1m / 5m / 1H / 1D) and after a reload. The chart draws into a canvas, so the
// panel exposes what its last paint placed: `data-drawn` (the drawings that have a position on the chart now) and `data-drawn-visible` (of those, the ones touching the plot); the stored
// entry in localStorage is checked beside it. A drawing is never lost by an interval switch: it is placed on every interval (a trend line whose two times fall into one daily bar is
// very short, not gone; times before the first loaded bar or after the newest one are placed on the bar grid), and what the user drew stays in storage untouched.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

test.use({ autoOpen: false });

const INTERVALS = ['1m', '5m', '1h', '1d'] as const;
const chartOf = (page: Page) => page.getByTestId('price-chart');
const layerOf = (page: Page) => page.getByTestId('chart-draw-layer');
const STORE = (token: string) => `kob.chart.drawings.testnet-10.${token}`;

async function at(page: Page, fx: number, fy: number): Promise<{ x: number; y: number }> {
  const b = (await layerOf(page).boundingBox())!;
  return { x: b.x + b.width * fx, y: b.y + b.height * fy };
}

/** Picks a tool on a mouse screen (the tool row) or a touch screen (the Draw sheet). */
async function pick(page: Page, tool: string, touch: boolean): Promise<void> {
  if (touch) {
    if ((await page.getByTestId('chart-draw-panel').count()) === 0) await page.getByTestId('chart-draw-sheet').tap();
    await page.getByTestId(`draw-${tool}`).tap();
    await expect(page.getByTestId('chart-draw-panel')).toHaveCount(0);
  } else {
    await page.getByTestId(`draw-${tool}`).click();
  }
}

async function click(page: Page, p: { x: number; y: number }, touch: boolean): Promise<void> {
  if (touch) await page.touchscreen.tap(p.x, p.y);
  else await page.mouse.click(p.x, p.y);
}

/** One of each kind, placed with the tools on the interval that is on screen. */
async function drawAll(page: Page, touch: boolean): Promise<void> {
  await pick(page, 'hline', touch);
  await click(page, await at(page, 0.5, 0.5), touch);
  const two: [string, [number, number], [number, number]][] = [
    ['trend', [0.4, 0.7], [0.8, 0.3]],
    ['xline', [0.5, 0.8], [0.75, 0.6]],
    ['box', [0.6, 0.85], [0.9, 0.55]],
  ];
  for (const [tool, a, b] of two) {
    await pick(page, tool, touch);
    await click(page, await at(page, ...a), touch);
    await click(page, await at(page, ...b), touch);
    await expect(layerOf(page)).toHaveCount(0);
  }
  await expect(chartOf(page)).toHaveAttribute('data-drawings', '4');
}

async function toInterval(page: Page, iv: string): Promise<void> {
  await page.getByTestId(`chart-interval-${iv}`).click();
  await expect(chartOf(page)).toHaveAttribute('data-interval', iv);
  await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('chart-loading')).toHaveCount(0);
}

interface Stored { kind: string; a?: { time: number }; b?: { time: number } }
const stored = (page: Page, key: string) => page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? '{"items":[]}').items as Stored[], key);

/** What the last paint placed: per drawing, the x of its two points (a horizontal line has none), and the pane's width. */
async function placed(page: Page): Promise<{ kind: string; x1?: number; x2?: number }[]> {
  const at = (await chartOf(page).getAttribute('data-drawn-at')) ?? '';
  return at.split('|').filter(Boolean).map((e) => {
    const [kind, xs] = e.split(':');
    const [x1, x2] = (xs ?? '').split(',').map(Number);
    return xs ? { kind: kind!, x1: x1!, x2: x2! } : { kind: kind! };
  });
}

/**
 * The x of every point follows its time: one straight line x = x0 + k * time through all the points of the two-point drawings (the bars are evenly spaced), within 2 px of rounding,
 * and a later time is further right.
 */
async function expectOnTimeLine(page: Page, key: string, why: string): Promise<void> {
  const items = await stored(page, key);
  const got = await placed(page);
  expect(got.length, `${why}: one placement per drawing`).toBe(items.length);
  const pts: { t: number; x: number }[] = [];
  items.forEach((d, i) => {
    if (!d.a || !d.b) return;
    pts.push({ t: d.a.time, x: got[i]!.x1! }, { t: d.b.time, x: got[i]!.x2! });
  });
  pts.sort((p, q) => p.t - q.t);
  const lo = pts[0]!;
  const hi = pts[pts.length - 1]!;
  expect(hi.t, `${why}: the points span some time`).toBeGreaterThan(lo.t);
  const k = (hi.x - lo.x) / (hi.t - lo.t);
  expect(k, `${why}: later is further right`).toBeGreaterThan(0);
  for (const p of pts) expect(Math.abs(lo.x + k * (p.t - lo.t) - p.x), `${why}: x of t=${p.t} on the time line (not squeezed to the edge)`).toBeLessThanOrEqual(2);
}

/** Every drawing is placed (and the stored ones are all still there) on the interval that is on screen. */
async function expectKept(page: Page, key: string, n: number, why: string): Promise<void> {
  await expect(chartOf(page), `${why}: placed`).toHaveAttribute('data-drawn', String(n));
  await expect(chartOf(page), `${why}: data-drawings`).toHaveAttribute('data-drawings', String(n));
  expect((await stored(page, key)).length, `${why}: stored`).toBe(n);
  await expectOnTimeLine(page, key, why);
}

for (const [name, size, touch] of [['desktop 1280', { width: 1280, height: 900 }, false], ['phone 390', { width: 390, height: 844 }, true]] as const) {
  test.describe(name, () => {
    test.use({ viewport: size, hasTouch: touch });

    test('each kind of drawing stays on every interval, back again and after a reload', async ({ appPage: page, mock }) => {
      await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
      await fund(mock);
      const token = await openMarket(page, mock);
      await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
      if (touch) await page.locator('.chart-box').evaluate((el) => el.scrollIntoView({ block: 'center' }));
      const key = STORE(token);
      await expect(chartOf(page)).toHaveAttribute('data-interval', '5m');
      await drawAll(page, touch);
      await expectKept(page, key, 4, 'on 5m');
      await expect(chartOf(page)).toHaveAttribute('data-drawn-visible', '4');
      const kinds = (await stored(page, key)).map((d) => d.kind).sort();
      expect(kinds).toEqual(['box', 'hline', 'trend', 'xline']);

      for (const iv of ['1m', '1h', '1d', '5m'] as const) {
        await toInterval(page, iv);
        await expectKept(page, key, 4, `on ${iv}`);
        if (iv === '5m') await expect(chartOf(page), 'back on 5m: all four on the plot again').toHaveAttribute('data-drawn-visible', '4');
        // a second look a moment later: nothing drops off after the first paint
        await page.waitForTimeout(300);
        await expectKept(page, key, 4, `on ${iv} (later)`);
      }
      // every interval after another, in a row (quick switches that overlap the loads)
      for (const iv of [...INTERVALS, '1h', '1m', '1d', '5m'] as const) {
        await page.getByTestId(`chart-interval-${iv}`).click();
      }
      await expect(chartOf(page)).toHaveAttribute('data-interval', '5m');
      await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
      await expectKept(page, key, 4, 'after quick switches');

      // a reload on each interval
      for (const iv of ['1m', '1h', '1d'] as const) {
        await toInterval(page, iv);
        await page.reload();
        await expect(chartOf(page)).toHaveAttribute('data-interval', iv);
        await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
        await expectKept(page, key, 4, `reloaded on ${iv}`);
      }
    });
  });
}

/** Seeds the market's store with drawings placed by hand (times in unix seconds), then opens the chart on `iv`. */
async function openWith(page: Page, token: string, items: unknown[], iv: string): Promise<void> {
  await page.evaluate(([k, list, i]) => { localStorage.setItem(k as string, JSON.stringify({ v: 1, items: list })); localStorage.setItem('kob.chart.interval', i as string); }, [STORE(token), items, iv]);
  await page.reload();
  await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
  await expect(chartOf(page)).toHaveAttribute('data-interval', iv);
}

test.describe('desktop 1280, drawings at odd times', () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test('a trend line inside one daily bar is short, not gone; lines before the first bar and after the newest keep their side', async ({ appPage: page, mock }) => {
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    const token = await openMarket(page, mock);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    const day = Math.floor(Date.now() / 86_400_000) * 86_400 - 86_400; // yesterday 00:00 UTC, a daily bar the 48 h of history has
    const items = [
      { id: 'in1d', kind: 'trend', color: 'muted', a: { time: day + 5 * 3600 + 7, price: 0.0245 }, b: { time: day + 5 * 3600 + 37 * 60 + 11, price: 0.0255 } },
      { id: 'past', kind: 'trend', color: 'buy', a: { time: day - 30 * 86_400 + 13, price: 0.0245 }, b: { time: day - 29 * 86_400 + 29, price: 0.0255 } },
      { id: 'future', kind: 'trend', color: 'sell', a: { time: day + 30 * 86_400 + 17, price: 0.0245 }, b: { time: day + 31 * 86_400 + 41, price: 0.0255 } },
    ];
    for (const iv of ['1d', '1h', '5m', '1m'] as const) {
      await openWith(page, token, items, iv);
      await expect(chartOf(page), `on ${iv}`).toHaveAttribute('data-drawn', '3');
      const [short, past, future] = await placed(page);
      const width = (await page.locator('.chart-canvas').boundingBox())!.width;
      // before the first loaded bar: left of the plot; after the newest: right of it; in between: a real place (not the left edge), the two ends apart or at least not collapsed to 0
      expect(past!.x1, `${iv}: past start`).toBeLessThan(0);
      expect(past!.x2, `${iv}: past end`).toBeLessThan(0);
      expect(past!.x1!, `${iv}: past in order`).toBeLessThan(past!.x2!);
      expect(future!.x1, `${iv}: future start`).toBeGreaterThan(width);
      expect(future!.x2, `${iv}: future end`).toBeGreaterThan(width);
      expect(future!.x1!, `${iv}: future in order`).toBeLessThan(future!.x2!);
      expect(short!.x1, `${iv}: the short line is not squeezed to the left edge`).not.toBe(0);
      expect(short!.x1!, `${iv}: the short line is in order`).toBeLessThanOrEqual(short!.x2!);
      if (iv === '1d') {
        // both times in one daily bar: a few pixels apart, inside the plot
        expect(short!.x1, '1d: inside the plot').toBeGreaterThan(0);
        expect(short!.x2, '1d: inside the plot').toBeLessThan(width);
        expect(short!.x2! - short!.x1!, '1d: very short, not gone').toBeLessThan(20);
        expect(short!.x2! - short!.x1!, '1d: and apart (37 min of a day)').toBeGreaterThanOrEqual(1);
      }
      expect((await stored(page, STORE(token))).length, `${iv}: all three still stored`).toBe(3);
    }
  });
});
