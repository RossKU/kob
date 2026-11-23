// The operator's incident notice (config.json `incident`, docs/ops/web-hosting.md "The incident
// banner"): one warning banner at the top of every page, plain text, with no dismiss. It blocks
// nothing: an order is placed as usual. Against the mock stack.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType, placeOrder, waitReviewable } from '../helpers/ticket';

test.use({
  autoOpen: false,
  appConfig: { incident: { message: 'Indexer maintenance <b>now</b>: books may lag.', since: '2026-11-23T10:00:00Z', url: 'https://status.example.org/1' } },
});

test('the incident banner shows on every page as plain text, and an order is still placed', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  const banner = page.getByTestId('banner-incident');
  await expect(banner).toBeVisible();
  await expect(banner).toContainText('Service notice');
  // markup is text, not HTML
  await expect(page.getByTestId('banner-incident-text')).toHaveText('Indexer maintenance <b>now</b>: books may lag.');
  await expect(banner.locator('b')).toHaveCount(0);
  await expect(page.getByTestId('banner-incident-since')).toHaveText('(since 2026-11-23 10:00 UTC)');
  await expect(page.getByTestId('banner-incident-link')).toHaveAttribute('href', 'https://status.example.org/1');
  // no dismiss
  await expect(banner.getByRole('button')).toHaveCount(0);

  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
  const placed = await placeOrder(page, mock);
  expect(placed.submission.created).toHaveLength(1);

  await page.goto('/#/portfolio');
  await expect(page.getByTestId('banner-incident')).toBeVisible();
});
