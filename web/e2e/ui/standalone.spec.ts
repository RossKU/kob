// Standalone mode (src/data/standalone.ts): when the indexer cannot be used (or the user switches it on in Settings) the page follows the node's new
// blocks itself. The book is PARTIAL and says so ("Orders seen since HH:MM ..."): the mock's seeded orders were never in a block the page saw, an
// order placed meanwhile with a real transaction is (POST /mock/place), once the node holds it; the wallet's own orders come from its receipts.
// Trading still goes to the node directly: a market order starts at the best ask the node confirms. A fill in a block (POST /mock/take, a real
// batch) becomes a trade of the tape and the figures, all marked "since HH:MM". When the indexer is back, its book is used again.
// What the page already read from the indexer is KEPT when it goes standalone (data/stream-carry.ts): the candles, the last price and 24 h figures,
// the activity rows and the token (an open-list token's decimals come from the indexer's market scale) with its ticket and the Trade tab; those are
// marked "Indexer data until HH:MM, then from your node", the book is the node's partial one.
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
const banner = (page: Page) => page.getByTestId('banner-standalone');
/** the mark of what the indexer gave before the mode and what the node added after it */
const KEPT = /^Indexer data until \d\d:\d\d, then from your node \(since \d\d:\d\d\)$/;

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
  test('the indexer goes down: the book follows the node, labelled; an order placed meanwhile is listed; the indexer book is back when it returns', async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await listExkcc(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await expect(asks(page)).toHaveCount(10);
    await openBell(page);
    await expect(banner(page)).toHaveCount(0);

    await indexerDown(mock);
    await expect(banner(page)).toBeVisible({ timeout: 60_000 });
    await expect(banner(page)).toContainText(/Standalone mode: the market from the node since \d\d:\d\d/);
    await closeBell(page);
    const label = page.getByTestId('book-standalone');
    await expect(label).toHaveText(/^Orders seen since \d\d:\d\d \(indexer unavailable; older orders are not shown\)$/);
    // the seeded orders were never in a block this page saw: not shown
    await expect(page.getByTestId('book-empty')).toBeVisible({ timeout: 20_000 });
    // the market data is built from what is seen, and says since when
    // (the stats and the chart were read from the indexer before: that is kept; the depth is the node's book alone)
    for (const area of ['stats', 'chart']) await expect(page.getByTestId(`${area}-standalone`)).toHaveText(KEPT);
    await expect(page.getByTestId('depth-standalone')).toContainText(/^Since \d\d:\d\d only \(standalone mode\)$/);
    await expect(page.getByTestId('live-indicator')).toHaveText('Standalone');
    // the activity log: it was below the fold, so it had read nothing from the indexer: only what was seen since then, nothing older
    await expect(page.getByTestId('activity-since')).toHaveText(/^since \d\d:\d\d$/);
    await expect(page.getByTestId('activity-nothing-before')).toBeVisible();

    // an ask placed now, with a real transaction: listed once the node holds it
    const placed = await placeOnChain(mock, 'carol', HIGH_ASK, 50n);
    await expect(asks(page)).toHaveCount(1, { timeout: 20_000 });
    await expect(page.getByTestId('book-ask-row')).toHaveCount(1);
    expect(placed.covenant_id).toMatch(/^[0-9a-f]{64}$/);
    // and its placement is a row of the activity log
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await expect(page.locator('[data-testid=activity-row][data-kind=placed]')).toHaveCount(1, { timeout: 20_000 });

    // the indexer is back: its book wins (the seeded levels and the new ask), the labels go
    await indexerDown(mock, false);
    await openBell(page);
    await expect(banner(page)).toHaveCount(0, { timeout: 60_000 });
    await closeBell(page);
    await expect(asks(page)).toHaveCount(11, { timeout: 20_000 });
    await expect(label).toHaveCount(0);
    await expect(page.getByTestId('chart-standalone')).toHaveCount(0);
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
    await expect(page.getByTestId('book-standalone')).toBeVisible({ timeout: 20_000 });
    // the chart keeps its history, marked; the last price and the figures are the ones read
    await expect(chart).toHaveAttribute('data-state', 'ready', { timeout: 30_000 });
    expect(Number(await chart.getAttribute('data-bars'))).toBeGreaterThanOrEqual(bars);
    await expect(page.getByTestId('chart-standalone')).toHaveText(KEPT);
    await expect(page.getByTestId('stats-standalone')).toHaveText(KEPT);
    await expect(page.getByTestId('stat-last')).toHaveText(last);
    await expect(page.getByTestId('stat-volume')).not.toContainText('—');
    // the token stays: facts from the indexer (marked), its ticket, the Trade tab
    await expect(page.getByTestId('token-kept-standalone')).toHaveText(/^Token facts from the indexer \(read until \d\d:\d\d\)$/);
    await expect(page.getByTestId('ticket-container')).toBeVisible();
    await expect(page.getByTestId('ticket-standalone-scale')).toHaveCount(0);
    await expect(page.getByTestId('nav-trade')).toBeVisible();
    // the book is the node's partial book: the seeded orders were never in a block this page saw
    await expect(page.getByTestId('book-empty')).toBeVisible({ timeout: 20_000 });
    // the activity rows of the indexer stay, marked
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('activity-since')).toHaveText(/^Indexer rows until \d\d:\d\d, then from your node \(since \d\d:\d\d\)$/);
    await expect(fills.first()).toBeVisible();
    // the list keeps the token and says so
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${id}`)).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId('token-list-standalone')).toContainText('kept');
  });

  test('trading in standalone mode: own orders from receipts, a market buy starts at the ask the node confirms, a fill becomes a trade', async ({ appPage: page, mock, wallet }) => {
    test.setTimeout(240_000);
    await fund(mock, 'alice', { tokens: 50n });
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
    // the seeded book is not shown; alice's own order is, resolved on the node from its receipt
    await expect(asks(page)).toHaveCount(1, { timeout: 30_000 });
    await expect(page.getByTestId('book-ask-row').first()).toContainText('0.05');

    // carol's ask placed meanwhile: the best ask of the standalone book (the seeded 0.0251 .. are not in it)
    const carol = await placeOnChain(mock, 'carol', HIGH_ASK, 50n);
    await expect(asks(page)).toHaveCount(2, { timeout: 20_000 });

    // a market buy is planned from that book after the node check, signed by the wallet and sent to the node
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '2' });
    await clickPlace(page);
    const buy = await placedWithoutIndexer(mock, await waitSubmitted(page));
    expect(onChainTerms(buy)[0]!.state.price).toBe(HIGH_ASK.toString());
    expect(wallet).not.toBeNull();

    // a matcher fills 10 of carol's ask with a real transaction: the fill is in the activity log ("since HH:MM"), and the book follows the order to its continuation
    await mock.post('/mock/take', { covenant_id: carol.covenant_id, amount: (10n * TOKEN).toString() });
    // (the log on screen, the pointer off it: a pointer moving in it holds new rows back)
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await page.mouse.move(1, 1);
    const fills = page.locator('[data-testid=activity-row][data-kind=fill]');
    await expect(fills).toHaveCount(1, { timeout: 30_000 });
    await expect(fills.first()).toContainText('0.03');
    await expect(page.getByTestId('activity-since')).toBeVisible();
    await expect(page.getByTestId('activity-live')).toHaveAttribute('data-mode', 'standalone');
    await expect(asks(page)).toHaveCount(2);
    await expect(page.getByTestId('book-ask-row').filter({ hasText: '40' })).toHaveCount(1);
  });

  test('a phone (390 px) shows the entry, the label and the partial book', async ({ appPage: page, mock }) => {
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
    await expect(page.getByTestId('book-standalone')).toBeVisible();
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
      await expect(page.getByTestId('book-standalone')).toHaveText(/^Orders seen since \d\d:\d\d \(standalone mode; older orders are not shown\)$/);
      await expect(page.getByTestId('status-dot')).toHaveAttribute('data-indexer-code', 'standalone');
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
      await expect(asks(page)).toHaveCount(10, { timeout: 20_000 });
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
