// Wallet changes AFTER connect (account switch, network switch, lock) while the pre-sign confirmation screen is open, for all three wallets.
// The plan on that screen was built for one key on one network: it must never be signed for another. Nothing may reach the wallet or the node.
import { test, expect, TEST_KEYS } from '../fixtures';
import { fund, openMarket, type WalletName } from '../helpers/env';
import { acknowledgeAndSign, expectWalletSigned, fillFields, openReview, pickType, placedBy, waitReviewable } from '../helpers/ticket';
import type { Page } from '@playwright/test';

test.use({ autoOpen: false });

async function sellTicket(page: Page): Promise<void> {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
}

const addressText = (page: Page) => page.getByTestId('wallet-address').getAttribute('data-value');

for (const id of ['kasware', 'kaspire', 'kastle'] as WalletName[]) {
  test.describe(`${id}: account and network events with the confirmation screen open`, () => {
    test.use({ walletId: id });

    test('an account switch blocks the open confirmation, moves the session to the new account and asks the wallet for nothing', async ({ appPage: page, mock, wallet }) => {
      await fund(mock, 'alice');
      await fund(mock, 'bob');
      await openMarket(page, mock, { wallet: id });
      const aliceAddress = await addressText(page);
      expect(aliceAddress).toBe(TEST_KEYS.alice.testnetAddress);
      await sellTicket(page);
      await openReview(page);
      await page.getByTestId('confirm-ack').check();
      await expect(page.getByTestId('confirm-sign')).toBeEnabled();

      await wallet!.setAccount(TEST_KEYS.bob.secretKey);

      // the header follows the wallet, a banner says so, and the screen holding the old plan cannot sign it
      await expect(page.getByTestId('wallet-address')).toHaveAttribute('data-value', TEST_KEYS.bob.testnetAddress);
      const list = page.getByTestId('confirm-blocking-list');
      await expect(list.locator('li[data-code="account-changed"]')).toBeVisible();
      await expect(page.getByTestId('confirm-sign')).toBeDisabled();
      await expect(page.getByTestId('confirm-ack')).toBeDisabled();
      expect(await wallet!.calls()).toHaveLength(0);
      expect(await mock.submitted(false)).toHaveLength(0);

      // closing it goes back to the ticket, which is bound to the new account (its banner is in the shell behind the modal)
      await page.getByTestId('confirm-cancel').click();
      await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
      await expect(page.getByTestId('banner-wallet-account-changed')).toBeVisible();
      await waitReviewable(page);

      // the new account plans and signs its own order
      await openReview(page);
      const txid = await acknowledgeAndSign(page);
      const placed = await placedBy(mock, txid);
      await expectWalletSigned(wallet!, placed);
      expect((await mock.ordersOf('bob', 'active')).length).toBe(1);
      expect((await mock.ordersOf('alice', 'active')).length).toBe(0);
    });

    test('a network switch blocks signing with the mismatch banner and recovers when the wallet switches back', async ({ appPage: page, mock, wallet }) => {
      await fund(mock, 'alice');
      await openMarket(page, mock, { wallet: id });
      await sellTicket(page);
      await openReview(page);
      await page.getByTestId('confirm-ack').check();
      await expect(page.getByTestId('confirm-sign')).toBeEnabled();

      await wallet!.setNetwork('mainnet');

      await expect(page.getByTestId('banner-network-mismatch')).toBeVisible();
      await expect(page.getByTestId('wallet-network')).toHaveText('mainnet');
      await expect(page.getByTestId('confirm-blocking-list').locator('li[data-code="network"]')).toBeVisible();
      await expect(page.getByTestId('confirm-sign')).toBeDisabled();
      expect(await wallet!.calls()).toHaveLength(0);

      await wallet!.setNetwork('testnet-10');

      await expect(page.getByTestId('banner-network-mismatch')).toHaveCount(0);
      await expect(page.getByTestId('wallet-network')).toHaveText('testnet-10');
      await expect(page.getByTestId('banner-wallet-network-restored')).toBeVisible();
      await expect(page.getByTestId('confirm-blocking')).toHaveCount(0);
      // same key, same network, same plan: signing works again
      await expect(page.getByTestId('confirm-sign')).toBeEnabled();
      await page.getByTestId('confirm-sign').click();
      await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
      expect(await mock.submitted(false)).toHaveLength(1);
    });

    test('the wallet locks: the session closes with a banner, the confirmation shows no wallet, listeners are gone after disconnect', async ({ appPage: page, mock, wallet }) => {
      await fund(mock, 'alice');
      await openMarket(page, mock, { wallet: id });
      expect(await wallet!.listenerCount(), 'the app listens to the wallet while connected').toBeGreaterThan(0);
      await sellTicket(page);
      await openReview(page);
      await page.getByTestId('confirm-ack').check();

      await wallet!.lose();

      await expect(page.getByTestId('banner-wallet-lost')).toBeVisible();
      await expect(page.getByTestId('wallet-address')).toHaveCount(0);
      await expect(page.getByTestId('confirm-blocking-list').locator('li[data-code="no-wallet"]')).toBeVisible();
      await expect(page.getByTestId('confirm-sign')).toBeDisabled();
      expect(await wallet!.calls()).toHaveLength(0);
      expect(await wallet!.listenerCount(), 'the session is closed: no listener stays behind').toBe(0);
    });
  });
}

test.describe('listeners follow the session', () => {
  test('connect registers the listeners once, disconnect and reconnect never stack them', async ({ appPage: page, mock, wallet }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    const connected = await wallet!.listenerCount();
    expect(connected).toBeGreaterThan(0);
    await page.getByTestId('wallet-disconnect').click();
    await expect(page.getByTestId('wallet-address')).toHaveCount(0);
    expect(await wallet!.listenerCount()).toBe(0);
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
    expect(await wallet!.listenerCount()).toBe(connected);
  });
});
