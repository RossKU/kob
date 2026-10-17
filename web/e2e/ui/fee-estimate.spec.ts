// The node's fee estimate in the mock stack: the mock node answers `GET /node/fee-estimate` (every bucket at the relay floor), so a page logs no error for
// it, and the final confirmation names the network fee the node then takes; a node without an estimate (`{off: true}`: 404) is the floor rate.
// Either way the order goes through at that rate. (The confirmation shows the fee only: where the rate came from is no longer on screen.)
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { formatKas } from '../../src/kob/units';
import { fillFields, pickType, placeOrderAndRead, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

async function sellTicket(page: import('@playwright/test').Page) {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
}

test('the final confirmation names the fee at the node\'s estimate; the order is sent at that fee; nothing is logged as an error', async ({ appPage: page, mock, pageErrors }) => {
  await fund(mock);
  await openMarket(page, mock);
  await sellTicket(page);
  const placed = await placeOrderAndRead(page, mock);
  // the network fee the confirmation named (full precision) is the fee the node took
  expect(placed.receipt.fee).toBe(`${formatKas(BigInt(placed.submission.fee))} KAS`);
  expect(pageErrors.filter((e) => e.startsWith('console.error')), 'console errors').toEqual([]);
});

test('a node without a fee estimate: the floor rate, and the order still goes through with the fee the confirmation named', async ({ appPage: page, mock }) => {
  await mock.feeEstimate('off');
  await fund(mock);
  await openMarket(page, mock);
  await sellTicket(page);
  const placed = await placeOrderAndRead(page, mock);
  expect(placed.receipt.fee).toBe(`${formatKas(BigInt(placed.submission.fee))} KAS`);
});
