// Load, then show: a page's frame (cards, headings, column titles) is there at once, its contents come together once they settle, at most LIST_HOLD_MS (1.5 s) after the page
// opened; what a trader acts on waits for none of it. The market list has its own spec (market-list-rows.spec.ts). Slow answers come from `mock.latency` and `page.route`.
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { connectWallet, listExkcc, showActivity } from '../helpers/env';
import { seedOrders } from '../helpers/orders';

test.use({ autoOpen: false, viewport: { width: 1440, height: 1000 } });

/** Every number of `selector` matches the page has shown, from its first paint on (read in the page: a spec's polls would miss a row that comes alone). */
const watchCount = (page: Page, selector: string) =>
  page.addInitScript((sel) => {
    const seen = new Set<number>();
    (window as unknown as { __seen: Set<number> }).__seen = seen;
    setInterval(() => seen.add(document.querySelectorAll(sel).length), 10);
  }, selector);
const counts = (page: Page) => page.evaluate(() => [...(window as unknown as { __seen: Set<number> }).__seen].sort((a, b) => a - b));

async function seeded(page: Page, mock: MockClient): Promise<string> {
  await mock.seedHistory({ hours: 2, trades: 30, seed: 3 });
  await listExkcc(page);
  return (await mock.token()).covenant_id as string;
}

test('the Activity page: its heading and column titles at once, its rows all together, risen in, and a later row at once', async ({ appPage: page, mock }) => {
  await seeded(page, mock);
  await mock.latency(600, { scope: 'indexer' });
  await watchCount(page, '[data-testid="activity-row"]');
  await page.goto('/#/activity');
  const log = page.getByTestId('activity');
  await expect(log.locator('.act-head')).toBeVisible();
  await expect(log.getByTestId('activity-loading')).toBeVisible();
  await expect(log.getByTestId('activity-row')).toHaveCount(0);
  await mock.latency(0);
  await expect(log.getByTestId('activity-row').first()).toBeVisible();
  await expect(log.getByTestId('activity-loading')).toHaveCount(0);
  const rows = await log.getByTestId('activity-row').count();
  expect(await log.getByTestId('activity-row').first().evaluate((e) => getComputedStyle(e).animationName)).toBe('ml-rise');
  expect(await counts(page), 'nothing, then every row of the page').toEqual([0, rows]);
  // the rows stood where they belong all along: the rise is `translate`, their places a `transform`
  const second = log.getByTestId('activity-row').nth(1);
  await expect.poll(() => second.evaluate((e) => getComputedStyle(e).translate)).toBe('none');
  expect((await second.boundingBox())!.y).toBeGreaterThan((await log.getByTestId('activity-row').first().boundingBox())!.y);
  // the rise is over: a row that comes now is live
  await expect.poll(() => log.getByTestId('activity-row').first().evaluate((e) => e.classList.contains('ml-in'))).toBe(false);
});

test('the Activity page: an indexer that stays slow shows the frame until its answer, then the rows', async ({ appPage: page, mock }) => {
  await seeded(page, mock);
  await mock.latency(2500, { scope: 'indexer' });
  await page.goto('/#/activity');
  const log = page.getByTestId('activity');
  await expect(log.getByTestId('activity-loading')).toBeVisible();
  await page.waitForTimeout(1800);
  await expect(log.getByTestId('activity-loading')).toBeVisible();
  await expect(log.getByTestId('activity-empty')).toHaveCount(0);
  await mock.latency(0);
  await expect(log.getByTestId('activity-row').first()).toBeVisible({ timeout: 15_000 });
});

test('a token page: the ticket works at once while the book waits for the stats; book and stats come together', async ({ appPage: page, mock }) => {
  const token = await seeded(page, mock);
  let release: () => void = () => {};
  const gate = new Promise<void>((r) => (release = r));
  await page.route('**/v1/stats/**', async (route) => {
    await gate;
    await route.continue();
  });
  await page.goto(`/#/market/${token}`);
  // the frame of the book: the column titles over the rows' places
  const frame = page.getByTestId('book-loading');
  await expect(frame).toBeVisible();
  await expect(frame.locator('.ob-head')).toContainText('Price');
  await expect(page.getByTestId('book-asks')).toHaveCount(0);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'loading');
  // the ticket does not wait
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await page.getByTestId('order-amount').fill('1');
  await expect(page.getByTestId('order-amount')).toHaveValue('1');
  release();
  await expect(page.getByTestId('book-ask-row').first()).toBeVisible();
  await expect(frame).toHaveCount(0);
  await expect(page.getByTestId('stat-last')).toContainText(/\d/);
});

test('a token page: stats that never come hold the book for 1.5 s at most', async ({ appPage: page, mock }) => {
  const token = await seeded(page, mock);
  await page.route('**/v1/stats/**', () => undefined);
  const t0 = Date.now();
  await page.goto(`/#/market/${token}`);
  await expect(page.getByTestId('book-ask-row').first()).toBeVisible({ timeout: 5_000 });
  expect(Date.now() - t0).toBeLessThan(4_000);
  await expect(page.getByTestId('book-loading')).toHaveCount(0);
});

test('a token page: a level after the first paint comes at once, the book is not held again', async ({ appPage: page, mock }) => {
  const token = await seeded(page, mock);
  await page.goto(`/#/market/${token}`);
  await expect(page.getByTestId('book-ask-row').first()).toBeVisible();
  await page.route('**/v1/stats/**', () => undefined);
  await page.getByTestId('book-refresh').click();
  await expect(page.getByTestId('book-loading')).toHaveCount(0);
  await expect(page.getByTestId('book-ask-row').first()).toBeVisible();
});

test('a token page with a slow token list: the frame of the market, not a spinner', async ({ appPage: page, mock }) => {
  const id = 'c3'.repeat(32);
  await mock.seed({
    tokens: [{ ticker: '', covenant_id: id, program: 'KCC20PublicMint', template_id: 'KCC20PublicMint', extension_commitment: 'ee'.repeat(32), powers: ['public-mint'], decimals: null, order_scale: 100_000_000 }],
    orders: [{ token: id, side: 'ask', maker: 'alice', amount: '4500000000000', price: 1_000_000 }],
  });
  await mock.latency(800, { scope: 'indexer' });
  await page.goto(`/#/market/${id}`);
  const frame = page.getByTestId('token-loading');
  await expect(frame).toBeVisible();
  await expect(frame.getByRole('heading', { name: 'Order book' })).toBeVisible();
  await expect(page.locator('.loading-row')).toHaveCount(0);
  await mock.latency(0);
  await expect(page.getByTestId('token-page')).toBeVisible();
});

test('My orders: the cards and tabs at once; the rows together, as soon as the indexer answered, whatever the node takes', async ({ appPage: page, mock }) => {
  const ids = await seedOrders(mock, [1, 2, 3].map((n) => ({ side: 'ask' as const, price: 2_700_000 + n * 10_000, tokens: 1 })));
  await listExkcc(page);
  await watchCount(page, '[data-testid="orders-list"] > [data-testid^="order-row-"]');
  await page.goto('/#/market');
  await connectWallet(page);
  await mock.latency(600, { scope: 'indexer' });
  await page.goto('/#/orders/open');
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await expect(page.getByTestId('orders-section')).toBeVisible();
  await expect(page.getByTestId('orders-tab-active')).toBeVisible();
  await expect(page.getByTestId('orders-loading')).toBeVisible();
  await expect(page.getByTestId('orders-tab-balances')).toBeVisible();
  await mock.latency(0);
  for (const id of ids) await expect(page.getByTestId(`order-row-${id}`)).toBeVisible();
  await expect(page.getByTestId('orders-loading')).toHaveCount(0);
  const rows = await page.getByTestId('orders-list').locator(':scope > [data-testid^="order-row-"]').count();
  expect(await counts(page), 'nothing, then every row').toEqual([0, rows]);
});

test('a log below the fold reads nothing, and its time starts when it is shown', async ({ appPage: page, mock }) => {
  const token = await seeded(page, mock);
  await page.setViewportSize({ width: 1440, height: 500 });
  await page.goto(`/#/market/${token}`);
  await expect(page.getByTestId('book-ask-row').first()).toBeVisible();
  await page.waitForTimeout(1800);
  await showActivity(page);
  await expect(page.getByTestId('activity').getByTestId('activity-row').first()).toBeVisible();
  expect(await page.getByTestId('activity').getByTestId('activity-row').first().evaluate((e) => getComputedStyle(e).animationName)).toBe('ml-rise');
});
