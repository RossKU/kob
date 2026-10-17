// Screen details a user sees and clicks, each a regression of a defect found in a hands-on pass over the app (mock stack): the wallet
// address after a network switch, copy buttons, the pair ticket's price unit, panels when the
// indexer is down, the settings that pin servers and their reset, the skip link, a token of 18 decimals, the depth legend, the issue summary.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { connectWallet, fund, listExkcc, openMarket } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { fillFields, peekReceipt, pickType, placeOrder, settlePlan, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

const noPageScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth);

test.describe('KasWare that starts on mainnet and switches', () => {
  test.use({ walletOptions: { network: 'mainnet' } });
  test('the address shown is the testnet one after the switch', async ({ appPage: page, mock }) => {
    await openMarket(page, mock);
    // the wallet's network is shown only when it differs from the app's
    await expect(page.getByTestId('wallet-network')).toHaveCount(0);
    await expect(page.getByTestId('wallet-connected')).toHaveAttribute('data-network', 'testnet-10');
    await expect(page.getByTestId('wallet-address')).toHaveAttribute('data-value', TEST_KEYS.alice.testnetAddress);
  });
});

test('copy buttons keep their label on one line (token details)', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  const btn = page.getByTestId('token-covenant-id').locator('xpath=..').locator('.copy-btn');
  await expect(btn).toHaveText('Copy');
  const box = await btn.boundingBox();
  expect(box!.height).toBeLessThan(30);
});

test.describe('market panels when the indexer cannot be reached', () => {
  test('chart and depth show no endless skeleton, the trades panel no "No trades yet"', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    const tok = await mock.token();
    // the app learns the token from the registry; every indexer read fails
    await page.route('**/v1/**', (r) => r.abort());
    await page.goto(`/#/market/${tok.covenant_id}`);
    await expect(page.getByTestId('book-error')).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('chart-loading')).toHaveCount(0);
    await expect(page.getByTestId('trades-error')).toBeVisible();
    await expect(page.getByTestId('trades-section').getByText('No trades yet.')).toHaveCount(0);
    await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'empty');
    await expect(page.getByTestId('depth-empty')).toHaveText('The book could not be loaded.');
    // no book, no grouping to offer (it showed "0.00000001")
    await expect(page.getByTestId('book-group')).toHaveCount(0);
  });
});

test('a new page takes the keyboard focus to its content', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  await page.getByTestId('nav-orders').click();
  await expect(page).toHaveURL(/#\/orders/);
  await expect(page.getByTestId('main')).toBeFocused();
  await page.getByTestId('nav-settings').focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#\/settings/);
  await expect(page.getByTestId('main')).toBeFocused();
});

test('the depth legend keeps the estimate mark with its amounts', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  const summary = page.getByTestId('depth-summary');
  await expect(summary).toContainText(/^~ In view: /);
  // the price labels at the edges stay inside the chart
  const out = await page.getByTestId('depth-chart').locator('svg').evaluate((svg) => {
    const box = svg.getBoundingClientRect();
    return [...svg.querySelectorAll('text')].filter((t) => {
      const r = t.getBoundingClientRect();
      return r.left < box.left - 0.5 || r.right > box.right + 0.5;
    }).length;
  });
  expect(out).toBe(0);
});

test('the order book column titles keep their units (they wrap, no ellipsis)', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  const head = page.getByTestId('order-book').locator('.ob-head > span');
  await expect(head.first()).toHaveText('Price (EXKCC / KAS)');
  const cut = await head.evaluateAll((els) => els.filter((e) => e.scrollWidth > e.clientWidth + 1).length);
  expect(cut).toBe(0);
});

test.describe('the pair ticket', () => {
  test('the limit price help names the quote token, not KAS', async ({ appPage: page, mock }) => {
    const QUOTE = 'e7'.repeat(32);
    await mock.seed({ pair: true });
    const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
    for (const t of reg.templates) t.review_status = 'reviewed';
    const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
    Object.assign(exkcc, { status: 'listed', verified: true });
    reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6 });
    await page.route('**/registry/tokens.json', (route) => route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }));
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    const hint = page.getByTestId('order-price').locator('xpath=ancestor::div[contains(@class,"field")][1]').locator('.field-hint');
    await expect(hint).toContainText('EXUSD per EXKCC');
    // the sell-first prefund of a pair if-done is in the quote token too (its box says EXKCC / EXUSD)
    await pickType(page, 'ifd', 'sell');
    await page.getByTestId('order-advanced').locator('summary').click();
    const prefund = page.getByTestId('field-prefund').locator('xpath=ancestor::div[contains(@class,"field")][1]').locator('.field-hint');
    await expect(prefund).toContainText('EXUSD per token');
  });
});

test('the registry template verdicts fit the settings column', async ({ appPage: page }) => {
  await page.goto('/#/settings');
  const wrap = page.getByTestId('settings-registry').locator('.registry-templates');
  await expect(wrap.locator('tbody tr').first()).toBeVisible();
  expect(await wrap.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
});

test.describe('settings', () => {
  test('saving one checkbox does not pin the site servers; a saved indexer can be reset from Settings', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    await page.goto('/#/settings');
    await page.getByTestId('settings-priority-fee').check();
    await page.getByTestId('settings-save').click();
    await expect(page.getByTestId('settings-saved')).toBeVisible();
    expect(JSON.parse((await page.evaluate(() => localStorage.getItem('kob.settings')))!)).toEqual({ features: { priorityFee: true } });
    await expect(page.getByTestId('settings-reset-servers')).toHaveCount(0);

    // another indexer (the same mock under another host name), saved: every page says so
    await page.getByTestId('settings-indexer-url').fill(mock.url.replace('127.0.0.1', 'localhost'));
    await page.getByTestId('settings-servers-ack').check();
    await page.getByTestId('settings-save').click();
    await page.reload();
    await expect(page.getByTestId('banner-stored-indexer')).toBeVisible();
    // the banner says "reset it": Settings has the button
    await page.getByTestId('settings-reset-servers').click();
    await expect(page.getByTestId('settings-saved')).toBeVisible();
    expect(JSON.parse((await page.evaluate(() => localStorage.getItem('kob.settings')))!)).toEqual({ features: { priorityFee: true } });
    await page.reload();
    await expect(page.getByTestId('settings-view')).toBeVisible();
    await expect(page.getByTestId('banner-stored-indexer')).toHaveCount(0);
    await expect(page.getByTestId('settings-indexer-url')).toHaveValue(mock.url);
  });
});

test('the skip link is the first stop of the keyboard and moves the focus to the content', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  await page.locator('body').focus();
  await page.keyboard.press('Tab');
  await expect(page.getByTestId('skip-link')).toBeFocused();
  await expect(page.getByTestId('skip-link')).toBeInViewport();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('main')).toBeFocused();
  await expect(page).toHaveURL(/#\/market\//);
});

test.describe('a token of 18 decimals', () => {
  const D18 = 'd1'.repeat(32);
  test('price scale text, issue prices per whole token and a long balance that stays inside its card', async ({ appPage: page, mock }) => {
    await mock.seed({ tokens: [{ ticker: 'EIGHTEEN', name: 'Eighteen decimals', covenant_id: D18, program: 'KCC20Ref_8x8', extension_commitment: 'ee'.repeat(32), decimals: 18, tick: 10_000 }] });
    await mock.seed({ book: { token: D18, mid: 2_500_000, tick: 10_000, levels: 3 } });
    await mock.giveKas('alice', 1000n * 100_000_000n, 1);
    await mock.giveTokens('alice', 1_234_567_890_123_456_789n, { token: 'EIGHTEEN' });
    const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
    for (const t of reg.templates) t.review_status = 'reviewed';
    const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
    reg.tokens.push({ ...exkcc, status: 'listed', verified: true, ticker: 'EIGHTEEN', name: 'Eighteen decimals', covenant_id: D18, decimals: 18 });
    await page.route('**/registry/tokens.json', (route) => route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }));
    await page.goto(`/#/market/${D18}`);
    await connectWallet(page);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    // the standard scale is 10^9 base units: a billionth of a token, not "one whole token"
    await expect(page.getByTestId('token-scale')).toContainText('per 1000000000 base units (0.000000001 token)');
    // the book quotes 0.025 KAS per 10^9 base units = 25,000,000 KAS per token; a sell far below it crosses the book (the planner's price warning
    // itself is not shown), and the final confirmation names the token's amount and the price per whole token
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '1', price: '1' });
    await settlePlan(page);
    await expect(page.getByTestId('order-issue-PRICE_AGGRESSIVE_VS_MARKET')).toHaveCount(0);
    // the long balance stays whole in the Available line (the market page has no balance card any more)
    await expect(page.getByTestId('order-available')).toHaveText('Available 1.234567890123456789 EIGHTEEN');
    const shown = await peekReceipt(page);
    expect(shown.orders[0]!.token).toMatch(/^EIGHTEEN [(]d1d1d1d1…d1d1d1d1[)]$/);
    expect(shown.orders[0]!.rows['Expected price (auction start)']).toMatch(/^2[45],\d{3},\d{3} EIGHTEEN \/ KAS/);
    expect(await noPageScroll(page)).toBe(true);
  });
});

test('the issue summary does not ask to fix fields before the form is filled', async ({ appPage: page }) => {
  await page.goto('/#/issue');
  await expect(page.getByTestId('issue-summary-empty')).toHaveText('The summary appears once the form is complete.');
});

test('closing a buy-first position that bought nothing yet does not offer to sell the wallet\'s tokens', async ({ appPage: page, mock, wallet }) => {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'ifd', 'buy');
  await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' });
  const placed = await placeOrder(page, mock);
  const entryId = placed.views[0].covenant_id as string;
  await page.getByTestId('nav-orders').click();
  await page.getByTestId(`position-close-${entryId}`).click();
  await runFlow(page, mock, wallet, { steps: 1 });
  const banner = page.getByTestId('orders-close-followup');
  await expect(banner).toContainText('nothing to sell');
  await expect(page.getByTestId('orders-close-sell')).toHaveCount(0);
});

test('on a phone the auto-refund checkbox keeps its text beside it', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await openMarket(page, mock, { route: '/orders' });
  const box = await page.getByTestId('orders-auto-refund').boundingBox();
  const text = await page.getByTestId('orders-auto-refund-label').locator('span').boundingBox();
  expect(text!.x).toBeGreaterThan(box!.x + box!.width - 1);
  expect(Math.abs(text!.y - box!.y)).toBeLessThan(12);
});

test('without the registry the banner matches what the page does: indexer tokens trade as unlisted tokens', async ({ appPage: page, mock }) => {
  await page.route('**/registry/tokens.json', (r) => r.fulfill({ status: 500, body: 'down' }));
  const tok = await mock.token();
  await page.goto(`/#/market/${tok.covenant_id}`);
  await expect(page.getByTestId('banner-registry')).toContainText('unlisted tokens named by their covenant id');
  await expect(page.getByTestId('banner-registry')).not.toContainText('not tradable');
  await expect(page.getByTestId('order-ticket')).not.toHaveAttribute('data-state', 'untradable');
  await expect(page.getByTestId('token-open-caution')).toBeVisible();
});

test('a side or type switch keeps the amount and the price; the stop, exit and tip start over', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'ifd', 'buy');
  await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026', tip: '0.0001', 'entry.stop': '0.024' });
  await waitReviewable(page);
  // buy -> sell: a buy's entry stop (at or below its price; a sell's is at or above) and tip are not carried to the sell, nothing is left out of sight
  await page.getByTestId('order-side-sell').click();
  await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-side', 'sell');
  await expect(page.getByTestId('order-amount')).toHaveValue('2');
  await expect(page.getByTestId('order-price')).toHaveValue('0.0245');
  await expect(page.getByTestId('field-exit.takeProfit')).toHaveValue('');
  await expect(page.getByTestId('order-tip')).toHaveValue('');
  await expect(page.getByTestId('field-entry.stop')).toHaveValue('');
  await fillFields(page, { 'exit.takeProfit': '0.0235' });
  await waitReviewable(page);
  // ifd -> stop-limit: the amount stays, the stop and limit are new boxes
  await fillFields(page, { tip: '0.0002' });
  await pickType(page, 'stopLimit');
  await expect(page.getByTestId('order-amount')).toHaveValue('2');
  await expect(page.getByTestId('order-tip')).toHaveValue('');
  await expect(page.getByTestId('field-stop')).toHaveValue('');
});

test('the header after connecting: one network badge, a compact wallet chip, short on a phone', async ({ appPage: page, mock }) => {
  await openMarket(page, mock);
  const header = page.locator('header.header');
  // 1280 px: the app's network once (the wallet's only when it differs), the wallet chip with its name and address
  await expect(header.getByText('testnet-10', { exact: true })).toHaveCount(1);
  await expect(page.getByTestId('wallet-network')).toHaveCount(0);
  await expect(page.getByTestId('wallet-connected')).toContainText('KasWare');
  await expect(page.getByTestId('wallet-address')).not.toContainText('kaspatest:');
  await expect(page.getByTestId('wallet-disconnect')).toHaveAccessibleName('Disconnect');
  const wide = await header.boundingBox();
  expect(wide!.height).toBeLessThan(70);
  // 375 px: brand and tools, one line of navigation, the wallet chip (it took ~220 px)
  await page.setViewportSize({ width: 375, height: 812 });
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  const phone = await header.boundingBox();
  expect(phone!.height).toBeLessThan(130);
  expect(await noPageScroll(page)).toBe(true);
  await page.getByTestId('wallet-disconnect').click();
  await expect(page.getByTestId('wallet-connected')).toHaveCount(0);
});
