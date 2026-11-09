// The Portfolio's lists in pages: rows per page 10 / 20 / 50 at the right end of the tab row (kept in the browser), `‹ Newer  1–20 of 63  Older ›` under the
// list; the tab counts stay the totals; the same on a market page's panel. Seeded with 60+ finished orders and 70+ fills. Mock stack.
// KOB_SHOTS=<dir> saves screenshots (KOB_SHOT_TAG prefixes the names).
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { seedPortfolio, seedManyDone } from '../helpers/portfolio-seed';
import { horizontalOverflow } from '../helpers/i18n';

test.use({ autoOpen: false });

const rowIds = (page: Page) => page.locator('[data-testid^="order-row-"]').evaluateAll((els) => els.map((e) => e.getAttribute('data-testid')!));
const shot = async (page: Page, name: string) => {
  const dir = process.env.KOB_SHOTS;
  if (!dir) return;
  await page.waitForTimeout(1200); // the rows rise in with an animation
  await page.screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-${name}.png`, fullPage: true });
};
/** A fresh browser has no stored rows per page: the test starts from the default (the init script runs on every load, the flag keeps it to the first). */
const forgetRows = (page: Page) =>
  page.addInitScript(() => {
    if (sessionStorage.getItem('pf-seen')) return;
    sessionStorage.setItem('pf-seen', '1');
    localStorage.removeItem('kob.portfolio.rows');
  });

for (const width of [390, 800, 1280]) {
  test.describe(`${width} px wide`, () => {
    test.use({ viewport: { width, height: 900 } });

    test('Portfolio: rows per page and Newer / Older on Order history and Fills', async ({ appPage: page, mock }) => {
      test.setTimeout(240_000);
      await forgetRows(page);
      const base = await seedPortfolio(mock);
      await seedManyDone(mock, 60);
      await listExkcc(page);
      await page.goto('/#/portfolio/history');
      await connectWallet(page, 'kasware');
      const total = 3 + 60; // the history: the seeded finished orders and the sixty more
      await expect(page.getByTestId('orders-tab-history')).toContainText(`(${total})`);
      // the rows control, the default 20 rows
      const rows = page.getByTestId('portfolio-rows-box');
      await expect(rows).toBeVisible();
      await expect(page.getByTestId('portfolio-rows-20')).toHaveAttribute('aria-checked', 'true');
      await expect(page.locator('[data-testid^="order-row-"]')).toHaveCount(20);
      await expect(page.getByTestId('portfolio-showing')).toHaveText(`1–20 of ${total}`);
      // the tab line: the control is on the line of the tabs, centred with them, where there is room (1280 px, and 800 px unless the font is wide); else under the tabs, at the right edge
      const tabBox = (await page.getByTestId('orders-tab-history').boundingBox())!;
      const rowsBox = (await rows.boundingBox())!;
      const under = rowsBox.y > tabBox.y + tabBox.height - 2;
      if (width === 1280) expect(under, 'on the line of the tabs').toBe(false); // (at 800 px a wide font may leave no room beside the tabs)
      if (width === 390) expect(under, 'under the tabs').toBe(true);
      if (!under) expect(Math.abs(rowsBox.y + rowsBox.height / 2 - (tabBox.y + tabBox.height / 2)), 'centred with the tabs').toBeLessThan(8);
      else expect(Math.abs(rowsBox.x + rowsBox.width - (await page.getByTestId('portfolio-panel-history').boundingBox())!.x - (await page.getByTestId('portfolio-panel-history').boundingBox())!.width), 'right-aligned').toBeLessThan(2);
      expect(rowsBox.x + rowsBox.width, 'inside the page').toBeLessThanOrEqual(width + 0.5);
      expect(await horizontalOverflow(page)).toBe(0);
      await shot(page, `paging-${width}-1-history`);

      // 10 rows; older and newer page through the history; the counts stay
      await page.getByTestId('portfolio-rows-10').click();
      await expect(page.locator('[data-testid^="order-row-"]')).toHaveCount(10);
      await expect(page.getByTestId('portfolio-showing')).toHaveText(`1–10 of ${total}`);
      const first = await rowIds(page);
      await expect(page.getByTestId('portfolio-newer')).toBeDisabled();
      await page.getByTestId('portfolio-older').click();
      await expect(page.getByTestId('portfolio-showing')).toHaveText(`11–20 of ${total}`);
      const second = await rowIds(page);
      expect(second).toHaveLength(10);
      expect(second.filter((id) => first.includes(id))).toEqual([]);
      await page.getByTestId('portfolio-newer').click();
      await expect(page.getByTestId('portfolio-showing')).toHaveText(`1–10 of ${total}`);
      expect(await rowIds(page)).toEqual(first);
      await expect(page.getByTestId('orders-tab-history')).toContainText(`(${total})`);

      // 50 rows: the last page holds the rest, Older is over; changing the rows goes back to the first page
      await page.getByTestId('portfolio-rows-50').click();
      await expect(page.locator('[data-testid^="order-row-"]')).toHaveCount(50);
      await page.getByTestId('portfolio-older').click();
      await expect(page.getByTestId('portfolio-showing')).toHaveText(`51–${total} of ${total}`);
      await expect(page.locator('[data-testid^="order-row-"]')).toHaveCount(total - 50);
      await expect(page.getByTestId('portfolio-older')).toBeDisabled();
      await shot(page, `paging-${width}-2-history-last`);
      await page.getByTestId('portfolio-rows-20').click();
      await expect(page.getByTestId('portfolio-showing')).toHaveText(`1–20 of ${total}`);
      await page.getByTestId('portfolio-rows-10').click();

      // the choice is kept in the browser: a reload (the wallet reconnects) keeps 10
      await page.reload();
      await connectWallet(page, 'kasware');
      await expect(page.getByTestId('portfolio-rows-10')).toHaveAttribute('aria-checked', 'true');
      await expect(page.locator('[data-testid^="order-row-"]')).toHaveCount(10);

      // Open orders: few rows, no pager
      await page.getByTestId('orders-tab-active').click();
      await expect(page.getByTestId(`order-row-${base.open}`)).toBeVisible();
      await expect(page.getByTestId('portfolio-pager')).toHaveCount(0);

      // Fills: the same
      await page.getByTestId('orders-tab-fills').click();
      await expect(page.getByTestId('wallet-fill-row').first()).toBeVisible();
      await expect(page.getByTestId('wallet-fill-row')).toHaveCount(10);
      const shownText = (await page.getByTestId('wallet-fills-showing').textContent())!;
      const fillsTotal = Number(/of (\d+)$/.exec(shownText)![1]);
      expect(fillsTotal).toBeGreaterThanOrEqual(70);
      const firstTx = await page.getByTestId('wallet-fill-row').evaluateAll((els) => els.map((e) => e.getAttribute('data-txid')));
      await page.getByTestId('wallet-fills-older').click();
      await expect(page.getByTestId('wallet-fills-showing')).toHaveText(`11–20 of ${fillsTotal}`);
      const nextTx = await page.getByTestId('wallet-fill-row').evaluateAll((els) => els.map((e) => e.getAttribute('data-txid')));
      expect(nextTx.filter((x) => firstTx.includes(x))).toEqual([]);
      await shot(page, `paging-${width}-3-fills`);
      await page.getByTestId('portfolio-rows-50').click();
      await expect(page.getByTestId('wallet-fill-row')).toHaveCount(50);
      await expect(page.getByTestId('wallet-fills-showing')).toHaveText(`1–50 of ${fillsTotal}`);
      expect(await horizontalOverflow(page)).toBe(0);

      // Balances: the control is inert and unseen
      await page.getByTestId('orders-tab-balances').click();
      await expect(page.getByTestId('balances-panel')).toBeVisible();
      await expect(rows).toHaveAttribute('aria-hidden', 'true');
      await expect(rows).not.toBeVisible();
    });

    test("a market page's panel pages the same way, with 'This market only' ticked and not", async ({ appPage: page, mock }) => {
      test.setTimeout(240_000);
      await forgetRows(page);
      await seedPortfolio(mock);
      await seedManyDone(mock, 30);
      await listExkcc(page);
      const token = await mock.token();
      await page.goto(`/#/market/${token.covenant_id}`);
      await connectWallet(page, 'kasware');
      const panel = page.getByTestId('market-orders');
      await expect(panel).toBeVisible();
      await panel.getByTestId('orders-tab-history').click();
      await expect(panel.locator('[data-testid^="order-row-"]')).toHaveCount(20);
      const showing = (await panel.getByTestId('portfolio-showing').textContent())!;
      const here = Number(/of (\d+)$/.exec(showing)![1]);
      expect(here).toBeGreaterThanOrEqual(30); // the thirty more of this market, and the finished ones seeded before
      await panel.getByTestId('portfolio-older').click();
      await expect(panel.getByTestId('portfolio-showing')).toHaveText(`21–${Math.min(40, here)} of ${here}`);
      // all markets: back on the first page, the total can only grow
      await panel.getByTestId('market-orders-only-input').uncheck();
      await expect(panel.getByTestId('portfolio-showing')).toHaveText(/^1–20 of \d+$/);
      await panel.getByTestId('portfolio-rows-10').click();
      await expect(panel.locator('[data-testid^="order-row-"]')).toHaveCount(10);
      await shot(page, `paging-market-${width}`);
      expect(await horizontalOverflow(page)).toBe(0);
    });
  });
}
