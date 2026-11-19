// A curve sell order (KobCurveAsk) in the app: its levels in the book with a curve mark, one row in
// Open orders with its level under the price, the remaining-levels graph in its details, a buy
// filling a level (the matcher's fill, mock), and its cancel. Mock stack.
import { test, expect, type MockClient } from '../fixtures';
import { TOKEN, connectWallet, listExkcc } from '../helpers/env';
import { openOrderRow, runFlow } from '../helpers/orders';

test.use({ autoOpen: false, viewport: { width: 1280, height: 1000 } });

/**
 * alice's 20-level curve of 100 EXKCC (2.55 KAS if sold out, last level 1.04x the first: its levels
 * among the plain asks) with 3 levels sold. Returns its id.
 */
async function seedCurve(mock: MockClient): Promise<string> {
  const tok = await mock.token();
  await mock.seed({ orders: [{ token: tok.covenant_id, kind: 'KobCurveAsk', maker: 'alice', amount: (100n * TOKEN).toString(), levels: 20, total: '255000000', multiple: '1.04' }] });
  const id = (await mock.ordersOf('alice')).find((o: { contract: string }) => o.contract === 'KobCurveAsk')!.covenant_id as string;
  await mock.fill(id, 15n * TOKEN, { taker: 'bob' });
  return id;
}

test('the book marks a curve order\'s levels; Open orders shows one row with its level; its details draw the levels left; a fill moves it on; cancel', async ({ appPage: page, mock, wallet }) => {
  test.setTimeout(120_000);
  const id = await seedCurve(mock);
  const view = await mock.order(id);
  expect(view.curve).toMatchObject({ level: 3, levels: 20, unbound: false });
  expect(view.expiry_daa).toBeNull();

  // The book: the curve's remaining levels are levels like any other, each with the mark and its
  // level in the title.
  await listExkcc(page);
  const tok = await mock.token();
  await page.goto(`/#/market/${tok.covenant_id}`);
  await connectWallet(page, 'kasware');
  const book = page.getByTestId('order-book');
  const marked = book.locator('[data-testid=book-ask-row][data-curve]');
  await expect(marked.first()).toBeVisible();
  // The levels from 4 on (1-based), one per row. The plain asks carry no mark.
  const levels = await marked.evaluateAll((els) => els.map((e) => e.getAttribute('data-curve')));
  expect(levels).toContain('4');
  expect(levels).toContain('5');
  await expect(book.locator('[data-testid=book-ask-row][data-curve="4"]')).toHaveAttribute('title', /Level 4 of a curve order/);
  await expect(book.locator('[data-testid=book-ask-row]:not([data-curve]) [data-testid=book-curve-mark]')).toHaveCount(0);
  await expect(book.locator('[data-testid=book-ask-row][data-curve="4"] [data-testid=book-curve-mark]')).toHaveCount(1);

  // Open orders: one row for the curve, "Level 4 / 20" under its price.
  await page.goto('/#/portfolio/open');
  await expect(page.getByTestId('orders-list')).toBeVisible();
  const rows = page.locator(`[data-testid=order-row-${id}]`);
  await expect(rows).toHaveCount(1);
  await expect(rows).toHaveAttribute('data-type', 'curve');
  await expect(page.getByTestId(`order-curve-level-${id}`)).toHaveText('Level 4 / 20');
  // Its details: the graph of the levels left and the prices.
  await openOrderRow(page, id);
  const details = page.getByTestId(`order-curve-${id}`);
  await expect(details.getByTestId('order-curve-graph')).toBeVisible();
  await expect(details.getByTestId('order-curve-graph')).toHaveAttribute('data-steps', '20');
  await expect(details.getByTestId('order-curve-left')).toContainText('17 of 20');
  await expect(page.getByTestId(`order-details-${id}`).getByTestId('order-price')).toContainText('0.0');

  // A buy fills two more levels (the matcher's fill, mock): the row moves on.
  await mock.fill(id, 10n * TOKEN, { taker: 'bob' });
  await page.getByTestId('orders-refresh').click();
  await expect(page.getByTestId(`order-curve-level-${id}`)).toHaveText('Level 6 / 20', { timeout: 15_000 });

  // Cancel: the custody (75 EXKCC) and the carriers come back to alice.
  await page.getByTestId(`order-cancel-${id}`).click();
  const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
  expect(placed!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
  const alice = (await mock.balance('alice')).pubkey;
  const back = placed!.submission.token_outputs.filter((o) => o.role === 'owned' && o.owner === alice).reduce((a, o) => a + BigInt(o.amount), 0n);
  expect(back).toBe(75n * TOKEN);
  expect((await mock.order(id)).status).toBe('cancelled');
});
