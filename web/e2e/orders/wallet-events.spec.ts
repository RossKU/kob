// Wallet changes AFTER connect (account switch, network switch, lock) while a ready ticket is on screen, for all three wallets.
// The plan of the ticket was built for one key on one network: it must never be signed for another. The ticket refuses (button disabled, a need line)
// while the wallet is wrong, and nothing may reach the wallet or the node.
import { test, expect, TEST_KEYS } from '../fixtures';
import { fund, openMarket, type WalletName } from '../helpers/env';
import { clickPlace, expectWalletSigned, fillFields, pickType, placedBy, waitReviewable, waitSubmitted } from '../helpers/ticket';
import type { Page } from '@playwright/test';

test.use({ autoOpen: false });

async function sellTicket(page: Page): Promise<void> {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
}

const addressText = (page: Page) => page.getByTestId('wallet-address').getAttribute('data-value');

for (const id of ['kasware', 'kaspire', 'kastle'] as WalletName[]) {
  test.describe(`${id}: account and network events with a ready ticket`, () => {
    test.use({ walletId: id });

    test('an account switch moves the session to the new account: the old plan is not signed, the new account places its own order', async ({ appPage: page, mock, wallet }) => {
      await fund(mock, 'alice');
      await fund(mock, 'bob');
      await openMarket(page, mock, { wallet: id });
      const aliceAddress = await addressText(page);
      expect(aliceAddress).toBe(TEST_KEYS.alice.testnetAddress);
      await sellTicket(page);

      await wallet!.setAccount(TEST_KEYS.bob.secretKey);

      // the header follows the wallet and a banner says so; nothing was asked of the wallet or sent
      await expect(page.getByTestId('wallet-address')).toHaveAttribute('data-value', TEST_KEYS.bob.testnetAddress);
      await expect(page.getByTestId('banner-wallet-account-changed')).toBeVisible();
      expect(await wallet!.calls()).toHaveLength(0);
      expect(await mock.submitted(false)).toHaveLength(0);

      // the ticket is bound to the new account: it plans for bob and signs for bob
      await waitReviewable(page);
      await page.getByTestId('order-place').click();
      const placed = await placedBy(mock, await waitSubmitted(page));
      await expectWalletSigned(wallet!, placed);
      expect((await mock.ordersOf('bob', 'active')).length).toBe(1);
      expect((await mock.ordersOf('alice', 'active')).length).toBe(0);
    });

    test('a network switch disables the order button with the mismatch banner and recovers when the wallet switches back', async ({ appPage: page, mock, wallet }) => {
      await fund(mock, 'alice');
      await openMarket(page, mock, { wallet: id });
      await sellTicket(page);
      await expect(page.getByTestId('order-place')).toBeEnabled();

      await wallet!.setNetwork('mainnet');

      await expect(page.getByTestId('banner-network-mismatch')).toBeVisible();
      await expect(page.getByTestId('wallet-network')).toHaveText('mainnet');
      await expect(page.getByTestId('order-need-network')).toBeVisible();
      await expect(page.getByTestId('order-place')).toBeDisabled();
      expect(await wallet!.calls()).toHaveLength(0);

      await wallet!.setNetwork('testnet-10');

      await expect(page.getByTestId('banner-network-mismatch')).toHaveCount(0);
      // the wallet's network is shown only when it differs from the app's
      await expect(page.getByTestId('wallet-network')).toHaveCount(0);
      await expect(page.getByTestId('wallet-connected')).toHaveAttribute('data-network', 'testnet-10');
      await expect(page.getByTestId('banner-wallet-network-restored')).toBeVisible();
      await expect(page.getByTestId('order-need-network')).toHaveCount(0);
      // same key, same network: placing works again
      await clickPlace(page);
      await waitSubmitted(page);
      expect(await mock.submitted(false)).toHaveLength(1);
      expect(await wallet!.calls()).toHaveLength(1);
    });

    test('the wallet locks: the session closes with a banner, the ticket asks for a wallet, listeners are gone after disconnect', async ({ appPage: page, mock, wallet }) => {
      await fund(mock, 'alice');
      await openMarket(page, mock, { wallet: id });
      expect(await wallet!.listenerCount(), 'the app listens to the wallet while connected').toBeGreaterThan(0);
      await sellTicket(page);
      await expect(page.getByTestId('order-place')).toBeEnabled();

      await wallet!.lose();

      await expect(page.getByTestId('banner-wallet-lost')).toBeVisible();
      await expect(page.getByTestId('wallet-address')).toHaveCount(0);
      await expect(page.getByTestId('order-need-wallet')).toBeVisible();
      await expect(page.getByTestId('order-place')).toBeDisabled();
      expect(await wallet!.calls()).toHaveLength(0);
      expect(await mock.submitted(false)).toHaveLength(0);
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
