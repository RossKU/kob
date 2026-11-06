// The congestion note of the final confirmation: when the node's fee estimate shows contended blocks (the first normal bucket at least 10 percent above the
// relay floor of 100 sompi per gram) the order's confirmation says KOB transactions are ranked by their storage mass, which the normal fee does not cover, and
// where the one control is (Settings: "Pay the priority fee"). The ticket itself shows nothing. Nothing when the toggle is already on, when the network is
// quiet, or when the node has no estimate.
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { clickPlaceOnly, fillFields, pickType, waitReviewable } from '../helpers/ticket';

const BUSY = { priority: 211, normal: 194, low: 115 };

/** The confirmation of a plain buy, open. */
async function openConfirmation(page: Page, mock: MockClient): Promise<void> {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'limit', 'buy');
  await fillFields(page, { amount: '2', price: '0.0245' });
  await waitReviewable(page);
  await clickPlaceOnly(page);
  await expect(page.getByTestId('confirm-order')).toBeVisible();
}

const note = (page: Page) => page.getByTestId('confirm-order-note').filter({ hasText: 'Blocks are contended' });

test('contended blocks: the confirmation says so and names the setting; the ticket shows nothing', async ({ appPage: page, mock }) => {
  await mock.feeEstimate(BUSY);
  await openConfirmation(page, mock);
  await expect(note(page)).toContainText('ranked by their storage mass');
  await expect(note(page)).toContainText('priority fee is in Settings');
  await expect(page.getByTestId('order-congestion')).toHaveCount(0);
});

test('quiet blocks (every bucket at the relay floor, or within 10 percent of it): no note', async ({ appPage: page, mock }) => {
  await mock.feeEstimate({ priority: 109, normal: 100, low: 100 });
  await openConfirmation(page, mock);
  await expect(note(page)).toHaveCount(0);
});

test('a node without a fee estimate: no note', async ({ appPage: page, mock }) => {
  await mock.feeEstimate('off');
  await openConfirmation(page, mock);
  await expect(note(page)).toHaveCount(0);
});

test.describe('the priority fee is already on', () => {
  test.use({ appConfig: { features: { priorityFee: true } } });
  test('contended blocks: no note', async ({ appPage: page, mock }) => {
    await mock.feeEstimate(BUSY);
    await openConfirmation(page, mock);
    await expect(note(page)).toHaveCount(0);
  });
});
