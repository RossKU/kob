// The order ticket never changes size and never moves anything (founder, 2026-11-10; the usual exchange layout): the panel has the height of its row (beside the book
// and the chart) or a fixed height (a phone), its form scrolls inside it and the Buy / Sell button is pinned to its bottom. Switching Limit / Market / Stop, the
// Ladder, the expiry fields and the Advanced box, typing an amount and an error in the reason slot change nothing outside the form: the button, the book, the
// charts and the activity log keep their boxes (+-1 px), and nothing on the page scrolls sideways. Mock stack.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType, settlePlan } from '../helpers/ticket';
import { horizontalOverflow } from '../helpers/i18n';

test.use({ autoOpen: false });

/** What must not move: the ticket panel, its button and scroll area's footer, the book's panel (the depth chart is its Depth view), the chart and the activity log under them. */
const watched = (_page: Page): string[] => ['ticket-container', 'order-ticket', 'order-place', 'order-book', 'book-section', 'chart-section', 'activity-list'];
type Box = { x: number; y: number; width: number; height: number };

/** Document coordinates (the page may scroll between two readings). */
const boxes = (page: Page): Promise<Record<string, Box>> =>
  page.evaluate((ids) => {
    const out: Record<string, Box> = {};
    for (const id of ids) {
      const el = document.querySelector(`[data-testid="${id}"]`);
      if (!el) continue;
      const r = el.getBoundingClientRect();
      out[id] = { x: r.x + scrollX, y: r.y + scrollY, width: r.width, height: r.height };
    }
    return out;
  }, watched(page));

/** Three columns (from 1200 px): the book, the chart and the ticket are one row and end on one line, where the ticket's button is pinned. */
async function expectOneBottomLine(page: Page): Promise<void> {
  const bottom = (id: string) => page.getByTestId(id).evaluate((el) => el.getBoundingClientRect().bottom + scrollY);
  const chart = await bottom('chart-section');
  expect(Math.abs((await bottom('order-ticket')) - chart), 'the ticket ends at the chart').toBeLessThanOrEqual(1);
  expect(Math.abs((await bottom('book-section')) - chart), 'the book ends at the chart').toBeLessThanOrEqual(1);
}

async function run(page: Page, mock: import('../fixtures').MockClient): Promise<void> {
  test.setTimeout(120_000);
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'limit', 'buy');
  await settlePlan(page);
  // what loads under the grid (the activity log's rows, the wallet's panel) changes the page by itself: the boxes are taken once it is in
  await page.getByTestId('activity-list').scrollIntoViewIfNeeded(); // it reads its rows once it is on screen
  await expect(page.getByTestId('activity-list').getByTestId('activity-row').first()).toBeVisible();
  await expect(page.getByTestId('market-orders').getByTestId('orders-section')).toBeVisible();
  await page.waitForTimeout(500);
  if ((page.viewportSize()?.width ?? 0) >= 1200) await expectOneBottomLine(page);
  const base = await boxes(page);
  for (const id of watched(page)) expect(base[id], `${id} is on the page`).toBeDefined();
  const check = async (step: string) => {
    await settlePlan(page);
    await page.waitForTimeout(250);
    const now = await boxes(page);
    for (const id of watched(page)) {
      for (const k of ['x', 'y', 'width', 'height'] as const) {
        expect(Math.abs(now[id]![k] - base[id]![k]), `${step}: ${id}.${k} ${base[id]![k]} -> ${now[id]![k]}`).toBeLessThanOrEqual(1);
      }
    }
    expect(await horizontalOverflow(page), `${step}: nothing scrolls sideways`).toBe(0);
    // the form's own scroll area never makes the page's scroll area wider either
    expect(await page.getByTestId('order-scroll').evaluate((el) => el.scrollWidth - el.clientWidth), `${step}: the form does not scroll sideways`).toBeLessThanOrEqual(0);
  };

  await check('limit');
  await pickType(page, 'market', 'buy');
  await check('market');
  // a market order keeps its price row, disabled and reading "Market price" (Limit's price row is where it stands), so the rows and the button do not move
  await expect(page.getByTestId('order-price-market')).toBeDisabled();
  await expect(page.getByTestId('order-price-market')).toHaveValue('Market price');
  await pickType(page, 'stopLimit', 'buy');
  await expect(page.getByTestId('order-price-market')).toHaveCount(0);
  await check('stop limit');
  await pickType(page, 'limit', 'buy');
  await check('limit again');
  await fillFields(page, { amount: '1', price: '0.0245' });
  await check('an amount and a price typed');
  await fillFields(page, { 'ladder.on': true, 'ladder.levels': '3', 'ladder.pct': '2' });
  await expect(page.getByTestId('order-ladder')).toBeVisible();
  await check('limit + ladder');
  await fillFields(page, { amount: '9999999' });
  await expect(page.getByTestId('order-issues').locator('.tk-reason-item').first()).toBeVisible();
  await check('a validation error');
  await fillFields(page, { amount: '1', lifetime: 'gtd' });
  await check('expiry fields');
  await pickType(page, 'ifo', 'buy');
  await check('a long form (if-done with a bracket)');
  // the button stays where it is while the form scrolls inside the panel
  await page.getByTestId('order-scroll').evaluate((el) => { el.scrollTop = el.scrollHeight; });
  await check('the form scrolled to its end');
  await expect(page.getByTestId('order-place')).toBeVisible();
}

test.describe('the ticket keeps its size', () => {
  test.describe('desktop, 1280 px', () => {
    test.use({ viewport: { width: 1280, height: 900 } });
    test('Limit, Market, Stop, Ladder, expiry, typing and errors move nothing', async ({ appPage: page, mock }) => {
      await run(page, mock);
    });
  });

  test.describe('wide, 1920 px', () => {
    test.use({ viewport: { width: 1920, height: 1000 } });
    test('the book, the chart and the ticket end on one line, through every type', async ({ appPage: page, mock }) => {
      await run(page, mock);
      await pickType(page, 'limit', 'buy');
      await expectOneBottomLine(page);
    });
  });

  test.describe('phone, 390 px', () => {
    test.use({ viewport: { width: 390, height: 844 } });
    test('Limit, Market, Stop, Ladder, expiry, typing and errors move nothing', async ({ appPage: page, mock }) => {
      await run(page, mock);
    });
  });
});
