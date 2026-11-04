// The congestion notice of the order ticket: when the node's fee estimate shows contended blocks (the first normal bucket at least 10 percent above the
// relay floor of 100 sompi per gram) a small dismissible notice under the order button says a transaction at the relay minimum may wait, with a link to
// the one control (Settings: "Pay the priority fee"). Nothing when the toggle is already on, when the network is quiet, or when the node has no estimate.
import { test, expect } from '../fixtures';
import { openMarket } from '../helpers/env';

const BUSY = { priority: 211, normal: 194, low: 115 };

test('contended blocks: a notice under the order button with a link to the priority-fee setting; dismissing hides it', async ({ appPage: page, mock }) => {
  await mock.feeEstimate(BUSY);
  await openMarket(page, mock);
  const notice = page.getByTestId('order-congestion');
  await expect(notice).toBeVisible();
  await expect(notice).toContainText('Blocks are contended');
  await expect(notice).toContainText('relay minimum may wait');
  // below the order button, so it never moves it
  const place = await page.getByTestId('order-place').boundingBox();
  const note = await notice.boundingBox();
  expect(note!.y).toBeGreaterThan(place!.y + place!.height - 1);

  await notice.getByRole('button').click();
  await expect(notice).toHaveCount(0);
});

test('the notice links to the Settings toggle, which is off and stays the only control', async ({ appPage: page, mock }) => {
  await mock.feeEstimate(BUSY);
  await openMarket(page, mock);
  await page.getByTestId('order-congestion-settings').click();
  await expect(page.getByTestId('settings-view')).toBeVisible();
  await expect(page.getByTestId('settings-priority-fee')).not.toBeChecked();
});

test('quiet blocks (every bucket at the relay floor, or within 10 percent of it): no notice', async ({ appPage: page, mock }) => {
  await mock.feeEstimate({ priority: 109, normal: 100, low: 100 });
  await openMarket(page, mock);
  await expect(page.getByTestId('order-place')).toBeVisible();
  await expect(page.getByTestId('order-congestion')).toHaveCount(0);
});

test('a node without a fee estimate: no notice', async ({ appPage: page, mock }) => {
  await mock.feeEstimate('off');
  await openMarket(page, mock);
  await expect(page.getByTestId('order-place')).toBeVisible();
  await expect(page.getByTestId('order-congestion')).toHaveCount(0);
});

test.describe('the priority fee is already on', () => {
  test.use({ appConfig: { features: { priorityFee: true } } });
  test('contended blocks: no notice', async ({ appPage: page, mock }) => {
    await mock.feeEstimate(BUSY);
    await openMarket(page, mock);
    await expect(page.getByTestId('order-place')).toBeVisible();
    await expect(page.getByTestId('order-congestion')).toHaveCount(0);
  });
});
