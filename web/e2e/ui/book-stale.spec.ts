// The order book never disappears because a pull failed, is late or came back crossed: the last good book stays with a subtle "stale since hh:mm:ss"
// marker, and only the next good (uncrossed) response replaces it.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';

async function open(page: Page, mock: import('../fixtures').MockClient): Promise<string> {
  const id = (await mock.token()).covenant_id as string;
  await page.goto(`/#/market/${id}`);
  await expect(page.getByTestId(TESTID.bookAsks).locator('[data-testid^="book-"]')).toHaveCount(10);
  return id;
}

const asks = (page: Page) => page.getByTestId(TESTID.bookAsks).locator('.book-row');

test.describe('order book keeps the last good book', () => {
  test.use({ autoOpen: false });

  test('a failed pull keeps the book, marks it stale since the last good response, and the next good one replaces it', async ({ appPage: page, mock }) => {
    await open(page, mock);
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await page.route('**/v1/books/**', (route) => route.abort());
    await page.getByTestId('book-refresh').click();
    // still there, 10 rows a side, a subtle marker with the time of the last good response, no big error banner in place of the book
    await expect(page.getByTestId('book-stale-since')).toHaveText(/^stale since \d{2}:\d{2}:\d{2}$/);
    await expect(asks(page)).toHaveCount(10);
    await expect(page.getByTestId('book-error')).toHaveCount(0);
    await expect(page.getByTestId('book-loading')).toHaveCount(0);
    // a later failure keeps the SAME since-time (the time of the last good book, not of the failure)
    const since = await page.getByTestId('book-stale-since').textContent();
    await page.waitForTimeout(1100);
    await page.getByTestId('book-refresh').click();
    await expect(page.getByTestId('book-stale-since')).toHaveText(since!);
    // the next good response replaces the book and clears the marker
    await page.unroute('**/v1/books/**');
    await page.getByTestId('book-refresh').click();
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await expect(asks(page)).toHaveCount(10);
  });

  test('a slow pull keeps the book and marks it stale while the answer is late', async ({ appPage: page, mock }) => {
    await open(page, mock);
    let release: () => void = () => {};
    const gate = new Promise<void>((r) => (release = r));
    await page.route('**/v1/books/**', async (route) => {
      await gate;
      await route.continue();
    });
    await page.getByTestId('book-refresh').click();
    await expect(asks(page)).toHaveCount(10);
    await expect(page.getByTestId('book-stale-since')).toHaveText(/^stale since /, { timeout: 15_000 });
    release();
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await expect(asks(page)).toHaveCount(10);
  });

  test('a book that stays crossed (matchers behind) never replaces the last good one: it stays, stale since, until an uncrossed book arrives', async ({ appPage: page, mock }) => {
    await page.clock.install();
    await open(page, mock);
    let crossed = true;
    await page.route('**/v1/books/**', async (route) => {
      const r = await route.fetch();
      const j = await r.json();
      if (crossed && Array.isArray(j.asks) && j.asks.length && j.bids.length) {
        // the best bid above the best ask
        j.bids[0].price = String(BigInt(j.asks[0].price) + 10n);
      }
      await route.fulfill({ response: r, json: j });
    });
    await page.getByTestId('book-refresh').click();
    await expect(page.getByTestId('book-matching')).toBeVisible();
    await expect(asks(page)).toHaveCount(10); // the last uncrossed snapshot
    // well past the old 10 s hold: it is still there
    await page.clock.runFor(60_000);
    await expect(asks(page)).toHaveCount(10);
    await expect(page.getByTestId('book-stale-since')).toHaveText(/^stale since \d{2}:\d{2}:\d{2}$/);
    // the next good response replaces it
    crossed = false;
    await page.getByTestId('book-refresh').click();
    await expect(page.getByTestId('book-matching')).toHaveCount(0);
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await expect(asks(page)).toHaveCount(10);
  });
});
