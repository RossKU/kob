// Smoke tests of the order ticket and its one-button send flow (checks, the final confirmation popup, the wallet's own approval, send) against the mock stack (mock indexer + node + KasWare-style mock wallet).
// The per-order-type matrix is the next phase's job: here every type is reachable, and the limit-sell flow runs end to end.
//
// The example registry of the mock lists EXKCC as pending-review (tradable; the UI no longer shows any verification state). Every test serves a copy in which
// the token is listed and verified, exactly what a fully reviewed registry entry looks like.
import { readFileSync } from 'node:fs';
import { test, expect } from '../fixtures';
import type { Page } from '@playwright/test';
import { formatKas } from '../../src/kob/units';
import { mockClock } from '../helpers/env';
import { clickPlace, clickPlaceOnly, expectStopped, expectWalletSigned, fillFields, peekReceipt, pickType, placedBy, placeOrder, placeOrderAndRead, readReceipt, waitReviewable, waitSubmitted } from '../helpers/ticket';

const KAS = 100_000_000n;
const TOKEN = 100_000_000n; // one EXKCC = 1e8 base units

test.use({ autoOpen: false });

async function listExkcc(page: Page) {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  for (const tk of reg.tokens) {
    if (tk.ticker === 'EXKCC') {
      tk.status = 'listed';
      tk.verified = true;
    }
  }
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

async function openTicket(page: Page, mock: { token(): Promise<{ covenant_id: string }> }, connect = true) {
  await listExkcc(page);
  const token = await mock.token();
  await page.goto(`${process.env.KOB_E2E_BASE ?? '/'}#/market/${token.covenant_id}`);
  if (connect) {
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
  }
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  return token.covenant_id;
}

async function fundAlice(mock: { giveKas(k: string, n: bigint): Promise<unknown>; giveTokens(k: string, n: bigint): Promise<unknown> }) {
  await mock.giveKas('alice', 1000n * KAS);
  await mock.giveTokens('alice', 100n * TOKEN);
}

test.describe('order ticket', () => {
  test('limit sell end to end: form, one click, the final confirmation, the wallet signature, accepted transaction', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);

    await page.getByTestId('order-side-sell').click();
    await expect(page.getByTestId('order-type')).toHaveValue('limit');
    await expect(page.getByTestId('order-place')).toBeDisabled();
    await page.getByTestId('order-amount').fill('3');
    await page.getByTestId('order-price').fill('2.6');
    await fillFields(page, { tip: '0.01' }); // the tip sits in Advanced options
    await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
    // the ticket itself lists no cost and terms: those are in the final confirmation only
    await expect(page.getByTestId('order-disclosure')).toHaveCount(0);

    // one click: the final confirmation (what the order costs and does), Continue, then the wallet is asked once and signs exactly the transaction the node accepts
    await expect(page.getByTestId('order-place')).toBeEnabled();
    const placed = await placeOrderAndRead(page, mock);
    const shown = placed.receipt;
    expect(shown.title).toBe('Place order');
    expect(shown.lines['Locked in orders']).toBe('4 KAS');
    expect(Object.values(shown.lines), 'the 3 EXKCC leave the wallet').toContain('−3');
    expect(shown.fee).toBe(`${formatKas(BigInt(placed.submission.fee))} KAS`);
    expect(shown.orders).toHaveLength(1);
    expect(shown.orders[0]!.title).toBe('Sell limit order');
    // 3 EXKCC at 2.6 KAS with a 0.01 KAS tip: the all-in price is 2.59
    expect(shown.orders[0]!.rows).toMatchObject({ Amount: '3 EXKCC', 'Limit price': '2.6 EXKCC / KAS', 'You receive at least per token (all-in)': '2.59 EXKCC / KAS', 'KAS on the order output': '2 KAS' });
    expect(shown.orders[0]!.rows.Expires, 'the expiry is shown').toMatch(/^DAA score [0-9]+$/);
    await expectWalletSigned(wallet!, placed);
    expect(placed.submission.created).toHaveLength(1);
    expect(placed.submission.created[0]!.kind).toBe('KobAsk');
    // the order as the node holds it
    const view = placed.views[0];
    expect(view.side).toBe(1); // a sell
    expect(view.state.state).toMatchObject({ amountLeft: '300000000', price: '260000000', tip: '1000000', refundTip: '4900000' }); // 3 EXKCC at 2.6 KAS, tip 0.01 KAS
    // the form starts the next order empty
    await expect(page.getByTestId('order-amount')).toHaveValue('');

    // the order is on the book, with its tokens in escrow
    const mine = await mock.ordersOf('alice');
    expect(mine).toHaveLength(1);
    const bal = await mock.balance('alice');
    expect(BigInt(bal.tokens.EXKCC ?? '0')).toBe(97n * TOKEN);
  });

  test('the final confirmation: Cancel sends nothing and the wallet is never asked; Continue asks it exactly once', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('2.7');

    // the click opens the popup: its title and the order's terms; the status line says it is waiting for the user, the wallet has not been asked
    await clickPlaceOnly(page);
    const shown = await readReceipt(page);
    expect(shown.from).toBe('app');
    expect(shown.title).toBe('Place order');
    expect(shown.orders.map((o) => o.title)).toEqual(['Sell limit order']);
    expect(shown.orders[0]!.rows).toMatchObject({ Amount: '2 EXKCC', 'Limit price': '2.7 EXKCC / KAS' });
    expect(shown.lines['Locked in orders']).toBe('4 KAS');
    expect(shown.fee).toMatch(/^[0-9.]+ KAS$/);
    await expect(page.getByTestId('order-status')).toHaveAttribute('data-phase', 'confirming');
    expect(await wallet!.calls(), 'no wallet request before Continue').toHaveLength(0);

    // Cancel: the neutral outcome, nothing was sent, no wallet request
    await page.getByTestId('confirm-order-cancel').click();
    await expect(page.getByTestId('confirm-order')).toHaveCount(0);
    const stopped = await expectStopped(page, 'rejected', { code: 'cancelled' });
    expect(stopped.text).toBe('Cancelled. Nothing was sent.');
    expect(await wallet!.calls()).toHaveLength(0);
    expect(await mock.submitted(false)).toHaveLength(0);

    // the same button opens it again; Continue asks the wallet exactly once and the order goes through
    await clickPlaceOnly(page);
    await readReceipt(page);
    await page.getByTestId('confirm-order-ok').click();
    await expect(page.getByTestId('confirm-order')).toHaveCount(0);
    const txid = await waitSubmitted(page);
    expect(await wallet!.calls(), 'exactly one wallet request').toHaveLength(1);
    const placed = await placedBy(mock, txid);
    await expectWalletSigned(wallet!, placed);
    expect(await mock.submitted(false)).toHaveLength(1);
  });

  test('a market buy confirmed after reading the popup for 3 s: the clock of the order is the one at Confirm; one wallet request, no second popup', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '2' });
    await clickPlaceOnly(page);
    const first = await readReceipt(page);
    expect(first.title).toBe('Place order');
    expect(first.orders.map((o) => o.title)).toEqual(['Buy market order (auction)']);
    expect(first.updated).toBe(false);
    // the user reads for ~3 s (the mock chain advances 10 DAA a second): the auction the popup showed started on the clock of the click
    await page.waitForTimeout(3_000);
    expect(await wallet!.calls()).toHaveLength(0);
    const confirmedAt = await mockClock(mock);
    await page.getByTestId('confirm-order-ok').click();
    // nothing got worse: no second popup, the wallet is asked directly (once)
    const txid = await waitSubmitted(page);
    await expect(page.getByTestId('confirm-order')).toHaveCount(0);
    await expect(page.getByTestId('confirm-order-updated')).toHaveCount(0);
    expect(await wallet!.calls(), 'exactly one wallet request').toHaveLength(1);
    const placed = await placedBy(mock, txid);
    await expectWalletSigned(wallet!, placed);
    expect(await mock.submitted(false)).toHaveLength(1);
    // the stale clock of the popup did not end up in the order: its auction starts at or after the node clock when Confirm was clicked
    const state = placed.views[0].state.state;
    expect(placed.views[0].contract).toBe('KobBid');
    expect(Number(state.activeFrom), 'activeFrom (the auction start) >= the DAA at Confirm').toBeGreaterThanOrEqual(confirmedAt.daa);
    expect(Number(state.expiryDaa)).toBeGreaterThan(Number(state.activeFrom));
  });

  test('a wallet that declines is a neutral outcome and nothing is sent; trying again works', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('2.7');
    await wallet!.configure({ approve: false, rejectMessage: 'User rejected the request' });
    await clickPlace(page);
    await expectStopped(page, 'rejected', { code: 'rejected' });
    expect(await wallet!.calls()).toHaveLength(1);
    expect(await mock.submitted()).toHaveLength(0);

    // approve now: the same button opens the confirmation again, then asks the wallet, signs and submits
    await wallet!.configure({ approve: true });
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await mock.submitted()).toHaveLength(1);
  });

  test('a wallet signature that does not match the transaction is caught before anything is broadcast', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('2.7');
    await wallet!.configure({ wrongSignature: true });
    await clickPlace(page);
    const stopped = await expectStopped(page, 'failed');
    expect(stopped.code).toBe('signature');
    expect(stopped.text).toMatch(/signature/i);
    expect((await wallet!.calls()).at(-1)!.status).toBe('signed'); // the wallet did answer: the answer was wrong
    expect(await mock.submitted()).toHaveLength(0);
  });

  test('a node that refuses the transaction shows a readable error; the ticket stays usable', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('2.7');
    await mock.failNextSubmit('mock: injected submit failure');
    await clickPlace(page);
    const stopped = await expectStopped(page, 'failed');
    expect(stopped.text).toContain('injected submit failure');
    expect(await mock.submitted()).toHaveLength(0);
    // the ticket stays as it was: the same button tries again and the node accepts
    await expect(page.getByTestId('order-place')).toBeEnabled();
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await mock.submitted()).toHaveLength(1);
  });

  test('errors of the planner block the order button and name the exact shortfall', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 1000n * KAS);
    await mock.giveTokens('alice', 5n * TOKEN);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('8');
    await page.getByTestId('order-price').fill('2.7');
    const issue = page.getByTestId('order-issue-INSUFFICIENT_TOKENS');
    await expect(issue).toBeVisible();
    await expect(issue).toContainText('3 EXKCC'); // short by 3 of the 8 EXKCC
    await expect(page.getByTestId('order-place')).toBeDisabled();
    // "max" fills what the balance allows and clears the error
    await page.getByTestId('order-available').click();
    await expect(page.getByTestId('order-amount')).toHaveValue('5');
    await expect(page.getByTestId('order-issue-INSUFFICIENT_TOKENS')).toHaveCount(0);
    await expect(page.getByTestId('order-place')).toBeEnabled();
  });

  test('self-trade prevention: a buy that would cross your own resting sell is refused', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('3.4');
    await placeOrder(page, mock);

    await page.getByTestId('order-side-buy').click();
    await page.getByTestId('order-amount').fill('1');
    await page.getByTestId('order-price').fill('3.5');
    await expect(page.getByTestId('order-issue-SELF_TRADE')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-place')).toBeDisabled();
  });

  test('every order type is reachable and shows its own fields', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    const types = await page.getByTestId('order-type').locator('option').evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value));
    expect(types).toEqual(
      expect.arrayContaining(['limit', 'market', 'ioc', 'fok', 'streaming', 'close', 'stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco', 'ifd', 'ifo', 'repeatIfd', 'repeatIfo', 'twap', 'dca', 'dutch']),
    );
    expect(types).toHaveLength(18);
    const expectField: Record<string, string[]> = {
      limit: ['order-price', 'order-tip'],
      market: ['field-slippageBps'],
      ioc: ['order-price'],
      fok: ['order-price'],
      streaming: ['field-displayedPrice', 'field-toleranceBps'],
      close: ['order-tip'],
      stopMarket: ['field-stop'],
      stopLimit: ['field-stop', 'field-limit'],
      trailingStop: ['field-stop', 'field-trail.step', 'field-trail.gap'],
      takeProfit: ['order-price'],
      oco: ['field-takeProfit', 'field-stop'],
      ifd: ['order-price', 'field-exit.takeProfit'],
      ifo: ['order-price', 'field-exit.takeProfit', 'field-exit.stop'],
      repeatIfd: ['order-price', 'field-exit.takeProfit', 'field-repeat.count'],
      repeatIfo: ['order-price', 'field-exit.takeProfit', 'field-exit.stop', 'field-repeat.count'],
      twap: ['field-sliceAmount', 'field-interval'],
      dca: ['field-sliceAmount', 'field-interval'],
      dutch: ['order-price', 'field-priceEnd', 'field-duration'],
    };
    for (const type of types) {
      await page.getByTestId('order-type').selectOption(type);
      await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-type', type);
      await expect(page.getByTestId('order-amount')).toBeVisible();
      await expect(page.getByTestId('order-type-help')).not.toBeEmpty();
      for (const id of expectField[type] ?? []) {
        // the priority tip sits in Advanced options for every type (closed by default); the other fields are on the ticket itself
        if (id === 'order-tip') await expect(page.getByTestId('order-advanced').getByTestId(id), `${type}: ${id}`).toHaveCount(1);
        else await expect(page.getByTestId(id), `${type}: ${id}`).toBeVisible();
      }
    }
    // the side follows the type: TWAP only sells, DCA only buys
    await page.getByTestId('order-type').selectOption('twap');
    await expect(page.getByTestId('order-side-buy')).toBeDisabled();
    await page.getByTestId('order-type').selectOption('dca');
    await expect(page.getByTestId('order-side-sell')).toBeDisabled();
  });

  test('a stop-market sell is ready to place with its trigger rules', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-type').selectOption('stopMarket');
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('field-stop').fill('2.2');
    await waitReviewable(page);
    await expect(page.getByTestId('order-place')).toHaveText('Sell Stop (market)');
    await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
    // the trigger rules are in the final confirmation (read and cancelled: nothing is sent)
    const shown = await peekReceipt(page);
    expect(shown.orders.map((o) => o.title)).toEqual(['Sell stop order']);
    const rows = shown.orders[0]!.rows;
    expect(rows['Stop trigger']).toBe('2.2 EXKCC / KAS');
    expect(rows['Worst fill price after the trigger'], 'the worst fill price of the stop').toContain('EXKCC / KAS');
    expect(rows['Fill band'], 'the trigger band').toBeTruthy();
    expect(await mock.submitted()).toHaveLength(0);
  });

  test('without a wallet the form works but the order button is disabled and says why', async ({ appPage: page, mock }) => {
    await openTicket(page, mock, false);
    await page.getByTestId('order-amount').fill('1');
    await page.getByTestId('order-price').fill('2.7');
    await expect(page.getByTestId('order-need-wallet')).toBeVisible();
    await expect(page.getByTestId('order-place')).toBeDisabled();
  });


  test('a streaming order fills in the quote its side meets and follows it until a price is typed', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    const token = await openTicket(page, mock);
    const seen = page.getByTestId('field-displayedPrice');
    const priceText = (sompi: bigint) => (Number(sompi) / 1e8).toString();
    const best = async () => (await mock.book(token, false)).asks as { covenant_id: string; price: string; amount_left: string }[];
    const asks = await best();
    const top = BigInt(asks[0]!.price);
    await page.getByTestId('order-side-buy').click();
    await page.getByTestId('order-type').selectOption('streaming');
    // a buy meets the best ask
    await expect(seen).toHaveValue(priceText(top));
    // a sell meets the best bid
    await page.getByTestId('order-side-sell').click();
    await expect(seen).toHaveValue(priceText(BigInt((await mock.book(token, false)).bids[0].price)));
    await page.getByTestId('order-side-buy').click();
    await expect(seen).toHaveValue(priceText(top));
    // the best asks are taken: the box follows the new quote
    for (const o of asks.filter((a) => BigInt(a.price) === top)) await mock.fill(o.covenant_id, o.amount_left);
    const next = BigInt((await best())[0]!.price);
    expect(next).toBeGreaterThan(top);
    await expect(seen).toHaveValue(priceText(next));
    // a price of the user's own stays
    await seen.fill('0.03');
    const asks2 = await best();
    for (const o of asks2.filter((a) => BigInt(a.price) === next)) await mock.fill(o.covenant_id, o.amount_left);
    await expect.poll(async () => BigInt((await best())[0]!.price)).toBeGreaterThan(next);
    await page.getByTestId('order-amount').fill('1');
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('aria-busy', 'false');
    await expect(seen).toHaveValue('0.03');
  });

  test('the amount slider: 0 / 25 / 50 / 75 / 100 % of Max, 1 % steps by keyboard, a typed amount moves it', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    const amount = page.getByTestId('order-amount');
    const slider = page.getByTestId('order-amount-slider');
    const range = page.getByTestId('order-amount-range');
    const base = async () => BigInt(Math.round(Number(await amount.inputValue()) * 1e8));
    // a sell: Max is the free balance (100 EXKCC)
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-price').fill('0.03');
    await expect(range).toBeEnabled();
    await page.getByTestId('order-amount-pct-50').click();
    await expect(amount).toHaveValue('50');
    await expect(slider).toHaveAttribute('data-pct', '50');
    await page.getByTestId('order-amount-pct-100').click();
    await expect(amount).toHaveValue('100');
    // keyboard: one step is 1 %
    await range.focus();
    await page.keyboard.press('ArrowLeft');
    await expect(amount).toHaveValue('99');
    await expect(range).toHaveAttribute('aria-valuetext', '99% · 99 EXKCC');
    // typing moves the slider
    await amount.fill('25');
    await expect(slider).toHaveAttribute('data-pct', '25');
    await expect(page.getByTestId('order-amount-pct-25')).toHaveAttribute('aria-pressed', 'true');
    // a minimum fill of 30 tokens: the 25 % point is off, 0 % empties the box
    await page.getByTestId('order-advanced').locator('summary').click();
    await page.getByTestId('field-minFill').fill('30');
    await expect(page.getByTestId('order-amount-pct-25')).toBeDisabled();
    await page.getByTestId('order-amount-pct-0').click();
    await expect(amount).toHaveValue('');
    await page.getByTestId('field-minFill').fill('');

    // a buy: 50 % is half of what Max gives (the planner's search)
    await page.getByTestId('order-side-buy').click();
    await page.getByTestId('order-price').fill('0.0245');
    // the slider waits for the buy's Max (a planner search) before it takes a pick
    await expect(range).toBeEnabled();
    await page.getByTestId('order-available').click();
    await expect(amount).not.toHaveValue('');
    const max = await base();
    expect(max).toBeGreaterThan(0n);
    await expect(slider).toHaveAttribute('data-pct', '100');
    await page.getByTestId('order-amount-pct-50').click();
    await expect.poll(base).toBe(max / 2n);
    await expect(slider).toHaveAttribute('data-pct', '50');
    await expect(page.getByTestId('order-place')).toBeEnabled();
    // a phone: the slider is one row inside the ticket, nothing scrolls sideways
    await page.setViewportSize({ width: 375, height: 800 });
    const box = (await slider.boundingBox())!;
    const pt = (await page.getByTestId('order-amount-pct-100').boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(375);
    expect(pt.y).toBeLessThan(box.y + box.height);
    expect(pt.x + pt.width).toBeLessThanOrEqual(375);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(375);
  });
});
