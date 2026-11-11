// Drawing tools on the market page's price chart (a horizontal line, a trend line, delete, clear all): placed with clicks, kept in this browser per
// market and network (localStorage), restored after a reload, each market with its own, and usable on a phone by tapping. Lightweight Charts draws into
// canvases, so a drawing is read back through the chart's `data-drawings` count and the screen-reader list of the drawings (`chart-draw-list`).
// Mock stack: the token page of EXKCC with a seeded history, and the EXKCC/EXUSD pair page for a second market.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);
const chartOf = (page: Page) => page.getByTestId('price-chart');
const listOf = (page: Page) => page.getByTestId('chart-draw-list').locator('li');
const layerOf = (page: Page) => page.getByTestId('chart-draw-layer');
const STORE = (token: string) => `kob.chart.drawings.${'testnet-10'}.${token}`;

/** The example registry with EXKCC and a second listed KCC-20, EXUSD (6 decimals), matching the mock pair seed. */
async function listPair(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional pair quote token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

/** The pair seed and the history of both KAS markets, the registry with both tokens, then the token page of EXKCC. Returns EXKCC's covenant id. */
async function openWithPair(page: Page, mock: MockClient): Promise<string> {
  await mock.seed({ pair: true });
  await mock.seedHistory({});
  await mock.seedHistory({ token: QUOTE, mid: 50_000_000, tick: 100_000, seed: 2 });
  await fund(mock);
  await listPair(page);
  const base = (await mock.token()).covenant_id;
  await page.goto(`/#/market/${base}`);
  await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
  return base;
}

/** A point of the drawing layer (the plot area), as fractions of its width and height, in page coordinates. */
async function at(page: Page, fx: number, fy: number): Promise<{ x: number; y: number }> {
  const b = (await layerOf(page).boundingBox())!;
  return { x: b.x + b.width * fx, y: b.y + b.height * fy };
}

async function drawHline(page: Page, fy: number): Promise<void> {
  await page.getByTestId('draw-hline').click();
  const p = await at(page, 0.5, fy);
  await page.mouse.click(p.x, p.y);
  await expect(layerOf(page)).toHaveCount(0); // a placed drawing ends its tool
}

async function drawTrend(page: Page, from: [number, number], to: [number, number]): Promise<void> {
  await page.getByTestId('draw-trend').click();
  const a = await at(page, ...from);
  await page.mouse.click(a.x, a.y);
  await expect(page.getByTestId('chart-draw-hint')).toContainText('second point');
  const b = await at(page, ...to);
  await page.mouse.move(b.x - 20, b.y - 5);
  await page.mouse.click(b.x, b.y);
  await expect(layerOf(page)).toHaveCount(0);
}

test.describe('desktop', () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test('a horizontal line and a trend line are drawn, restored after a reload, deleted one by one and all at once', async ({ appPage: page, mock }) => {
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    const token = await openMarket(page, mock);
    const chart = chartOf(page);
    await expect(chart).toHaveAttribute('data-state', 'ready');
    await expect(chart).toHaveAttribute('data-drawings', '0');
    await expect(page.getByTestId('draw-clear')).toBeDisabled();
    // no tool on: no layer over the chart, the chart takes the pointer (pan and zoom)
    await expect(layerOf(page)).toHaveCount(0);
    for (const id of ['draw-hline', 'draw-trend', 'draw-erase']) await expect(page.getByTestId(id)).toHaveAttribute('aria-pressed', 'false');
    // the toolbar is not over the plot or its price axis, and says that drawings stay in this browser
    const tools = (await page.getByTestId('chart-tools').boundingBox())!;
    const box = (await page.locator('.chart-box').boundingBox())!;
    expect(tools.y + tools.height, 'toolbar above the plot').toBeLessThanOrEqual(box.y + 1);
    await expect(page.getByTestId('draw-hline')).toHaveAttribute('title', /stay in this browser/);

    // horizontal line: one click sets the level; the layer leaves the price axis and the time axis free
    await page.getByTestId('draw-hline').click();
    await expect(page.getByTestId('draw-hline')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('chart-draw-hint')).toContainText('price level');
    const lb = (await layerOf(page).boundingBox())!;
    expect(lb.x + lb.width, 'the price axis stays free').toBeLessThan(box.x + box.width - 20);
    expect(lb.y + lb.height, 'the time axis stays free').toBeLessThan(box.y + box.height - 10);
    const p = await at(page, 0.5, 0.4);
    await page.mouse.click(p.x, p.y);
    await expect(chart).toHaveAttribute('data-drawings', '1');
    await expect(layerOf(page)).toHaveCount(0);
    await expect(page.getByTestId('draw-hline')).toHaveAttribute('aria-pressed', 'false');
    await expect(listOf(page)).toHaveCount(1);
    const level = Number(/at ([\d.]+)/.exec((await listOf(page).first().textContent()) ?? '')?.[1]);
    expect(level, 'a price near the mock book (0.025 KAS)').toBeGreaterThan(0.015);
    expect(level).toBeLessThan(0.04);

    // trend line: two clicks, Esc steps back from the first point and then leaves the tool
    await page.getByTestId('draw-trend').click();
    const a = await at(page, 0.25, 0.7);
    await page.mouse.click(a.x, a.y);
    await expect(page.getByTestId('chart-draw-hint')).toContainText('second point');
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('chart-draw-hint')).toContainText('first point');
    await expect(layerOf(page)).toHaveCount(1);
    await page.keyboard.press('Escape');
    await expect(layerOf(page)).toHaveCount(0);
    await expect(page.getByTestId('draw-trend')).toHaveAttribute('aria-pressed', 'false');
    await expect(chart).toHaveAttribute('data-drawings', '1');
    await drawTrend(page, [0.25, 0.7], [0.75, 0.25]);
    await expect(chart).toHaveAttribute('data-drawings', '2');
    await expect(listOf(page)).toHaveCount(2);
    await expect(listOf(page).nth(1)).toContainText('Trend line from');

    // kept in this browser, per network and market: back after a reload
    const stored = await page.evaluate((k) => localStorage.getItem(k), STORE(token));
    expect(JSON.parse(stored!).items.map((d: { kind: string }) => d.kind)).toEqual(['hline', 'trend']);
    await page.reload();
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');
    await expect(listOf(page)).toHaveCount(2);
    await expect(listOf(page).first()).toContainText(`Horizontal line at ${level}`);

    // delete: the delete tool removes the drawing under the click; the other one stays
    await page.getByTestId('draw-erase').click();
    await expect(page.getByTestId('chart-draw-hint')).toContainText('delete');
    const onLine = await at(page, 0.9, 0.4); // the horizontal line spans the plot
    await page.mouse.click(onLine.x, onLine.y);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
    await expect(listOf(page).first()).toContainText('Trend line');
    const off = await at(page, 0.9, 0.9); // nothing there
    await page.mouse.click(off.x, off.y);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
    await page.keyboard.press('Escape');
    await expect(layerOf(page)).toHaveCount(0);
    await page.reload();
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');

    // clear all
    await page.getByTestId('draw-clear').click();
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '0');
    await expect(page.getByTestId('draw-clear')).toBeDisabled();
    expect(await page.evaluate((k) => localStorage.getItem(k), STORE(token))).toBeNull();
    await page.reload();
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '0');
  });

  test('drawings survive an interval change and a theme change, and the chart still pans and zooms with no tool on', async ({ appPage: page, mock }) => {
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    await openMarket(page, mock);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await drawHline(page, 0.5);
    await drawTrend(page, [0.3, 0.6], [0.7, 0.3]);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');
    await page.getByTestId('chart-interval-1h').click();
    await expect(chartOf(page)).toHaveAttribute('data-interval', '1h');
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');
    await page.getByTestId('theme-toggle').click();
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');
    // with no tool on the wheel reaches the chart: zoom keeps working
    const b = (await page.locator('.chart-canvas').boundingBox())!;
    await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
    await page.mouse.wheel(0, -300);
    await page.mouse.down();
    await page.mouse.move(b.x + b.width / 2 - 80, b.y + b.height / 2);
    await page.mouse.up();
    await expect(layerOf(page)).toHaveCount(0);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');
    await expect(page.getByTestId('chart-intervals')).toBeVisible();
  });

  test('the toolbar works from the keyboard', async ({ appPage: page, mock }) => {
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    await openMarket(page, mock);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(page.getByTestId('draw-hline')).toHaveAccessibleName('Horizontal line');
    await expect(page.getByTestId('draw-trend')).toHaveAccessibleName('Trend line');
    await expect(page.getByTestId('draw-erase')).toHaveAccessibleName('Delete a drawing');
    await expect(page.getByTestId('draw-clear')).toHaveAccessibleName('Clear all drawings');
    await page.getByTestId('draw-trend').focus();
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('draw-trend')).toHaveAttribute('aria-pressed', 'true');
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('draw-trend')).toHaveAttribute('aria-pressed', 'false');
    await page.keyboard.press('Space');
    await expect(page.getByTestId('draw-trend')).toHaveAttribute('aria-pressed', 'true');
    // a second tool replaces the first
    await page.getByTestId('draw-erase').click();
    await expect(page.getByTestId('draw-trend')).toHaveAttribute('aria-pressed', 'false');
    await expect(page.getByTestId('draw-erase')).toHaveAttribute('aria-pressed', 'true');
  });

  test('every market keeps its own drawings: the token market, its pair, and back', async ({ appPage: page, mock }) => {
    const base = await openWithPair(page, mock);
    await drawHline(page, 0.5);
    await drawTrend(page, [0.3, 0.7], [0.6, 0.3]);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');

    // the pair page: no drawings of its own yet; one horizontal line there
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('pair-page')).toHaveAttribute('data-base', base);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-quote', 'usd');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '0');
    await drawHline(page, 0.35);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
    await expect(listOf(page).first()).toContainText('Horizontal line');
    const keys = await page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith('kob.chart.drawings.')).sort());
    expect(keys).toEqual([STORE(base), STORE(`${base}/${QUOTE}`)].sort());

    // back on the token market: its two, not the pair's; and the pair's still there after a reload of the pair
    await page.goto(`/#/market/${base}`);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');
    await expect(listOf(page).nth(1)).toContainText('Trend line');
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await page.reload();
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
  });

  test('without storage the page works and the drawings last until the page is left', async ({ appPage: page, mock }) => {
    await page.addInitScript(() => {
      const deny = () => {
        throw new DOMException('denied', 'SecurityError');
      };
      for (const m of ['setItem', 'removeItem'] as const) Storage.prototype[m] = deny;
    });
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    await openMarket(page, mock);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await drawHline(page, 0.5);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
    await page.reload();
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '0');
  });

  test('stored drawings that are damaged are dropped and the chart still shows', async ({ appPage: page, mock }) => {
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    const token = (await mock.token()).covenant_id;
    await page.addInitScript(
      ([k]) => localStorage.setItem(k!, JSON.stringify({ v: 1, items: [{ id: 'a', kind: 'hline', price: 0.025 }, { id: 'b', kind: 'hline', price: 'x' }, { id: 'c', kind: 'trend' }] })),
      [STORE(token)],
    );
    await openMarket(page, mock);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
  });
});

test.describe('phone, 390 px wide, touch', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test('taps place a horizontal line and a trend line, the layer leaves the axes free and the page does not scroll sideways', async ({ appPage: page, mock }) => {
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    const token = await openMarket(page, mock);
    await expect(chartOf(page)).toHaveAttribute('data-state', 'ready');
    // the whole plot on screen (taps outside the viewport go nowhere), clear of the floating Trade button
    await page.locator('.chart-box').evaluate((el) => el.scrollIntoView({ block: 'center' }));
    const tools = (await page.getByTestId('chart-tools').boundingBox())!;
    const box = (await page.locator('.chart-box').boundingBox())!;
    expect(tools.y + tools.height, 'toolbar above the plot').toBeLessThanOrEqual(box.y + 1);
    expect(tools.x + tools.width).toBeLessThanOrEqual(390);

    await page.getByTestId('draw-hline').tap();
    await expect(layerOf(page)).toHaveCount(1);
    const lb = (await layerOf(page).boundingBox())!;
    expect(lb.x + lb.width, 'the price axis stays free').toBeLessThan(box.x + box.width - 20);
    const p = await at(page, 0.5, 0.45);
    await page.touchscreen.tap(p.x, p.y);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
    await expect(layerOf(page)).toHaveCount(0);

    await page.getByTestId('draw-trend').tap();
    const a = await at(page, 0.2, 0.7);
    await page.touchscreen.tap(a.x, a.y);
    await expect(page.getByTestId('chart-draw-hint')).toContainText('second point');
    const b = await at(page, 0.8, 0.3);
    await page.touchscreen.tap(b.x, b.y);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '2');
    expect(await horizontalOverflow(page), 'market page').toBe(0);

    // a finger deletes a drawing with some margin around its line
    await page.getByTestId('draw-erase').tap();
    const near = await at(page, 0.9, 0.45);
    await page.touchscreen.tap(near.x, near.y + 9);
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
    await page.reload();
    await expect(chartOf(page)).toHaveAttribute('data-drawings', '1');
    expect(await page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? '{}').items?.length, STORE(token))).toBe(1);
  });
});
