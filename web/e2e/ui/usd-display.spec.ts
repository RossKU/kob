// The optional USD display (Settings, "Display currency"): KAS stays the default; with USD the values the viewer reads (the market list's price / volume / market cap /
// depth, a market's last price and 24 h statistics, the Portfolio's summary and balances value) are shown as approximate dollars, with the KAS figure in the tooltip and a
// one-line note that says what the dollars are. The order book, the ticket, the order and fill tables and the Activity feed stay in KAS, and no price the user types is converted.
// The price is the indexer's (`GET /v1/price/kas-usd`: the mock serves $0.03 taken now unless a test steers it); a missing, unavailable or too old (over 10 minutes) price
// falls back to KAS with a notice, and the app never asks for it while KAS is the display currency.
import { expect, test, type MockClient } from '../fixtures';
import { connectWallet, fund, gotoNav, listExkcc, openMarket } from '../helpers/env';

test.use({ autoOpen: false });

const PRICE = 0.03;

/** The number in a shown amount (`$1,066.77`, `6,000`, `≈ 6,000`). */
const numberOf = (text: string | null): number => Number((text ?? '').replace(/[^0-9.]/g, ''));

/** USD is the viewer's choice before the app starts (what Settings stores). */
async function chooseUsd(page: import('@playwright/test').Page): Promise<void> {
  await page.addInitScript(() => localStorage.setItem('kob.currency', 'usd'));
}

/** The first token of the mock, listed for trading, and its list row waiting for its figures. */
async function openList(page: import('@playwright/test').Page, mock: MockClient): Promise<string> {
  await listExkcc(page);
  const id = (await mock.token()).covenant_id;
  await page.goto('/#/market');
  await expect(page.getByTestId(`token-row-${id}`)).toHaveAttribute('data-state', 'ready');
  return id;
}

test('KAS is the default: nothing is dollars, no note, and the app never asks for a price', async ({ appPage: page, mock }) => {
  const id = await openList(page, mock);
  await expect(page.getByTestId(`token-last-${id}`)).not.toContainText('$');
  await expect(page.getByTestId(`token-volume-${id}`)).not.toContainText('$');
  await expect(page.getByTestId('sort-last')).toContainText('Price (KAS)');
  await expect(page.getByTestId('fiat-note')).toHaveCount(0);
  await expect(page.getByTestId('fiat-notice')).toHaveCount(0);
  await page.waitForTimeout(1_500);
  expect(await mock.kasPriceReads()).toBe(0);
});

test('Settings: the choice applies at once, is kept in this browser (kob.currency) across a reload, and choosing KAS removes it; the price is asked only while USD is chosen', async ({ appPage: page, mock }) => {
  await page.goto('/#/settings');
  const usd = page.getByTestId('settings-currency-usd');
  await expect(page.getByTestId('settings-currency')).toBeVisible();
  await expect(page.getByTestId('settings-currency-kas')).toHaveAttribute('aria-checked', 'true');
  await expect(usd).toHaveAttribute('aria-checked', 'false');
  expect(await page.evaluate(() => localStorage.getItem('kob.currency'))).toBeNull();
  expect(await mock.kasPriceReads()).toBe(0);

  await usd.click();
  await expect(usd).toHaveAttribute('aria-checked', 'true');
  expect(await page.evaluate(() => localStorage.getItem('kob.currency'))).toBe('usd');
  await expect.poll(() => mock.kasPriceReads()).toBeGreaterThanOrEqual(1);
  // the note says what the dollars are, on any page, once the price is in
  await expect(page.getByTestId('settings-currency-choice')).toBeVisible();

  await page.reload();
  await expect(page.getByTestId('settings-currency-usd')).toHaveAttribute('aria-checked', 'true');

  await page.getByTestId('settings-currency-kas').click();
  await expect(page.getByTestId('settings-currency-kas')).toHaveAttribute('aria-checked', 'true');
  expect(await page.evaluate(() => localStorage.getItem('kob.currency'))).toBeNull();
  const reads = await mock.kasPriceReads();
  await page.waitForTimeout(1_000);
  expect(await mock.kasPriceReads()).toBe(reads);
  // clearing the app's data lists the choice with the settings
  await expect(page.getByTestId('settings-local')).toBeVisible();
});

test('the market list shows the price, 24 h volume, market cap and depth in dollars with the KAS figure in the tooltip; the change and the spread stay; the note says what the dollars are', async ({ appPage: page, mock }) => {
  await chooseUsd(page);
  const id = await openList(page, mock);
  const stats = await mock.get(`/v1/stats/${id}`);
  const row = page.getByTestId(`token-row-${id}`);
  const last = row.getByTestId(`token-last-${id}`);
  await expect(last).toHaveText(/^\$0\.\d+$/);
  expect(numberOf(await last.textContent())).toBeCloseTo((Number(stats.last) / 1e8) * PRICE, 5);
  // the KAS price is in the tooltip with the rate used
  await expect(last).toHaveAttribute('title', /KAS per EXKCC\) · USD estimate at \$0\.03 per KAS$/);
  const volume = row.getByTestId(`token-volume-${id}`);
  await expect(volume).toHaveText(/^\$[\d,]+\.?\d*$/);
  expect(numberOf(await volume.textContent())).toBeCloseTo((Number(stats.quote_volume_24h) / 1e8) * PRICE, 2);
  await expect(volume).toHaveAttribute('title', /KAS · \d+ trades in 24 h · USD estimate at \$0\.03 per KAS$/);
  const depth = row.getByTestId(`token-depth-${id}`);
  await expect(depth).toHaveText(/^\$[\d,]+\.\d+$/);
  expect(numberOf(await depth.textContent())).toBeCloseTo((Number(stats.depth_2pct_quote) / 1e8) * PRICE, 3);
  // a ratio is not a value: the change is still a percentage
  await expect(row.getByTestId(`token-change-${id}`)).toHaveText(/^[+-]?\d+\.\d\d%$/);
  await expect(page.getByTestId('sort-last')).toContainText('Price (USD)');
  await expect(page.getByTestId('sort-volume')).toContainText('24h volume (USD)');
  await expect(page.getByTestId('sort-depth')).toContainText('Depth ±2% (USD)');
  // the note: the source, the age, an estimate and, on testnet, that its KAS has no value
  const note = page.getByTestId('fiat-note');
  await expect(note).toHaveText(/^USD at the KAS price from CoinGecko, (under a minute|\d+ min) ago\. An estimate, not a quote\. Testnet KAS has no value: priced as mainnet KAS, for display only\.$/);
  // one line of text in a table cell: nothing wraps or overlaps
  for (const cell of [last, volume, depth]) {
    const box = await cell.locator('span').first().boundingBox();
    expect(box!.height).toBeLessThan(30);
  }
});

test('an average of exchanges is named, and its USDT is said to be taken as USD', async ({ appPage: page, mock }) => {
  await mock.kasPrice({ usd: 0.0301, source: 'vwap:bybit,kucoin,gate' });
  await chooseUsd(page);
  await openList(page, mock);
  await expect(page.getByTestId('fiat-note')).toContainText('USD (USDT) at the KAS price from Bybit, KuCoin, Gate,');
  await expect(page.getByTestId('fiat-note')).toHaveAttribute('data-source', 'vwap:bybit,kucoin,gate');
});

test('a market page shows its last price and 24 h statistics in dollars; the order book, the ticket and the Activity feed stay in KAS', async ({ appPage: page, mock }) => {
  await chooseUsd(page);
  await fund(mock);
  const id = await openMarket(page, mock);
  const stats = await mock.get(`/v1/stats/${id}`);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
  const last = page.getByTestId('stat-last');
  await expect(last).toContainText('$');
  await expect(last).toContainText('USD per EXKCC');
  expect(numberOf(await last.locator('.mkt-stat-value').evaluate((e) => e.firstChild?.textContent ?? ''))).toBeCloseTo((Number(stats.last) / 1e8) * PRICE, 5);
  await expect(page.getByTestId('stat-high')).toContainText('$');
  await expect(page.getByTestId('stat-low')).toContainText('$');
  await expect(page.getByTestId('stat-quote-volume')).toContainText('$');
  await expect(page.getByTestId('stat-quote-volume')).toContainText('24h volume (USD)');
  // the volume in the token and the change are not KAS values
  await expect(page.getByTestId('stat-volume')).not.toContainText('$');
  await expect(page.getByTestId('stat-change')).toContainText('%');
  // the tooltip keeps the KAS figure
  await expect(last.locator('.mkt-stat-value')).toHaveAttribute('title', /KAS per EXKCC · USD estimate at \$0\.03 per KAS$/);
  await expect(page.getByTestId('fiat-note')).toBeVisible();

  // the order book and the ticket: KAS, never dollars; the price field takes what the user types
  await expect(page.getByTestId('book-section')).toBeVisible();
  await expect(page.getByTestId('book-section')).not.toContainText('$');
  const ticket = page.getByTestId('order-ticket');
  await expect(ticket).not.toContainText('$');
  const price = page.getByTestId('order-price');
  if (await price.isVisible()) {
    await price.fill('0.0251');
    await expect(price).toHaveValue('0.0251');
    await expect(ticket).not.toContainText('$');
  }
  // the Activity feed
  await gotoNav(page, 'nav-activity');
  await expect(page.getByTestId('activity-list')).toBeVisible();
  await expect(page.getByTestId('activity-list')).not.toContainText('$');
});

test('the Portfolio summary and the Balances value column are dollars with the KAS figure in the tooltip; the Available and In orders columns of the balances stay KAS', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock, { route: '/portfolio' });
  const strip = page.getByTestId('portfolio-summary');
  await expect(strip).toHaveAttribute('data-currency', 'kas');
  await expect(page.getByTestId('portfolio-available')).toContainText('KAS');
  const kasFree = numberOf(await page.getByTestId('portfolio-available').textContent());
  expect(kasFree).toBeGreaterThan(0);
  const kasTotal = numberOf(await page.getByTestId('portfolio-total').textContent());

  // Settings, then back: the choice applies at once, the wallet stays connected
  await gotoNav(page, 'nav-settings');
  await page.getByTestId('settings-currency-usd').click();
  await expect.poll(() => mock.kasPriceReads()).toBeGreaterThanOrEqual(1);
  await gotoNav(page, 'nav-portfolio');
  await expect(strip).toHaveAttribute('data-currency', 'usd');
  const available = page.getByTestId('portfolio-available');
  await expect(available).toHaveText(/^\$[\d,]+\.\d\d$/);
  expect(numberOf(await available.textContent())).toBeCloseTo(kasFree * PRICE, 1);
  await expect(available).not.toContainText('KAS');
  await expect(available).toHaveAttribute('title', /^[\d,.]+ KAS · USD estimate at \$0\.03 per KAS$/);
  await expect(page.getByTestId('portfolio-total')).toHaveText(/^≈\s\$[\d,]+\.\d\d$/);
  expect(numberOf(await page.getByTestId('portfolio-total').textContent())).toBeCloseTo(kasTotal * PRICE, 1);
  await expect(page.getByTestId('portfolio-in-orders')).toHaveText(/^\$[\d,]+\.\d\d$/);
  // an open-orders count is a count
  await expect(page.getByTestId('portfolio-open-count')).not.toContainText('$');
  await expect(page.getByTestId('fiat-note')).toBeVisible();

  // the balances: the Value column is dollars, the held amounts are KAS and tokens
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  await expect(page.getByTestId('kas-total')).toHaveText(/^\$[\d,]+\.\d\d$/);
  await expect(page.getByTestId('kas-free')).not.toContainText('$');
  await expect(page.getByTestId('kas-locked')).not.toContainText('$');
  await expect(page.getByTestId('token-balances-table')).toContainText('Value (USD)');
  // a token with a last price has a value in dollars; its held amount stays in tokens
  const tokenValue = page.locator('[data-testid="balance-value"]').first();
  if ((await tokenValue.count()) > 0 && !(await tokenValue.innerText()).includes('—')) {
    await expect(tokenValue).toContainText('$');
    await expect(page.locator('[data-testid^="balance-row-"] [data-testid="balance-escrowed"]').first()).not.toContainText('$');
  }

  // back to KAS
  await gotoNav(page, 'nav-settings');
  await page.getByTestId('settings-currency-kas').click();
  await gotoNav(page, 'nav-portfolio');
  await expect(strip).toHaveAttribute('data-currency', 'kas');
  await expect(page.getByTestId('portfolio-available')).toContainText('KAS');
  await expect(page.getByTestId('token-balances-table')).toContainText('Value (KAS)');
});

test('a price older than 10 minutes is not used: KAS with a one-line notice; 9 minutes old still is', async ({ appPage: page, mock }) => {
  await mock.kasPrice({ ageS: 9 * 60 });
  await chooseUsd(page);
  const id = await openList(page, mock);
  await expect(page.getByTestId(`token-last-${id}`)).toHaveText(/^\$/);
  await expect(page.getByTestId('fiat-note')).toContainText('9 min ago');

  await mock.kasPrice({ ageS: 11 * 60 });
  await page.reload();
  await expect(page.getByTestId(`token-row-${id}`)).toHaveAttribute('data-state', 'ready');
  const notice = page.getByTestId('fiat-notice');
  await expect(notice).toHaveAttribute('data-reason', 'stale');
  await expect(notice).toHaveText('The KAS price is 11 min old, too old to show: showing KAS.');
  // KAS, never the stale dollars
  await expect(page.getByTestId(`token-last-${id}`)).not.toContainText('$');
  await expect(page.getByTestId(`token-volume-${id}`)).not.toContainText('$');
  await expect(page.getByTestId('sort-last')).toContainText('Price (KAS)');
  await expect(page.getByTestId('fiat-note')).toHaveCount(0);
  // the notice is one muted line
  const box = await notice.boundingBox();
  expect(box!.height).toBeLessThan(28);
});

test('an indexer without the route (404) or without a price yet (503) shows KAS with a notice and no error', async ({ appPage: page, mock }) => {
  await mock.kasPrice({ mode: 'off' });
  await chooseUsd(page);
  const id = await openList(page, mock);
  await expect(page.getByTestId('fiat-notice')).toHaveAttribute('data-reason', 'unavailable');
  await expect(page.getByTestId('fiat-notice')).toContainText('serves no KAS price');
  await expect(page.getByTestId(`token-last-${id}`)).not.toContainText('$');
  await expect(page.getByTestId('sort-last')).toContainText('Price (KAS)');
  await expect(page.getByTestId('token-list-error')).toHaveCount(0);

  await mock.kasPrice({ mode: 'down' });
  await page.reload();
  await expect(page.getByTestId(`token-row-${id}`)).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('fiat-notice')).toHaveAttribute('data-reason', 'missing');
  await expect(page.getByTestId('fiat-notice')).toContainText('could not be read');
  await expect(page.getByTestId(`token-last-${id}`)).not.toContainText('$');
  await expect(page.getByTestId('token-list-error')).toHaveCount(0);

  // the price comes back: dollars, without a reload of the choice
  await mock.kasPrice({ mode: 'ok' });
  await page.reload();
  await expect(page.getByTestId(`token-last-${id}`)).toHaveText(/^\$/);
  await expect(page.getByTestId('fiat-notice')).toHaveCount(0);
});

test('the price is read once at the start and again every minute while the page is shown, and a failed read waits twice as long', async ({ appPage: page, mock }) => {
  await page.clock.install();
  await chooseUsd(page);
  const id = await openList(page, mock);
  await expect.poll(() => mock.kasPriceReads()).toBe(1);
  await mock.kasPrice({ usd: 0.06 });
  await page.clock.fastForward(50_000);
  expect(await mock.kasPriceReads()).toBe(1);
  await page.clock.fastForward(11_000);
  await expect.poll(() => mock.kasPriceReads()).toBe(2);
  // the new price is in force
  const stats = await mock.get(`/v1/stats/${id}`);
  await expect(page.getByTestId(`token-last-${id}`)).toHaveAttribute('title', /USD estimate at \$0\.06 per KAS$/);
  expect(numberOf(await page.getByTestId(`token-last-${id}`).textContent())).toBeCloseTo((Number(stats.last) / 1e8) * 0.06, 5);

  // the indexer has no price (503): the last price is kept, and the next read is two minutes away, not one
  await mock.kasPrice({ mode: 'down' });
  await page.clock.fastForward(61_000);
  await expect.poll(() => mock.kasPriceReads()).toBe(3);
  await page.clock.fastForward(61_000);
  expect(await mock.kasPriceReads()).toBe(3);
  await page.clock.fastForward(61_000);
  await expect.poll(() => mock.kasPriceReads()).toBe(4);
  await expect(page.getByTestId(`token-last-${id}`)).toHaveText(/^\$/);
  // a second failure in a row: four minutes. The price comes back meanwhile, and the pace of a minute with the first success
  await mock.kasPrice({ mode: 'ok', usd: 0.04 });
  await page.clock.fastForward(121_000);
  expect(await mock.kasPriceReads()).toBe(4);
  await page.clock.fastForward(121_000);
  await expect.poll(() => mock.kasPriceReads()).toBe(5);
  await page.clock.fastForward(61_000);
  await expect.poll(() => mock.kasPriceReads()).toBe(6);
  await expect(page.getByTestId(`token-last-${id}`)).toHaveAttribute('title', /USD estimate at \$0\.04 per KAS$/);
});

test('the dollars of the Portfolio keep a connected wallet\'s page on one line at a phone width', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await chooseUsd(page);
  await fund(mock);
  await listExkcc(page);
  await page.goto('/#/portfolio');
  await connectWallet(page);
  const strip = page.getByTestId('portfolio-summary');
  await expect(strip).toHaveAttribute('data-currency', 'usd');
  await expect(page.getByTestId('portfolio-available')).toHaveText(/^\$[\d,]+\.\d\d$/);
  const rows = await page.locator('.pf-v').evaluateAll((els) => els.map((e) => ({ h: e.getBoundingClientRect().height, line: parseFloat(getComputedStyle(e).lineHeight) || 20 })));
  for (const r of rows) expect(r.h).toBeLessThan(r.line * 1.6);
});
