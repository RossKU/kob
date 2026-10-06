// Chain re-organisations: a routine (shallow) one is silent, a deep one shows a subtle header indicator, and one that undoes something of the user's own
// (a fill they were told about) is a notification that says exactly what changed. There is no global re-org banner.
import type { Page } from '@playwright/test';
import { expect, test, TEST_KEYS, type MockClient } from '../fixtures';
import { TESTID, orderRow } from '../testids';

async function seedAsk(mock: MockClient, price = 2_505_000, tokens = 5) {
  const tok = await mock.token();
  await mock.seed({ orders: [{ token: tok.covenant_id, side: 'ask', maker: 'alice', price, amount: (BigInt(tokens) * 100_000_000n).toString() }] });
  const [order] = await mock.ordersOf('alice');
  return { tok, order };
}

async function closePanel(page: Page) {
  await page.getByTestId('notify-panel').getByRole('button', { name: 'Close' }).click();
  await expect(page.getByTestId('notify-panel')).toHaveCount(0);
}

/** Turns the in-app notifications on from the bell and waits for the first poll (the baseline) to be stored. */
async function enableNotifications(page: Page) {
  await page.getByTestId('notify-bell').click();
  await page.getByTestId('notify-enable').click();
  await closePanel(page);
  await expect
    .poll(() => page.evaluate((pk) => { try { return JSON.parse(localStorage.getItem(`kob.notify.testnet-10.${pk}`) ?? 'null')?.snaps != null; } catch { return false; } }, TEST_KEYS.alice.pubkey))
    .toBe(true);
}

test.describe('chain re-organisations', () => {
  test('a routine re-org is silent: no banner, no indicator, no notification, and the page keeps working', async ({ appPage, mock }) => {
    const { order } = await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await expect(appPage.getByTestId(orderRow(order.covenant_id))).toHaveAttribute('data-status', 'open');
    await enableNotifications(appPage);
    for (const blocks of [1, 2, 5, 30]) await mock.reorg(blocks);
    await appPage.waitForTimeout(1500);
    await expect(appPage.getByTestId('banner-reorg')).toHaveCount(0);
    await expect(appPage.getByTestId('reorg-indicator')).toHaveCount(0);
    await expect(appPage.getByText(/chain re-organised/i)).toHaveCount(0);
    await expect(appPage.getByTestId('notify-unread')).toHaveCount(0);
    await expect(appPage.getByTestId(orderRow(order.covenant_id))).toHaveAttribute('data-status', 'open');
  });

  test('a deep re-org shows a subtle header indicator with a tooltip, still no banner', async ({ appPage, mock }) => {
    await appPage.goto('/#/market');
    await expect(appPage.getByTestId('brand')).toBeVisible();
    await expect(appPage.getByTestId('reorg-indicator')).toHaveCount(0);
    await mock.reorg(45);
    const ind = appPage.getByTestId('reorg-indicator');
    await expect(ind).toBeVisible();
    await expect(ind).toHaveAttribute('title', /45 blocks/);
    await expect(appPage.getByTestId('banner-reorg')).toHaveCount(0);
  });

  test('a reverted fill of the user\'s own order is a notification naming the amount, the price and the state of the order', async ({ appPage, mock }) => {
    const { order } = await seedAsk(mock);
    await mock.fill(order.covenant_id, 2n * 100_000_000n, { taker: 'bob' });
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    const row = appPage.getByTestId(orderRow(order.covenant_id));
    await expect(row).toHaveAttribute('data-status', 'partial');
    await enableNotifications(appPage); // the baseline already holds the 2 filled EXKCC
    await expect(appPage.getByTestId('notify-unread')).toHaveCount(0);

    await mock.revertFill(order.covenant_id);
    await expect(appPage.getByTestId('notify-unread')).toHaveText('1', { timeout: 20_000 });
    await expect(row).toHaveAttribute('data-status', 'open'); // the data refreshed on its own
    await expect(appPage.getByTestId('banner-reorg')).toHaveCount(0);
    await expect(appPage.getByText(/chain re-organised/i)).toHaveCount(0);

    await appPage.getByTestId('notify-bell').click();
    const item = appPage.getByTestId('notify-item-reorg');
    await expect(item).toHaveCount(1);
    await expect(item).toContainText('Chain reorganisation changed your order');
    await expect(item).toContainText('Your fill of 2 EXKCC');
    await expect(item).toContainText(' at ');
    await expect(item).toContainText('was reverted by a chain reorganisation');
    await expect(item).toContainText('The order is open again');

    // the same state on the next polls does not repeat it
    await closePanel(appPage);
    await mock.reorg(1);
    await appPage.waitForTimeout(1500);
    await appPage.getByTestId('notify-bell').click();
    await expect(appPage.getByTestId('notify-item-reorg')).toHaveCount(1);
  });
});
