// The node's fee estimate in the mock stack: the mock node answers `GET /node/fee-estimate` (every bucket at the relay floor), so a page logs no error for
// it and the confirmation screen shows where the rate came from; a node without an estimate (`{off: true}`: 404) is the floor, said explicitly.
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

async function openConfirm(page: import('@playwright/test').Page) {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
  await page.getByTestId('order-review').click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
}

test('the confirmation screen shows the node\'s fee estimate; nothing is logged as an error', async ({ appPage: page, mock, pageErrors }) => {
  await fund(mock);
  await openMarket(page, mock);
  await openConfirm(page);
  await expect(page.getByTestId('confirm-fee-fee-rate')).toContainText('100 sompi per gram');
  await expect(page.getByTestId('confirm-fee-fee-rate')).toContainText('the node suggests 100');
  await expect(page.getByTestId('confirm-fee-fee-speed')).toBeVisible();
  await expect(page.getByTestId('confirm-screen')).not.toContainText('Fee estimate unavailable');
  expect(pageErrors.filter((e) => e.startsWith('console.error')), 'console errors').toEqual([]);
});

test('a node without a fee estimate: the floor rate, and the screen says the estimate is unavailable', async ({ appPage: page, mock }) => {
  await mock.feeEstimate('off');
  await fund(mock);
  await openMarket(page, mock);
  await openConfirm(page);
  await expect(page.getByTestId('confirm-fee-fee-rate')).toContainText('the minimum rate (relay floor)');
  await expect(page.getByTestId('confirm-warnings')).toContainText('Fee estimate unavailable: paying the minimum rate (100 sompi per gram)');
});
