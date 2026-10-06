// Market data of the token page against the mock indexer's market endpoints (`/v1/trades|candles|stats|depth`): the 24 h stats strip, the
// candlestick chart with its interval switch, the depth chart, the trade tape, price grouping of the book, the theme toggle, and a phone
// layout without horizontal scroll. The mock seeds a deterministic 48 h price history (`mock.seedHistory`) on top of the default book.
// `KOB_SHOTS=1` also saves full-page screenshots (dark / light / phone) to test-results/shots/.
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';
import { listExkcc } from '../helpers/env';

const SHOTS = process.env.KOB_SHOTS === '1';

async function openWithHistory(page: import('@playwright/test').Page, mock: import('../fixtures').MockClient) {
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await listExkcc(page);
  const tok = await mock.token();
  await page.goto(`/#/market/${tok.covenant_id}`);
  return tok.covenant_id as string;
}

test.describe('market data', () => {
  test('stats, candles, depth and the trade tape render from the market-data endpoints', async ({ appPage: page, mock }) => {
    await openWithHistory(page, mock);
    // 24 h stats
    const stats = page.getByTestId('market-stats');
    await expect(stats).toHaveAttribute('data-state', 'ready');
    await expect(page.getByTestId('stat-last')).toContainText(/0\.02\d{4}/);
    await expect(page.getByTestId('stat-change')).toContainText(/\d+\.\d{2}%/);
    await expect(page.getByTestId('stat-high')).toContainText(/0\.0\d+/);
    await expect(page.getByTestId('stat-low')).toContainText(/0\.0\d+/);
    await expect(page.getByTestId('stat-volume')).not.toContainText('—');
    await expect(page.getByTestId('stat-quote-volume')).not.toContainText('—');

    // candles: the library draws into canvases; the gap-filled series has bars
    const chart = page.getByTestId('price-chart');
    await expect(chart).toHaveAttribute('data-state', 'ready');
    await expect(chart.locator('canvas').first()).toBeVisible();
    expect(Number(await chart.getAttribute('data-bars'))).toBeGreaterThan(50);
    await expect(page.getByTestId('chart-legend')).toContainText(/0\.02/);
    // the TradingView attribution stays on the chart and in the footer
    await expect(chart.locator('a[href*="tradingview.com"]')).toHaveCount(1);
    await expect(page.getByTestId('footer-chart-notice')).toContainText('TradingView');

    // interval switch (remembered)
    await page.getByTestId('chart-interval-1h').click();
    await expect(chart).toHaveAttribute('data-interval', '1h');
    await expect(chart).toHaveAttribute('data-state', 'ready');
    const hourly = Number(await chart.getAttribute('data-bars'));
    expect(hourly).toBeGreaterThanOrEqual(48);
    expect(hourly).toBeLessThanOrEqual(51);
    await page.getByTestId('chart-interval-1d').click();
    await expect(chart).toHaveAttribute('data-interval', '1d');
    await page.reload();
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-interval', '1d');
    await page.getByTestId('chart-interval-5m').click();

    // depth from /v1/depth
    const depth = page.getByTestId('depth-chart');
    await expect(depth).toHaveAttribute('data-source', 'depth');
    await expect(depth).toHaveAttribute('data-state', 'ready');
    await expect(depth.locator('path.bid-area')).toHaveCount(1);
    await expect(depth.locator('path.ask-area')).toHaveCount(1);

    // trade tape from /v1/trades: newest first, both aggressor sides coloured
    const tape = page.getByTestId(TESTID.tradesList);
    await expect(tape).toHaveAttribute('data-source', 'trades');
    await expect(tape.getByTestId('trade-row')).toHaveCount(50);
    await expect(tape.locator('[data-testid="trade-row"][data-side="buy"]').first()).toBeVisible();
    await expect(tape.locator('[data-testid="trade-row"][data-side="sell"]').first()).toBeVisible();
    await expect(tape.getByTestId('trade-row').first()).toContainText(/\d{2}:\d{2}:\d{2}/);
  });

  test('a new fill reaches the tape and the stats without a reload', async ({ appPage: page, mock }) => {
    await openWithHistory(page, mock);
    await expect(page.getByTestId('live-indicator')).toHaveText('Live');
    const trades = await page.getByTestId('stat-trades').innerText();
    const ask = (await mock.ordersOf('maker', 'active')).find((o: { side: number }) => o.side === 1);
    await mock.fill(ask!.covenant_id);
    await expect(page.getByTestId('stat-trades')).not.toHaveText(trades);
    await expect(page.getByTestId(TESTID.tradesList).getByTestId('trade-row').first()).toHaveAttribute('data-side', 'buy');
  });

  test('price grouping merges book levels and keeps asks above bids', async ({ appPage: page, mock }) => {
    await openWithHistory(page, mock);
    const asks = page.getByTestId(TESTID.bookAsks).getByTestId('book-ask-row');
    await expect(asks).toHaveCount(10);
    const amounts = async (rows: typeof asks) => (await rows.evaluateAll((els) => els.map((e) => Number((e as HTMLElement).dataset.amount)))).reduce((a, b) => a + b, 0);
    const bids = page.getByTestId(TESTID.bookBids).getByTestId('book-bid-row');
    const askAmount = await amounts(asks);
    const bidAmount = await amounts(bids);
    // protocol v3 has no registry tick: the step is the one the visible prices sit on (their gcd, 10,000 sompi per whole token in the mock book), and
    // 10 x it = 0.001 KAS per token: asks 0.0251 .. 0.026 round UP into one 0.026 row, bids 0.0249 .. 0.024 DOWN into 0.024
    const select = page.getByTestId('book-group');
    await select.selectOption({ index: 1 });
    await expect(asks).toHaveCount(1);
    await expect(bids).toHaveCount(1);
    await expect(asks.first()).toHaveAttribute('data-price', '2600000');
    await expect(bids.first()).toHaveAttribute('data-price', '2400000');
    expect(await amounts(asks)).toBe(askAmount);
    expect(await amounts(bids)).toBe(bidAmount);
    await select.selectOption({ index: 0 });
    await expect(asks).toHaveCount(10);
  });

  test('the theme toggle switches the palette and is remembered', async ({ appPage: page, mock }) => {
    await openWithHistory(page, mock);
    const html = page.locator('html');
    const initial = (await html.getAttribute('data-theme'))!;
    expect(['dark', 'light']).toContain(initial);
    const other = initial === 'dark' ? 'light' : 'dark';
    await page.getByTestId('theme-toggle').click();
    await expect(html).toHaveAttribute('data-theme', other);
    const bg = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
    await page.reload();
    await expect(html).toHaveAttribute('data-theme', other);
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe(bg);
    await page.getByTestId('theme-toggle').click();
    await expect(html).toHaveAttribute('data-theme', initial);
  });

  test('screenshots (dark and light, desktop)', async ({ appPage: page, mock }) => {
    test.skip(!SHOTS, 'KOB_SHOTS=1 saves screenshots');
    await page.setViewportSize({ width: 1600, height: 1000 });
    await openWithHistory(page, mock);
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
    await page.evaluate(() => localStorage.setItem('kob.theme', 'dark'));
    await page.reload();
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
    await page.waitForTimeout(800);
    await page.screenshot({ path: 'test-results/shots/market-dark.png', fullPage: true });
    await page.getByTestId('theme-toggle').click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: 'test-results/shots/market-light.png', fullPage: true });
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.getByTestId('theme-toggle').click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: 'test-results/shots/market-1280.png', fullPage: false });
  });
});

test.describe('market data on a phone', () => {
  test.use({ viewport: { width: 375, height: 812 } });
  test('stacks the panels without horizontal scroll', async ({ appPage: page, mock }) => {
    await openWithHistory(page, mock);
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
    await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
    await expect(page.getByTestId(TESTID.tradesList).getByTestId('trade-row').first()).toBeVisible();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBe(0);
    if (SHOTS) {
      await page.waitForTimeout(600);
      await page.screenshot({ path: 'test-results/shots/market-phone.png', fullPage: true });
    }
  });
});
