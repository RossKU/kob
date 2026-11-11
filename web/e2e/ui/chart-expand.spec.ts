// The price chart's "Expand chart" button (token and pair market pages): at the chart panel's top right, an icon button with aria-pressed and the label "Expand chart" / "Restore".
// Three columns (>= 1200 px): expanded, the order book and the depth chart give way and the chart spans the book's column and its own, the ticket stays; restored, every box is back
// exactly. Two columns and a phone: the chart box is taller and nothing else disappears. Esc restores, the choice is kept in this browser, the chart keeps its interval and its drawings
// and fits its new box (Lightweight Charts autoSize). The page never scrolls sideways. Mock stack: the token page of EXKCC with a seeded history.
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

const KEY = 'kob.chart.expand';
type Box = { x: number; y: number; width: number; height: number };
const box = async (page: Page, id: string): Promise<Box> => (await page.getByTestId(id).boundingBox())!;
const sameBox = (a: Box, b: Box) => {
  for (const k of ['x', 'y', 'width', 'height'] as const) expect(Math.abs(a[k] - b[k]), `${k}: ${a[k]} vs ${b[k]}`).toBeLessThanOrEqual(1);
};
const noSideScroll = async (page: Page) => expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBe(0);
const chartBox = async (page: Page): Promise<Box> => (await page.locator('.chart-box').boundingBox())!;
const btn = (page: Page) => page.getByTestId('chart-expand');

async function open(page: Page, mock: MockClient, width: number, height: number) {
  await page.setViewportSize({ width, height });
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await fund(mock);
  await openMarket(page, mock);
  await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
  await expect(btn(page)).toHaveAttribute('aria-pressed', 'false');
}

/** The canvas Lightweight Charts draws into fills the chart box (autoSize followed the box). */
async function canvasFits(page: Page) {
  const b = await chartBox(page);
  await expect
    .poll(async () => page.locator('.chart-canvas canvas').first().evaluate((c) => (c as HTMLCanvasElement).getBoundingClientRect().height), { message: 'the chart is as tall as its box' })
    .toBeGreaterThan(b.height - 40);
  const c = await page.locator('.chart-canvas canvas').first().boundingBox();
  expect(c!.width).toBeGreaterThan(b.width - 80);
}

for (const [width, height] of [[1280, 800], [1920, 1080]] as const) {
  test(`${width} px: Expand hides the book and the depth chart and widens the chart, Restore brings every box back, Esc restores, the choice is kept`, async ({ appPage: page, mock }) => {
    await open(page, mock, width, height);
    await expect(page.getByTestId('book-section')).toBeVisible();
    await expect(page.getByTestId('depth-section')).toBeVisible();
    // the button: at the chart panel's top right, in the header row beside the drawing tools
    const head = await page.locator('.chart-head').boundingBox();
    const tools = await box(page, 'chart-tools');
    const b0 = await box(page, 'chart-section');
    const eb = (await btn(page).boundingBox())!;
    expect(Math.abs(eb.y + eb.height / 2 - (tools.y + tools.height / 2)), 'on the tools row').toBeLessThan(8);
    expect(eb.x, 'right of the tools').toBeGreaterThan(tools.x + tools.width - 1);
    expect(eb.x + eb.width, 'at the right end of the chart panel').toBeGreaterThan(b0.x + b0.width - 24);
    expect(head!.x + head!.width).toBeGreaterThanOrEqual(eb.x + eb.width - 1);
    await expect(btn(page)).toHaveAccessibleName('Expand chart');

    const before = { book: await box(page, 'book-section'), chart: b0, depth: await box(page, 'depth-section'), ticket: await box(page, 'ticket-container'), plot: await chartBox(page) };
    // state kept across the expand: the interval, a drawing
    await page.getByTestId('chart-interval-1h').click();
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-interval', '1h');
    await page.getByTestId('draw-hline').click();
    const pb = await chartBox(page);
    await page.mouse.click(pb.x + pb.width * 0.4, pb.y + pb.height * 0.4);
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-drawings', '1');

    await btn(page).click();
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'true');
    await expect(btn(page)).toHaveAccessibleName('Restore');
    await expect(page.getByTestId('book-section')).toBeHidden();
    await expect(page.getByTestId('depth-section')).toBeHidden();
    const big = await box(page, 'chart-section');
    // the chart spans the book's column and its own: from the book's left edge to where the middle columns end, the ticket stays on the right
    expect(Math.abs(big.x - before.book.x), 'from the left edge of the book column').toBeLessThanOrEqual(1);
    expect(Math.abs(big.x + big.width - (before.chart.x + before.chart.width)), 'to where the chart column ended').toBeLessThanOrEqual(1);
    expect(big.width).toBeGreaterThan(before.chart.width + before.book.width);
    const ticket = await box(page, 'ticket-container');
    expect(Math.abs(ticket.x - before.ticket.x)).toBeLessThanOrEqual(1);
    expect(Math.abs(ticket.width - before.ticket.width)).toBeLessThanOrEqual(1);
    await expect(page.getByTestId('ticket-container')).toBeVisible();
    // the chart and the ticket end on one line; the plot is at least as tall as before
    const sec = await page.locator('.mkt-area-ticket').first().boundingBox();
    expect(Math.abs(big.y + big.height - (sec!.y + sec!.height)), 'chart and ticket end on one line').toBeLessThanOrEqual(1);
    expect((await chartBox(page)).height).toBeGreaterThanOrEqual(before.plot.height);
    await canvasFits(page);
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-interval', '1h');
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-drawings', '1');
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
    await noSideScroll(page);
    expect(await page.evaluate((k) => localStorage.getItem(k), KEY)).toBe('1');

    // Restore: every box back where it was
    await btn(page).click();
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'false');
    await expect(page.getByTestId('book-section')).toBeVisible();
    await expect(page.getByTestId('depth-section')).toBeVisible();
    sameBox(await box(page, 'book-section'), before.book);
    sameBox(await box(page, 'chart-section'), before.chart);
    sameBox(await box(page, 'depth-section'), before.depth);
    sameBox(await box(page, 'ticket-container'), before.ticket);
    sameBox(await chartBox(page), before.plot);
    await canvasFits(page);
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-drawings', '1');
    expect(await page.evaluate((k) => localStorage.getItem(k), KEY)).toBeNull();

    // the choice is kept: expanded, a reload comes back expanded; Esc restores
    await btn(page).click();
    await expect(page.getByTestId('book-section')).toBeHidden();
    await page.reload();
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('book-section')).toBeHidden();
    await expect(page.getByTestId('depth-section')).toBeHidden();
    // Esc with a drawing tool on leaves the tool and keeps the chart expanded; the next Esc restores
    await page.getByTestId('draw-trend').click();
    await expect(page.getByTestId('chart-draw-layer')).toHaveCount(1);
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('chart-draw-layer')).toHaveCount(0);
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'true');
    await page.keyboard.press('Escape');
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'false');
    await expect(page.getByTestId('book-section')).toBeVisible();
    await expect(page.getByTestId('depth-section')).toBeVisible();
    sameBox(await box(page, 'chart-section'), before.chart);
    await noSideScroll(page);
  });
}

for (const [width, height] of [[850, 1000], [390, 844]] as const) {
  test(`${width} px: Expand makes the chart taller, nothing else disappears, Restore and Esc bring the height back`, async ({ appPage: page, mock }) => {
    await open(page, mock, width, height);
    const before = { book: await box(page, 'book-section'), ticket: await box(page, 'ticket-container'), plot: await chartBox(page) };
    await page.getByTestId('chart-interval-1h').click();
    await btn(page).click();
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'true');
    const big = await chartBox(page);
    expect(big.height, 'taller').toBeGreaterThan(before.plot.height + 100);
    expect(big.height, 'about 70 % of the window').toBeGreaterThan(height * 0.6);
    expect(big.height).toBeLessThan(height);
    expect(Math.abs(big.width - before.plot.width)).toBeLessThanOrEqual(1);
    // the book and the ticket are all there, just lower by the height the chart gained
    await expect(page.getByTestId('book-section')).toBeVisible();
    await expect(page.getByTestId('ticket-container')).toBeVisible();
    await expect(page.getByTestId('depth-section')).toHaveCount(0);
    const grew = big.height - before.plot.height;
    expect(Math.abs((await box(page, 'book-section')).y - (before.book.y + grew))).toBeLessThanOrEqual(1);
    sameBox({ ...(await box(page, 'book-section')), y: 0 }, { ...before.book, y: 0 });
    sameBox({ ...(await box(page, 'ticket-container')), y: 0 }, { ...before.ticket, y: 0 });
    await canvasFits(page);
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-interval', '1h');
    await noSideScroll(page);
    // the button stays on screen (on a phone the drawing tools scroll, the button does not)
    const eb = (await btn(page).boundingBox())!;
    expect(eb.x + eb.width).toBeLessThanOrEqual(width);
    expect(eb.x).toBeGreaterThanOrEqual(0);

    await btn(page).click();
    sameBox(await chartBox(page), before.plot);
    sameBox(await box(page, 'book-section'), before.book);
    sameBox(await box(page, 'ticket-container'), before.ticket);
    await canvasFits(page);
    await btn(page).click();
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'true');
    await page.keyboard.press('Escape');
    await expect(btn(page)).toHaveAttribute('aria-pressed', 'false');
    sameBox(await chartBox(page), before.plot);
  });
}

test('the chart height follows the window: 420 px on a 1080 px window, taller on a taller one (never above 900 px), 320 px on a phone', async ({ appPage: page, mock }) => {
  await open(page, mock, 1920, 1080);
  expect((await chartBox(page)).height).toBe(420);
  await page.setViewportSize({ width: 1920, height: 1440 });
  await expect.poll(async () => (await chartBox(page)).height).toBeGreaterThan(600);
  expect((await chartBox(page)).height).toBeLessThanOrEqual(900);
  await page.setViewportSize({ width: 390, height: 1200 });
  await expect.poll(async () => (await chartBox(page)).height).toBe(320);
});
