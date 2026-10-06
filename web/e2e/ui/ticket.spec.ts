// Smoke tests of the order ticket and the pre-sign confirmation flow against the mock stack (mock indexer + node + KasWare-style mock wallet).
// The per-order-type matrix is the next phase's job: here every type is reachable, and the limit-sell flow runs end to end.
//
// The example registry of the mock lists EXKCC as pending-review (tradable, but shown with the pending-review caveat). Every test serves a copy in which
// the token is listed and verified, exactly what a fully reviewed registry entry looks like.
import { readFileSync } from 'node:fs';
import { test, expect } from '../fixtures';
import type { Page } from '@playwright/test';

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
  test('limit sell end to end: form, disclosure, review, confirmation screen, signing, accepted transaction', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);

    await page.getByTestId('order-side-sell').click();
    await expect(page.getByTestId('order-type')).toHaveValue('limit');
    await expect(page.getByTestId('order-review')).toBeDisabled();
    await page.getByTestId('order-amount').fill('3');
    await page.getByTestId('order-price').fill('2.6');
    await page.getByTestId('order-tip').fill('0.01');

    // disclosure: all-in price, carriers, escrow, expiry
    const disclosure = page.getByTestId('order-disclosure');
    await expect(disclosure).toBeVisible();
    await expect(page.getByTestId('disc-allInPrice')).toContainText('2.59 KAS / EXKCC');
    await expect(page.getByTestId('disc-allInTotal')).toContainText('7.77 KAS');
    await expect(page.getByTestId('disc-tokensEscrowed')).toContainText('3 EXKCC');
    await expect(page.getByTestId('disc-kasLocked')).toContainText('20 KAS');
    await expect(page.getByTestId('disc-expiry')).toBeVisible();
    await expect(page.getByTestId('disc-expiry-extra')).toBeVisible(); // day-85 renewal date of a GTC order
    await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);

    // review: the confirmation screen decodes what the wallet will sign
    await expect(page.getByTestId('order-review')).toBeEnabled();
    await page.getByTestId('order-review').click();
    const screen = page.getByTestId('confirm-screen');
    await expect(screen).toBeVisible();
    await expect(page.getByTestId('confirm-blocking')).toHaveCount(0);
    const summary = page.getByTestId('confirm-summary');
    await expect(summary).toContainText('Sell limit order');
    await expect(summary).toContainText('3 EXKCC');
    await expect(summary).toContainText('2.6 KAS / EXKCC');
    await expect(summary).toContainText('2.59 KAS / EXKCC'); // all-in: limit minus tip
    await expect(page.getByTestId('confirm-locked-locked-total')).toContainText('20 KAS');
    await expect(page.getByTestId('confirm-wallet-notice')).toBeVisible();
    // signing needs the explicit acknowledgement
    await expect(page.getByTestId('confirm-sign')).toBeDisabled();
    await page.getByTestId('confirm-ack').check();
    await expect(page.getByTestId('confirm-sign')).toBeEnabled();
    await page.getByTestId('confirm-sign').click();

    // accepted by the mock node (it runs the covenants): status, tx id, wallet call
    await expect(page.getByTestId('tx-id')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 60_000 });
    const txid = await page.getByTestId('tx-id').getAttribute('data-value');
    expect(txid).toMatch(/^[0-9a-f]{64}$/);
    const calls = await wallet!.calls();
    expect(calls).toHaveLength(1);
    const submitted = await mock.submitted();
    const last = submitted[submitted.length - 1]!;
    expect(last.txid).toBe(txid);
    expect(last.created).toHaveLength(1);
    expect(last.created[0]!.kind).toBe('KobAsk');

    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    await expect(page.getByTestId('order-result')).toBeVisible();
    await expect(page.getByTestId('order-result-txid')).toHaveAttribute('data-value', txid!);
    // the form starts the next order empty
    await expect(page.getByTestId('order-amount')).toHaveValue('');

    // the order is on the book, with its tokens in escrow
    const mine = await mock.ordersOf('alice');
    expect(mine).toHaveLength(1);
    const bal = await mock.balance('alice');
    expect(BigInt(bal.tokens.EXKCC ?? '0')).toBe(97n * TOKEN);
  });

  test('a wallet that declines is a neutral outcome and nothing is sent; trying again works', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('2.7');
    await wallet!.configure({ approve: false, rejectMessage: 'User rejected the request' });
    await page.getByTestId('order-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-rejected')).toBeVisible();
    await expect(page.getByTestId('confirm-error')).toHaveCount(0);
    expect(await mock.submitted()).toHaveLength(0);

    // approve now: the same screen signs and submits
    await wallet!.configure({ approve: true });
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    expect(await mock.submitted()).toHaveLength(1);
  });

  test('a wallet signature that does not match the transaction is caught before anything is broadcast', async ({ appPage: page, mock, wallet }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('2.7');
    await wallet!.configure({ wrongSignature: true });
    await page.getByTestId('order-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-error')).toBeVisible();
    await expect(page.getByTestId('tx-status')).toHaveCount(0);
    expect(await mock.submitted()).toHaveLength(0);
  });

  test('a node that refuses the transaction shows a readable error; the screen stays open', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('2.7');
    await mock.failNextSubmit('mock: injected submit failure');
    await page.getByTestId('order-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-error')).toBeVisible();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    expect(await mock.submitted()).toHaveLength(0);
    await page.getByTestId('confirm-cancel').click();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  });

  test('errors of the planner block the review and name the exact shortfall', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 1000n * KAS);
    await mock.giveTokens('alice', 5n * TOKEN);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('8');
    await page.getByTestId('order-price').fill('2.7');
    const issue = page.getByTestId('order-issue-INSUFFICIENT_TOKENS');
    await expect(issue).toBeVisible();
    await expect(issue).toContainText('3 EXKCC'); // short by 3 of the 8 EXKCC
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // "max" fills what the balance allows and clears the error
    await page.getByTestId('order-amount-max').click();
    await expect(page.getByTestId('order-amount')).toHaveValue('5');
    await expect(page.getByTestId('order-issue-INSUFFICIENT_TOKENS')).toHaveCount(0);
    await expect(page.getByTestId('order-review')).toBeEnabled();
  });

  test('self-trade prevention: a buy that would cross your own resting sell is refused', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('order-price').fill('3.4');
    await page.getByTestId('order-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();

    await page.getByTestId('order-side-buy').click();
    await page.getByTestId('order-amount').fill('1');
    await page.getByTestId('order-price').fill('3.5');
    await expect(page.getByTestId('order-issue-SELF_TRADE')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-review')).toBeDisabled();
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
      for (const id of expectField[type] ?? []) await expect(page.getByTestId(id), `${type}: ${id}`).toBeVisible();
    }
    // the side follows the type: TWAP only sells, DCA only buys
    await page.getByTestId('order-type').selectOption('twap');
    await expect(page.getByTestId('order-side-buy')).toBeDisabled();
    await page.getByTestId('order-type').selectOption('dca');
    await expect(page.getByTestId('order-side-sell')).toBeDisabled();
  });

  test('a stop-market sell reaches the confirmation screen with its trigger rules', async ({ appPage: page, mock }) => {
    await fundAlice(mock);
    await openTicket(page, mock);
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-type').selectOption('stopMarket');
    await page.getByTestId('order-amount').fill('2');
    await page.getByTestId('field-stop').fill('2.2');
    await expect(page.getByTestId('disc-stopWorst')).toBeVisible();
    await expect(page.getByTestId('disc-notes').locator('[data-note="stopTrigger"]')).toBeVisible();
    await page.getByTestId('order-review').click();
    await expect(page.getByTestId('confirm-summary')).toContainText('Sell stop order');
    await expect(page.getByTestId('confirm-summary')).toContainText('2.2 KAS / EXKCC');
    await expect(page.getByTestId('confirm-blocking')).toHaveCount(0);
    await page.getByTestId('confirm-cancel').click();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    expect(await mock.submitted()).toHaveLength(0);
  });

  test('without a wallet the form works but review is disabled and says why', async ({ appPage: page, mock }) => {
    await openTicket(page, mock, false);
    await page.getByTestId('order-amount').fill('1');
    await page.getByTestId('order-price').fill('2.7');
    await expect(page.getByTestId('order-need-wallet')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });

});
