// A token the registry does not list (TN10 2026-10-17: the founder's own token read "13,500,000,000,000 base units" and "10 KAS per 100000000
// base units", taken for a wrong supply). Its amounts read in whole tokens: its decimals come from its declaration, its program's default, the scale of its
// order or market, else the issuing browser's issuance record (its ticker too, marked unverified), with a small marker by its name.
import { test, expect } from '../fixtures';
import { connectWallet, fund } from '../helpers/env';
import { openOrders } from '../helpers/orders';
import { cancelReceipt, clickPlaceOnly, fillFields, pickType, readReceipt, waitReviewable } from '../helpers/ticket';

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

test('no record: the amounts read in whole tokens on the scale of its order, the token named by its covenant id', async ({ appPage: page, mock }) => {
  const id = await seedToken(mock);
  await openOrders(page, mock);
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row).toHaveAttribute('data-status', 'open');
  await expect(row.getByTestId('order-token')).toHaveText(SHORT);
  await expect(row.getByTestId('token-decimals-note')).toHaveText('(8 decimals, from its market)');
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
  await expect(page.getByTestId('badge-family')).toHaveAttribute('title', /Amounts are whole tokens of 8 decimals/);
  const body = (await page.locator('main').textContent()) ?? '';
  expect(body).not.toMatch(/base units/);
  expect(body).not.toMatch(/KAS per \)|KAS per $|\(\)| of ,|\/ KAS/);
});

const OTHER = '84'.repeat(32);
const OTHER_SHORT = '84848484…84848484';
const tokenSpec = { ticker: '', covenant_id: OTHER, program: 'KCC20Ref', template_id: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null };

test('a market of 6 decimals with nothing on chain stating them: the final confirmation reads the order as the ticket did, never on invented decimals', async ({ appPage: page, mock }) => {
  await mock.seed({ tokens: [{ ...tokenSpec, order_scale: 1_000_000 }], orders: [{ token: OTHER, side: 'ask', maker: 'maker', amount: '4000000', price: 900_000_000 }] });
  await fund(mock, 'alice', { tokens: 0n });
  await mock.giveTokens('alice', 5_000_000n, { token: OTHER });
  await page.goto(`/#/market/${OTHER}`);
  await connectWallet(page);
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { price: '10', amount: '2' });
  await waitReviewable(page);
  await clickPlaceOnly(page);
  const receipt = await readReceipt(page);
  expect(receipt.orders[0]!.token).toBe(`${OTHER_SHORT} (6 decimals, from its price scale)`);
  expect(receipt.orders[0]!.rows).toMatchObject({ Amount: `2 ${OTHER_SHORT}`, 'Limit price': `10 KAS per ${OTHER_SHORT}` });
  await cancelReceipt(page);
});

test('a token nothing states the decimals of (no declaration, no program default, no order): its balance is its raw amount, said so; no decimals are invented', async ({ appPage: page, mock }) => {
  await mock.seed({ tokens: [{ ...tokenSpec, order_scale: null }] });
  await fund(mock, 'alice', { tokens: 0n });
  await mock.giveTokens('alice', 1_234_567n, { token: OTHER });
  await openOrders(page, mock);
  const row = page.getByTestId(`balance-row-${OTHER}`);
  await expect(row).toContainText(OTHER_SHORT);
  await expect(row).toContainText('1,234,567');
  await expect(row.getByTestId('token-decimals-note')).toHaveText('(raw amount)');
  await expect(page.getByTestId('orders-view')).not.toContainText(/decimals assumed|base units/);
});
