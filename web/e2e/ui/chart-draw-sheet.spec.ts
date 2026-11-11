// On a touch screen (`pointer: coarse`, review H-11) the chart's drawing tools and colours sit behind ONE "Draw" button that opens a small sheet; the intervals and Expand stay in the row; a pick of a tool
// closes the sheet and turns the tool on, a colour keeps it open; the legend is one line and its volume has a unit. A mouse keeps the tools' row (chart-drawings.spec.ts). Mock stack, a touch context.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

test.use({ autoOpen: false, hasTouch: true, isMobile: true, viewport: { width: 390, height: 844 } });

test('the tools are behind a Draw button; a tool picked in the sheet closes it and is on; Expand and the intervals stay in the row', async ({ appPage: page, mock }) => {
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await fund(mock);
  await openMarket(page, mock);
  const chart = page.getByTestId('price-chart');
  await expect(chart).toHaveAttribute('data-state', 'ready');
  // the row: intervals, Draw, Expand, all inside the card; no tools row
  const draw = page.getByTestId('chart-draw-sheet');
  await expect(draw).toBeVisible();
  await expect(draw).toContainText('Draw');
  await expect(page.getByTestId('chart-tools').filter({ visible: true })).toHaveCount(0);
  await expect(page.getByTestId('chart-intervals')).toBeVisible();
  await expect(page.getByTestId('chart-expand')).toBeVisible();
  const card = (await page.getByTestId('chart-section').boundingBox())!;
  for (const id of ['chart-intervals', 'chart-draw-sheet', 'chart-expand']) {
    const b = (await page.getByTestId(id).boundingBox())!;
    expect(b.x, id).toBeGreaterThanOrEqual(card.x);
    expect(b.x + b.width, id).toBeLessThanOrEqual(card.x + card.width);
  }
  expect(await draw.evaluate((el) => el.getBoundingClientRect().height), 'a touch target').toBeGreaterThanOrEqual(34);
  // the sheet: the six tools and the clear button, the six colours
  await draw.tap();
  const sheet = page.getByTestId('chart-draw-panel');
  await expect(sheet).toBeVisible();
  for (const k of ['hline', 'trend', 'xline', 'box', 'paint', 'erase']) await expect(sheet.getByTestId(`draw-${k}`)).toBeVisible();
  await expect(sheet.getByTestId('draw-clear')).toBeVisible();
  for (const c of ['muted', 'buy', 'sell', 'info', 'warn', 'accent']) await expect(sheet.getByTestId(`draw-color-${c}`)).toBeVisible();
  // a colour keeps the sheet open
  await sheet.getByTestId('draw-color-sell').tap();
  await expect(sheet).toBeVisible();
  await expect(sheet.getByTestId('draw-color-sell')).toHaveAttribute('aria-pressed', 'true');
  // a tool closes it and is on (a layer over the plot takes the taps; the Draw button says a tool is on)
  await sheet.getByTestId('draw-hline').tap();
  await expect(sheet).toHaveCount(0);
  await expect(page.getByTestId('chart-draw-layer')).toHaveAttribute('data-tool', 'hline');
  await expect(draw).toHaveAttribute('data-active', '1');
  const layer = (await page.getByTestId('chart-draw-layer').boundingBox())!;
  await page.touchscreen.tap(layer.x + layer.width * 0.4, layer.y + layer.height * 0.4);
  await expect(chart).toHaveAttribute('data-drawings', '1');
  await expect(draw).toHaveAttribute('data-active', '0');
  expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBe(0);
});

test('the legend is one line and the volume has its unit', async ({ appPage: page, mock }) => {
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await fund(mock);
  await openMarket(page, mock);
  await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
  const legend = page.getByTestId('chart-legend');
  await expect(legend).toContainText('Vol');
  await expect(legend).toContainText('EXKCC');
  const lines = await legend.evaluate((el) => Math.round(el.getBoundingClientRect().height / parseFloat(getComputedStyle(el).lineHeight)));
  expect(lines, 'one line').toBe(1);
  // the read-out is cut, never wrapped: it fits or ends in an ellipsis inside the legend
  expect(await page.locator('.chart-ohlc').evaluate((el) => el.getBoundingClientRect().right <= el.parentElement!.getBoundingClientRect().right + 1)).toBe(true);
});
