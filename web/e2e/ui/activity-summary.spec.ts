// The summary above the Activity page's log (ui/activity/ActivitySummary.tsx), against the mock indexer: all time, the time window (1D / 7D / 1M / 1Y)
// with its volume chart, the books now; "—" for what the indexer does not report yet; no horizontal scroll on a phone. Only on the Activity page.
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { listExkcc } from '../helpers/env';

test.use({ autoOpen: false });

async function seed(page: Page, mock: MockClient): Promise<void> {
  await mock.seedHistory({ hours: 2, trades: 30, seed: 3 });
  await listExkcc(page);
}

const num = (s: string | null) => Number((s ?? '').replace(/[^0-9.]/g, ''));

test.describe('activity summary', () => {
  test.use({ viewport: { width: 1440, height: 1000 } });

  test('all time, the window and the books now; the window switches and a bar shows its numbers; "—" where nothing is reported', async ({ appPage: page, mock }) => {
    await seed(page, mock);
    await page.goto('/#/activity');
    const sum = page.getByTestId('activity-summary');
    await expect(sum).toBeVisible();
    await expect(sum).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
    // all time: the KAS markets' volume and trades, and the first day
    await expect(page.getByTestId('summary-all-volume')).toContainText('KAS');
    expect(num(await page.getByTestId('summary-all-trades').locator('.sum-v').textContent())).toBeGreaterThan(0);
    await expect(page.getByTestId('activity-summary-since')).toContainText('since the first trade');
    for (const id of ['summary-all-orders', 'summary-all-traders', 'summary-all-executors', 'summary-all-paid', 'summary-window-executors', 'summary-now-locked']) {
      await expect(page.getByTestId(id).locator('.sum-v')).toHaveText('—');
      await expect(page.getByTestId(id).locator('.sum-v')).toHaveAttribute('title', 'Not reported by this indexer yet');
    }
    // the window: 24 hourly bars, the trades of the last two hours in it
    await expect(page.getByTestId('summary-bars').locator('button')).toHaveCount(24);
    expect(num(await page.getByTestId('summary-window-trades').locator('.sum-v').textContent())).toBeGreaterThan(0);
    await expect(page.getByTestId('summary-window-active').locator('.sum-v')).toHaveText('1');
    // the readout follows the bar under the pointer (the last one by default)
    await expect(page.getByTestId('summary-readout')).toContainText('trades');
    await page.getByTestId('summary-bars').locator('button').first().hover();
    await expect(page.getByTestId('summary-bars').locator('button').first()).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('summary-readout')).toContainText('0 trades');
    // 7D: 28 bars of six hours; 1M: 30 days; 1Y: 52 weeks
    for (const [w, n] of [['7D', 28], ['1M', 30], ['1Y', 52]] as const) {
      await page.getByTestId(`summary-window-${w}`).click();
      await expect(page.getByTestId('summary-bars').locator('button')).toHaveCount(n);
    }
    // the books now: open orders split into bids, asks and pair orders
    await expect(page.getByTestId('summary-now-orders')).toContainText('bids');
    expect(num(await page.getByTestId('summary-now-orders').locator('.sum-v').textContent())).toBeGreaterThan(0);
    // the log stays below it
    await expect(page.getByTestId('activity-list').first()).toBeVisible();
    const sumBox = (await sum.boundingBox())!;
    const logBox = (await page.getByTestId('activity').first().boundingBox())!;
    expect(logBox.y).toBeGreaterThanOrEqual(sumBox.y + sumBox.height - 1);
  });

  test('not on the token page', async ({ appPage: page, mock }) => {
    await seed(page, mock);
    const id = (await mock.token()).covenant_id as string;
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('activity-list').first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId('activity-summary')).toHaveCount(0);
  });
});

test.describe('activity summary on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('two columns, no horizontal scroll', async ({ appPage: page, mock }) => {
    await seed(page, mock);
    await page.goto('/#/activity');
    await expect(page.getByTestId('activity-summary')).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'no horizontal scroll').toBe(true);
    const a = (await page.getByTestId('summary-all-volume').boundingBox())!;
    const b = (await page.getByTestId('summary-all-trades').boundingBox())!;
    expect(Math.abs(a.y - b.y)).toBeLessThan(2);
  });
});

test.describe('activity summary while it loads', () => {
  // the lines that come with the figures (since the first trade, the trades' change, the markets' split) keep their places while they load: the summary
  // does not grow, and the log below it does not jump, when they arrive
  for (const width of [1440, 390]) {
    test(`keeps its height when the figures arrive (${width} px)`, async ({ appPage: page, mock }) => {
      await page.setViewportSize({ width, height: 1000 });
      await seed(page, mock);
      const held: (() => void)[] = [];
      let hold = true;
      await page.route(/\/v1\/candles\//, async (route) => {
        if (hold) await new Promise<void>((go) => held.push(go));
        await route.fallback();
      });
      await page.goto('/#/activity');
      const sum = page.getByTestId('activity-summary');
      await expect(sum).toHaveAttribute('aria-busy', 'true');
      await expect.poll(() => held.length).toBeGreaterThan(0);
      const before = await sum.evaluate((e) => e.getBoundingClientRect().height);
      hold = false;
      for (const go of held.splice(0)) go();
      await expect(sum).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
      await expect(page.getByTestId('activity-summary-since')).toContainText('since the first trade');
      expect(await sum.evaluate((e) => e.getBoundingClientRect().height)).toBe(before);
    });
  }
});
