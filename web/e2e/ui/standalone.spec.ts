// Standalone mode (src/data/standalone.ts): when the indexer cannot be used (or the user switches it on in Settings) the page follows the node's new
// blocks itself. What the page already read from the indexer is KEPT (data/stream-carry.ts): the book starts from the resting orders it listed (each
// asked on the node), the candles, the last price and 24 h figures, the activity rows and the token (an open-list token's decimals come from the
// indexer's market scale) with its ticket and the Trade tab. Nothing on the page marks it: the bell has one calm line and the header's dot is yellow
// (e2e/ui/indexer-outage.spec.ts). An order placed meanwhile with a real transaction (POST /mock/place) is listed once the node holds it; the
// wallet's own orders come from its receipts. Trading still goes to the node directly: a market order starts at the best ask the node confirms. A
// fill in a block (POST /mock/take, a real batch) becomes a trade of the activity log. When the indexer is back, its book is used again. A page that
// read no book from the indexer (the mode on from the start) has only what the node showed, and its book says since when ("orders seen since HH:MM").
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { closeBell, expect, openBell, test, type MockClient } from '../fixtures';
import { TOKEN, fund, listExkcc, openMarket } from '../helpers/env';
import { clickPlace, fillFields, onChainTerms, pickType, placeOrder, waitSubmitted, type Placed } from '../helpers/ticket';

test.use({ autoOpen: false });

/** where the screenshots of the layouts go: `SHOTS` of the environment, else a temporary folder */
const SHOTS = process.env.SHOTS ?? join(tmpdir(), 'kob-standalone-shots');
/** 0.03 KAS per EXKCC: above every seeded ask (0.0251 .. 0.026), so it is the best ask only where the seeded book is not shown */
const HIGH_ASK = 3_000_000n;

const asks = (page: Page) => page.getByTestId('book-asks').locator('.book-row:not(.book-blank)');
/** The levels the book lists of `n`: its card holds as many as its height (8 to 40; the ticket's row sets it), so a book of more than that shows what fits. */
const listed = async (page: Page, n: number) => Math.min(n, await page.getByTestId('book-asks').locator('.book-row').count());
const banner = (page: Page) => page.getByTestId('banner-standalone');
/** the marks the page used to scatter in standalone mode: none of them is shown any more */
const MARKS = ['book-standalone', 'stats-standalone', 'chart-standalone', 'depth-standalone', 'token-kept-standalone', 'activity-since', 'activity-nothing-before', 'token-list-standalone'];

async function indexerDown(mock: MockClient, down = true): Promise<void> {
  await mock.post('/mock/indexer', { down });
}

/** A plain ask of `key` placed with a real transaction (a block the node announces). */
async function placeOnChain(mock: MockClient, key: string, price: bigint, tokens: bigint): Promise<{ covenant_id: string; transactionId: string }> {
  return mock.post('/mock/place', { key, side: 'ask', price: price.toString(), amount: (tokens * TOKEN).toString() });
}

/** `placedBy` while the indexer is down: the mock's own record of the submission and of the orders it created. */
async function placedWithoutIndexer(mock: MockClient, txid: string): Promise<Placed> {
  const sub = (await mock.submitted()).find((s) => s.txid === txid);
  expect(sub, `the mock node accepted ${txid}`).toBeTruthy();
  const views = await Promise.all(sub!.created.map((c) => mock.get(`/mock/order?id=${c.covenantId}`)));
  return { txid, submission: sub!, views };
}

test.describe('standalone mode', () => {
  test('the indexer goes down: the book read from it stays and follows the node; an order placed meanwhile is listed; the indexer book is back when it returns', async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    // a short window from the start: the activity log is more than its 200 px of reading margin below the fold, so it reads nothing from the indexer until it is scrolled to (the page is short, the
    // log starts right under the trading row)
    await page.setViewportSize({ width: 1280, height: 420 });
    await listExkcc(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await expect(asks(page).first()).toBeVisible();
    await expect.poll(() => page.getByTestId('book-asks').locator('.book-row').count()).toBeGreaterThanOrEqual(8);
    await openBell(page);
    await expect(banner(page)).toHaveCount(0);

    await indexerDown(mock);
    await expect(banner(page)).toBeVisible({ timeout: 60_000 });
    await expect(banner(page)).toContainText(/^Trading through your node since \d\d:\d\d: the indexer cannot be used\./);
    await closeBell(page);
    // the book read from the indexer stays (every order of it asked on the node), and nothing marks the page
    await expect(page.getByTestId('live-indicator')).toHaveText('Via node');
    await expect.poll(() => asks(page).count()).toBeGreaterThanOrEqual(8);
    for (const id of MARKS) await expect(page.getByTestId(id), id).toHaveCount(0);
    await page.getByTestId('book-view-depth').click();
    await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
    await page.getByTestId('book-view-book').click();

    // an ask placed now, with a real transaction: listed once the node holds it (beside a seeded ask of the same price: their level grows)
    const before = (await page.getByTestId('book-ask-row').filter({ hasText: '0.0252' }).textContent()) ?? '';
    const placed = await placeOnChain(mock, 'carol', 2_520_000n, 50n);
    await expect(page.getByTestId('book-ask-row').filter({ hasText: '0.0252' })).not.toHaveText(before, { timeout: 20_000 });
    expect(placed.covenant_id).toMatch(/^[0-9a-f]{64}$/);
    // and its placement is a row of the activity log
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await expect(page.locator('[data-testid=activity-row][data-kind=placed]')).toHaveCount(1, { timeout: 20_000 });

    // the indexer is back: its book wins (the seeded levels and the new ask)
    await indexerDown(mock, false);
    await openBell(page);
    await expect(banner(page)).toHaveCount(0, { timeout: 60_000 });
    await closeBell(page);
    await expect.poll(() => asks(page).count(), { timeout: 20_000 }).toBeGreaterThanOrEqual(8);
    await expect(page.getByTestId('live-indicator')).not.toHaveText('Via node');
  });

  test('a market already loaded stays when the page goes standalone: candles, figures, activity rows, the open-list token with its ticket and the Trade tab', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    // no listExkcc: the token is an open-list one, its decimals come from the indexer's market scale
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('ticket-container')).toBeVisible();
    const chart = page.getByTestId('price-chart');
    await expect(chart).toHaveAttribute('data-state', 'ready', { timeout: 30_000 });
    const bars = Number(await chart.getAttribute('data-bars'));
    expect(bars).toBeGreaterThan(50);
    await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
    const last = (await page.getByTestId('stat-last').textContent()) ?? '';
    expect(last).toMatch(/0\.02\d{4}/);
    // the activity log reads only once it is on screen
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    const fills = page.locator('[data-testid=activity-row][data-kind=fill]');
    await expect(fills.first()).toBeVisible();
    await expect(page.getByTestId('nav-trade')).toBeVisible();

    // the switch in Settings, then back to the market: the page does not start empty
    await page.goto('/#/settings');
    await page.getByTestId('settings-standalone').check();
    await page.getByTestId('settings-save').click();
    await expect(page.getByTestId('settings-saved')).toBeVisible();
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('live-indicator')).toHaveText('Via node', { timeout: 20_000 });
    // the chart keeps its history; the last price and the figures are the ones read
    await expect(chart).toHaveAttribute('data-state', 'ready', { timeout: 30_000 });
    expect(Number(await chart.getAttribute('data-bars'))).toBeGreaterThanOrEqual(bars);
    await expect(page.getByTestId('stat-last')).toHaveText(last);
    await expect(page.getByTestId('stat-volume')).not.toContainText('—');
    // the token stays: its ticket, the Trade tab
    await expect(page.getByTestId('ticket-container')).toBeVisible();
    await expect(page.getByTestId('ticket-standalone-scale')).toHaveCount(0);
    await expect(page.getByTestId('nav-trade')).toBeVisible();
    // the book is the one read from the indexer, asked on the node
    await expect(asks(page).first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId('book-empty')).toHaveCount(0);
    // the activity rows of the indexer stay
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await expect(fills.first()).toBeVisible();
    for (const mark of MARKS) await expect(page.getByTestId(mark), mark).toHaveCount(0);
    // the list keeps the token
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${id}`)).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId('token-list-standalone')).toHaveCount(0);
  });

  test('trading in standalone mode: own orders from receipts, a market buy starts at the ask the node confirms, a fill becomes a trade', async ({ appPage: page, mock, wallet }) => {
    test.setTimeout(240_000);
    await fund(mock, 'alice', { tokens: 50n });
    const token = (await mock.token()).covenant_id as string;
    const best = BigInt((await mock.get(`/v1/books/${token}?aggregate=false&depth=1`)).asks[0].price);
    await openMarket(page, mock);
    // alice's own sell, placed while the indexer is up: this browser keeps its receipt
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '30', price: '0.05' });
    const own = await placeOrder(page, mock);
    expect(own.txid).toMatch(/^[0-9a-f]{64}$/);

    await indexerDown(mock);
    await openBell(page);
    await expect(banner(page)).toBeVisible({ timeout: 60_000 });
    await closeBell(page);
    // the book read from the indexer stays (alice's own order is in it, and resolved on the node from its receipt too)
    await expect(asks(page).first()).toBeVisible({ timeout: 30_000 });

    // carol's ask placed meanwhile
    const carol = await placeOnChain(mock, 'carol', HIGH_ASK, 50n);

    // a market buy is planned from that book after the node check (the best ask the indexer listed, which the node confirms), signed by the wallet and sent to the node
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '2' });
    await clickPlace(page);
    const buy = await placedWithoutIndexer(mock, await waitSubmitted(page));
    expect(onChainTerms(buy)[0]!.state.price).toBe(best.toString());
    expect(wallet).not.toBeNull();

    // a matcher fills 10 of carol's ask with a real transaction: the fill is in the activity log ("since HH:MM"), and the book follows the order to its continuation
    await mock.post('/mock/take', { covenant_id: carol.covenant_id, amount: (10n * TOKEN).toString() });
    // (the log on screen, the pointer off it: a pointer moving in it holds new rows back)
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await page.mouse.move(1, 1);
    const fills = page.locator('[data-testid=activity-row][data-kind=fill]');
    await expect(fills).toHaveCount(1, { timeout: 30_000 });
    await expect(fills.first()).toContainText('0.03');
    await expect(page.getByTestId('activity-live')).toHaveAttribute('data-mode', 'standalone');
  });

  test('a phone (390 px) shows the entry, and the book of a page that read none from the indexer says since when', async ({ appPage: page, mock }) => {
    test.setTimeout(150_000);
    await page.setViewportSize({ width: 390, height: 844 });
    await listExkcc(page);
    const id = (await mock.token()).covenant_id;
    await indexerDown(mock);
    await page.goto(`/#/market/${id}`);
    await openBell(page);
    await expect(banner(page)).toBeVisible({ timeout: 60_000 });
    await closeBell(page);
    await placeOnChain(mock, 'carol', HIGH_ASK, 50n);
    await placeOnChain(mock, 'bob', HIGH_ASK + 100_000n, 40n);
    await expect(asks(page)).toHaveCount(2, { timeout: 20_000 });
    await expect(page.getByTestId('book-partial')).toHaveText(/^orders seen since \d\d:\d\d$/);
    // no horizontal scroll
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
    await page.screenshot({ path: `${SHOTS}/standalone-phone-390.png`, fullPage: false });
    await page.getByTestId('book-section').scrollIntoViewIfNeeded();
    await page.screenshot({ path: `${SHOTS}/standalone-phone-390-book.png`, fullPage: false });
  });

  test.describe('switched on in Settings', () => {
    test.use({ appConfig: { standalone: { manual: true } } });

    test('the market comes from the node and the indexer is not asked for it', async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      const asked: string[] = [];
      page.on('request', (r) => {
        if (/\/v1\//.test(new URL(r.url()).pathname)) asked.push(new URL(r.url()).pathname);
      });
      await listExkcc(page);
      const id = (await mock.token()).covenant_id;
      await page.goto(`/#/market/${id}`);
      await openBell(page);
      await expect(banner(page)).toBeVisible({ timeout: 20_000 });
      await closeBell(page);
      await expect(page.getByTestId('book-partial')).toHaveText(/^orders seen since \d\d:\d\d$/);
      await expect(page.getByTestId('status-dot')).toHaveAttribute('data-indexer-code', 'standalone');
      await expect(page.getByTestId('status-dot')).toHaveAttribute('data-trading', 'manual');
      await expect(page.getByTestId('status-dot')).toHaveAttribute('data-level', 'warn');
      await placeOnChain(mock, 'carol', HIGH_ASK, 50n);
      await expect(asks(page)).toHaveCount(1, { timeout: 20_000 });
      await page.waitForTimeout(16_000);
      // the logo blocklist is the only indexer route left (logos, not the market)
      expect(asked.filter((p) => p !== '/v1/logo-blocklist')).toEqual([]);
      await page.screenshot({ path: `${SHOTS}/standalone-manual-desktop.png`, fullPage: false });
    });

    test('a token this page never read from the indexer says why it is not opened, and the Trade tab stays', async ({ appPage: page }) => {
      test.setTimeout(60_000);
      await page.goto(`/#/market/${'ab'.repeat(32)}`);
      await expect(page.getByTestId('token-not-found')).toContainText('Token not known in standalone mode');
      await expect(page.getByTestId('token-not-found')).toContainText(/the indexer is back/);
      await expect(page.getByTestId('nav-trade')).toBeVisible();
    });
  });

  test.describe('without returning by itself', () => {
    test.use({ appConfig: { standalone: { autoReturn: false } } });

    test('the indexer answers again: the bell offers to switch back, the button does', async ({ appPage: page, mock }) => {
      test.setTimeout(150_000);
      await listExkcc(page);
      const id = (await mock.token()).covenant_id;
      await indexerDown(mock);
      await page.goto(`/#/market/${id}`);
      await openBell(page);
      await expect(banner(page)).toBeVisible({ timeout: 60_000 });
      await indexerDown(mock, false);
      await expect(page.getByTestId('standalone-back')).toBeVisible({ timeout: 40_000 });
      await expect(banner(page)).toBeVisible();
      await page.getByTestId('standalone-return').click();
      await expect(banner(page)).toHaveCount(0);
      await closeBell(page);
      await expect(asks(page)).toHaveCount(await listed(page, 10), { timeout: 20_000 });
    });
  });

  test('the settings: three switches, saved and applied at once', async ({ appPage: page, mock }) => {
    test.setTimeout(90_000);
    await listExkcc(page);
    void mock;
    await page.goto('/#/settings');
    await expect(page.getByTestId('settings-standalone-auto')).toBeChecked();
    await expect(page.getByTestId('settings-standalone-return')).toBeChecked();
    await page.getByTestId('settings-standalone').check();
    await page.getByTestId('settings-save').click();
    await expect(page.getByTestId('settings-saved')).toBeVisible();
    await openBell(page);
    await expect(banner(page)).toBeVisible({ timeout: 10_000 });
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem('kob.settings') ?? '{}'));
    expect(stored.standalone).toEqual({ manual: true });
  });
});

// founder 2026-11-11: switched on and saved, the mode could not be switched off in the same visit (the form compared itself with the page's settings, so
// the box turned back read as no change and Save stayed disabled)
test('Settings: standalone mode switched on and saved, then off and saved, in one visit', async ({ appPage: page }) => {
  await page.goto('/#/settings');
  await page.getByTestId('settings-standalone').check();
  await page.getByTestId('settings-save').click();
  await expect(page.getByTestId('settings-saved')).toBeVisible();
  await page.getByTestId('settings-standalone').uncheck();
  await expect(page.getByTestId('settings-save')).toBeEnabled();
  await page.getByTestId('settings-save').click();
  await expect(page.getByTestId('settings-saved')).toBeVisible();
  const stored = await page.evaluate(() => Object.values(localStorage).filter((v) => v.includes('"standalone"')).join('|'));
  expect(stored).toContain('"manual":false');
});
