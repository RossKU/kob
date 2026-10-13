// Screen details a user sees and clicks, each a regression of a defect found in a hands-on pass over the app (mock stack): the wallet
// address after a network switch, a confirmation screen that opens at its title, copy buttons, the pair ticket's price unit, panels when the
// indexer is down, the settings that pin servers and their reset, the skip link, a token of 18 decimals, the depth legend, the issue summary.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { connectWallet, fund, listExkcc, openMarket } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { fillFields, pickType, reviewAndSign, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });


test.describe('KasWare that starts on mainnet and switches', () => {
  test.use({ walletOptions: { network: 'mainnet' } });
  test('the address shown is the testnet one after the switch', async ({ appPage: page, mock }) => {
    await openMarket(page, mock);
    await expect(page.getByTestId('wallet-network')).toHaveText('testnet-10');
    await expect(page.getByTestId('wallet-address')).toHaveAttribute('data-value', TEST_KEYS.alice.testnetAddress);
  });
});

test('the confirmation screen opens at its title, not scrolled past it', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'ifd', 'buy');
  await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' });
  await waitReviewable(page);
  await page.getByTestId('order-review').click();
  const screen = page.getByTestId('confirm-screen');
  await expect(screen).toBeVisible();
  await expect(screen.locator('.modal-head h2')).toBeInViewport();
  expect(await page.locator('.modal-backdrop').evaluate((el) => el.scrollTop)).toBe(0);
});
