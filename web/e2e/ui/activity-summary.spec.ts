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

/** `GET /v1/stats` of the mock indexer (the activity totals), not `/v1/stats/{token}` */
const STATS_URL = /\/v1\/stats(\?|$)/;

const num = (s: string | null) => Number((s ?? '').replace(/[^0-9.]/g, ''));

/** the tooltips of the executors' tiles (the help text is the label's title and follows the exact figure in the figure's) */
const INCOME_HELP = 'What KOB transactions left to the executors that sent them: the spread between crossing orders, auction margins and keeper tips, before network fees. Transactions in which an executor trades for itself are not counted.';
const MINERS_HELP = 'Network fees of the KOB transactions the executors sent.';

/** sompi (a decimal string, maybe negative) as the tiles' tooltip words it: whole KAS, grouped by thousands, trailing zeros trimmed */
function exactKas(sompi: string): string {
  const v = BigInt(sompi);
  const a = v < 0n ? -v : v;
  const frac = (a % 100_000_000n).toString().padStart(8, '0').replace(/0+$/, '');
  const whole = (a / 100_000_000n).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${v < 0n ? '-' : ''}${whole}${frac ? `.${frac}` : ''} KAS`;
}

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
    // the indexer's totals (GET /v1/stats): orders placed, traders, what the executors earned and paid the miners, the KAS locked in live orders; executors are recorded by no indexer ("—")
    expect(num(await page.getByTestId('summary-all-orders').locator('.sum-v').textContent())).toBeGreaterThan(0);
    expect(num(await page.getByTestId('summary-all-traders').locator('.sum-v').textContent())).toBeGreaterThan(0);
    await expect(page.getByTestId('summary-all-paid').locator('.sum-v')).toContainText('KAS');
    // what the executors earned and paid the miners, from the indexer's own fields (never the tip sum): the mock's fill transactions leave 0.03 KAS plus their tips each and cost 0.02 KAS each
    const totals = await mock.get<{ executor_income: string; miner_fees: string; executor_income_txs: number }>('/v1/stats');
    expect(totals.executor_income_txs).toBeGreaterThan(0);
    for (const [id, help, sompi] of [['paid', INCOME_HELP, totals.executor_income], ['miners', MINERS_HELP, totals.miner_fees]] as const) {
      const tile = page.getByTestId(`summary-all-${id}`);
      await expect(tile.locator('.sum-v')).toContainText('KAS');
      await expect(tile.locator('.sum-v')).toHaveAttribute('title', `${exactKas(sompi)}\n${help}`);
      await expect(tile.locator('.sum-k')).toHaveAttribute('title', help);
    }
    await expect(page.getByTestId('summary-all-paid').locator('.sum-k')).toHaveText('Paid to executors');
    await expect(page.getByTestId('summary-all-miners').locator('.sum-k')).toHaveText('Paid to miners');
    await expect(page.getByTestId('summary-window-miners').locator('.sum-v')).toContainText('KAS');
    await expect(page.getByTestId('summary-window-paid').locator('.sum-v')).toContainText('KAS');
    expect(num(await page.getByTestId('summary-now-locked').locator('.sum-v').textContent())).toBeGreaterThan(0);
    for (const id of ['summary-all-executors', 'summary-window-executors']) {
      await expect(page.getByTestId(id).locator('.sum-v')).toHaveText('—');
      await expect(page.getByTestId(id).locator('.sum-v')).toHaveAttribute('title', 'Not reported by this indexer yet');
    }
    // the window's own totals (the last 24 hours): the history seeded two hours back, 30 trades
    await expect(page.getByTestId('summary-window-orders').locator('.sum-v')).not.toHaveText('—');
    expect(num(await page.getByTestId('summary-window-orders').locator('.sum-v').textContent())).toBeGreaterThan(0);
    expect(num(await page.getByTestId('summary-window-traders').locator('.sum-v').textContent())).toBeGreaterThan(0);
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

  test('transactions the indexer could not price are said under the tile; a negative income keeps its minus sign', async ({ appPage: page, mock }) => {
    await seed(page, mock);
    await mock.activityStats({ incomeUnknownTxs: 3, minerFeeUnknownTxs: 1, negativeIncome: true });
    await page.goto('/#/activity');
    await expect(page.getByTestId('activity-summary')).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
    for (const scope of ['all', 'window']) {
      await expect(page.getByTestId(`summary-${scope}-paid`).locator('.sum-k')).toHaveAttribute('title', `${INCOME_HELP} 3 transactions not counted.`);
      await expect(page.getByTestId(`summary-${scope}-paid`).locator('.sum-v')).toHaveAttribute('title', /^-[\d,.]+ KAS\n.* 3 transactions not counted\.$/);
      await expect(page.getByTestId(`summary-${scope}-paid`).locator('.sum-v')).toHaveText(/^-[\d,.]+KAS$/);
      await expect(page.getByTestId(`summary-${scope}-miners`).locator('.sum-k')).toHaveAttribute('title', `${MINERS_HELP} 1 transaction not counted.`);
      await expect(page.getByTestId(`summary-${scope}-miners`).locator('.sum-v')).toHaveText(/^[\d,.]+KAS$/);
    }
    // none left out: no suffix (the default of the mock, checked by the test above)
    await mock.activityStats({ incomeUnknownTxs: 0, minerFeeUnknownTxs: 0, negativeIncome: false });
    await page.reload();
    await expect(page.getByTestId('activity-summary')).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
    await expect(page.getByTestId('summary-all-paid').locator('.sum-k')).toHaveAttribute('title', INCOME_HELP);
    await expect(page.getByTestId('summary-all-miners').locator('.sum-k')).toHaveAttribute('title', MINERS_HELP);
  });

  test('an older indexer (only fees_paid, the tip sum) shows a dash for both tiles, not the tips', async ({ appPage: page, mock }) => {
    await seed(page, mock);
    await mock.activityStats({ legacy: true });
    const old = await mock.get<Record<string, unknown>>('/v1/stats');
    expect(old.fees_paid).toBeDefined();
    expect(old.executor_income).toBeUndefined();
    await page.goto('/#/activity');
    await expect(page.getByTestId('activity-summary')).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
    for (const id of ['summary-all-paid', 'summary-all-miners', 'summary-window-paid', 'summary-window-miners']) {
      await expect(page.getByTestId(id).locator('.sum-v')).toHaveText('—');
      await expect(page.getByTestId(id).locator('.sum-v')).toHaveAttribute('title', 'Not reported by this indexer yet');
    }
    // the rest of the totals are still there
    expect(num(await page.getByTestId('summary-all-orders').locator('.sum-v').textContent())).toBeGreaterThan(0);
  });

  test('an indexer without GET /v1/stats keeps the dash for every total and the rest of the summary works', async ({ appPage: page, mock }) => {
    await seed(page, mock);
    await page.route(STATS_URL, (route) => route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: { code: 'not_found', message: 'no such route' } }) }));
    await page.goto('/#/activity');
    await expect(page.getByTestId('activity-summary')).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
    for (const id of ['summary-all-orders', 'summary-all-traders', 'summary-all-executors', 'summary-all-paid', 'summary-all-miners', 'summary-now-locked', 'summary-window-orders', 'summary-window-traders', 'summary-window-paid', 'summary-window-miners', 'summary-window-executors']) {
      await expect(page.getByTestId(id).locator('.sum-v')).toHaveText('—');
    }
    expect(num(await page.getByTestId('summary-all-trades').locator('.sum-v').textContent())).toBeGreaterThan(0);
    expect(num(await page.getByTestId('summary-now-orders').locator('.sum-v').textContent())).toBeGreaterThan(0);
  });

  test('the totals of the window follow the window chosen', async ({ appPage: page, mock }) => {
    await seed(page, mock);
    const since: string[] = [];
    await page.route(STATS_URL, (route) => {
      since.push(new URL(route.request().url()).searchParams.get('since') ?? '');
      return route.continue();
    });
    await page.goto('/#/activity');
    await expect(page.getByTestId('activity-summary')).not.toHaveAttribute('aria-busy', 'true', { timeout: 20_000 });
    await expect(page.getByTestId('summary-window-orders').locator('.sum-v')).not.toHaveText('');
    await page.getByTestId('summary-window-1M').click();
    await expect.poll(() => since.filter((v) => v !== '').length, { timeout: 10_000 }).toBeGreaterThanOrEqual(2);
    const spans = since.filter((v) => v !== '').map((v) => Date.now() - Number(v));
    expect(Math.min(...spans)).toBeGreaterThan(23 * 3_600_000);
    expect(Math.max(...spans)).toBeGreaterThan(29 * 86_400_000);
    expect(since.includes(''), 'the all-time totals are asked without since').toBe(true);
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
