// The order book never disappears because a pull failed or is late: the last good book stays with a subtle "stale since hh:mm:ss" marker, and only the
// next good response replaces it. A crossed book is no special case: it is the live book, drawn with a negative spread and never marked stale.
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
    // The page's clock is moved past LATE_MS (8 s, src/ui/market/book-stale.ts) instead of waiting for it in real time: waiting raced the
    // indexer client's own 10 s timeout (src/data/indexer.ts), which turns the late pull into a failed one before the answer is released.
    // setSystemTime moves Date.now only and fires no timer, so the 10 s timeout of the pull in flight stays about 10 s of real time away.
    await page.clock.install();
    await open(page, mock);
    let release: () => void = () => {};
    const gate = new Promise<void>((r) => (release = r));
    let held!: () => void;
    const requested = new Promise<void>((r) => (held = r));
    await page.route('**/v1/books/**', async (route) => {
      held();
      await gate;
      await route.continue();
    });
    await page.getByTestId('book-refresh').click();
    await requested;
    // the pull is in flight (the refresh button shows it), so its start time is taken: now make it late
    await expect(page.getByTestId('book-refresh')).toHaveAttribute('aria-busy', 'true');
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await page.clock.setSystemTime((await page.evaluate(() => Date.now())) + 9_000);
    await expect(page.getByTestId('book-stale-since')).toHaveText(/^stale since /);
    await expect(asks(page)).toHaveCount(10);
    // the late answer itself replaces the book and clears the marker
    release();
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await expect(page.getByTestId('book-refresh')).not.toHaveAttribute('aria-busy', 'true');
    await expect(asks(page)).toHaveCount(10);
  });

  test('a crossed book (matchers a moment behind) is drawn as it is with a negative spread, is not stale, and the next uncrossed book replaces it', async ({ appPage: page, mock }) => {
    await open(page, mock);
    let crossed = true;
    await page.route('**/v1/books/**', async (route) => {
      const r = await route.fetch();
      const j = await r.json();
      if (crossed && Array.isArray(j.asks) && j.asks.length && j.bids.length) {
        // the best bid 1% above the best ask
        j.bids[0].price = String((BigInt(j.asks[0].price) * 101n) / 100n);
      }
      await route.fulfill({ response: r, json: j });
    });
    await page.getByTestId('book-refresh').click();
    await expect(page.getByTestId('book-spread')).toContainText(/Spread -[\d.,]+ \(-[\d.]+%\)/);
    await expect(asks(page)).toHaveCount(10);
    await expect(page.getByTestId('book-matching')).toHaveCount(0);
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
    // the next good response replaces it: a positive spread again
    crossed = false;
    await page.getByTestId('book-refresh').click();
    await expect(page.getByTestId('book-spread')).not.toContainText('Spread -');
    await expect(page.getByTestId('book-stale-since')).toHaveCount(0);
    await expect(asks(page)).toHaveCount(10);
  });
});
