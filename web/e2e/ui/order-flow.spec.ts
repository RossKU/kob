// The order flow of the ticket: ONE button, the final confirmation (an app popup for external wallets), the wallet's own approval, the result inline.
// There is no issuance-style confirmation screen for orders: a click plans again, runs the silent checks, opens the final confirmation, asks the wallet
// once and sends. An auction (market order) is planned with the node clock at the click; a signature that comes back more than 20 DAA (2 s) after the
// auction start is never sent (wallet/sign.ts `auctionSubmitBy`): the send re-times the same order with a later start (the lead grows with each
// attempt and leaves at least the signing time the late attempt took) and asks the wallet again, up to three times, and stops with `clock-moved`
// when every attempt was too late. A wallet's signing time is remembered: its next auction starts late enough for it (ui/confirm/signing-time.ts).
import type { Page } from '@playwright/test';
import { expect, test, type MockWalletHandle } from '../fixtures';
import { fund, mockClock, openMarket } from '../helpers/env';
import { clickPlace, expectStopped, expectWalletSigned, fillFields, pickType, placedBy, readSendStatus, settlePlan, waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false });

/** The mock DAA advances 10 per second; the ticket allows 20 DAA between the auction start it planned and the signature. */
const SLOW_WALLET_MS = 6_000;
/**
 * A wallet that keeps getting slower: a re-timed attempt leaves the last signing time plus 3 s (RETIME_LEAD_DAA) on top of the 5 s window
 * (ui/confirm/signing-time.ts `retimeLead`), so a request 12 s slower than the one before is signed too late again.
 */
const EVER_SLOWER_MS = [10_000, 22_000, 34_000, 46_000];
/** a wallet that always takes 15 s: longer than the 5 s window of a first attempt, shorter than what a re-timing then leaves */
const STEADY_SLOW_MS = 15_000;
/** a start planned at the click is a 30 DAA lead ahead of the node clock (MARKET_ACTIVATION_DAA) */
const REPLAN_GAP_DAA = 60;

/** Waits until the wallet has answered its `n`-th request (a slow wallet outlasts the 30 s waits of the ticket helpers). */
async function walletAnswered(wallet: MockWalletHandle, n: number, timeoutMs = 90_000): Promise<void> {
  const t0 = Date.now();
  for (;;) {
    const calls = await wallet.calls();
    if (calls.length >= n && calls[n - 1]!.status !== 'pending') return;
    if (Date.now() - t0 > timeoutMs) throw new Error(`the wallet did not answer request ${n} within ${timeoutMs} ms`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

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

    test('a wallet that always takes 15 s: the re-timing leaves its signing time, the second request is sent, and the next order fits at once', async ({ appPage: page, mock, wallet }) => {
      test.setTimeout(150_000);
      await fund(mock);
      await openMarket(page, mock);
      await typeMarketBuy(page);
      await wallet!.configure({ delayMs: STEADY_SLOW_MS });
      const before = (await mock.submitted(false)).length;

      await clickPlace(page);
      await walletAnswered(wallet!, 2);
      const first = await waitSubmitted(page);
      expect(await wallet!.calls(), 'the first request was too late for the 5 s window, the re-timed one was not').toHaveLength(2);

      // the wallet's signing time is remembered: the next auction is planned with a start that leaves it, and the wallet is asked once
      await typeMarketBuy(page);
      await clickPlace(page);
      await walletAnswered(wallet!, 3);
      const second = await waitSubmitted(page);
      expect(second).not.toBe(first);
      expect(await wallet!.calls(), 'the next order was sent on its first request').toHaveLength(3);
      expect((await mock.submitted(false)).slice(before).map((s) => s.txid)).toEqual([first, second]);
    });

    test('every approval too slow: three re-timings, then nothing is sent and the ticket says the price moved', async ({ appPage: page, mock, wallet }) => {
      test.setTimeout(240_000);
      await fund(mock);
      await openMarket(page, mock);
      await typeMarketBuy(page);
      await wallet!.configure({ delayMs: EVER_SLOWER_MS[0]! });
      const before = (await mock.submitted(false)).length;

      await clickPlace(page);
      // the wallet reads its delay when a request arrives: each next request is made slower than what the re-timing before it left
      for (let n = 1; n < EVER_SLOWER_MS.length; n++) {
        await wallet!.waitForCalls(n, 90_000);
        await wallet!.configure({ delayMs: EVER_SLOWER_MS[n]! });
      }
      await wallet!.waitForCalls(4, 90_000); // the first request and three re-timed ones
      await walletAnswered(wallet!, 4);
      const stopped = await expectStopped(page, 'failed', { code: 'clock-moved' });
      expect(stopped.text).toMatch(/price moved/i);
      expect(stopped.text).toMatch(/nothing was sent/i);
      await expect(page.getByTestId('order-status').getByTestId('send-error')).toHaveAttribute('data-code', 'clock-moved');

      expect(await wallet!.calls(), 'three re-timings, then it stops').toHaveLength(4);
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

test.describe('the book moves between the ticket and the click', () => {
  /** Takes the whole best ask level out of the book (every order at the best price filled by another key); returns the old and the new best. */
  async function liftBestAsks(mock: import('../fixtures').MockClient): Promise<{ before: bigint; after: bigint }> {
    const tok = await mock.token();
    const book = await mock.book(tok.covenant_id, false, 20);
    const best = BigInt(book.asks[0].price);
    for (const o of book.asks.filter((a: { price: string }) => BigInt(a.price) === best)) await mock.fill(o.covenant_id, o.amount_left, { taker: 'bob' });
    const next = await mock.book(tok.covenant_id, false, 1);
    return { before: best, after: BigInt(next.asks[0].price) };
  }

  /** The ticket keeps its book while the move is read again: its book reads answer late, so the click plans from the parts on hand */
  async function slowBooks(page: Page): Promise<void> {
    await page.route('**/v1/books/**', async (route) => {
      await new Promise((r) => setTimeout(r, 1_500));
      await route.continue();
    });
  }

  test('within the slippage the plan of the state at the click is the order: it reaches the wallet and is sent at the new price', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '1' });
    await expect(page.getByTestId('order-place')).toBeEnabled({ timeout: 15_000 });
    await slowBooks(page);
    const { before, after } = await liftBestAsks(mock);
    expect(after).toBeGreaterThan(before);
    await page.waitForTimeout(500); // the book notice reaches the page: the book on hand is no longer the state at the click
    await clickPlace(page);
    const txid = await waitSubmitted(page);
    expect(await wallet!.calls()).toHaveLength(1);
    const placed = await placedBy(mock, txid);
    // the auction starts at the moved best ask, and ends no higher than the worst price the ticket showed (3 % above the old best ask)
    const st = placed.views[0].state.state as { price: string; priceEnd: string };
    expect(BigInt(st.price)).toBe(after);
    expect(BigInt(st.priceEnd)).toBeLessThanOrEqual(before + (before * 300n) / 10_000n);
  });

  test('past the slippage it is blocked with one line and the wallet is never asked', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '1', slippageBps: '0.1' });
    await expect(page.getByTestId('order-place')).toBeEnabled({ timeout: 15_000 });
    await slowBooks(page);
    const { before, after } = await liftBestAsks(mock);
    expect(after * 10_000n).toBeGreaterThan(before * 10_010n); // more than the 0.1 % slippage away
    await page.waitForTimeout(500);
    await clickPlace(page);
    const stopped = await expectStopped(page, 'blocked', { code: 'terms' });
    expect(stopped.text).toMatch(/moved past the price you set/);
    expect(await wallet!.calls()).toHaveLength(0);
  });
});

test.describe('the planner\'s warnings are one-line notes next to the button: they never stop the order', () => {
  test('a price far through the market: a note shows, the button stays enabled, and the order goes through with one click', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    // twice the market (0.025): a typing error, but the user's order
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '1', price: '0.05' });
    await settlePlan(page);
    const note = page.getByTestId('order-note-PRICE_AGGRESSIVE_VS_MARKET');
    await expect(note).toBeVisible();
    await expect(note).toContainText('through the market');
    await expect(note).toContainText('typing error');
    // a note is not an error: no red line, the button is enabled, and nothing asks for a click first
    await expect(page.getByTestId('order-issue-PRICE_AGGRESSIVE_VS_MARKET')).toHaveCount(0);
    await expect(page.getByTestId('order-place')).toBeEnabled();
    await clickPlace(page);
    await wallet!.waitForCalls(1);
    const txid = await waitSubmitted(page);
    expect((await placedBy(mock, txid)).views).toHaveLength(1);
    expect(await wallet!.calls()).toHaveLength(1);
  });

  test('a price in line with the market has no note', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '1', price: '0.0245' });
    await settlePlan(page);
    await expect(page.getByTestId('order-notes')).toHaveCount(0);
  });

  test('a stop the market has already reached, on a token whose genesis is not verified, shows both notes and still places', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'stopMarket', 'sell');
    // a sell stop above the market (0.025): it arms at the next qualifying trade
    await fillFields(page, { amount: '2', stop: '0.03' });
    await settlePlan(page);
    await expect(page.getByTestId('order-note-COND_STOP_ALREADY_REACHED')).toBeVisible();
    await expect(page.getByTestId('order-note-STOP_EVIDENCE_GENESIS_UNVERIFIED')).toContainText('genesis');
    await expect(page.getByTestId('order-place')).toBeEnabled();
    await clickPlace(page);
    await wallet!.waitForCalls(1);
    await waitSubmitted(page);
  });

  test('on a phone the notes wrap and the page does not scroll sideways', async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '1', price: '0.05' });
    await settlePlan(page);
    const note = page.getByTestId('order-note-PRICE_AGGRESSIVE_VS_MARKET');
    await note.scrollIntoViewIfNeeded();
    await expect(note).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });
});
