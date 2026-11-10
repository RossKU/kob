// The rows of the Portfolio's table open to their details (a chevron: a button with aria-expanded, keyboard operable) and stay as the viewer left them across refreshes: an
// opened row is not closed by a refresh, and nothing shifts. On the Portfolio page and in a market page's panel; Open orders, Order history and Trade history. Mock stack.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

const RECORD = () => {
  const w = window as unknown as { __cls: number };
  w.__cls = 0;
  new PerformanceObserver((list) => {
    for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[]) if (!e.hadRecentInput) w.__cls += e.value;
  }).observe({ type: 'layout-shift', buffered: false });
};
const cls = (page: Page) => page.evaluate(() => (window as unknown as { __cls: number }).__cls);

for (const width of [390, 1280]) {
  test.describe(`${width} px wide`, () => {
    test.use({ viewport: { width, height: 1000 } });

    test('an order and a fill open by the chevron, by the keyboard, and stay open across a refresh without a shift', async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      const ids = await seedPortfolio(mock);
      await listExkcc(page);
      await page.goto('/#/portfolio/open');
      await connectWallet(page, 'kasware');
      const toggle = page.getByTestId(`order-toggle-${ids.open}`);
      const details = page.getByTestId(`order-details-${ids.open}`);
      await expect(toggle).toBeVisible();
      // collapsed by default
      await expect(toggle).toHaveAttribute('aria-expanded', 'false');
      await expect(details).toBeHidden();
      // by a click
      await toggle.click();
      await expect(toggle).toHaveAttribute('aria-expanded', 'true');
      await expect(details).toBeVisible();
      // by the keyboard: Enter closes it, Space opens it again
      await toggle.focus();
      await page.keyboard.press('Enter');
      await expect(toggle).toHaveAttribute('aria-expanded', 'false');
      await expect(details).toBeHidden();
      await page.keyboard.press('Space');
      await expect(toggle).toHaveAttribute('aria-expanded', 'true');
      await expect(details).toBeVisible();
      // a refresh keeps it open and moves nothing
      await page.waitForTimeout(800);
      await page.evaluate(RECORD);
      await page.getByTestId('orders-refresh').click();
      await page.waitForTimeout(2_500);
      await expect(toggle).toHaveAttribute('aria-expanded', 'true');
      await expect(details).toBeVisible();
      expect(await cls(page), 'layout shift across a refresh').toBeLessThan(0.02);
      // another tab and back: still open
      await page.getByTestId('orders-tab-history').click();
      await page.getByTestId('orders-tab-active').click();
      await expect(page.getByTestId(`order-toggle-${ids.open}`)).toHaveAttribute('aria-expanded', 'true');

      // the trade history: the same
      await page.getByTestId('orders-tab-fills').click();
      const fillToggle = page.getByTestId('wallet-fill-toggle').first();
      await expect(fillToggle).toBeVisible();
      await expect(fillToggle).toHaveAttribute('aria-expanded', 'false');
      await fillToggle.click();
      await expect(fillToggle).toHaveAttribute('aria-expanded', 'true');
      await expect(page.getByTestId('wallet-fill-details').first()).toBeVisible();
      await page.getByTestId('orders-refresh').click();
      await page.waitForTimeout(2_000);
      await expect(page.getByTestId('wallet-fill-toggle').first()).toHaveAttribute('aria-expanded', 'true');
    });

    test("in a market page's panel too", async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      const ids = await seedPortfolio(mock);
      await listExkcc(page);
      const token = await mock.token();
      await page.goto(`/#/market/${token.covenant_id}`);
      await connectWallet(page, 'kasware');
      const mine = page.getByTestId('market-orders');
      const toggle = mine.getByTestId(`order-toggle-${ids.open}`);
      await expect(toggle).toBeVisible();
      await toggle.click();
      await expect(toggle).toHaveAttribute('aria-expanded', 'true');
      await expect(mine.getByTestId(`order-details-${ids.open}`)).toBeVisible();
      await mine.getByTestId('orders-tab-history').click();
      await mine.getByTestId('orders-tab-active').click();
      await expect(mine.getByTestId(`order-toggle-${ids.open}`)).toHaveAttribute('aria-expanded', 'true');
    });
  });
}
