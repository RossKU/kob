// The order ticket with a slow indexer (every answer 2-6 s, the node fast): typing never waits for the network. Right after the page opens the
// ticket says what it is still reading; once its environment is in, the order button is enabled within 1 s of the last keystroke even while
// background reads are in flight. A click then opens the final confirmation only after the re-check against the state at the click
// (ui/ticket/review-check.ts), which reads again the parts that are older than the click, were read before the feed went live, or may have been
// changed by a notice or by a submission of this page: with this indexer that is a few answers of 2-6 s; Continue in the popup asks the wallet at once.
// With a ready ticket and a quick indexer the confirmation is there within 1 s of the click, also for an order right after the first one. A later order never picks the coins an earlier one has just spent
// (app/pending-spends.ts).
import type { Page } from '@playwright/test';
import { expect, test, type MockWalletHandle } from '../fixtures';
import { connectWallet, fund, listExkcc } from '../helpers/env';
import { waitSubmitted } from '../helpers/ticket';

/** milliseconds from a click to the final confirmation popup, and then from Continue to the wallet's sign request */
interface ClickTimes {
  toPopup: number;
  toWallet: number;
}

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

/** The order button enabled for THIS order (the amount box holds it and its plan is not pending; background reads may still be in flight), polled; returns when. */
async function placeableFor(page: Page, amount: string): Promise<number> {
  await expect
    .poll(
      async () =>
        (await page.getByTestId('order-amount').inputValue()) === amount &&
        (await page.getByTestId('order-pending').count()) === 0 &&
        (await page.getByTestId('order-place').isEnabled()),
      { intervals: [25], timeout: 30_000 },
    )
    .toBe(true);
  return Date.now();
}

/**
 * Clicks the order button; returns the milliseconds until the final confirmation popup is there, and from its Continue until the wallet received its
 * sign request (approved at once by the mock wallet). The wallet is not asked before Continue.
 */
async function clickToWallet(page: Page, wallet: MockWalletHandle, request: number): Promise<ClickTimes> {
  const t0 = Date.now();
  await page.getByTestId('order-place').click();
  await expect(page.getByTestId('confirm-order')).toBeVisible({ timeout: 30_000 });
  const t1 = Date.now();
  expect(await wallet.calls(), 'the wallet is asked only after Continue').toHaveLength(request - 1);
  await page.getByTestId('confirm-order-ok').click();
  await wallet.waitForCalls(request, 30_000);
  return { toPopup: t1 - t0, toWallet: Date.now() - t1 };
}

test.describe('order ticket with a slow indexer (2-6 s an answer)', () => {
  test('the order button within 1 s of the last keystroke, the confirmation after the re-check reads; a second and a third order right after', async ({ appPage: page, mock, wallet }) => {
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
      expect(clickToWallet1.toPopup).toBeLessThan(RECHECK_BUDGET_MS);
      expect(clickToWallet1.toWallet).toBeLessThan(BUDGET_MS);
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
      expect(clickToWallet2.toPopup).toBeLessThan(RECHECK_BUDGET_MS);
      expect(clickToWallet2.toWallet).toBeLessThan(BUDGET_MS);
      const txid2 = await waitSubmitted(page);
      expect(txid2).not.toBe(txid);

      // a third order right after: planned from the environment on hand, which still lists the coins the second one spent; they are left out
      typed = await typeOrder(page, '5', '0.0203');
      const keyToPlace2 = (await placeableFor(page, '5')) - typed;
      expect(keyToPlace2).toBeLessThan(BUDGET_MS);
      const clickToWallet3 = await clickToWallet(page, wallet!, 3);
      expect(clickToWallet3.toPopup).toBeLessThan(RECHECK_BUDGET_MS);
      expect(clickToWallet3.toWallet).toBeLessThan(BUDGET_MS);
      const txid3 = await waitSubmitted(page);
      expect(new Set([txid, txid2, txid3]).size).toBe(3);
      const submitted = await mock.submitted(false);
      expect(submitted.map((s) => s.txid)).toEqual(expect.arrayContaining([txid, txid2, txid3]));
      test.info().annotations.push({ type: 'ticket-latency', description: `keystroke->button ${keyToPlace} / ${keyToPlace2} ms; click->popup ${clickToWallet1.toPopup} / ${clickToWallet2.toPopup} / ${clickToWallet3.toPopup} ms, continue->wallet request ${clickToWallet1.toWallet} / ${clickToWallet2.toWallet} / ${clickToWallet3.toWallet} ms` });
    } finally {
      await mock.latency(0);
    }
  });

  test('a ready ticket: the confirmation within 1 s of the click and the wallet request within 1 s of Continue, also for an order right after the first', async ({ appPage: page, mock, wallet }) => {
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

      // checks, planning and the node clock are quick: the confirmation is there within 1 s of the click, the wallet is asked within 1 s of Continue
      const first = await clickToWallet(page, wallet!, 1);
      expect(first.toPopup).toBeLessThan(BUDGET_MS);
      expect(first.toWallet).toBeLessThan(BUDGET_MS);
      const txid = await waitSubmitted(page);

      // an order right after: the first one's coins are left out, the re-check reads what the submission may have changed, still within 1 s
      await typeOrder(page, '4', '0.0202');
      await placeableFor(page, '4');
      const second = await clickToWallet(page, wallet!, 2);
      expect(second.toPopup).toBeLessThan(BUDGET_MS);
      expect(second.toWallet).toBeLessThan(BUDGET_MS);
      expect(await waitSubmitted(page)).not.toBe(txid);
      test.info().annotations.push({ type: 'ticket-latency', description: `click->popup ${first.toPopup} / ${second.toPopup} ms, continue->wallet request ${first.toWallet} / ${second.toWallet} ms` });
    } finally {
      await mock.latency(0);
    }
  });
});
