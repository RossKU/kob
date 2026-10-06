// Transaction links to the block explorer (kaspa.stream; the base follows the network, TN10 in this stack): a recent-trades row, an order's placement
// transaction and its fill rows in My orders open the transaction in a NEW tab. The explorer itself is stubbed (no network access in the mock stack).
import type { BrowserContext, Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { fund, listExkcc, openMarket } from '../helpers/env';
import { fillFields, pickType, reviewAndSign } from '../helpers/ticket';

test.use({ autoOpen: false });

const TN10 = /^https:\/\/tn10\.kaspa\.stream\/transactions\/[0-9a-f]{64}$/;

async function stubExplorer(ctx: BrowserContext): Promise<void> {
  await ctx.route('https://tn10.kaspa.stream/**', (route) => route.fulfill({ status: 200, contentType: 'text/html', body: '<title>explorer stub</title>' }));
}

test('recent trades: a row is a link to its transaction on the TN10 explorer, opened in a new tab', async ({ appPage: page, mock }) => {
  await stubExplorer(page.context());
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await listExkcc(page);
  const tok = await mock.token();
  await page.goto(`/#/market/${tok.covenant_id}`);
  const row = page.getByTestId('trades-list').getByTestId('trade-row').first();
  await expect(row).toHaveAttribute('href', TN10);
  await expect(row).toHaveAttribute('target', '_blank');
  await expect(row).toHaveAttribute('rel', /noopener/);
  const txid = (await row.getAttribute('data-txid'))!;
  expect(txid).toMatch(/^[0-9a-f]{64}$/);
  const [popup] = await Promise.all([page.context().waitForEvent('page'), row.click()]);
  await expect(popup).toHaveURL(`https://tn10.kaspa.stream/transactions/${txid}`);
  await popup.close();
  // the app page stays where it was
  await expect(page).toHaveURL(new RegExp(`#/market/${tok.covenant_id}$`));
});

async function placeAndPartlyFill(page: Page, mock: MockClient): Promise<string> {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'limit', 'buy');
  // a 1 EXKCC minimum fill lets the order fill in part (the default minimum of a small order is its whole amount)
  await fillFields(page, { amount: '2', minFill: '1', price: '0.0245' });
  const { placed } = await reviewAndSign(page, mock);
  const id = placed.views[0].covenant_id as string;
  await mock.fill(id);
  return id;
}

test('My orders: the placement transaction and every fill row link to the explorer', async ({ appPage: page, mock }) => {
  await stubExplorer(page.context());
  const id = await placeAndPartlyFill(page, mock);
  await page.getByTestId('nav-orders').click();
  const card = page.getByTestId(`order-row-${id}`);
  await expect(card).toBeVisible();
  const placement = card.getByTestId(`order-tx-${id}-link`);
  await expect(placement).toHaveAttribute('href', TN10);
  await expect(placement).toHaveAttribute('target', '_blank');
  const txid = (await card.getByTestId(`order-tx-${id}`).getAttribute('data-value'))!;
  await expect(card.getByTestId(`order-explorer-${id}`)).toHaveAttribute('href', `https://tn10.kaspa.stream/transactions/${txid}`);
  // fills: the row (anywhere) and the id open the fill's transaction
  await card.getByTestId(`order-fills-toggle-${id}`).click();
  const fill = card.getByTestId('order-fill-row').first();
  await expect(fill).toBeVisible();
  const fillTx = (await fill.getAttribute('data-txid'))!;
  await expect(fill.locator('a.explorer-link')).toHaveAttribute('href', `https://tn10.kaspa.stream/transactions/${fillTx}`);
  const [popup] = await Promise.all([page.context().waitForEvent('page'), fill.locator('td').first().click()]);
  await expect(popup).toHaveURL(`https://tn10.kaspa.stream/transactions/${fillTx}`);
  await popup.close();
});
