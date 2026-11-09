// The landing page: what the empty hash shows by default (config `home` = `landing`), a full page in place of the app's tabs and banners, in the same
// frame as the app (one header row and one footer, Frame.tsx). "Open the app" goes to the market list (#/market); the app's logo comes back to it. Phone (390 px) and desktop (1440 px).
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
    await expect(page.getByTestId('landing-chip')).toHaveText('testnet-10 · open review');
    // no app shell: no app header (tabs, wallet, bell) or app links; the shared frame instead: one header, one footer
    await expect(page.getByTestId('brand')).toHaveCount(0);
    await expect(page.getByTestId('nav-market')).toHaveCount(0);
    await expect(page.getByTestId('wallet-connect-list')).toHaveCount(0);
    await expect(page.getByTestId('notify-bell')).toHaveCount(0);
    await expect(page.getByTestId('banner-preaudit')).toHaveCount(0);
    await expect(page.getByTestId('landing-nav')).toBeVisible();
    await expect(page.locator('header')).toHaveCount(1);
    await expect(page.locator('footer')).toHaveCount(1);
    await expect(page.getByTestId('footer')).toHaveAttribute('data-context', 'landing');
    await expect(page.getByTestId('network-chip')).toBeVisible();
    await expect(page.getByTestId('network-chip')).toHaveText(width < 1200 ? 'tn10 · open review' : 'testnet-10 · open review');
    await expect(page.getByTestId('theme-toggle')).toHaveCount(1);
    await expect(page.getByTestId('theme-toggle')).toBeVisible();
    // the header has the app's height; see-through at the top (the page's background runs under it), a blurred bar once the page is scrolled
    const nav = (await page.getByTestId('landing-nav').boundingBox())!;
    expect(nav.y).toBe(0);
    expect(nav.height).toBe(width < 720 ? 52 : 56);
    const bar = () => page.getByTestId('landing-nav').evaluate((e) => ({ opacity: getComputedStyle(e, '::before').opacity, scrolled: e.hasAttribute('data-scrolled') }));
    expect(await bar()).toEqual({ opacity: '0', scrolled: false });
    await page.mouse.wheel(0, 600);
    await expect.poll(bar).toEqual({ opacity: '1', scrolled: true });
    expect((await page.getByTestId('landing-nav').boundingBox())!.y, 'sticky').toBe(0);
    await page.evaluate(() => window.scrollTo(0, 0));
    await expect(page).toHaveTitle(/^KOB/);
    expect(await noScroll(page), 'no horizontal scroll at the top').toBe(true);

    // the hero rises in, the bars grow in when scrolled to, the footer carries the network's disclaimer and the configured indexer's API
    await expect(page.getByTestId('landing-hero')).toHaveClass(/lp-go/);
    await page.getByTestId('landing-bigbook').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('landing-bigbook')).toHaveClass(/lp-seen/);
    await expect.poll(() => page.locator('.lp-bb').first().evaluate((e) => e.getBoundingClientRect().width)).toBeGreaterThan(width * 0.5);
    await expect(page.getByTestId('landing-not-business')).toHaveText('KOB is not a business. No fee goes to the project and no company stands in the middle. It is software for Kaspa, and it keeps working without us.');
    await page.getByTestId('footer').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('landing-disclaimer')).toContainText('Open review.');
    await expect(page.getByTestId('landing-disclaimer')).toContainText('never holds funds or orders, does not trade and does not operate executors');
    await expect(page.getByTestId('landing-disclaimer').getByRole('link', { name: 'Security policy' })).toHaveAttribute('href', /SECURITY.md$/);
    // no charts on this page: no chart attribution, no app-only links
    await expect(page.getByTestId('footer-chart-notice')).toHaveCount(0);
    await expect(page.getByTestId('footer').getByRole('link', { name: 'Recover orders' })).toHaveCount(0);
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

test('the trust model: no off switch but Kaspa itself, right after the operator row', async ({ appPage: page }) => {
  await page.goto('/');
  await expect(page.getByTestId('landing-offswitch')).toContainText('Only Kaspa itself could stop KOB');
  await expect(page.locator('.lp-ledger .lp-lr .lp-k')).toHaveText(['Admin keys', 'Oracle', 'Custody', 'Operator', 'Off switch', 'Closed source', 'Trust us']);
});

test.describe('the trust us row', () => {
  test('open review, no review shows the code is free of bugs; a report, an issue, a message on X, and no bug bounty, in the row; no section of their own', async ({ appPage: page }) => {
    await page.goto('/');
    const row = page.locator('.lp-lr').filter({ has: page.getByTestId('landing-audit') });
    await row.scrollIntoViewIfNeeded();
    await expect(row).toContainText('Trust us');
    await expect(row).toContainText('None.');
    await expect(page.getByTestId('landing-audit')).toContainText('Verify it yourself');
    await expect(page.getByTestId('landing-audit')).toContainText('no review can show the code is free of bugs');
    await expect(page.getByTestId('landing-audit')).toContainText('Red-team work and private reports are welcome.');
    // one text on every network: the testnet build does not mention test tokens here either
    await expect(page.getByTestId('landing-audit')).not.toContainText(/testnet|test tokens/i);
    await expect(page.getByTestId('landing-audit-note')).toHaveText('Please test only with your own funds and orders. There is no bug bounty.');
    for (const id of ['landing-feedback-security', 'landing-feedback-issue', 'landing-feedback-message']) await expect(row.getByTestId(id)).toBeVisible();
    await expect(page.getByTestId('landing-feedback')).toHaveCount(0);
    await expect(page.getByTestId('landing')).not.toContainText('to break it');
    await expect(page.getByTestId('landing-feedback-security')).toHaveAttribute('href', 'https://github.com/RossKU/kob/security/advisories/new');
    await expect(page.getByTestId('landing-feedback-issue')).toHaveAttribute('href', 'https://github.com/RossKU/kob/issues');
    await expect(page.getByTestId('landing-feedback-message')).toHaveAttribute('href', 'https://x.com/kaspaorderbook');
    await expect(page.getByTestId('landing-feedback-message')).toHaveText('Send a message on X →');
  });
});

test.describe('mainnet build', () => {
  test.use({ appConfig: { network: 'mainnet' } });
  test('the chip names mainnet, and nothing on the page says testnet', async ({ appPage: page }) => {
    await page.goto('/');
    await expect(page.getByTestId('landing')).toHaveAttribute('data-network', 'mainnet');
    await expect(page.getByTestId('landing-chip')).toHaveText('mainnet · open review');
    await expect(page.getByTestId('landing-disclaimer')).toContainText('Open review.');
    await expect(page.getByTestId('landing-audit')).toContainText('no review can show the code is free of bugs');
    await expect(page.getByTestId('landing')).not.toContainText(/testnet|test tokens/i);
    await expect(page.getByTestId('landing-nav')).not.toContainText(/testnet|tn10/i);
    await expect(page.getByTestId('footer')).not.toContainText(/testnet|test tokens/i);
    // the app's footer says the same
    await page.goto('/#/settings');
    await expect(page.getByTestId('footer-preaudit')).toContainText('Open review.');
    await expect(page.getByTestId('footer')).not.toContainText(/testnet|test tokens/i);
  });
});

for (const width of [360, 375]) {
  test(`${width} px: the header row (brand, chip, Open the app, theme toggle) fits on one line`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height: 800 });
    await page.goto('/');
    await expect(page.getByTestId('landing')).toBeVisible();
    const boxes = await Promise.all(['landing-brand', 'network-chip', 'landing-open-app', 'theme-toggle'].map(async (id) => (await page.getByTestId(id).boundingBox())!));
    for (const b of boxes) {
      expect(b.x + b.width).toBeLessThanOrEqual(width - 15);
      expect(Math.abs(b.y + b.height / 2 - 26)).toBeLessThan(2);
    }
    expect((await page.getByTestId('landing-nav').boundingBox())!.height).toBe(52);
    expect(await noScroll(page)).toBe(true);
  });
}

test('the landing page has the app theme toggle: one stored preference for both pages', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/');
  const theme = () => page.evaluate(() => document.documentElement.dataset.theme);
  const before = await theme();
  await page.getByTestId('theme-toggle').click();
  const after = await theme();
  expect(after).not.toBe(before);
  await page.getByTestId('landing-open-app').click();
  await expect(page.getByTestId('token-list-view')).toBeVisible();
  expect(await theme()).toBe(after);
  await page.reload();
  await expect(page.getByTestId('token-list-view')).toBeVisible();
  expect(await theme()).toBe(after);
  await page.getByTestId('theme-toggle').click();
  await page.goto('/');
  await expect(page.getByTestId('landing')).toBeVisible();
  expect(await theme()).toBe(before);
});

test('1440 px: one frame on both pages, the logo does not move when "Open the app" is clicked and the hero text starts under it', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/');
  await expect(page.getByTestId('landing-hero')).toHaveClass(/lp-go/);
  const box = async (id: string) => (await page.getByTestId(id).locator('svg').first().boundingBox())!;
  const lpLogo = await box('landing-brand');
  const lpHeader = (await page.locator('header').boundingBox())!;
  const kicker = (await page.locator('.lp-kicker').boundingBox())!;
  expect(Math.abs(kicker.x - lpLogo.x), 'the hero text starts under the logo').toBeLessThan(1);
  await page.getByTestId('footer').scrollIntoViewIfNeeded();
  const lpFooterLogo = await box('landing-footer-brand');

  await page.getByTestId('landing-open-app').click();
  await expect(page.getByTestId('token-list-view')).toBeVisible();
  const appLogo = await box('brand');
  const appHeader = (await page.locator('header').boundingBox())!;
  expect(appLogo.x, 'the logo keeps its left edge').toBeCloseTo(lpLogo.x, 1);
  expect(appLogo.y).toBeCloseTo(lpLogo.y, 1);
  expect(appLogo.width).toBeCloseTo(lpLogo.width, 1);
  expect(appHeader.height).toBe(lpHeader.height);
  expect(appHeader.height).toBe(56);
  // the content column is the frame's: the app's content starts under the logo too
  const mainLeft = await page.getByTestId('main').evaluate((e) => e.getBoundingClientRect().left + parseFloat(getComputedStyle(e).paddingLeft));
  expect(mainLeft).toBeCloseTo(appLogo.x, 1);
  await page.getByTestId('footer').scrollIntoViewIfNeeded();
  expect((await box('footer-brand')).x).toBeCloseTo(lpFooterLogo.x, 1);
  expect(lpFooterLogo.x).toBeCloseTo(lpLogo.x, 1);
});

test('a portrait screen (a phone showing the desktop site, a tablet) gets no full-screen hero: the headline sits near the top', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 980, height: 2200 });
  await page.goto('/#/');
  await expect(page.locator('.lp-hero h1')).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => document.querySelector('.lp-hero h1')?.getBoundingClientRect().y ?? Infinity), { message: 'no screen of empty space above the headline' })
    .toBeLessThan(500);
});

for (const width of [1440, 900]) {
  test(`${width} px: the bars' caption body is exactly two lines, each sentence half on one line`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/#/');
    const body = page.getByTestId('landing-bleed-body');
    await body.scrollIntoViewIfNeeded();
    await expect(body).toBeVisible();
    const lines = await body.evaluate((p) => {
      const lh = parseFloat(getComputedStyle(p).lineHeight);
      const spans = [...p.querySelectorAll('.lp-line')].map((s) => Math.round(s.getBoundingClientRect().height / lh));
      return { all: Math.round(p.getBoundingClientRect().height / lh), spans };
    });
    expect(lines).toEqual({ all: 2, spans: [1, 1] });
  });
}

test('desktop: the steps of "How it works" centre the number, the title and the text on one line', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/#/');
  const step = page.locator('.lp-st').first();
  await step.scrollIntoViewIfNeeded();
  const centres = await step.evaluate((el) => ['.lp-n', 'h3', 'p'].map((s) => { const b = el.querySelector(s)!.getBoundingClientRect(); return b.y + b.height / 2; }));
  expect(Math.max(...centres) - Math.min(...centres)).toBeLessThan(2);
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

// the CI runner's fonts (DejaVu Sans) are wider than a desktop's: the phone row keeps room to spare on a font as wide (Verdana), and nothing scrolls sideways
for (const width of [360, 390]) {
  test(`${width} px on a wide font: the header row has room to spare and the page does not scroll sideways`, async ({ appPage: page }) => {
    await page.addInitScript(() => {
      document.addEventListener('DOMContentLoaded', () => {
        const s = document.createElement('style');
        s.textContent = 'html, body, button, input, select, a, span, h1, h2, h3, p { font-family: Verdana, "DejaVu Sans", sans-serif !important; }';
        document.head.append(s);
      });
    });
    await page.setViewportSize({ width, height: 800 });
    await page.goto('/');
    await expect(page.getByTestId('landing-hero')).toHaveClass(/lp-go/);
    const box = async (id: string) => (await page.getByTestId(id).boundingBox())!;
    const [brand, chip, toggle] = [await box('landing-brand'), await box('network-chip'), await box('theme-toggle')];
    // the row's own width (the brand, a gap, the tools) at most 330 px at 360: about 30 px to spare for a still wider font
    const natural = brand.x + brand.width + 6 + (toggle.x + toggle.width - chip.x);
    expect(natural).toBeLessThanOrEqual(width - 30);
    expect(toggle.x + toggle.width).toBeLessThanOrEqual(width - 15);
    expect(await noScroll(page)).toBe(true);
    await page.getByTestId('landing-bigbook').scrollIntoViewIfNeeded();
    expect(await noScroll(page)).toBe(true);
  });
}
