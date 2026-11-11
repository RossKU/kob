// Below 1200 px the depth chart has no panel of its own: it is the "Depth" view of the ORDER BOOK's panel (an "Order book | Depth" switch in the panel's header, the choice
// remembered in this browser). The price chart is never hidden, at any width. A click on the depth chart prefills the ticket like a click on a book row (an ask a buy,
// a bid a sell, at the price under the pointer, on the market's price step), also on three columns where the depth chart is a panel of its own. Mock stack.
import type { Locator, Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { listExkcc } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';

async function open(page: Page, mock: MockClient): Promise<void> {
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await listExkcc(page);
  const tok = await mock.token();
  await page.goto(`/#/market/${tok.covenant_id}`);
  await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('order-ticket')).toBeVisible();
}

const rect = async (loc: Locator) => (await loc.boundingBox())!;
/** Document coordinates (the page may scroll between two measurements). */
const docRect = (loc: Locator) => loc.evaluate((el) => { const r = el.getBoundingClientRect(); return { x: r.x + scrollX, y: r.y + scrollY, width: r.width, height: r.height }; });

/** The price the depth chart's tooltip reads at `fraction` of the way from the mid line to the right (ask) or the left (bid) edge, after moving the pointer there. */
async function hoverPrice(page: Page, depth: Locator, side: 'ask' | 'bid', fraction: number): Promise<{ x: number; y: number; price: number }> {
  const svg = depth.locator('svg.depth-svg');
  await svg.scrollIntoViewIfNeeded();
  const box = await rect(svg);
  const mid = Number(await svg.locator('line.mid-line').getAttribute('x1'));
  const x = box.x + (side === 'ask' ? mid + (box.width - mid) * fraction : mid * (1 - fraction));
  const y = box.y + box.height * 0.4;
  await page.mouse.move(x, y);
  const tip = depth.locator('.depth-tip');
  await expect(tip).toBeVisible();
  await expect(tip.getByTestId('depth-pick-hint')).toHaveText('Click to use this price in the ticket');
  await expect(tip).toContainText(side === 'ask' ? 'Asks' : 'Bids');
  const text = (await tip.innerText()).replace(/,/g, '');
  return { x, y, price: Number(/(\d+\.\d+)\s/.exec(text)![1]) };
}

/** A click on the chart's `side` prefills the ticket with that side's order at a price within a step of the one the tooltip read. */
async function pickFrom(page: Page, depth: Locator, side: 'ask' | 'bid'): Promise<void> {
  const at = await hoverPrice(page, depth, side, 0.6);
  await expect(depth.locator('svg.depth-svg')).toHaveCSS('cursor', 'pointer');
  await page.mouse.click(at.x, at.y);
  const ticket = page.getByTestId('order-ticket');
  await expect(ticket).toHaveAttribute('data-side', side === 'ask' ? 'buy' : 'sell');
  const price = Number(await page.getByTestId('order-price').inputValue());
  // the market's step is 0.0001: a buy rounds up to it, a sell down, so the order reaches what the pointer pointed at
  expect(Math.abs(price - at.price), `the ticket's ${price} against the ${at.price} under the pointer`).toBeLessThan(0.00015);
  if (side === 'ask') expect(price).toBeGreaterThanOrEqual(at.price - 1e-9);
  else expect(price).toBeLessThanOrEqual(at.price + 1e-9);
  // what the book itself would have prefilled has the same shape: a level's price, on the same step
  expect(Math.round(price * 1e4) / 1e4).toBeCloseTo(price, 9);
}

for (const width of [390, 850]) {
  test.describe(`${width} px`, () => {
    test.use({ viewport: { width, height: 900 } });

    test('the depth chart is a view of the order book panel, the price chart is never hidden, and the choice is remembered', async ({ appPage: page, mock }) => {
      await open(page, mock);

      // the price chart's panel is only the price chart: its title, no switch
      await expect(page.getByTestId('chart-section').locator('h2')).toHaveText('Price');
      await expect(page.getByTestId('chart-section').getByRole('radiogroup')).toHaveCount(0);
      await expect(page.getByTestId('price-chart')).toBeVisible();

      // the book is the default view
      const view = page.getByTestId('book-view');
      await expect(view).toHaveAttribute('role', 'radiogroup');
      await expect(page.getByTestId('book-view-book')).toHaveAttribute('aria-checked', 'true');
      await expect(page.getByTestId('book-view-depth')).toHaveAttribute('aria-checked', 'false');
      expect((await rect(page.getByTestId('book-view-book'))).height, 'the switch labels stay on one line').toBeLessThan(40);
      await expect(page.getByTestId('book-section').getByTestId('order-book')).toBeVisible();
      await expect(page.getByTestId('depth-section')).toHaveCount(0);
      await expect(page.getByTestId('depth-chart')).toHaveCount(0);
      const group = page.getByTestId('book-group');
      await expect(group).toBeVisible();

      const panel = page.getByTestId('book-section');
      const chartBefore = await rect(page.locator('[data-testid="price-chart"] .chart-box'));
      const panelBefore = await rect(panel);
      const headBefore = await rect(panel.locator('.section-head'));
      const groupBefore = await docRect(group);

      await page.getByTestId('book-view-depth').click();
      await expect(page.getByTestId('book-view-depth')).toHaveAttribute('aria-checked', 'true');
      const depth = page.getByTestId('book-depth-pane').getByTestId('depth-chart');
      await expect(depth).toHaveAttribute('data-state', 'ready');
      await expect(depth.locator('svg')).toBeVisible();
      await expect(depth.locator('.bid-area')).toHaveCount(1);
      await expect(depth.locator('.ask-area')).toHaveCount(1);
      // the price chart is still there, as it was
      await expect(page.getByTestId('price-chart')).toBeVisible();
      await expect(page.locator('[data-testid="price-chart"] canvas').first()).toBeVisible();
      const chartNow = await rect(page.locator('[data-testid="price-chart"] .chart-box'));
      expect(Math.abs(chartNow.height - chartBefore.height) + Math.abs(chartNow.width - chartBefore.width)).toBeLessThanOrEqual(1);
      // the book is behind the depth chart (hidden, not unmounted); the panel keeps its size; the depth chart fills the panel's body under the header
      await expect(page.getByTestId('order-book')).toBeHidden();
      await expect(page.getByTestId('book-section')).toHaveCount(1);
      const panelNow = await rect(panel);
      expect(Math.abs(panelNow.height - panelBefore.height) + Math.abs(panelNow.width - panelBefore.width)).toBeLessThanOrEqual(1);
      const headNow = await rect(panel.locator('.section-head'));
      expect(Math.abs(headNow.height - headBefore.height)).toBeLessThanOrEqual(1);
      const d = await rect(depth.locator('.depth-box'));
      const body = await rect(panel.locator('.section-body'));
      expect(d.height, 'the depth chart is as tall as the body less its legend (a line or three)').toBeGreaterThan(body.height - 110);
      expect(d.y + d.height).toBeLessThanOrEqual(body.y + body.height + 1);
      expect(Math.abs(d.width - body.width)).toBeLessThanOrEqual(1);
      // the book's own controls keep their place and are not offered meanwhile
      await expect(group).toBeHidden();
      await expect(page.getByTestId('book-refresh')).toBeHidden();
      const groupNow = await docRect(group);
      expect(Math.abs(groupNow.x - groupBefore.x) + Math.abs(groupNow.y - groupBefore.y)).toBeLessThanOrEqual(1);
      expect(await horizontalOverflow(page), 'nothing scrolls sideways').toBe(0);

      // a click on the ask side buys, on the bid side sells
      await pickFrom(page, depth, 'ask');
      await pickFrom(page, depth, 'bid');

      // the choice is remembered across a reload
      await page.reload();
      await expect(page.getByTestId('book-view-depth')).toHaveAttribute('aria-checked', 'true');
      await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
      await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');

      // back to the book: the same book, its controls again
      await page.getByTestId('book-view-book').click();
      await expect(page.getByTestId('depth-chart')).toHaveCount(0);
      await expect(page.getByTestId('order-book')).toBeVisible();
      await expect(group).toBeVisible();
      await page.reload();
      await expect(page.getByTestId('book-view-book')).toHaveAttribute('aria-checked', 'true');
      await expect(page.getByTestId('depth-chart')).toHaveCount(0);
    });
  });
}

test.describe('1280 px', () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test('no switch: the book, the chart and the depth chart are panels of their own, and a click on the depth chart prefills the ticket', async ({ appPage: page, mock }) => {
    await open(page, mock);
    await expect(page.getByTestId('book-view')).toHaveCount(0);
    await expect(page.getByTestId('book-section').getByTestId('order-book')).toBeVisible();
    await expect(page.getByTestId('chart-section').locator('h2')).toHaveText('Price');
    const depth = page.getByTestId('depth-section');
    await expect(depth.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
    const c = await rect(page.getByTestId('chart-section'));
    const d = await rect(depth);
    expect(d.y).toBeGreaterThan(c.y + c.height - 1);
    await expect(page.getByTestId('chart-intervals')).toBeVisible();

    await pickFrom(page, depth.getByTestId('depth-chart'), 'ask');
    await pickFrom(page, depth.getByTestId('depth-chart'), 'bid');
  });
});
