// My orders with a slow node (TN10 2026-10-17: ~30 s of spinner on the founder's phone). The browser holds placement records of orders that have
// ended (filled): the indexer describes them (their placement state), so none is looked up on the node, and the list never waits for the node:
// it shows from the indexer at once, the node's answers (its clock, records the indexer does not list) follow in the background.
import { test, expect } from '../fixtures';
import { openOrders, seedOrders } from '../helpers/orders';

test('ended orders of the browser\'s records do not hold My orders back: the list shows from the indexer within a second with a node that answers in 8 s', async ({ appPage: page, mock }) => {
  test.setTimeout(120_000);
  const ids = await seedOrders(mock, [1, 2, 3, 4, 5].map((n) => ({ side: 'ask' as const, price: 2_700_000 + n * 10_000, tokens: 1 })));
  // My orders writes a placement record for every live order of the wallet (orders-data.ts syncRecords): the browser now holds five
  await openOrders(page, mock);
  for (const id of ids) await expect(page.getByTestId(`order-row-${id}`)).toHaveAttribute('data-status', 'open');
  const records = await page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith('kob.records.v1:')).map((k) => Object.keys(JSON.parse(localStorage.getItem(k) ?? '{}').records ?? JSON.parse(localStorage.getItem(k) ?? '{}')).length));
  expect(records.reduce((a, b) => a + b, 0), 'the browser holds records of the orders').toBeGreaterThanOrEqual(ids.length);
  // they all end (filled): the indexer lists them without a current state (state_known false)
  for (const id of ids) await mock.fill(id);
  for (const id of ids) await mock.until(() => mock.order(id), (o) => o.status === 'filled');
  // the node now answers in 8 s (a slow tunnel); the indexer at once
  await mock.latency(8_000, { scope: 'node' });
  await page.goto('/#/market');
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  const t0 = Date.now();
  await page.goto('/#/orders');
  await page.getByTestId('orders-tab-history').click();
  for (const id of ids) await expect(page.getByTestId(`order-row-${id}`)).toHaveAttribute('data-status', 'filled', { timeout: 30_000 });
  const shownMs = Date.now() - t0;
  await expect(page.getByTestId('orders-refresh')).toBeEnabled();
  await expect(page.getByTestId('orders-refresh')).not.toHaveAttribute('aria-busy', 'true');
  test.info().annotations.push({ type: 'my-orders', description: `list shown ${shownMs} ms after opening, node latency 8000 ms` });
  console.log(`my-orders: list shown ${shownMs} ms after opening (node latency 8000 ms)`);
  expect(shownMs, 'the list does not wait for the node').toBeLessThan(2_500);
  // the tab counts are the rows each tab lists
  const count = async (tab: string) => Number(/\((\d+)\)/.exec((await page.getByTestId(`orders-tab-${tab}`).textContent()) ?? '')?.[1] ?? -1);
  const rows = async () => page.getByTestId('orders-list').locator(':scope > [data-testid^="order-row-"], :scope > [data-testid^="position-"]').count();
  expect(await count('history')).toBe(await rows());
  expect(await count('history')).toBeGreaterThanOrEqual(ids.length);
  await page.getByTestId('orders-tab-all').click();
  expect(await count('all')).toBe(await rows());
  await page.getByTestId('orders-tab-active').click();
  expect(await count('active')).toBe(await rows());
  await mock.latency(0);
});
