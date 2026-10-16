// The node's fee estimate in the mock stack: the mock node answers `GET /node/fee-estimate` (every bucket at the relay floor), so a page logs no error for
// it and the ticket's disclosure shows where the rate came from; a node without an estimate (`{off: true}`: 404) is the floor, said in the rate row.
// Either way the order goes through at that rate.
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType, placeOrder, readDisclosure, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

async function sellTicket(page: import('@playwright/test').Page) {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
}

test('the disclosure shows the node\'s fee estimate; the order is sent at that fee; nothing is logged as an error', async ({ appPage: page, mock, pageErrors }) => {
  await fund(mock);
  await openMarket(page, mock);
  await sellTicket(page);
  const disc = await readDisclosure(page);
  expect(disc.rows['fee-rate']).toContain('100 sompi per gram');
  expect(disc.rows['fee-rate']).toContain('the node suggests 100');
  await expect(page.getByTestId('disc-fee-speed')).toBeVisible();
  expect(disc.text).not.toContain('Fee estimate unavailable');
  const placed = await placeOrder(page, mock);
  // the network fee the disclosure named is the fee the node took
  expect(Number(disc.fee!.replace(/[^0-9.]/g, ''))).toBeCloseTo(Number(placed.submission.fee) / 1e8, 8);
  expect(pageErrors.filter((e) => e.startsWith('console.error')), 'console errors').toEqual([]);
});

test('a node without a fee estimate: the floor rate, said in the rate row, and the order still goes through', async ({ appPage: page, mock }) => {
  await mock.feeEstimate('off');
  await fund(mock);
  await openMarket(page, mock);
  await sellTicket(page);
  const disc = await readDisclosure(page);
  expect(disc.rows['fee-rate']).toContain('100 sompi per gram');
  expect(disc.rows['fee-rate']).toContain('the minimum rate (relay floor)');
  await expect(page.getByTestId('disc-fee-speed')).toHaveCount(0);
  const placed = await placeOrder(page, mock);
  expect(Number(disc.fee!.replace(/[^0-9.]/g, ''))).toBeCloseTo(Number(placed.submission.fee) / 1e8, 8);
});
