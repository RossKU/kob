// Backing out of the private-key signer's confirmation (Reject in the frame): the signer closes, nothing is sent, the ticket is usable again. Checked on a
// desktop and on a phone (390 px; the browser may drop the very first click into the freshly shown frame, a second one a moment later is taken: cancelReceipt taps again), for a listed order and for one below the minimum value (its confirmation carries the app's note, so it is taller).
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { KAS, fund, openConnect, openMarket } from '../helpers/env';
import { cancelReceipt, expectStopped, fillFields, pickType, waitReviewable, waitSubmitted, confirmReceipt } from '../helpers/ticket';

test.use({ walletId: null, autoOpen: false });

const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');

for (const width of [1280, 390]) {
  test.describe(`${width} px`, () => {
    test.use({ viewport: { width, height: width === 390 ? 844 : 720 } });

    test('Reject in the signer frame closes it and sends nothing, for a listed order and for one below the minimum; the next order still places', async ({ appPage: page, mock }) => {
      await fund(mock);
      await mock.minOrderValue(KAS);
      await openMarket(page, mock, { connect: false });
      if (width === 390) await openConnect(page, 'wallet-connect-local-key'); // the phone's menu
      else await page.getByTestId('wallet-connect-local-key').click();
      await expect(page.getByTestId('signer-overlay')).toBeVisible();
      await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
      await signer(page).getByTestId('signer-connect').click();
      await expect(page.getByTestId('wallet-address')).toBeVisible();
      await pickType(page, 'limit', 'sell');
      for (const [amount, price] of [['40', '0.0255'], ['38', '0.026']] as const) {
        await fillFields(page, { amount, price });
        await waitReviewable(page);
        await page.getByTestId('order-place').click();
        await expect(signer(page).getByTestId('signer-title')).toBeVisible({ timeout: 30_000 });
        await cancelReceipt(page);
        await expectStopped(page, 'rejected');
        await expect(page.getByTestId('signer-overlay')).toBeHidden();
        expect(await mock.submitted(false)).toHaveLength(0);
      }
      // the flow is not stuck: the same ticket places an order after the rejections
      await page.getByTestId('order-place').click();
      await expect(signer(page).getByTestId('signer-title')).toBeVisible({ timeout: 30_000 });
      await confirmReceipt(page);
      await waitSubmitted(page);
    });
  });
}
