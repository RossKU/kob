// The Portfolio page on a phone (360 and 390 px, dark): every tab reads cleanly. No horizontal scroll, the summary strip's figures whole (never an ellipsis),
// the Fills tab's transaction on one line, the order lists as aligned cards (full market, status at the right edge, a short budget label, a tap-sized Cancel),
// the balances as cards. Seeded: long figures, a KAS-budget buy, an unregistered token named by its hex id, a dozen fills, a part-filled order.
// KOB_SHOTS=<dir> also saves a full-page screenshot of every tab (KOB_SHOT_TAG prefixes the names, KOB_THEME picks dark / light). Mock stack.
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';
import { PF_ID as ID, seedPortfolio as seed } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

/** Saves a full-page screenshot when KOB_SHOTS is set. */
async function shot(page: Page, name: string): Promise<void> {
  const dir = process.env.KOB_SHOTS;
  if (!dir) return;
  await page.waitForTimeout(1200); // the rows rise in with an animation
  const base = `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-${name}`;
  await page.screenshot({ path: `${base}.png`, fullPage: true });
  // the same page in screen-sized pieces, to read them at full size (the page below the tab's panel is the footer)
  const h = await page.evaluate(() => document.documentElement.scrollHeight);
  const w = await page.evaluate(() => window.innerWidth);
  for (let i = 0; i * 800 < Math.min(h, 2400); i++) await page.screenshot({ path: `${base}-p${i + 1}.png`, fullPage: true, clip: { x: 0, y: i * 800, width: w, height: Math.min(800, h - i * 800) } });
}

/** No text of `loc` is cut: its box shows all of its content (an ellipsis or a clip would leave scrollWidth above clientWidth). */
async function expectWhole(loc: Locator, what: string): Promise<void> {
  const cut = await loc.evaluateAll((els) => els.filter((e) => e.scrollWidth > e.clientWidth + 1).map((e) => e.textContent));
  expect.soft(cut, `${what} cut`).toEqual([]);
}

/** Every element of `loc` is on one line (its box is not taller than 1.6 lines of its text). */
async function expectOneLine(loc: Locator, what: string): Promise<void> {
  const tall = await loc.evaluateAll((els) => els.filter((e) => e.getBoundingClientRect().height > 1.6 * parseFloat(getComputedStyle(e).lineHeight)).map((e) => e.textContent));
  expect.soft(tall, `${what} on several lines`).toEqual([]);
}

for (const width of [360, 390, 430]) {
  test.describe(`phone, ${width} px wide`, () => {
    test.use({ viewport: { width, height: 800 } });

    test(`Portfolio: every tab reads cleanly at ${width} px`, async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      const theme = process.env.KOB_THEME ?? 'dark';
      await page.addInitScript((th) => localStorage.setItem('kob.theme', th), theme);
      const ids = await seed(mock);
      await listExkcc(page);
      await page.goto('/#/portfolio');
      await connectWallet(page, 'kasware');
      const tag = `${width}-${theme}`;

      const noOverflow = async (where: string) => expect.soft(await horizontalOverflow(page), `horizontal overflow on ${where}`).toBe(0);

      // the summary strip: four stats, two by two, every figure whole
      await expect(page.getByTestId('portfolio-total')).toContainText('KAS');
      await expect(page.getByTestId('portfolio-open-count')).toHaveText(/^[1-9]/);
      const strip = page.getByTestId('portfolio-summary');
      await expect(strip).toBeVisible();
      await expectWhole(strip.locator('.pf-v, .pf-v > span, .pf-s, .pf-k'), 'the summary strip');
      const cols = await strip.evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length);
      expect.soft(cols, 'summary columns').toBe(2);

      // the four tabs stand on one line (one offsetTop), short on a phone, their full words kept as the accessible names; the Rows control takes no room on Balances
      const tops = await page.locator('.portfolio [role="tab"]').evaluateAll((els) => els.map((e) => (e as HTMLElement).offsetTop));
      expect.soft(new Set(tops).size, `the tabs' offsetTop: ${tops}`).toBe(1);
      expect.soft(tops).toHaveLength(4);
      await expect(page.getByTestId('orders-tab-active')).toHaveAttribute('aria-label', /^Open orders [(][0-9]+[)]$/);
      await expect(page.getByTestId('orders-tab-history')).toHaveAttribute('aria-label', /^Order history [(][0-9]+[)]$/);
      await expect(page.getByTestId('orders-tab-history')).toHaveText(/^History [(][0-9]+[)]$/);
      expect.soft(await page.getByTestId('portfolio-rows-box').isVisible(), 'no Rows control on Balances').toBe(false);
      const tabsBottom = (await page.locator('.pf-tabbar').boundingBox())!.y + (await page.locator('.pf-tabbar').boundingBox())!.height;
      const cardTop = (await page.getByTestId('balances-panel').boundingBox())!.y;
      expect.soft(cardTop - tabsBottom, 'no empty band between the tabs and the card').toBeLessThan(20);

      // Balances
      await expect(page.getByTestId('token-balances-table')).toBeVisible();
      await expect(page.getByTestId(`balance-row-${ID}`)).toBeVisible();
      await noOverflow('Balances');
      // KAS is drawn with the Kaspa mark (an inline picture named KAS), not a letter
      await expect(page.getByTestId('kas-balance').getByTestId('kas-icon')).toBeVisible();
      await expect(page.getByTestId('kas-balance').getByTestId('kas-icon')).toHaveAttribute('aria-label', 'KAS');
      await shot(page, `${tag}-1-balances`);
      const row = page.getByTestId('balance-row-' + ID);
      expect(await row.evaluate((el) => getComputedStyle(el).display)).not.toBe('table-row'); // a card
      const rb = (await row.boundingBox())!;
      expect.soft(rb.x).toBeGreaterThanOrEqual(0);
      expect.soft(rb.x + rb.width).toBeLessThanOrEqual(width + 1);

      /** the icon, the market and the status of every line at one x (the lines are cards of one grid) */
      const expectAligned = async (where: string) => {
        const lines = page.locator('[data-testid^="order-line-"]');
        expect(await lines.count()).toBeGreaterThanOrEqual(3);
        const xs = await lines.evaluateAll((els) => els.map((el) => ({
          icon: el.querySelector('.fp, [data-testid^="order-identity-"]')?.getBoundingClientRect().left ?? -1,
          market: el.querySelector('.or-market-name')?.getBoundingClientRect().left ?? -1,
          status: el.querySelector('.or-status')?.getBoundingClientRect().right ?? -1,
        })));
        for (const k of ['icon', 'market', 'status'] as const) {
          const set = new Set(xs.map((x) => Math.round(x[k])));
          expect.soft(set.size, `${where}: ${k} x positions across the rows: ${[...set]}`).toBe(1);
        }
        await expectWhole(page.locator('.or-market-name, .or-price, .or-filled'), `${where}: the lines`);
      };

      // Open orders
      await page.getByTestId('orders-tab-active').click();
      await expect(page.getByTestId(`order-row-${ids.open}`)).toBeVisible();
      await noOverflow('Open orders');
      await shot(page, `${tag}-2-open`);
      await expectAligned('Open orders');
      await expect.soft(page.getByTestId(`order-line-${ids.budget}`)).not.toContainText('the amount follows from the price');
      await expect.soft(page.getByTestId(`order-line-${ids.budget}`).locator('.or-filled')).toContainText('KAS budget');
      const cancel = page.getByTestId(`order-cancel-${ids.open}`);
      await expect(cancel).toBeVisible();
      expect.soft((await cancel.boundingBox())!.height, 'Cancel tap target').toBeGreaterThanOrEqual(40);

      // Order history: the same lines, aligned (the icon, the market and the status of every row at one x), a short budget label
      await page.getByTestId('orders-tab-history').click();
      for (const id of ids.done) await expect(page.getByTestId(`order-row-${id}`)).toBeVisible();
      await noOverflow('Order history');
      await shot(page, `${tag}-3-history`);
      await expectAligned('Order history');

      // Fills: one card per fill, the transaction whole on one line
      await page.getByTestId('orders-tab-fills').click();
      const fills = page.getByTestId('wallet-fill-row');
      await expect(fills.first()).toBeVisible();
      expect(await fills.count()).toBeGreaterThanOrEqual(12);
      await noOverflow('Fills');
      await shot(page, `${tag}-4-fills`);
      await expectOneLine(page.getByTestId('wallet-fill-tx'), 'the transaction');
      await expectWhole(fills.locator('.wf-tx code'), 'the transaction');
      const txt = await page.getByTestId('wallet-fill-tx').first().textContent();
      expect.soft(txt, 'shortened txid').toMatch(/^[0-9a-f]{6}…[0-9a-f]{4}$/);

      // the order card's detail still opens without overflow
      await page.getByTestId('orders-tab-active').click();
      await page.getByTestId(`order-toggle-${ids.budget}`).click().catch(() => undefined);
      await noOverflow('an order\'s details');
    });
  });
}

test.describe('desktop, 1280 px wide', () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test('Portfolio keeps its tables and one-line orders', async ({ appPage: page, mock }) => {
    test.setTimeout(120_000);
    const theme = process.env.KOB_THEME ?? 'dark';
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), theme);
    const ids = await seed(mock);
    await listExkcc(page);
    await page.goto('/#/portfolio');
    await connectWallet(page, 'kasware');
    await expect(page.getByTestId('portfolio-total')).toContainText('KAS');
    await expect(page.getByTestId(`balance-row-${ID}`)).toBeVisible();
    expect(await page.getByTestId(`balance-row-${ID}`).evaluate((el) => getComputedStyle(el).display)).toBe('table-row');
    expect(await page.getByTestId('portfolio-summary').evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length)).toBe(4);
    await shot(page, `1280-${theme}-1-balances`);
    await page.getByTestId('orders-tab-active').click();
    await expect(page.getByTestId(`order-row-${ids.open}`)).toBeVisible();
    await shot(page, `1280-${theme}-2-open`);
    await page.getByTestId('orders-tab-history').click();
    for (const id of ids.done) await expect(page.getByTestId(`order-row-${id}`)).toBeVisible();
    await shot(page, `1280-${theme}-3-history`);
    await page.getByTestId('orders-tab-fills').click();
    await expect(page.getByTestId('wallet-fills-table')).toBeVisible();
    await expect(page.getByTestId('wallet-fills-cards')).toHaveCount(0);
    await shot(page, `1280-${theme}-4-fills`);
  });
});
