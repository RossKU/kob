// Indexer load of one open market page: a live fill refreshes every resource coalesced and the chart reads only its newest bars (`from=`), a
// hidden tab neither refetches nor polls and catches up once shown, and Review reuses an environment read a moment ago instead of reading
// everything again (an older one opens the confirmation at once and is re-checked before signing; ticket-latency.spec.ts times it).
import type { Page, Request } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';
import { fund, listExkcc, openMarket } from '../helpers/env';
import { fillFields, settlePlan, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

const MARKET_READS = /\/v1\/(books|trades|stats|depth|candles)\//;

/** Every request whose URL matches `re`, from now on. */
function record(page: Page, re: RegExp): Request[] {
  const seen: Request[] = [];
  page.on('request', (r) => {
    if (re.test(r.url())) seen.push(r);
  });
  return seen;
}

/** Makes the page hidden / shown as a background tab is (document.visibilityState + the visibilitychange event). */
async function setHidden(page: Page, hidden: boolean): Promise<void> {
  await page.evaluate((h) => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => (h ? 'hidden' : 'visible') });
    document.dispatchEvent(new Event('visibilitychange'));
  }, hidden);
}

async function openWithHistory(page: Page, mock: import('../fixtures').MockClient): Promise<string> {
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await listExkcc(page);
  const tok = await mock.token();
  await page.goto(`/#/market/${tok.covenant_id}`);
  await expect(page.getByTestId('live-indicator')).toHaveText('Live');
  await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-state', 'ready');
  return tok.covenant_id as string;
}

test.describe('indexer load per viewer', () => {
  test('a live fill reads only the newest candles (from=) and never the full series again', async ({ appPage: page, mock }) => {
    await openWithHistory(page, mock);
    const candles = record(page, /\/v1\/candles\//);
    const trades = (await page.getByTestId('stat-trades').textContent()) ?? '';
    const ask = (await mock.ordersOf('maker', 'active')).find((o: { side: number }) => o.side === 1);
    await mock.fill(ask!.covenant_id);
    await expect(page.getByTestId('stat-trades')).not.toHaveText(trades);
    await expect.poll(() => candles.length).toBeGreaterThan(0);
    for (const r of candles) expect(new URL(r.url()).searchParams.get('from'), r.url()).not.toBeNull();
    // the chart still draws the full series
    expect(Number(await page.getByTestId('price-chart').getAttribute('data-bars'))).toBeGreaterThan(50);
    // a resync (events may have been missed) reads the full series once
    candles.length = 0;
    await mock.resync();
    await expect.poll(() => candles.some((r) => new URL(r.url()).searchParams.get('from') === null)).toBe(true);
  });

  test('a hidden tab does not refetch on live notices and catches up once when shown', async ({ appPage: page, mock }) => {
    await openWithHistory(page, mock);
    const trades = (await page.getByTestId('stat-trades').textContent()) ?? '';
    await setHidden(page, true);
    const reads = record(page, MARKET_READS);
    const ask = (await mock.ordersOf('maker', 'active')).find((o: { side: number }) => o.side === 1);
    await mock.fill(ask!.covenant_id);
    // the notice arrives within the debounce (1 s): give it well over that, nothing may be read
    await page.waitForTimeout(3_000);
    expect(reads.map((r) => r.url())).toEqual([]);
    await expect(page.getByTestId('stat-trades')).toHaveText(trades);
    await setHidden(page, false);
    await expect(page.getByTestId('stat-trades')).not.toHaveText(trades);
    expect(reads.length).toBeGreaterThan(0);
  });

  test('Review reuses an environment read a moment ago; an older one opens at once and is re-checked before signing', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await fillFields(page, { amount: '3', price: '0.03' });
    await waitReviewable(page);

    // the ticket's own book read (per order, aggregate=false) marks an environment build
    const envReads = record(page, /\/v1\/books\/[0-9a-f]+\?.*aggregate=false/);
    await mock.resync(); // the feed says "reload": the environment is read again
    await expect.poll(() => envReads.length).toBeGreaterThan(0);
    await settlePlan(page);
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('aria-busy', 'false');
    envReads.length = 0;
    const t0 = Date.now();
    await page.getByTestId(TESTID.orderReview).click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    const reused = Date.now() - t0;
    expect(envReads, 'Review must not read the environment again').toHaveLength(0);
    // that environment is the state at the click: no re-check holds the signature
    await expect(page.getByTestId('confirm-checking-list').locator('[data-code="recheck-running"]')).toHaveCount(0);
    await page.getByTestId('confirm-cancel').click();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);

    // a slow indexer and node (400 ms a request), an environment older than the reuse window
    await mock.latency(400);
    try {
      // the feed was live all along and sent no notice since: it still is the state at the click, nothing is read again
      await waitReviewable(page);
      await page.waitForTimeout(3_500);
      envReads.length = 0;
      await page.getByTestId(TESTID.orderReview).click();
      await expect(page.getByTestId('confirm-screen')).toBeVisible();
      await expect(page.getByTestId('confirm-checking-list').locator('[data-code="recheck-running"]')).toHaveCount(0);
      expect(envReads).toHaveLength(0);
      await page.getByTestId('confirm-cancel').click();
      await expect(page.getByTestId('confirm-screen')).toHaveCount(0);

      // the book moved since (a notice, its read still on its way: an indexer at 3 s an answer): the confirmation still opens at once, and the
      // signature waits for the re-check
      const ask = (await mock.ordersOf('maker', 'active')).find((o: { side: number }) => o.side === 1);
      await mock.latency(3_000, { scope: 'indexer' });
      envReads.length = 0;
      await mock.fill(ask!.covenant_id, 1000n);
      await page.waitForTimeout(1_200);
      const t1 = Date.now();
      await page.getByTestId(TESTID.orderReview).click();
      await expect(page.getByTestId('confirm-screen')).toBeVisible();
      const opened = Date.now() - t1;
      await expect(page.getByTestId('confirm-checking-list').locator('[data-code="recheck-running"]')).toBeVisible();
      await page.getByTestId('confirm-ack').check();
      await expect(page.getByTestId('confirm-sign')).toBeEnabled({ timeout: 15_000 });
      expect(envReads.length).toBeGreaterThan(0);
      test.info().annotations.push({ type: 'review-latency', description: `reused ${reused} ms; from an older environment at 400 ms/request ${opened} ms` });
    } finally {
      await mock.latency(0);
    }
  });
});
