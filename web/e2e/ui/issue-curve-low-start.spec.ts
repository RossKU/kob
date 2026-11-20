// Create token, "Sell on a curve", chromatic: a start price too low for a higher price at every
// semitone is refused by the protocol's table, and the form says why in plain words (the review
// stays off). 10^9 tokens for 50 KAS would start at a few sompi. Mock stack.
import { mkdirSync } from 'node:fs';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption } from '../helpers/env';

test.use({ autoOpen: false });

test('a chromatic start too low for distinct steps is refused with a plain reason', async ({ appPage: page, mock }) => {
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await page.getByTestId('issue-name').fill('Many Coin');
  await page.getByTestId('issue-ticker').fill('MANY');
  await page.getByTestId('issue-supply').fill('1000000000');
  await page.getByTestId('issue-after-curve').click();
  await page.getByTestId('sale-shape-chromatic').click();
  await page.getByTestId('sale-total').fill('50');
  const err = page.getByTestId('sale-curve-error');
  const dir = process.env.KOB_SHOTS;
  if (dir) mkdirSync(dir, { recursive: true });
  const shot = async () => {
    if (!dir) return;
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.getByTestId('issue-sale').scrollIntoViewIfNeeded();
    await page.getByTestId('issue-sale').screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-issue-curve-low-start.png` });
  };
  await page.waitForTimeout(500);
  await shot();
  await expect(err).toBeVisible();
  await expect(err).toHaveText('The first price is too low for this curve. Raise the total or sell fewer tokens.');
  await expect(page.getByTestId('issue-review')).toBeDisabled();
  // a total that starts high enough takes the curve again
  await page.getByTestId('sale-total').fill('10000');
  await expect(err).toHaveCount(0);
  await expect(page.getByTestId('sale-graph')).toHaveAttribute('data-steps', '25');
});
