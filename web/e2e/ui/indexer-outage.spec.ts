// The indexer goes away while a market is open (founder 2026-11-13): the page switches to the node by itself and says so ONCE, in the bell; the header's
// dot turns yellow and nothing else on the page warns. What the page read from the indexer is kept (the book, the wallet's holdings, the chart), so
// trading goes on: a limit sell of tokens the indexer listed, and a market buy against the resting orders it listed, each checked on the node before it is
// signed and sent to the node. The indexer back, the page returns to it, says so once more, and the dot is green again.
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { closeBell, expect, openBell, test, type MockClient } from '../fixtures';
import { TOKEN, fund, openMarket } from '../helpers/env';
import { clickPlace, fillFields, onChainTerms, pickType, waitReviewable, waitSubmitted, type Placed } from '../helpers/ticket';

test.use({ autoOpen: false });

/** where the screenshots go: `KOB_OUTAGE_SHOTS` of the environment, else a temporary folder */
const SHOTS = process.env.KOB_OUTAGE_SHOTS ?? join(tmpdir(), 'kob-outage-shots');
const KAS = 100_000_000n;

const asks = (page: Page) => page.getByTestId('book-asks').locator('.book-row:not(.book-blank)');
const dot = (page: Page) => page.getByTestId('status-dot');
const system = (page: Page) => page.getByTestId('notify-item-system');
/** the bell's dialog once it has faded in (for a screenshot) */
const settled = (page: Page) => expect.poll(() => page.getByTestId('notify-panel').evaluate((el) => getComputedStyle(el).opacity)).toBe('1');

/** `placedBy` while the indexer is down: the mock's own record of the submission and of the orders it created. */
async function placedWithoutIndexer(mock: MockClient, txid: string): Promise<Placed> {
  const sub = (await mock.submitted()).find((s) => s.txid === txid);
  expect(sub, `the mock node accepted ${txid}`).toBeTruthy();
  const views = await Promise.all(sub!.created.map((c) => mock.get(`/mock/order?id=${c.covenantId}`)));
  return { txid, submission: sub!, views };
}

/** The warnings the page used to scatter while the indexer was away: none of them is left. */
const GONE = ['book-stale', 'book-standalone', 'book-partial', 'book-error', 'stats-standalone', 'chart-standalone', 'depth-standalone', 'token-kept-standalone', 'activity-since', 'orders-indexer-down', 'banner-standalone'];

test('the indexer goes down: one notification, a yellow dot, no warnings; a limit sell and a market buy go through the node; one more notification and green when it is back', async ({ appPage: page, mock }) => {
  test.setTimeout(240_000);
  await fund(mock, 'alice', { tokens: 50n });
  const token = (await mock.token()).covenant_id as string;
  const book = await mock.get(`/v1/books/${token}?aggregate=false&depth=5`);
  const bestAsk = BigInt(book.asks[0].price);
  // the market's resting orders are read beside its book: what the page keeps for the node
  const resting = page.waitForResponse((r) => /\/v1\/orders\?/.test(r.url()) && r.url().includes(`token=${token}`) && r.url().includes('status=active'));
  await openMarket(page, mock);
  await resting;
  await expect(asks(page).first()).toBeVisible();
  await expect(dot(page)).toHaveAttribute('data-level', 'ok');
  // the ticket has read alice's tokens from the indexer
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '30', price: '0.05' });
  await waitReviewable(page);

  await mock.post('/mock/indexer', { down: true });
  // ONE notification, in the bell; the dot is yellow (trading through the node)
  await expect(dot(page)).toHaveAttribute('data-trading', 'node', { timeout: 30_000 });
  await expect(dot(page)).toHaveAttribute('data-level', 'warn');
  await expect(page.getByTestId('notify-unread')).toHaveText('1');
  await openBell(page);
  await expect(system(page)).toHaveCount(1);
  await expect(system(page)).toHaveAttribute('data-kind', 'node');
  await expect(system(page)).toContainText('Trading through your node');
  // the current status is one calm line
  await expect(page.getByTestId('notify-status').locator('.banner-warn, .banner-error').filter({ hasText: /indexer/i })).toHaveCount(0);
  await expect(page.getByTestId('banner-standalone')).toHaveClass(/banner-info/);
  await settled(page);
  await page.screenshot({ path: `${SHOTS}/after-bell-1280.png` });
  await closeBell(page);
  // no warning is left on the page; the book is the one read from the indexer, kept (and asked on the node)
  for (const id of GONE) await expect(page.getByTestId(id), id).toHaveCount(0);
  await expect(page.locator('.banner-warn:visible, .banner-error:visible')).toHaveCount(0);
  await expect(page.getByTestId('live-indicator')).toHaveText('Via node');
  await expect.poll(() => asks(page).count()).toBeGreaterThanOrEqual(8);
  await page.getByTestId('order-ticket').scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${SHOTS}/after-market-1280.png` });
  // the phone
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByTestId('order-ticket').scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBeLessThanOrEqual(0);
  await page.screenshot({ path: `${SHOTS}/after-market-390.png` });
  await page.evaluate(() => window.scrollTo(0, 0));
  await openBell(page);
  await settled(page);
  await page.screenshot({ path: `${SHOTS}/after-bell-390.png` });
  await closeBell(page);
  await page.setViewportSize({ width: 1280, height: 720 });

  // a limit sell of the tokens the indexer listed, re-read on the node: planned, signed and sent
  await fillFields(page, { amount: '30', price: '0.05' });
  await clickPlace(page);
  const sell = await placedWithoutIndexer(mock, await waitSubmitted(page));
  expect(onChainTerms(sell)[0]!.state.price).toBe('5000000');
  expect(onChainTerms(sell)[0]!.custody).toEqual([(30n * TOKEN).toString()]);

  // a market buy against the kept asks: it starts at the best ask the node confirms
  await pickType(page, 'market', 'buy');
  await fillFields(page, { amount: '2' });
  await clickPlace(page);
  const buy = await placedWithoutIndexer(mock, await waitSubmitted(page));
  expect(onChainTerms(buy)[0]!.state.price).toBe(bestAsk.toString());
  expect(BigInt(onChainTerms(buy)[0]!.locked)).toBeGreaterThan(0n);
  expect(BigInt(onChainTerms(buy)[0]!.locked)).toBeLessThan(10n * KAS);

  // the indexer is back: one more notification, the dot green
  await mock.post('/mock/indexer', { down: false });
  await expect(dot(page)).toHaveAttribute('data-level', 'ok', { timeout: 40_000 });
  await expect(dot(page)).toHaveAttribute('data-trading', 'indexer');
  await openBell(page);
  await expect(system(page)).toHaveCount(2);
  await expect(system(page).first()).toHaveAttribute('data-kind', 'indexer');
  await expect(system(page).first()).toContainText('Back on the indexer');
  await expect(page.getByTestId('banner-standalone')).toHaveCount(0);
  await expect(page.getByTestId('banner-switching')).toHaveCount(0);
  await closeBell(page);
  for (const id of GONE) await expect(page.getByTestId(id), id).toHaveCount(0);
  await expect(page.getByTestId('live-indicator')).not.toHaveText('Via node');
});

test('the indexer answers but falls behind the chain: one notification, a yellow dot, trading through the node; one more and green once it has caught up', async ({ appPage: page, mock }) => {
  test.setTimeout(240_000);
  await fund(mock, 'alice', { tokens: 50n });
  const resting = page.waitForResponse((r) => /\/v1\/orders\?/.test(r.url()) && r.url().includes('status=active'));
  await openMarket(page, mock);
  await resting;
  await expect(asks(page).first()).toBeVisible();
  await expect(dot(page)).toHaveAttribute('data-level', 'ok');
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '30', price: '0.05' });
  await waitReviewable(page);

  // five minutes behind (3000 DAA, 10 per second): over the two-minute bound
  await mock.post('/mock/health', { state: 'following', lag_daa: 3_000 });
  await expect(dot(page)).toHaveAttribute('data-trading', 'node', { timeout: 40_000 });
  await expect(dot(page)).toHaveAttribute('data-level', 'warn');
  await expect(page.getByTestId('notify-unread')).toHaveText('1');
  await openBell(page);
  await expect(system(page)).toHaveCount(1);
  await expect(system(page)).toHaveAttribute('data-kind', 'node');
  await expect(system(page)).toContainText('Trading through your node');
  await expect(system(page)).toContainText('behind the chain');
  await closeBell(page);
  await expect(page.getByTestId('live-indicator')).toHaveText('Via node');
  await expect.poll(() => asks(page).count()).toBeGreaterThanOrEqual(8);

  // trading works through the node on the kept data
  await fillFields(page, { amount: '30', price: '0.05' });
  await clickPlace(page);
  const sell = await placedWithoutIndexer(mock, await waitSubmitted(page));
  expect(onChainTerms(sell)[0]!.state.price).toBe('5000000');

  // a minute behind is still not caught up: it stays on the node (no flapping)
  await mock.post('/mock/health', { state: 'following', lag_daa: 600 });
  await page.waitForTimeout(12_000);
  await expect(dot(page)).toHaveAttribute('data-trading', 'node');

  // caught up: one more notification, the dot green
  await mock.post('/mock/health', { state: 'following', lag_daa: 0 });
  await expect(dot(page)).toHaveAttribute('data-level', 'ok', { timeout: 40_000 });
  await expect(dot(page)).toHaveAttribute('data-trading', 'indexer');
  await openBell(page);
  await expect(system(page)).toHaveCount(2);
  await expect(system(page).first()).toHaveAttribute('data-kind', 'indexer');
  await expect(system(page).first()).toContainText('Back on the indexer');
  await closeBell(page);
  await expect(page.getByTestId('live-indicator')).not.toHaveText('Via node');
});
