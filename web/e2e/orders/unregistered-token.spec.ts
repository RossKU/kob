// A token the registry does not list (TN10 2026-10-17: the founder's own token read "13,500,000,000,000 base units" and "10 KAS per 100000000
// base units", taken for a wrong supply). Founder decision: never "base units" in front of a user. Its decimals come from the registry, else the
// issuing browser's issuance record (its ticker too, marked unverified), else the assumed 8 (the Kaspa convention: 1e8 base units = 1 token),
// with a small marker by its name.
import { test, expect } from '../fixtures';
import { connectWallet } from '../helpers/env';
import { openOrders } from '../helpers/orders';

const ID = '83'.repeat(32);
const SHORT = '83838383…83838383';

async function seedToken(mock: Parameters<typeof openOrders>[1]): Promise<string> {
  await mock.seed({
    tokens: [{ ticker: '', covenant_id: ID, program: 'KCC20Ref', template_id: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null, order_scale: 100_000_000 }],
    // the founder's sell order: 45,000 tokens (8 decimals) at 10 KAS each
    orders: [{ token: ID, side: 'ask', maker: 'alice', amount: '4500000000000', price: 1_000_000_000 }],
  });
  const mine = await mock.ordersOf('alice');
  return mine.find((o: { token: string }) => o.token === ID)!.covenant_id;
}

test('no record: the amounts read in whole tokens on 8 assumed decimals, the token named by its covenant id and marked "decimals assumed 8"', async ({ appPage: page, mock }) => {
  const id = await seedToken(mock);
  await openOrders(page, mock);
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row).toHaveAttribute('data-status', 'open');
  await expect(row.getByTestId('order-token')).toHaveText(SHORT);
  await expect(row.getByTestId('token-decimals-note')).toHaveText('(decimals assumed 8)');
  await expect(row.getByTestId('order-amount')).toContainText(`45,000 / 45,000 ${SHORT}`);
  await expect(row.getByTestId('order-price')).toContainText(new RegExp(`^10\\s*KAS per ${SHORT}`));
  await expect(page.getByTestId('orders-view')).not.toContainText(/base units/);
});

test('the issuing browser\'s record: its ticker and decimals, marked unverified; nothing reads "base units"', async ({ appPage: page, mock }) => {
  const id = await seedToken(mock);
  await openOrders(page, mock);
  await page.evaluate((k) => localStorage.setItem('kob.issuedDecimals.v1', JSON.stringify({ 'testnet-10': { [k]: { decimals: 8, ticker: 'XYZ', name: 'Xyz coin' } } })), ID);
  await page.reload();
  if (!(await page.getByTestId('wallet-address').isVisible())) await connectWallet(page);
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row).toHaveAttribute('data-status', 'open', { timeout: 20_000 });
  await expect(row.getByTestId('order-token')).toHaveText(`XYZ (${SHORT})`);
  await expect(row.getByTestId('token-decimals-note')).toHaveText('(unverified)');
  await expect(row.getByTestId('order-amount')).toContainText('45,000 / 45,000 XYZ');
  await expect(row.getByTestId('order-price')).toContainText(/^10\s*KAS per XYZ/);
  await expect(page.getByTestId('orders-view')).not.toContainText(/base units/);
});

test('its market page names it everywhere (title, stats, book, chart, hints) and never says "base units"', async ({ appPage: page, mock }) => {
  await seedToken(mock);
  await page.goto(`/#/market/${ID}`);
  await expect(page.getByTestId('order-book')).toBeVisible();
  await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', `${SHORT}/KAS`);
  await expect(page.getByTestId('order-book')).toContainText(`Price (KAS per ${SHORT})`);
  await expect(page.getByTestId('order-book')).toContainText(`Size (${SHORT})`);
  await expect(page.getByTestId('chart-unit')).toHaveText(`KAS per ${SHORT}`);
  await expect(page.getByTestId('token-open-caution')).toContainText('amounts are whole tokens of 8 decimals');
  const body = (await page.locator('main').textContent()) ?? '';
  expect(body).not.toMatch(/base units/);
  expect(body).not.toMatch(/KAS per \)|KAS per $|\(\)| of ,|\/ KAS/);
});
