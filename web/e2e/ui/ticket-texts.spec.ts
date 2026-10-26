// Wording of the ticket and the settings that used to mislead: the amount help promised a cost line the ticket does not have, the day-order text named
// one country's time zone for everybody, a failed read blamed the node whatever failed, and the dynamic-fee hint promised a review screen.
import { expect, test } from '../fixtures';
import { connectWallet, fund, listExkcc, openMarket } from '../helpers/env';
import { pickType } from '../helpers/ticket';

test.use({ autoOpen: false });

test('the amount help does not promise a cost line, and the day-order text gives the reader\'s own time instead of a fixed zone', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'limit', 'buy');
  const form = page.getByTestId('order-ticket');
  await expect(form).not.toContainText('cost is shown below');
  await expect(form).toContainText('Amount of EXKCC to buy or sell.');
  await expect(form).not.toContainText('JST');
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
