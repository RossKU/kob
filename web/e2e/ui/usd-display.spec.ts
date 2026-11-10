// The optional USD display (Settings, "Display currency"): KAS stays the default; with USD every READ-ONLY KAS value is shown as approximate dollars (the market list, a market's
// header strip and its Activity panel, the Activity page's summary, volume chart and feed, the Portfolio's summary and balances), with the KAS figure in the tooltip and a one-line
// note that says what the dollars are. The order book, the ticket's inputs, the price chart and everything signed stay in KAS, and no price the user types is converted.
// The price is the indexer's (`GET /v1/price/kas-usd`: the mock serves $0.03 taken now unless a test steers it); a missing, unavailable or too old (over 10 minutes) price
// falls back to KAS with a notice, and the app never asks for it while KAS is the display currency.
import { expect, test, type MockClient } from '../fixtures';
import { connectWallet, fund, gotoNav, listExkcc, openMarket, showActivity } from '../helpers/env';
import { seedOrders } from '../helpers/orders';

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

test('a market page shows its last price and 24 h statistics in dollars; the order book and the ticket stay in KAS', async ({ appPage: page, mock }) => {
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
  // the market's Activity panel is dollars too (the Activity page has its own test)
  await expect(page.getByTestId('activity-list')).toBeVisible();
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
  await expect(page.getByTestId('portfolio-total')).toHaveText(/^\$[\d,]+\.\d\d$/);
  expect(numberOf(await page.getByTestId('portfolio-total').textContent())).toBeCloseTo(kasTotal * PRICE, 1);
  await expect(page.getByTestId('portfolio-in-orders')).toHaveText(/^\$[\d,]+\.\d\d$/);
  // the Estimate tag of the total says it is dollars, not KAS
  await page.getByTestId('portfolio-estimate').click();
  await expect(page.getByTestId('portfolio-estimate-detail')).toContainText('An estimate in US dollars');
  await page.keyboard.press('Escape');
  // an open-orders count is a count
  await expect(page.getByTestId('portfolio-open-count')).not.toContainText('$');
  await expect(page.getByTestId('fiat-note')).toBeVisible();

  // the balances: the Value column is dollars, the held amounts are KAS and tokens
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  await expect(page.getByTestId('kas-total')).toHaveText(/^\$[\d,]+\.\d\d$/);
  // every read-only KAS amount of the balances is dollars (the amounts of a token stay in tokens)
  await expect(page.getByTestId('kas-free')).toContainText('$');
  await expect(page.getByTestId('kas-free')).not.toContainText('KAS');
  await expect(page.getByTestId('kas-locked')).toContainText('$');
  await expect(page.getByTestId('kas-locked')).not.toContainText('KAS');
  await expect(page.getByTestId('token-balances-table')).toContainText('Locked in orders (USD)');
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

/** The visible text of the page's main area, without the parts that stay KAS (`skip`: test ids). */
async function textWithout(page: import('@playwright/test').Page, skip: string[]): Promise<string> {
  return page.evaluate((ids) => {
    const root = (document.querySelector('main') ?? document.body).cloneNode(true) as HTMLElement;
    for (const id of ids) root.querySelectorAll(`[data-testid="${id}"]`).forEach((e) => e.remove());
    root.querySelectorAll('[aria-hidden="true"], script, style').forEach((e) => e.remove());
    // one line per text node: the end of a figure and the words of the next element are not one text
    const lines: string[] = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node: Node | null;
    while ((node = walker.nextNode())) lines.push(node.textContent ?? '');
    return lines.join('\n');
  }, skip);
}

/** The lines of a text that are a figure in KAS (a number with the unit). */
const kasAmounts = (text: string): string[] => text.split(/\n/).filter((l) => KAS_AMOUNT.test(l));

/** A number with the KAS unit: what no read-only figure may be while USD is on. */
const KAS_AMOUNT = /\d[  ]?KAS\b/;

test('the Activity page: the summary (all time, windows, the volume chart and its read-out, the books now) and the feed rows are dollars with the KAS figure in the tooltip', async ({ appPage: page, mock }) => {
  const [ask] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
  await mock.fill(ask!);
  await listExkcc(page);
  await page.goto('/#/activity');
  const sum = page.getByTestId('activity-summary');
  await expect(sum).toBeVisible();
  // KAS first: the units are KAS
  await expect(page.getByTestId('summary-all-volume')).toContainText('KAS');
  await expect(page.getByTestId('activity-sort-price')).toContainText('Price (KAS)');

  await chooseUsd(page);
  await page.reload();
  await expect(page.getByTestId('fiat-note').first()).toBeVisible();
  const vol = page.getByTestId('summary-all-volume');
  await expect(vol).toContainText('$');
  await expect(vol).not.toContainText('KAS');
  await expect(vol.locator('.sum-v')).toHaveAttribute('title', /^[\d,.]+ KAS · USD estimate at \$0\.03 per KAS$/);
  await expect(page.getByTestId('summary-window-volume')).toContainText('$');
  await expect(page.getByTestId('summary-window-volume')).not.toContainText('KAS');
  await expect(sum).toContainText('Volume, KAS markets (USD)');
  await expect(page.getByTestId('summary-readout')).toContainText('$');
  await expect(page.getByTestId('summary-readout')).not.toContainText('KAS');
  // the volume chart's axis
  const axis = sum.locator('.sum-y span').first();
  await expect(axis).toHaveText(/^\$/);
  // paid to executors and locked in orders
  await expect(page.getByTestId('summary-all-paid')).not.toContainText('KAS');
  await expect(page.getByTestId('summary-window-paid')).not.toContainText('KAS');
  await expect(page.getByTestId('summary-now-locked')).toContainText('$');
  await expect(page.getByTestId('summary-now-locked')).not.toContainText('KAS');
  // every window
  for (const w of ['7D', '1M', '1Y', '1D']) {
    await page.getByTestId(`summary-window-${w}`).click();
    await expect(page.getByTestId('summary-window-volume')).not.toContainText('KAS');
  }

  // the feed: a row's KAS value line and its price are dollars, the header says USD
  await expect(page.getByTestId('activity-sort-price')).toContainText('Price (USD)');
  const row = page.getByTestId('activity-row').first();
  await expect(row).toBeVisible();
  await expect(row.locator('.act-sub')).toHaveText(/^\$/);
  await expect(row.locator('.act-sub')).toHaveAttribute('title', /KAS · USD estimate at \$0\.03 per KAS$/);
  await expect(row.locator('.act-c-px')).toHaveText(/^\$/);
  await expect(row.locator('.act-c-px')).toHaveAttribute('title', /KAS · USD estimate at \$0\.03 per KAS$/);
  // the token amount of the row stays in tokens
  await expect(row.locator('.act-c-amt b')).not.toContainText('$');

  // sorting by the price column still orders by the number
  await page.getByTestId('activity-sort-price').click();
  await expect(page.getByTestId('activity-sort-price').locator('xpath=ancestor::*[@role="columnheader"][1]')).toHaveAttribute('aria-sort', /ascending|descending/);

  // nothing on the page is still a number in KAS (the explanatory note and the markets' names are not figures)
  expect(kasAmounts(await textWithout(page, []))).toEqual([]);
});

test('the market page: the Activity panel rows are dollars, and no read-only figure left on the page is a number in KAS except the book, the ticket and the price chart', async ({ appPage: page, mock }) => {
  await chooseUsd(page);
  await fund(mock);
  const [ask] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
  await mock.fill(ask!);
  await openMarket(page, mock);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
  await showActivity(page);
  const row = page.getByTestId('activity-list').getByTestId('activity-row').first();
  await expect(row).toBeVisible();
  await expect(row.locator('.act-sub')).toHaveText(/^\$/);
  await expect(row.locator('.act-c-px')).toHaveText(/^\$/);
  await expect(page.getByTestId('activity-sort-price')).toContainText('Price (USD)');
  expect(kasAmounts(await textWithout(page, ['book-section', 'order-ticket', 'price-chart', 'chart-legend', 'market-orders']))).toEqual([]);
});

test('the market list and the Portfolio balances: no figure left is a number in KAS', async ({ appPage: page, mock }) => {
  await chooseUsd(page);
  await fund(mock);
  const id = await openList(page, mock);
  await expect(page.getByTestId(`token-row-${id}`)).toHaveAttribute('data-state', 'ready');
  expect(kasAmounts(await textWithout(page, []))).toEqual([]);
  await page.goto('/#/portfolio');
  await connectWallet(page);
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  await expect(page.getByTestId('kas-total')).toContainText('$');
  expect(kasAmounts(await textWithout(page, []))).toEqual([]);
});

/** Where the ticket's cost line stands against its amount box, with an amount typed and with none: the layout a viewer sees (the figures change, the places must not). */
async function costGeometry(page: import('@playwright/test').Page): Promise<number[]> {
  const out: number[] = [];
  for (const amount of ['100', '250', '']) {
    await page.getByTestId('order-amount').fill(amount);
    await page.waitForTimeout(150);
    const a = (await page.getByTestId('order-amount').boundingBox())!;
    const c = (await page.getByTestId('order-cost-preview').boundingBox())!;
    out.push(c.x - a.x, c.y - a.y, c.width, c.height);
  }
  return out;
}

test('the ticket shows a muted read-only dollar estimate beside the order total, in a slot that does not move; its own figures stay KAS', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await page.getByTestId('order-price').fill('0.0251');
  // KAS first: the layout of the cost line with an amount typed and without
  await expect(page.getByTestId('order-cost-usd')).toHaveCount(0);
  const inKas = await costGeometry(page);

  await page.evaluate(() => localStorage.setItem('kob.currency', 'usd'));
  await page.reload();
  await connectWallet(page);
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await page.getByTestId('order-price').fill('0.0251');
  await page.getByTestId('order-amount').fill('100');
  const cost = page.getByTestId('order-cost-preview');
  const usd = page.getByTestId('order-cost-usd');
  await expect(usd).toHaveText(/^≈ \$[\d,]+\.\d+$/);
  await expect(usd).toHaveAttribute('title', /KAS · USD estimate at \$0\.03 per KAS$/);
  // 100 tokens at about 0.0251 KAS is about 2.5 KAS (the ticket's own estimate, with the order's tip and fee) = about $0.075 at the mock's $0.03
  expect(numberOf(await usd.textContent())).toBeCloseTo(2.5 * PRICE, 2);
  // the ticket's own cost figure and unit are KAS
  await expect(cost).toContainText('KAS');
  // the same places as in KAS, whatever is typed
  const inUsd = await costGeometry(page);
  expect(inUsd).toEqual(inKas);
  await expect(usd).toHaveText('');
  // it is a muted figure, not an input: nothing to type in or tab to
  expect(await usd.evaluate((e) => e.tagName)).toBe('SPAN');
});

test('the four Portfolio tabs with a fill: the orders, the trade history and the balances show no figure in KAS (the token amounts stay tokens), the headers say USD, the sort stays numeric', async ({ appPage: page, mock }) => {
  const [open] = await seedOrders(mock, [{ side: 'ask', price: 2_800_000, tokens: 3 }]);
  const [done] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 2 }]);
  await mock.fill(done!, 200_000_000n);
  await chooseUsd(page);
  await fund(mock);
  await openMarket(page, mock, { route: '/portfolio/open' });
  await expect(page.getByTestId(`order-row-${open}`)).toBeVisible();
  await expect(page.getByTestId('orders-sort-price')).toContainText('Price (USD)');
  await expect(page.getByTestId('orders-sort-total')).toContainText('Total (USD)');
  expect(kasAmounts(await textWithout(page, []))).toEqual([]);
  await expect(page.getByTestId(`order-line-${open}`).locator('.or-price .dt-val')).toHaveText(/^\$[\d,.]+$/);
  await expect(page.getByTestId(`order-line-${open}`).locator('.or-value .dt-val')).toHaveText(/^\$[\d,.]+$/);
  // the amount of tokens is not converted
  await expect(page.getByTestId(`order-line-${open}`).locator('.or-amount')).toContainText('EXKCC');

  await page.getByTestId('orders-tab-history').click();
  await expect(page.getByTestId(`order-row-${done}`)).toBeVisible();
  await expect(page.getByTestId('orders-sort-avg')).toContainText('Avg price (USD)');
  expect(kasAmounts(await textWithout(page, []))).toEqual([]);

  await page.getByTestId('orders-tab-fills').click();
  await expect(page.getByTestId('wallet-fill-row').first()).toBeVisible();
  await expect(page.getByTestId('wallet-fills-sort-price')).toContainText('Price (USD)');
  await expect(page.getByTestId('wallet-fills-sort-value')).toContainText('Value (USD)');
  expect(kasAmounts(await textWithout(page, []))).toEqual([]);
  const price = page.getByTestId('wallet-fill-row').first().locator('.dt-c-price .dt-val');
  await expect(price).toHaveText(/^\$[\d,.]+$/);
  // sorted by the price: numeric (the dollar texts are read as numbers)
  await page.getByTestId('wallet-fills-sort-price').click();
  const prices = (await page.locator('[data-testid="wallet-fill-row"] .dt-c-price .dt-val').allTextContents()).map(numberOf);
  expect(prices).toEqual([...prices].sort((a, b) => b - a));

  await page.getByTestId('orders-tab-balances').click();
  await expect(page.getByTestId('kas-balance')).toBeVisible();
  expect(kasAmounts(await textWithout(page, []))).toEqual([]);
});

test('a flipped market (KAS/TOKEN): the 24 h KAS volume tile is dollars with the KAS figure in its tooltip; the prices (tokens per KAS) and the token volume stay', async ({ appPage: page, mock }) => {
  await fund(mock);
  const id = await openMarket(page, mock);
  await page.getByTestId('market-flip').click();
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
  // KAS first: the volume tile is in KAS
  const volume = page.getByTestId('stat-volume');
  await expect(volume).toContainText('24h volume (KAS)');
  await expect(volume).not.toContainText('$');
  const stats = await mock.get(`/v1/stats/${id}`);
  const kasVolume = Number(stats.quote_volume_24h) / 1e8;

  await page.evaluate(() => localStorage.setItem('kob.currency', 'usd'));
  await page.reload();
  await connectWallet(page);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
  await expect(volume).toContainText('24h volume (USD)');
  await expect(volume).toContainText('$');
  await expect(volume).not.toContainText('KAS');
  expect(numberOf(await volume.locator('.mkt-stat-value').textContent())).toBeCloseTo(kasVolume * PRICE, 1);
  await expect(volume.locator('.mkt-stat-value')).toHaveAttribute('title', /KAS · USD estimate at \$0\.03 per KAS$/);
  // the prices are tokens per KAS and the token volume is tokens: not dollars
  for (const tile of ['stat-last', 'stat-high', 'stat-low', 'stat-quote-volume']) await expect(page.getByTestId(tile)).not.toContainText('$');
  await expect(page.getByTestId('stat-quote-volume')).toContainText('24h volume (EXKCC)');
  await expect(page.getByTestId('stat-last')).toContainText('per KAS');
});
