// How the Portfolio's tables (and the market page's panel: the same components) change their structure as the room shrinks: a table, then the table folded, then cards, decided by FIT in
// one fixed order, so the same width always gives the same structure (the founder's tablet showed cards and, a minute later at the same width, a table). A phone (below 640 px) is cards
// with no sort control. The Connect button of a page is the header's. KOB_WIDE_FONTS=1 runs the page in the CI runner's wide font. Mock stack.
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

const fold = (table: Locator) => table.getAttribute('data-fold');

/** The embedded panel of a market page, on `tab` (a tab id of the orders view), with the wallet connected. */
async function openPanel(page: Page, mock: import('../fixtures').MockClient, tab: string): Promise<Locator> {
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
  await seedPortfolio(mock);
  await listExkcc(page);
  const token = await mock.token();
  await page.goto(`/#/market/${token.covenant_id}`);
  await connectWallet(page, 'kasware');
  const panel = page.getByTestId('market-orders');
  await expect(panel).toBeVisible();
  await panel.scrollIntoViewIfNeeded();
  await panel.getByTestId(`orders-tab-${tab}`).click();
  await expect(panel.locator('.dt-row').first()).toBeVisible();
  await page.waitForTimeout(600);
  return panel.locator('.dt').first();
}

test.describe('a tablet: the structure follows the room, not the history', () => {
  test.use({ viewport: { width: 1000, height: 600 } });

  for (const tab of ['fills', 'history', 'active']) {
    test(`${tab}: a table (folded as far as it needs), never cards; the same at the same width however it got there`, async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      const table = await openPanel(page, mock, tab);
      const first = await fold(table);
      expect(['0', '1', '2'], `fold at 1000 px (${tab})`).toContain(first);
      await expect(table.locator('.dt-head')).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth), 'no sideways scroll').toBeLessThanOrEqual(0);
      // narrowed to a phone and widened back, widened and narrowed back: the same structure at 1000 px each time
      for (const w of [390, 1000, 1400, 1000, 700, 1000]) {
        await page.setViewportSize({ width: w, height: 600 });
        await page.waitForTimeout(250);
        if (w === 390) expect(await fold(table), 'a phone is cards').toBe('cards');
        if (w === 1000) expect(await fold(table), 'back at 1000 px').toBe(first);
      }
      // the data arriving again changes nothing either (a refresh of the page)
      await page.reload();
      await connectWallet(page, 'kasware');
      const panel = page.getByTestId('market-orders');
      await panel.scrollIntoViewIfNeeded();
      await panel.getByTestId(`orders-tab-${tab}`).click();
      await expect(panel.locator('.dt-row').first()).toBeVisible();
      await page.waitForTimeout(600);
      expect(await fold(panel.locator('.dt').first()), 'after a reload').toBe(first);
    });
  }
});

test.describe('a phone: cards with no sort control', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  for (const tab of ['fills', 'history', 'active', 'balances']) {
    test(`${tab}: cards, no Sort, no sideways scroll`, async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      const table = await openPanel(page, mock, tab);
      expect(await fold(table)).toBe('cards');
      await expect(table.locator('.dt-sortctl')).toHaveCount(0);
      await expect(table.locator('.dt-head')).toBeHidden();
      expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth), 'no sideways scroll').toBeLessThanOrEqual(0);
    });
  }
});

for (const theme of ['light', 'dark']) {
  test.describe(`the Connect button, ${theme}`, () => {
    test.use({ viewport: { width: 1280, height: 800 } });

    test('the Portfolio\'s Connect is the header\'s: one class, one size, one colour pair', async ({ appPage: page }) => {
      await page.addInitScript((th) => localStorage.setItem('kob.theme', th), theme);
      await page.goto('/#/portfolio');
      const header = page.getByTestId('wallet-connect-list').locator('.btn-pill').first();
      const inPage = page.getByTestId('orders-connect').locator('.btn-pill').first();
      await expect(header).toBeVisible();
      await expect(inPage).toBeVisible();
      const look = (l: Locator) =>
        l.evaluate((el) => {
          const cs = getComputedStyle(el);
          const r = el.getBoundingClientRect();
          return { bg: cs.backgroundColor, color: cs.color, radius: cs.borderTopLeftRadius, font: cs.fontSize, weight: cs.fontWeight, height: Math.round(r.height), padX: cs.paddingLeft, border: cs.borderTopColor };
        });
      expect(await look(inPage)).toEqual(await look(header));
    });
  });
}
