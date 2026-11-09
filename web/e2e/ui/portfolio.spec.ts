// The Portfolio page (`#/portfolio`, ui/orders/OrdersView.tsx): four tabs over one panel, Balances first, each with its own address (`#/portfolio/<tab>`), so a
// reload or a link keeps the tab; the Fills tab lists every fill of the wallet's orders, one line each; the folded section of strays, automatic
// refunds and recovery under the tabs stays closed unless strays wait, and `#/portfolio/recover` (the footer's Recover link) opens it.
import { expect, test } from '../fixtures';
import { openMarket } from '../helpers/env';
import { seedOrders } from '../helpers/orders';

test.use({ autoOpen: false });

test('Balances first; Open orders, Order history and Fills each have their address and survive a reload', async ({ appPage: page, mock }) => {
  const [filled, open] = await seedOrders(mock, [
    { side: 'ask', price: 2_700_000, tokens: 2 },
    { side: 'ask', price: 2_800_000, tokens: 1 },
  ]);
  await mock.fill(filled!);
  await openMarket(page, mock, { route: '/portfolio' });
  await expect(page.locator('h1')).toHaveText('Portfolio');
  await expect(page.getByTestId('nav-portfolio')).toHaveText('Portfolio');
  await expect(page.getByTestId('orders-tab-balances')).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  await expect(page.getByTestId('orders-list')).toHaveCount(0);
  // the summary strip over the tabs: the total value, the KAS to spend and in orders, the open orders
  await expect(page.getByTestId('portfolio-open-count')).toHaveText('1');
  await expect(page.getByTestId('portfolio-total')).toContainText('KAS');
  await expect(page.getByTestId('portfolio-available')).toContainText('KAS');
  // KAS is the first row of the balances, the tokens follow with their icon and fingerprint
  const rows = page.getByTestId('token-balances-table').locator('tbody tr');
  await expect(rows.first()).toHaveAttribute('data-testid', 'kas-balance');
  await expect(page.locator('[data-testid^="balance-row-"] .fp').first()).toBeVisible();

  await page.getByTestId('orders-tab-active').click();
  await expect(page).toHaveURL(/#\/portfolio\/open$/);
  await expect(page.getByTestId(`order-row-${open}`)).toBeVisible();
  await expect(page.getByTestId(`order-row-${filled}`)).toHaveCount(0);
  await expect(page.getByTestId('balances-panel')).toHaveCount(0);
  // one line per order: market with its fingerprint, side, filled part, status and Cancel; the details open under it on demand
  const line = page.getByTestId(`order-line-${open}`);
  await expect(line).toContainText('EXKCC/KAS');
  await expect(line).toContainText('Sell');
  await expect(line).toContainText('0 / 1');
  await expect(line.locator('.fp')).toHaveCount(1);
  await expect(page.getByTestId(`order-cancel-${open}`)).toBeVisible();
  await expect(page.getByTestId(`order-details-${open}`)).toBeHidden();
  await page.getByTestId(`order-toggle-${open}`).click();
  await expect(page.getByTestId(`order-details-${open}`)).toBeVisible();
  await expect(page.getByTestId(`order-toggle-${open}`)).toHaveAttribute('aria-expanded', 'true');
  await page.getByTestId(`order-toggle-${open}`).click();
  await expect(page.getByTestId(`order-details-${open}`)).toBeHidden();

  await page.getByTestId('orders-tab-history').click();
  await expect(page).toHaveURL(/#\/portfolio\/history$/);
  await expect(page.getByTestId(`order-row-${filled}`)).toHaveAttribute('data-status', 'filled');
  await expect(page.getByTestId('orders-cancel-all')).toHaveCount(0);

  await page.getByTestId('orders-tab-fills').click();
  await expect(page).toHaveURL(/#\/portfolio\/fills$/);
  await expect(page.getByTestId('orders-tab-fills')).toContainText('(1)');
  const fill = page.locator(`[data-testid="wallet-fill-row"][data-order="${filled}"]`);
  await expect(fill.first()).toBeVisible();
  await expect(fill.first().getByTestId('wallet-fill-side')).toHaveText('Sell');
  await expect(fill.first()).toContainText('EXKCC/KAS');

  // the tab is in the address: a reload opens it again
  await page.reload();
  await expect(page.getByTestId('orders-tab-fills')).toHaveAttribute('aria-selected', 'true');
  // no strays: the folded section stays closed
  expect(await page.getByTestId('orders-settings').evaluate((e) => (e as HTMLDetailsElement).open)).toBe(false);
  await expect(page.getByTestId('orders-strays-notice')).toHaveCount(0);
});

test('#/portfolio/recover opens the page with its folded section of strays, automatic refunds and recovery open', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { route: '/portfolio/recover' });
  const box = page.getByTestId('orders-settings');
  await expect.poll(() => box.evaluate((e) => (e as HTMLDetailsElement).open)).toBe(true);
  await expect(page.getByTestId('recover-count')).toBeVisible();
  await expect(page.getByTestId('orders-auto-refund')).toBeVisible();
  // the folded section closes and opens again from its title
  await page.getByTestId('orders-settings-toggle').click();
  await expect.poll(() => box.evaluate((e) => (e as HTMLDetailsElement).open)).toBe(false);
});
