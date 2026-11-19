// Wording of the ticket and the settings.
// - The amount help does not promise a cost line the ticket does not have.
// - The day-order text names no one country's time zone for everybody.
// - A failed read does not blame the node whatever failed.
// - The dynamic-fee hint does not promise a review screen.
import { expect, test } from '../fixtures';
import { TOKEN, connectWallet, fund, listExkcc, openMarket } from '../helpers/env';
import { fillFields, pickType, placeOrder, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

test('the amount box has no help line to promise a cost line, and the day-order text gives the reader\'s own time instead of a fixed zone', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'limit', 'buy');
  const form = page.getByTestId('order-ticket');
  await expect(form).not.toContainText('cost is shown below');
  // No help line under the amount box at all: the label and the unit say what it is. The lines
  // under it are Available and the cost.
  await expect(form).not.toContainText('Amount of EXKCC to buy or sell.');
  await expect(form).not.toContainText('JST');
  // The field help is off at first (its long text is the field's tooltip). The "?" shows it under
  // the fields.
  await page.getByTestId('order-help-toggle').click();
  await expect(form.getByText(/A day order ends by 00:00 UTC \(\d\d:\d\d \S+ for you\)\./)).toBeVisible();
});

test('a node that does not answer is named as the node', async ({ appPage: page, mock }) => {
  await fund(mock);
  await listExkcc(page);
  await page.route('**/node/utxos', (route) => route.abort());
  const token = await mock.token();
  await page.goto(`/#/market/${token.covenant_id}`);
  await connectWallet(page);
  const env = page.getByTestId('order-need-env');
  await expect(env).toBeVisible();
  await expect(env).toContainText('Node not answering: orders cannot be planned yet.');
});

test('the dynamic fee hint says where the fee is shown and that the rate is not', async ({ appPage: page }) => {
  await page.goto('/#/settings');
  const hint = page.getByTestId('settings-view');
  await expect(hint).not.toContainText('review screen');
  await expect(hint).toContainText('The fee is shown where you approve');
  await expect(hint).toContainText('the rate itself is not shown');
});

test('a key-owned holding with a borrow setting counts in Available and sells on the owner path; its change keeps the setting', async ({ appPage: page, mock }) => {
  await fund(mock, 'alice', { tokens: 5n });
  // Borrow scheme 2 (a guard key), as a TN10 holding had: spendable by its owner like a plain one
  // (option A).
  await mock.giveTokens('alice', 7n * TOKEN, { borrow_scheme: 2 });
  const token = await openMarket(page, mock);
  await pickType(page, 'limit', 'sell');
  await expect(page.getByTestId('order-available')).toContainText('12');
  await expect(page.getByTestId('order-unusable')).toHaveCount(0);
  // 6 is more than the plain 5: the sale takes the borrow-enabled 7 alone, never both kinds in one
  // transaction.
  await fillFields(page, { amount: '6', price: '0.2' });
  await waitReviewable(page);
  await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
  await page.screenshot({ path: test.info().outputPath('sell-ticket-borrow-holding.png'), animations: 'disabled' });
  const placed = await placeOrder(page, mock);
  expect(placed.submission.created).toHaveLength(1);
  const alice = (await mock.balance('alice')).pubkey;
  const owned = ((await mock.get(`/v1/token-utxos?owner=${alice}&token=${token}&spent=false&limit=50`)).items as { amount: string; role: string; state: { borrow_scheme: number } }[]).filter((u) => u.role === 'owned');
  // The plain 5 untouched. The change of 1 keeps borrow scheme 2.
  expect(owned.map((u) => [BigInt(u.amount), u.state.borrow_scheme]).sort((a, b) => Number(a[0]) - Number(b[0]))).toEqual([[1n * TOKEN, 2], [5n * TOKEN, 0]]);
  // The custody of the new ask is plain.
  const custody = ((await mock.get(`/v1/token-utxos?owner=${placed.submission.created[0]!.covenantId}&token=${token}&spent=false`)).items as { role: string; state: { borrow_scheme: number } }[]);
  expect(custody.map((u) => [u.role, u.state.borrow_scheme])).toEqual([['custody', 0]]);
});
