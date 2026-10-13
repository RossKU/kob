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
