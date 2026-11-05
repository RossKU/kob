// The landing page: what the empty hash shows by default (config `home` = `landing`), a full page with its own nav in place of the app's header,
// banners and footer. "Open the app" goes to the market list (#/market); the app's logo comes back to it. Phone (390 px) and desktop (1440 px).
// KOB_LP_SHOTS=<dir> also saves full-page screenshots (reduced motion: every block in its final place) to compare with the approved prototype.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';

test.use({ autoOpen: false });

const noScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
const shots = process.env.KOB_LP_SHOTS;

for (const [width, height] of [
  [390, 844],
  [1440, 900],
] as const) {
  test(`${width} px: the landing page opens at the empty hash, without the app shell; Open the app leads to the market and the logo back`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height });
    await page.goto('/');
    const landing = page.getByTestId('landing');
    await expect(landing).toBeVisible();
    await expect(landing).toHaveAttribute('data-network', 'testnet-10');
    // the h1's real text for screen readers (its three visual lines are hidden from them)
    await expect(page.getByRole('heading', { level: 1, name: 'Decentralized order book on Kaspa.' })).toBeVisible();
    await expect(page.getByTestId('landing-chip')).toHaveText('testnet-10 · pre-audit');
    // no app shell: no header, banners or app footer
    await expect(page.getByTestId('brand')).toHaveCount(0);
    await expect(page.getByTestId('banner-preaudit')).toHaveCount(0);
    await expect(page.getByTestId('footer')).toHaveCount(0);
    await expect(page.getByTestId('landing-nav')).toBeVisible();
    await expect(page).toHaveTitle(/^KOB/);
    expect(await noScroll(page), 'no horizontal scroll at the top').toBe(true);

    // the hero rises in, the bars grow in when scrolled to, the footer carries the network's disclaimer and the configured indexer's API
    await expect(page.getByTestId('landing-hero')).toHaveClass(/lp-go/);
    await page.getByTestId('landing-bigbook').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('landing-bigbook')).toHaveClass(/lp-seen/);
    await expect.poll(() => page.locator('.lp-bb').first().evaluate((e) => e.getBoundingClientRect().width)).toBeGreaterThan(width * 0.5);
    await page.getByTestId('landing-footer').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('landing-disclaimer')).toContainText('Testnet · pre-audit.');
    await expect(page.getByTestId('landing-disclaimer')).toContainText('never holds funds or orders, does not trade and does not operate executors');
    await expect(page.getByTestId('landing-api')).toHaveAttribute('href', `${mock.url}/v1/health`);
    expect(await noScroll(page), 'no horizontal scroll at the bottom').toBe(true);

    // every link and button can be reached with the keyboard: the nav's CTA is a link to the market list
    const cta = page.getByTestId('landing-open-app');
    await expect(cta).toHaveAttribute('href', '#/market');
    await expect(page.getByTestId('landing-cta').first()).toHaveAttribute('href', '#/market');
    await cta.focus();
    await expect(cta).toBeFocused();
    await cta.click();
    await expect(page).toHaveURL(/#\/market$/);
    await expect(page.getByTestId('token-list-view')).toBeVisible();
    await expect(page.getByTestId('brand')).toBeVisible();
    await expect(page.getByTestId('landing')).toHaveCount(0);
    expect(await page.evaluate(() => window.scrollY), 'the app page opens at its top').toBe(0);

    // the app's logo leads back to the landing page
    await expect(page.getByTestId('brand')).toHaveAttribute('href', '#/');
    await page.getByTestId('brand').click();
    await expect(page.getByTestId('landing')).toBeVisible();
    await expect(page.getByTestId('brand')).toHaveCount(0);
  });

  test(`${width} px: the landing page follows the app's theme (dark and light)`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height });
    const bg = () => page.getByTestId('landing').evaluate((e) => getComputedStyle(e).backgroundColor);
    await page.addInitScript(() => localStorage.getItem('kob.theme') ?? localStorage.setItem('kob.theme', 'dark'));
    await page.goto('/');
    await expect(page.getByTestId('landing')).toBeVisible();
    expect(await bg()).toBe('rgb(5, 8, 8)');
    if (shots) {
      await page.emulateMedia({ reducedMotion: 'reduce' });
      await page.screenshot({ path: `${shots}/app-${width}-dark.png`, fullPage: true });
    }
    await page.evaluate(() => localStorage.setItem('kob.theme', 'light'));
    await page.reload();
    await expect(page.getByTestId('landing')).toBeVisible();
    expect(await bg()).toBe('rgb(242, 241, 236)');
    if (shots) await page.screenshot({ path: `${shots}/app-${width}-light.png`, fullPage: true });
  });
}

test.describe("an operator's explicit home", () => {
  test.use({ appConfig: { home: 'list' } });
  test('home: list shows the market list at the empty hash, with the header', async ({ appPage: page }) => {
    await page.goto('/');
    await expect(page.getByTestId('token-list-view')).toBeVisible();
    await expect(page.getByTestId('brand')).toBeVisible();
    await expect(page.getByTestId('landing')).toHaveCount(0);
  });
});

test.describe('mainnet build', () => {
  test.use({ appConfig: { network: 'mainnet' } });
  test('the chip, the audit row and the disclaimer name mainnet, never testnet', async ({ appPage: page }) => {
    await page.goto('/');
    await expect(page.getByTestId('landing')).toHaveAttribute('data-network', 'mainnet');
    await expect(page.getByTestId('landing-chip')).toHaveText('mainnet · pre-audit');
    await expect(page.getByTestId('landing-disclaimer')).toContainText('Mainnet · pre-audit.');
    await expect(page.getByTestId('landing')).not.toContainText(/testnet|test tokens/i);
  });
});

test('a portrait screen (a phone showing the desktop site, a tablet) gets no full-screen hero: the headline sits near the top', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 980, height: 2200 });
  await page.goto('/#/');
  await expect(page.locator('.lp-hero h1')).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => document.querySelector('.lp-hero h1')?.getBoundingClientRect().y ?? Infinity), { message: 'no screen of empty space above the headline' })
    .toBeLessThan(500);
});

test('390 px: the bots section (command line, read API) fits the screen: no text or terminal runs past its edge', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 390, height: 900 });
  await page.goto('/#/');
  const sec = page.getByTestId('landing-bots');
  await sec.scrollIntoViewIfNeeded();
  await expect(sec.getByRole('link', { name: /Command line/ })).toBeVisible();
  const rights = await sec.evaluate((el) => [...el.querySelectorAll('p, h2, .lp-tx')].map((e) => e.getBoundingClientRect().right));
  for (const r of rights) expect(r).toBeLessThanOrEqual(390);
});
