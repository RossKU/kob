// The order ticket with a slow indexer (every answer 2-6 s, the node fast): typing never waits for the network. Right after the page opens the
// ticket says what it is still reading; once its environment is in, Review is enabled within 1 s of the last keystroke even while background reads
// are in flight, the confirmation opens within 1 s of the click, and signing waits for the re-check against the state at the click
// (ui/ticket/review-check.ts). A second order right after the first never picks the coins the first has just spent (app/pending-spends.ts).
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { connectWallet, fund, listExkcc } from '../helpers/env';
import { acknowledgeAndSign } from '../helpers/ticket';

test.use({ autoOpen: false });

const BUDGET_MS = 1_000;

/** The node on an origin of its own, as in production (wRPC): its calls never queue behind the slow indexer's in the browser's connection pool. */
async function nodeOnItsOwnOrigin(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const c = (window as unknown as { __KOB_CONFIG__: { nodeUrl: string } }).__KOB_CONFIG__;
    c.nodeUrl = c.nodeUrl.replace('127.0.0.1', 'localhost');
  });
}

/** Types amount and price; returns the time of the last keystroke. */
async function typeOrder(page: Page, amount: string, price: string): Promise<number> {
  await page.getByTestId('order-amount').fill(amount);
  await page.getByTestId('order-price').fill(price);
  return Date.now();
}

/** Review enabled for THIS order (the disclosure names its amount), polled; returns when. */
async function reviewableFor(page: Page, amount: string): Promise<number> {
  const summary = new RegExp(`^Buy ${amount}(\\.0+)? EXKCC`);
  await expect
    .poll(async () => (await page.getByTestId('order-review').isEnabled()) && summary.test((await page.getByTestId('disc-summary').textContent().catch(() => '')) ?? ''), { intervals: [25], timeout: 30_000 })
    .toBe(true);
  return Date.now();
}

/** Clicks Review; returns the milliseconds until the confirmation is visible. */
async function reviewToConfirm(page: Page): Promise<number> {
  const t0 = Date.now();
  await page.getByTestId('order-review').click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
  return Date.now() - t0;
}

test.describe('order ticket with a slow indexer (2-6 s an answer)', () => {
  test('Review within 1 s of the last keystroke, the confirmation within 1 s of the click, signing after the re-check; a second order right after', async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await fund(mock, 'alice');
    await nodeOnItsOwnOrigin(page);
    await listExkcc(page);
    const tok = await mock.token();
    await mock.latency(2_000, { jitterMs: 4_000, scope: 'indexer' });
    try {
      await page.goto(`/#/market/${tok.covenant_id}`);
      await connectWallet(page);
      await page.getByTestId('order-side-buy').click();

      // typed at once, before any indexer answer: within 1 s the ticket either lets review or names what it is still reading (never a silent, disabled button)
      let typed = await typeOrder(page, '3', '0.0201');
      await expect(page.locator('[data-testid="order-need-loading"], [data-testid="order-review"]:enabled').first()).toBeVisible({ timeout: BUDGET_MS });
      expect(Date.now() - typed).toBeLessThan(BUDGET_MS);
      await expect(page.getByTestId('order-need-loading')).toHaveCount(0, { timeout: 30_000 });

      // the environment is in; a live notice sets background reads going (2-6 s each): typing re-plans against what is on hand, at once
      const ask = (await mock.ordersOf('maker', 'active')).find((o: { side: number }) => o.side === 1);
      await mock.fill(ask!.covenant_id, 1000n);
      await page.waitForTimeout(1_200); // past the feed's debounce: the reads are in flight now
      typed = await typeOrder(page, '4', '0.0202');
      const keyToReview = (await reviewableFor(page, '4')) - typed;
      expect(keyToReview).toBeLessThan(BUDGET_MS);

      // the confirmation at once; the re-check against the state at the click shows and holds the signature until it is done
      const clickToConfirm = await reviewToConfirm(page);
      expect(clickToConfirm).toBeLessThan(BUDGET_MS);
      await expect(page.getByTestId('confirm-checking-list').locator('[data-code="recheck-running"]')).toBeVisible();
      await expect(page.getByTestId('confirm-sign')).toBeDisabled();
      const txid = await acknowledgeAndSign(page);
      await page.getByTestId('confirm-close').click();
      await expect(page.getByTestId('confirm-screen')).toHaveCount(0);

      // a second order right after: planned from the environment on hand, which still lists the coins the first one spent; they are left out
      typed = await typeOrder(page, '5', '0.0203');
      const keyToReview2 = (await reviewableFor(page, '5')) - typed;
      expect(keyToReview2).toBeLessThan(BUDGET_MS);
      const clickToConfirm2 = await reviewToConfirm(page);
      expect(clickToConfirm2).toBeLessThan(BUDGET_MS);
      const txid2 = await acknowledgeAndSign(page);
      expect(txid2).not.toBe(txid);
      const submitted = await mock.submitted(false);
      expect(submitted.map((s) => s.txid)).toEqual(expect.arrayContaining([txid, txid2]));
      test.info().annotations.push({ type: 'ticket-latency', description: `keystroke->review ${keyToReview} / ${keyToReview2} ms; click->confirm ${clickToConfirm} / ${clickToConfirm2} ms` });
    } finally {
      await mock.latency(0);
    }
  });
});
