// The order ticket with a slow indexer (every answer 2-6 s, the node fast): typing never waits for the network. Right after the page opens the
// ticket says what it is still reading; once its environment is in, the order button is enabled within 1 s of the last keystroke even while
// background reads are in flight. A click then asks the wallet only after the re-check against the state at the click (ui/ticket/review-check.ts),
// which reads again the parts that are older than the click, were read before the feed went live, or may have been changed by a notice or by a
// submission of this page: with this indexer that is a few answers of 2-6 s. With a ready ticket and a quick indexer the wallet's sign request is
// there within 1 s of the click, also for an order right after the first one. A later order never picks the coins an earlier one has just spent
// (app/pending-spends.ts).
import type { Page } from '@playwright/test';
import { expect, test, type MockWalletHandle } from '../fixtures';
import { connectWallet, fund, listExkcc } from '../helpers/env';
import { waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false });

const BUDGET_MS = 1_000;
/** a click on a ticket whose parts were read before the feed went live, or before a notice / a submission of this page, waits for the re-check reads of the slow indexer (2-6 s each, a few in a row) */
const RECHECK_BUDGET_MS = 20_000;

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

/** The order button enabled for THIS order (the disclosure names its amount), polled; returns when. */
async function placeableFor(page: Page, amount: string): Promise<number> {
  const summary = new RegExp(`^Buy ${amount}(\\.0+)? EXKCC`);
  await expect
    .poll(async () => (await page.getByTestId('order-place').isEnabled()) && summary.test((await page.getByTestId('disc-summary').textContent().catch(() => '')) ?? ''), { intervals: [25], timeout: 30_000 })
    .toBe(true);
  return Date.now();
}

/** Clicks the order button; returns the milliseconds until the wallet received its sign request (approved at once by the mock wallet). */
async function clickToWallet(page: Page, wallet: MockWalletHandle, request: number): Promise<number> {
  const t0 = Date.now();
  await page.getByTestId('order-place').click();
  await wallet.waitForCalls(request, 30_000);
  return Date.now() - t0;
}

test.describe('order ticket with a slow indexer (2-6 s an answer)', () => {
  test('the order button within 1 s of the last keystroke, the wallet request after the re-check reads; a second and a third order right after', async ({ appPage: page, mock, wallet }) => {
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

      // typed at once, before any indexer answer: within 1 s the ticket either enables the order button or names what it is still reading (never a silent, disabled button)
      let typed = await typeOrder(page, '3', '0.0201');
      await expect(page.locator('[data-testid="order-need-loading"], [data-testid="order-place"]:enabled').first()).toBeVisible({ timeout: BUDGET_MS });
      expect(Date.now() - typed).toBeLessThan(BUDGET_MS);
      await expect(page.getByTestId('order-need-loading')).toHaveCount(0, { timeout: 30_000 });
      await placeableFor(page, '3');

      // the parts were read before the feed went live: the re-check reads them again from the slow indexer before the wallet is asked
      const clickToWallet1 = await clickToWallet(page, wallet!, 1);
      expect(clickToWallet1).toBeLessThan(RECHECK_BUDGET_MS);
      const txid = await waitSubmitted(page);

      // a live notice sets background reads going (2-6 s each): typing re-plans against what is on hand, at once; the click reaches the wallet
      // after the re-check reads of the slow indexer (the wallet is not asked before them)
      const ask = (await mock.ordersOf('maker', 'active')).find((o: { side: number }) => o.side === 1);
      await mock.fill(ask!.covenant_id, 1000n);
      await page.waitForTimeout(1_200); // past the feed's debounce: the reads are in flight now
      typed = await typeOrder(page, '4', '0.0202');
      const keyToPlace = (await placeableFor(page, '4')) - typed;
      expect(keyToPlace).toBeLessThan(BUDGET_MS);
      const clickToWallet2 = await clickToWallet(page, wallet!, 2);
      expect(clickToWallet2).toBeLessThan(RECHECK_BUDGET_MS);
      const txid2 = await waitSubmitted(page);
      expect(txid2).not.toBe(txid);

      // a third order right after: planned from the environment on hand, which still lists the coins the second one spent; they are left out
      typed = await typeOrder(page, '5', '0.0203');
      const keyToPlace2 = (await placeableFor(page, '5')) - typed;
      expect(keyToPlace2).toBeLessThan(BUDGET_MS);
      const clickToWallet3 = await clickToWallet(page, wallet!, 3);
      expect(clickToWallet3).toBeLessThan(RECHECK_BUDGET_MS);
      const txid3 = await waitSubmitted(page);
      expect(new Set([txid, txid2, txid3]).size).toBe(3);
      const submitted = await mock.submitted(false);
      expect(submitted.map((s) => s.txid)).toEqual(expect.arrayContaining([txid, txid2, txid3]));
      test.info().annotations.push({ type: 'ticket-latency', description: `keystroke->button ${keyToPlace} / ${keyToPlace2} ms; click->wallet request ${clickToWallet1} / ${clickToWallet2} / ${clickToWallet3} ms` });
    } finally {
      await mock.latency(0);
    }
  });

  test('a ready ticket: the wallet request within 1 s of the click, also for an order right after the first', async ({ appPage: page, mock, wallet }) => {
    test.setTimeout(120_000);
    await fund(mock, 'alice');
    await nodeOnItsOwnOrigin(page);
    await listExkcc(page);
    const tok = await mock.token();
    await mock.latency(50, { jitterMs: 50 }); // a realistic round trip to the indexer and the node
    try {
      await page.goto(`/#/market/${tok.covenant_id}`);
      await connectWallet(page);
      await page.getByTestId('order-side-buy').click();
      await typeOrder(page, '3', '0.0201');
      await expect(page.getByTestId('order-need-loading')).toHaveCount(0, { timeout: 30_000 });
      await placeableFor(page, '3');

      // checks, planning and the node clock are quick: the wallet is asked within 1 s of the click
      const first = await clickToWallet(page, wallet!, 1);
      expect(first).toBeLessThan(BUDGET_MS);
      const txid = await waitSubmitted(page);

      // an order right after: the first one's coins are left out, the re-check reads what the submission may have changed, still within 1 s
      await typeOrder(page, '4', '0.0202');
      await placeableFor(page, '4');
      const second = await clickToWallet(page, wallet!, 2);
      expect(second).toBeLessThan(BUDGET_MS);
      expect(await waitSubmitted(page)).not.toBe(txid);
      test.info().annotations.push({ type: 'ticket-latency', description: `click->wallet request ${first} / ${second} ms` });
    } finally {
      await mock.latency(0);
    }
  });
});
