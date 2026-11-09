// The wallet's panel on a market page ("Your orders and balances", OrdersView embedded): every tab switches by a real click, at a phone, a tablet and a desktop
// width; the panel takes the full content width under the chart and the ticket (like the activity log under it) and its summary figures are whole.
// KOB_SHOTS=<dir> also saves screenshots (KOB_SHOT_TAG prefixes the names). Mock stack.
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

for (const width of [390, 800, 1280]) {
  test.describe(`market page, ${width} px wide`, () => {
    test.use({ viewport: { width, height: 900 } });

    test('every tab of the embedded panel switches on a click; the panel is as wide as the page content', async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
      await seedPortfolio(mock);
      await listExkcc(page);
      const token = await mock.token();
      await page.goto(`/#/market/${token.covenant_id}`);
      await connectWallet(page, 'kasware');
      const panel = page.getByTestId('market-orders');
      await expect(panel).toBeVisible();
      await panel.scrollIntoViewIfNeeded();
      const shot = async (name: string) => {
        const dir = process.env.KOB_SHOTS;
        if (!dir) return;
        await page.waitForTimeout(1200); // the rows rise in with an animation
        await page.screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-market-${width}-${name}.png`, fullPage: true });
        await panel.screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-market-${width}-${name}-panel.png` });
      };

      // whatever a panel beside it draws outside its box (the depth chart's drawing overflows its box) must not take the taps meant for the tabs: such a layer is added over the page
      await page.evaluate(() => {
        const box = document.querySelector('.depth-box');
        const over = document.createElement('div');
        over.setAttribute('data-testid', 'overflowing-layer');
        over.setAttribute('style', 'position:absolute;left:0;top:0;width:100%;height:3000px;background:transparent');
        box?.appendChild(over);
      });
      // each tab by a real click (no force): an element over it would stop the click
      const tabs: [string, string][] = [['active', 'open'], ['history', 'history'], ['fills', 'fills'], ['balances', 'balances']];
      for (const [id, panelId] of tabs) {
        await panel.getByTestId(`orders-tab-${id}`).click({ timeout: 5_000 });
        await expect(panel.getByTestId(`orders-tab-${id}`)).toHaveAttribute('aria-selected', 'true');
        await expect(panel.getByTestId(`portfolio-panel-${panelId}`)).toBeVisible();
        await shot(id);
      }
      // and back to the first
      await panel.getByTestId('orders-tab-active').click({ timeout: 5_000 });
      await expect(panel.getByTestId('portfolio-panel-open')).toBeVisible();

      // the summary figures are whole
      const cut = await panel.locator('.pf-v, .pf-v > span').evaluateAll((els) => els.filter((e) => e.scrollWidth > e.clientWidth + 1).map((e) => e.textContent));
      expect.soft(cut, 'summary figures cut').toEqual([]);

      // the panel is as wide as the content under the grid (the activity log, when there is one, else the page container)
      const pw = (await panel.boundingBox())!.width;
      const page_ = (await page.getByTestId('token-page').boundingBox())!.width;
      if (width >= 760) expect.soft(pw, `panel ${pw}px of ${page_}px`).toBeGreaterThanOrEqual(page_ - 2);
      expect.soft(await page.evaluate(() => Math.max(0, document.documentElement.scrollWidth - window.innerWidth)), 'horizontal overflow').toBe(0);
    });
  });
}
