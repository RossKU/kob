// The order flow of the ticket: ONE button, the wallet's own approval, the result inline. There is no confirmation screen for orders: a click
// plans again, runs the silent checks, asks the wallet once and sends. An auction (market order) is planned with the node clock at the click; a
// signature that comes back more than 20 DAA (2 s) after the auction start is never sent (wallet/sign.ts `auctionSubmitBy`): the ticket plans
// again with the current clock and asks the wallet once more, and stops with `clock-moved` when that is too late as well.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, mockClock, openMarket } from '../helpers/env';
import { clickPlace, expectStopped, expectWalletSigned, fillFields, pickType, placedBy, readSendStatus, waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false });

/** The mock DAA advances 10 per second; the ticket allows 20 DAA between the auction start it planned and the signature. */
const SLOW_WALLET_MS = 6_000;
/** a start planned at the click is a 30 DAA lead ahead of the node clock (MARKET_ACTIVATION_DAA) */
const REPLAN_GAP_DAA = 60;

/** Remembers whether a confirmation screen was ever in the page (it must never open for an order). */
async function watchConfirmScreen(page: Page): Promise<() => Promise<boolean>> {
  await page.evaluate(() => {
    const w = window as unknown as { __sawConfirm: boolean };
    w.__sawConfirm = false;
    const look = () => {
      if (document.querySelector('[data-testid="confirm-screen"]')) w.__sawConfirm = true;
    };
    new MutationObserver(look).observe(document.body, { childList: true, subtree: true });
    look();
  });
  return () => page.evaluate(() => (window as unknown as { __sawConfirm: boolean }).__sawConfirm);
}

test.describe('order flow: one button, the wallet approves', () => {
  test('a plain order: one click, exactly one wallet request, submitted then confirmed, no confirmation screen', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245' });
    const sawConfirm = await watchConfirmScreen(page);

    await clickPlace(page);
    await wallet!.waitForCalls(1);
    const txid = await waitSubmitted(page);
    const status = await readSendStatus(page);
    expect(status.phase).toBe('submitted');
    expect(status.txid).toBe(txid);
    await expect(page.getByTestId('tx-id')).toHaveText(/\w/); // the id links to the explorer

    // exactly one request, for exactly the transaction the node accepted
    const placed = await placedBy(mock, txid);
    await expectWalletSigned(wallet!, placed, { requests: 1 });
    expect(placed.submission.created).toHaveLength(1);
    expect(await sawConfirm(), 'no confirmation screen was ever shown').toBe(false);
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  });

  test.describe('an auction planned at the click', () => {
    /** A small market BUY (below 100 KAS: no start acknowledgement). */
    async function typeMarketBuy(page: Page): Promise<void> {
      await pickType(page, 'market', 'buy');
      await fillFields(page, { amount: '1' });
    }

    test('a slow first approval: the ticket plans again with the current clock and the second request is the one sent', async ({ appPage: page, mock, wallet }) => {
      test.setTimeout(90_000);
      await fund(mock);
      await openMarket(page, mock);
      await typeMarketBuy(page);
      await wallet!.configure({ delayMs: SLOW_WALLET_MS }); // longer than the 2 s margin: the first signature arrives too late
      const before = (await mock.submitted(false)).length;

      await clickPlace(page);
      await wallet!.waitForCalls(1);
      const daaAtFirstRequest = (await mockClock(mock)).daa;
      await wallet!.configure({ delayMs: 0 }); // the second request is answered at once

      const txid = await waitSubmitted(page);
      const calls = await wallet!.calls();
      expect(calls, 'the late signature was thrown away and the wallet asked once more').toHaveLength(2);
      expect(calls[0]!.status).toBe('signed'); // the wallet did sign the first one; the app just never sent it
      const submitted = (await mock.submitted(false)).slice(before);
      expect(submitted.map((s) => s.txid), 'exactly one transaction reached the node').toEqual([txid]);

      // the accepted order starts from the clock of the second plan, not from the stale clock of the first
      const placed = await placedBy(mock, txid);
      await expectWalletSigned(wallet!, placed, { requests: 2 });
      const activeFrom = Number(placed.views[0].state.state.activeFrom);
      expect(activeFrom, 'activeFrom follows the clock at the second plan').toBeGreaterThanOrEqual(daaAtFirstRequest + REPLAN_GAP_DAA);
      await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    });

    test('both approvals too slow: nothing is sent and the ticket says the price moved', async ({ appPage: page, mock, wallet }) => {
      test.setTimeout(90_000);
      await fund(mock);
      await openMarket(page, mock);
      await typeMarketBuy(page);
      await wallet!.configure({ delayMs: SLOW_WALLET_MS });
      const before = (await mock.submitted(false)).length;

      await clickPlace(page);
      const stopped = await expectStopped(page, 'failed', { code: 'clock-moved' });
      expect(stopped.text).toMatch(/price moved/i);
      expect(stopped.text).toMatch(/nothing was sent/i);
      await expect(page.getByTestId('order-status').getByTestId('send-error')).toHaveAttribute('data-code', 'clock-moved');

      expect(await wallet!.calls(), 'one retry, then it stops').toHaveLength(2);
      expect((await mock.submitted(false)).length, 'nothing reached the node').toBe(before);
      // the ticket is usable again
      await expect(page.getByTestId('order-place')).toBeEnabled();
    });
  });
});

test.describe('no acknowledgement boxes: what the ticket cannot check stops the order, what it can check is accepted', () => {
  test('a market sell needs no box: the same indexer is the only reference, the start is accepted', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'market', 'sell');
    await fillFields(page, { amount: '1' });
    await expect(page.getByTestId('order-place')).toBeEnabled({ timeout: 15_000 });
    await expect(page.getByTestId('order-ticket').locator('input[type="checkbox"][data-testid$="-ack"]')).toHaveCount(0);
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await wallet!.calls()).toHaveLength(1);
  });

  test.describe('a further indexer whose book is far from ours', () => {
    // the further indexer is the mock itself behind another origin, its asks at half the price: a market buy starts 100 % above it
    test.use({ appConfig: { extraIndexerUrls: ['http://idx2.invalid'] } });

    test('a market buy is a hard error (button disabled, one red line), nothing reaches the wallet', async ({ appPage: page, mock, wallet }) => {
      await page.route('http://idx2.invalid/**', async (route) => {
        const url = route.request().url().replace('http://idx2.invalid', mock.url);
        const res = await route.fetch({ url });
        const body = await res.json();
        if (/\/v1\/books\//.test(url) && Array.isArray(body.asks)) for (const a of body.asks) a.price = String(BigInt(a.price) / 2n);
        await route.fulfill({ response: res, json: body });
      });
      await fund(mock);
      await openMarket(page, mock);
      await pickType(page, 'market', 'buy');
      await fillFields(page, { amount: '1' });
      const issue = page.getByTestId('order-issue-MARKET_START_VS_INDEXER');
      await expect(issue).toBeVisible({ timeout: 20_000 });
      await expect(issue).toHaveAttribute('data-severity', 'error');
      await expect(issue).toContainText('nothing can be sent');
      await expect(page.getByTestId('order-place')).toBeDisabled();
      await expect(page.getByTestId('order-ticket').locator('input[type="checkbox"][data-testid$="-ack"]')).toHaveCount(0);
      expect(await wallet!.calls()).toHaveLength(0);
    });
  });

  test('guard data that cannot be read is a hard error that clears by itself once the data is read again', async ({ appPage: page, mock }) => {
    test.setTimeout(90_000);
    await fund(mock);
    // the wallet's own orders (self-trade prevention) cannot be listed
    const ownOrders = (u: URL) => u.pathname.endsWith('/v1/orders') && u.searchParams.has('maker');
    await page.route(ownOrders, (route) => route.fulfill({ status: 503, json: { error: { code: 'unavailable', message: 'down' } } }));
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245' });
    const issue = page.getByTestId('order-issue-GUARDS_UNAVAILABLE');
    await expect(issue).toBeVisible({ timeout: 20_000 });
    await expect(issue).toHaveText(/Could not read the token's guard data; try again in a moment\./);
    await expect(page.getByTestId('order-place')).toBeDisabled();
    await expect(page.getByTestId('order-ticket').locator('input[type="checkbox"][data-testid$="-ack"]')).toHaveCount(0);
    // the indexer answers again: the ticket's background refresh reads it and the error goes
    await page.unroute(ownOrders);
    await expect(issue).toHaveCount(0, { timeout: 45_000 });
    await expect(page.getByTestId('order-place')).toBeEnabled();
  });
});
