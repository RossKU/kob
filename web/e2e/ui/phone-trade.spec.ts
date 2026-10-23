// The "Trade" button of the market page: phone only, scrolls to the order ticket and focuses its amount box. Mock stack.
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';
import { fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';

test.use({ autoOpen: false });

test.describe('phone, 390 px wide', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('the Trade button scrolls to the ticket and focuses its amount box', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const ticket = page.getByTestId('order-ticket');
    const jump = page.getByTestId('market-trade-jump');
    await expect(ticket).toBeAttached();
    await expect(ticket).not.toBeInViewport(); // below the book and the chart
    await expect(jump).toBeVisible();
    await expect(jump).toHaveText('Trade');
    const box = (await jump.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(390);
    expect(box.y + box.height).toBeLessThanOrEqual(844);
    await jump.click();
    await expect(page.getByTestId(TESTID.orderAmount)).toBeFocused();
    await expect(page.getByTestId(TESTID.orderAmount)).toBeInViewport();
    await expect(jump).toBeHidden(); // the ticket is on screen: no button
    expect(await horizontalOverflow(page), 'market page').toBe(0);
  });
});

test.describe('desktop, 1280 px wide', () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test('there is no Trade button', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await expect(page.getByTestId('market-trade-jump')).toBeHidden();
  });
});
