// The market bar (review H-02 / H-10, founder 2026-11-11): ONE row of 56 to 64 px instead of a title block and a stats card: [picture + fingerprint] TICKER / QUOTE ▾ (flip) (star) | last price (change) | 24h high |
// 24h low | 24h volume | 24h volume KAS | trades | [Live]. The figures scroll sideways inside the bar where they do not fit (the page never does); a phone's first row is the pair, the star and Live,
// the figures a scrollable row under it. The covenant id with its copy button, the name and the program chip are in the token's details card, once; no "All markets" line; the tab title has no covenant id.
// The ticket's place button is on the first screen at 1280x800 and 1920x1080 (H-01). Mock stack: the token page of EXKCC, and the pair page.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);

async function listPair(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional pair quote token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

const rect = (page: Page, id: string) => page.getByTestId(id).first().evaluate((el) => { const r = el.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height, right: r.right, bottom: r.bottom }; });
const sideScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);

for (const [width, height] of [[1280, 800], [1920, 1080]] as const) {
  test(`${width}x${height}: one bar of 56 to 64 px with the pair, the figures and Live in one row, and the ticket's place button on the first screen`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height });
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    const id = await openMarket(page, mock);
    await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
    const bar = await rect(page, 'market-bar');
    expect(bar.height, 'the bar is one row').toBeGreaterThanOrEqual(56);
    expect(bar.height).toBeLessThanOrEqual(64);
    // left to right: the picture and the fingerprint, the pair, the flip, the star, the figures, Live
    const order = ['token-icon-' + id, 'fingerprint-' + id, 'market-pair', 'market-flip', 'token-page-favorite', 'stat-last', 'stat-change', 'stat-high', 'stat-low', 'stat-volume', 'stat-quote-volume', 'stat-trades'];
    let right = -1;
    for (const t of order) {
      const r = await rect(page, t);
      expect(r.x, `${t} is right of the one before`).toBeGreaterThanOrEqual(right - 1);
      expect(r.y + r.height / 2, `${t} is on the bar's row`).toBeGreaterThan(bar.y);
      expect(r.y + r.height / 2).toBeLessThan(bar.bottom);
      right = r.right;
    }
    // Live is at the bar's right end, after the figures' box (the figures scroll inside it where a wide font leaves them no room)
    const figures = await page.locator('.mkt-bar-stats').evaluate((el) => { const r = el.getBoundingClientRect(); return { right: r.right }; });
    const live = await rect(page, 'live-indicator');
    expect(live.x, 'Live is right of the figures').toBeGreaterThanOrEqual(figures.right - 1);
    expect(live.right).toBeLessThanOrEqual(bar.right);
    // the last price carries its unit; the old breadcrumb and the title block are gone; the page does not scroll sideways
    await expect(page.getByTestId('stat-last')).toContainText('KAS per EXKCC');
    await expect(page.getByTestId('market-all')).toHaveCount(0);
    await expect(page.getByTestId('token-short-id')).toHaveCount(0);
    expect(await sideScroll(page)).toBe(0);
    // H-01: the place button is on the first screen on load
    const place = await rect(page, 'order-place');
    expect(place.bottom, `order-place is inside the ${height} px window`).toBeLessThanOrEqual(height);
    expect(place.y).toBeGreaterThanOrEqual(0);
    expect(await page.getByTestId('order-place').isVisible()).toBe(true);
  });
}

test('the tab title has no covenant id; the details card has the name, the whole id with its copy button and the program chip, each once', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const id = await openMarket(page, mock);
  await expect(page).toHaveTitle('Testnet · EXKCC/KAS · KOB');
  expect(await page.title()).not.toContain(id);
  const details = page.getByTestId('token-details');
  await details.scrollIntoViewIfNeeded();
  await expect(details.getByTestId('token-name')).toHaveCount(1);
  await expect(details.getByTestId('token-name')).toContainText('Example KCC-20');
  await expect(details.getByTestId('token-covenant-id')).toContainText(id);
  await expect(details.getByTestId('token-covenant-id')).toHaveCount(1);
  await expect(details.getByTestId('badge-family')).toHaveCount(1);
  await expect(page.getByTestId('token-program')).toHaveCount(1);
  // and not in the bar
  await expect(page.getByTestId('market-bar').getByTestId('token-name')).toHaveCount(0);
  await expect(page.getByTestId('market-bar').getByTestId('badge-family')).toHaveCount(0);
  await expect(page.getByTestId('market-bar')).not.toContainText(id.slice(0, 12));
});

test('the figures scroll inside the bar on a narrow window: no scrollbar, the page does not scroll sideways', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 850, height: 1000 });
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await openMarket(page, mock);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
  const bar = await rect(page, 'market-bar');
  expect(bar.height).toBeLessThanOrEqual(64);
  const scroller = page.locator('.mkt-bar-stats');
  expect(await scroller.evaluate((el) => el.scrollWidth > el.clientWidth), 'the figures overflow their box here').toBe(true);
  expect(await scroller.evaluate((el) => getComputedStyle(el).overflowX)).toBe('auto');
  expect(await scroller.evaluate((el) => (el as HTMLElement).offsetHeight - el.clientHeight), 'no scrollbar is drawn').toBe(0);
  expect(await sideScroll(page)).toBe(0);
  // the last cell can be scrolled to
  await scroller.evaluate((el) => (el.scrollLeft = el.scrollWidth));
  await expect(page.getByTestId('stat-trades')).toBeInViewport();
});

for (const width of [360, 390]) {
  test(`${width} px: the first row is the picture, the pair, the star and Live, the figures one scrollable row under it`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 844 });
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    const id = await openMarket(page, mock, { connect: false, route: undefined });
    await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
    const [icon, pair, flip, star, live, stats, bar] = await Promise.all(['token-icon-' + id, 'market-pair', 'market-flip', 'token-page-favorite', 'live-indicator', 'market-stats', 'market-bar'].map((t) => rect(page, t)));
    for (const r of [icon, pair, flip, star, live]) expect(Math.abs(r.y + r.height / 2 - (icon.y + icon.height / 2)), 'one row').toBeLessThan(20);
    expect(stats.y, 'the figures are under the pair').toBeGreaterThan(pair.bottom - 1);
    expect(live.right).toBeLessThanOrEqual(bar.right + 1);
    expect(bar.height).toBeLessThan(135);
    expect(await sideScroll(page)).toBe(0);
  });

  test(`${width} px: the pair page has the same bar`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 844 });
    await mock.seed({ pair: true });
    await mock.seedHistory({});
    await mock.seedHistory({ token: QUOTE, mid: 50_000_000, tick: 100_000, seed: 2 });
    await listPair(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('pair-book').first()).toBeVisible();
    const [bar, pair, flip, star, stats] = await Promise.all(['market-bar', 'market-pair', 'market-flip', 'pair-favorite', 'market-stats'].map((t) => rect(page, t)));
    expect(flip.right, 'the flip stays in the bar').toBeLessThanOrEqual(bar.right);
    expect(star.right).toBeLessThanOrEqual(bar.right);
    expect(pair.bottom).toBeLessThan(stats.y + 1);
    expect(await sideScroll(page)).toBe(0);
  });
}

test('1280 px: the pair page has the token page\'s bar, the pictures of both tokens and the flip', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await mock.seed({ pair: true });
  await mock.seedHistory({});
  await mock.seedHistory({ token: QUOTE, mid: 50_000_000, tick: 100_000, seed: 2 });
  await listPair(page);
  const base = (await mock.token()).covenant_id;
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('pair-book').first()).toBeVisible();
  const bar = await rect(page, 'market-bar');
  expect(bar.height).toBeGreaterThanOrEqual(56);
  expect(bar.height).toBeLessThanOrEqual(64);
  for (const id of [base, QUOTE]) {
    await expect(page.getByTestId('pair-header').getByTestId(`token-icon-${id}`)).toBeVisible();
    await expect(page.getByTestId('pair-header').getByTestId(`fingerprint-${id}`)).toBeVisible();
  }
  await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
  await expect(page).toHaveTitle('Testnet · EXKCC/EXUSD · KOB');
  // the pair's book: no "via KAS" chip on a row, the sources said once under the book
  await expect(page.getByTestId('pair-via-kas')).toHaveCount(0);
  await expect(page.getByTestId('pair-legend')).toBeVisible();
  await expect(page.locator('[data-testid="pair-level"][data-source="route"]').first()).toBeVisible();
  await expect(page.getByTestId('pair-last')).toBeVisible();
  // each token's name, id and program are in its details card
  await expect(page.getByTestId('pair-token-details')).toHaveCount(2);
  for (const d of await page.getByTestId('pair-token-details').all()) {
    await expect(d.getByTestId('token-name')).toHaveCount(1);
    await expect(d.getByTestId('badge-family')).toHaveCount(1);
  }
  expect(await sideScroll(page)).toBe(0);
});
