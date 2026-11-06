// How the market list arrives: placeholders until the first figures are in, then the rows in their sorted places, a page at a time (the viewer picks how many);
// the next page when the end of the list comes into view; a figure that arrives moves no row. Mock stack with a mocked kascov (many tokens that only it names).
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { mockKascov, programHash, type MockKascovToken } from '../helpers/kascov';

test.use({ autoOpen: false });

const ID = (n: number) => n.toString(16).padStart(4, '0').repeat(16);
const MINT = programHash('KCC20PublicMint');
const tokens: MockKascovToken[] = Array.from({ length: 130 }, (_, i) => ({ covenant_id: ID(0x1000 + i), claimed_ticker: `T${String(i).padStart(3, '0')}`, claimed_name: `Token ${i}`, program: MINT }));
const rows = (page: Page) => page.locator('[data-testid^="token-row-"]:not([data-testid="token-row-skeleton"])');

test('a page of rows, the next one on scrolling, and the page size is kept', async ({ appPage: page }) => {
  await mockKascov(page, { tokens });
  await page.goto('/#/market');
  await expect(page.getByTestId('token-list-rows-20')).toHaveAttribute('aria-checked', 'true');
  await expect(rows(page)).toHaveCount(20);
  await expect(page.getByTestId('token-row-skeleton')).toHaveCount(0);
  await expect(page.getByTestId('token-list-showing')).toContainText('20 of');
  // only the rows on the page exist, so only their icons can load
  await page.getByTestId('token-list-more').scrollIntoViewIfNeeded();
  await expect.poll(async () => rows(page).count()).toBeGreaterThanOrEqual(40);

  await page.getByTestId('token-list-rows-50').click();
  await expect(rows(page)).toHaveCount(50);
  await page.reload();
  await expect(page.getByTestId('token-list-rows-50')).toHaveAttribute('aria-checked', 'true');
  await expect(rows(page)).toHaveCount(50);
});

test('a search looks through every token, not only the rows on the page', async ({ appPage: page }) => {
  await mockKascov(page, { tokens });
  await page.goto('/#/market');
  await expect(rows(page)).toHaveCount(20);
  await page.getByTestId('token-search').fill('T129');
  await expect(rows(page)).toHaveCount(1);
  await expect(page.getByTestId('token-list-more')).toHaveCount(0);
});

test('figures that come late move no row', async ({ appPage: page, mock }) => {
  await mockKascov(page, { tokens });
  await page.route('**/v1/stats/**', async (route) => {
    await new Promise((r) => setTimeout(r, 2_500));
    await route.fallback();
  });
  await mock.seedHistory({ hours: 2, trades: 30, seed: 3 });
  await page.goto('/#/market');
  // the hold ends before the figures: the rows show, with their skeletons
  await expect(rows(page).first()).toBeVisible({ timeout: 2_400 });
  const before = await rows(page).evaluateAll((trs) => trs.map((tr) => tr.getAttribute('data-testid')));
  await page.waitForTimeout(300);
  expect(await rows(page).evaluateAll((trs) => trs.map((tr) => tr.getAttribute('data-testid')))).toEqual(before);
});
