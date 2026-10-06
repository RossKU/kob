// Position cards in My orders (mock indexer available): an IFD / repeat entry with its exits is ONE card with the phase chip, the price path,
// amount entered / exited / open, the repeat state, and realised fills on demand. Mock KasWare wallet, default mock seed.
import { test, expect, type MockClient } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { fillFields, pickType, reviewAndSign } from '../helpers/ticket';

test.use({ autoOpen: false });

async function placeAndFill(page: import('@playwright/test').Page, mock: MockClient, type: string, fields: Record<string, string>, fillTokens: number) {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, type, 'buy');
  await fillFields(page, fields);
  const { placed } = await reviewAndSign(page, mock);
  const entryId = placed.views[0].covenant_id as string;
  if (fillTokens > 0) {
    await mock.fill(entryId, BigInt(fillTokens) * 100_000_000n);
    await mock.until(async () => (await mock.order(entryId)).children as string[], (c) => c.length >= 1);
  }
  return entryId;
}

test.describe('position cards', () => {
  test('an IFD in progress: phase, price path, amounts and the realised fills', async ({ appPage: page, mock }) => {
    const entryId = await placeAndFill(page, mock, 'ifd', { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' }, 1);
    await page.getByTestId('nav-orders').click();
    const card = page.getByTestId(`position-${entryId}`);
    await expect(card).toBeVisible();
    await expect(card).toHaveAttribute('data-kind', 'ifd');
    await expect(card).toHaveAttribute('data-phase', 'entry-partial');
    await expect(card.getByTestId('position-status')).toHaveText('entry partly filled');
    await expect(card.getByTestId('position-title')).toContainText('IFD');
    await expect(card.getByTestId('position-token')).toContainText('EXKCC');
    await expect(card.getByTestId('position-entry-price')).toContainText('0.0245');
    await expect(card.getByTestId('position-exit-prices')).toContainText('0.026');
    await expect(card.getByTestId('position-amount')).toContainText('2 EXKCC in total: 1 EXKCC entered, 0 EXKCC exited, 2 EXKCC open');
    await expect(card.getByTestId('position-repeat')).toHaveCount(0);
    // fills on demand: one entry fill of 1 EXKCC, no exit fill yet
    await card.getByTestId('position-fills-toggle').click();
    await expect(card.getByTestId('position-fills-entry')).toContainText('Entry: 1 EXKCC filled in 1 fill');
    await expect(card.getByTestId('position-fills-exits')).toContainText('Exits: 0 EXKCC filled in 0 fill');
  });

  test('a repeat IFD shows its re-arms left; the entry and the exit are sub-rows of the same card', async ({ appPage: page, mock }) => {
    const entryId = await placeAndFill(page, mock, 'repeatIfd', { amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04', 'repeat.count': '3' }, 1);
    const exitId = ((await mock.order(entryId)).children as string[])[0]!;
    await page.getByTestId('nav-orders').click();
    const card = page.getByTestId(`position-${entryId}`);
    await expect(card).toHaveAttribute('data-kind', 'repeat');
    await expect(card.getByTestId('position-title')).toContainText('Repeat');
    await expect(card.getByTestId('position-repeat')).toContainText('re-arms left');
    await expect(card.getByTestId(`order-row-${entryId}`)).toBeVisible();
    await expect(card.getByTestId('position-exits').getByTestId(`order-row-${exitId}`)).toBeVisible();
    await expect(card.getByTestId('position-exit-prices')).toContainText('0.04');
  });

  test('an IFD entry with nothing filled is a card (waiting for entry); cancelling it moves the card to history as cancelled', async ({ appPage: page, mock, wallet }) => {
    const entryId = await placeAndFill(page, mock, 'ifd', { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' }, 0);
    await page.getByTestId('nav-orders').click();
    const card = page.getByTestId(`position-${entryId}`);
    await expect(card).toHaveAttribute('data-phase', 'entry-waiting');
    await expect(card.getByTestId('position-status')).toHaveText('waiting for entry');
    await card.locator('[data-testid^="position-cancel-"]').click();
    await runFlow(page, mock, wallet, { steps: 1 });
    await expect(page.getByTestId(`position-${entryId}`)).toHaveCount(0, { timeout: 15_000 });
    await page.getByTestId('orders-tab-history').click();
    await expect(page.getByTestId(`position-${entryId}`)).toHaveAttribute('data-phase', 'cancelled');
  });

  // B1 / C5 R-9: close of a SELL-first position: cancel its orders (their KAS comes back), then buy back the amount it sold at market
  test('close a sell-first position: its orders are cancelled, then the ticket opens on a market buy of the amount sold', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'ifd', 'sell');
    await fillFields(page, { amount: '2', price: '0.026', 'exit.takeProfit': '0.0245' });
    const { placed } = await reviewAndSign(page, mock);
    const entryId = placed.views[0].covenant_id as string;
    await mock.fill(entryId);
    await mock.until(async () => (await mock.order(entryId)).children as string[], (c) => c.length >= 1);
    await page.getByTestId('nav-orders').click();
    const card = page.getByTestId(`position-${entryId}`);
    await expect(card).toBeVisible();
    await card.getByTestId(`position-close-${entryId}`).click();
    await runFlow(page, mock, wallet, { steps: 1 });
    await expect(page.getByTestId('orders-close-followup-sell')).toBeVisible();
    const buy = page.getByTestId('orders-close-buy');
    await expect(buy).toHaveAttribute('href', /\?ticket=cover&amount=100000000$/);
    await buy.click();
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-type', 'market');
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-side', 'buy');
    await expect(page.getByTestId('order-amount')).toHaveValue('1');
  });

});
