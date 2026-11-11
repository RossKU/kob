// The Buy | Sell bar of the market page (review H-11, founder 2026-11-11): phone only, a bar at the bottom edge instead of a floating "Trade" button; each button takes the ticket's side, scrolls to the
// ticket and focuses its amount box; it is gone while the ticket is on screen; the page keeps room under its end so the bar covers no content. Mock stack.
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';
import { fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';

test.use({ autoOpen: false });

test.describe('phone, 390 px wide', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('Buy and Sell take the ticket\'s side, scroll to it and focus its amount box; the bar is gone while the ticket is on screen', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const ticket = page.getByTestId('order-ticket');
    const bar = page.getByTestId('market-trade-bar');
    await expect(ticket).toBeAttached();
    await expect(ticket).not.toBeInViewport(); // below the book and the chart
    await expect(bar).toBeVisible();
    await expect(page.getByTestId('market-trade-buy')).toHaveText('Buy');
    await expect(page.getByTestId('market-trade-sell')).toHaveText('Sell');
    // a bar at the bottom edge, the full width, not a button over the plot
    const box = (await bar.boundingBox())!;
    expect(box.x).toBe(0);
    expect(box.width).toBe(390);
    expect(box.y + box.height).toBeCloseTo(844, 0);
    expect(box.height).toBeLessThan(80);
    await page.getByTestId('market-trade-buy').click();
    await expect(page.getByTestId(TESTID.orderAmount)).toBeFocused();
    await expect(page.getByTestId(TESTID.orderAmount)).toBeInViewport();
    await expect(ticket).toHaveAttribute('data-side', 'buy');
    await expect(bar).toBeHidden(); // the ticket is on screen: no bar
    // back up: the bar returns, and Sell takes the other side
    await page.evaluate(() => window.scrollTo(0, 0));
    await expect(bar).toBeVisible();
    await page.getByTestId('market-trade-sell').click();
    await expect(ticket).toHaveAttribute('data-side', 'sell');
    await expect(page.getByTestId(TESTID.orderAmount)).toBeInViewport();
    expect(await horizontalOverflow(page), 'market page').toBe(0);
  });

  test('the bar covers no content: the end of the page has room under it', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await expect(page.getByTestId('market-trade-bar')).toBeVisible();
    // the end of the page: what loads under the grid (the activity log, the wallet's panel) makes it longer while it is scrolled, so scroll again until it does not grow
    let height = 0;
    for (let i = 0; i < 12; i++) {
      await page.evaluate(() => window.scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' }));
      await page.waitForTimeout(400);
      const now = await page.evaluate(() => document.documentElement.scrollHeight);
      if (now === height && (await page.evaluate(() => Math.round(scrollY + innerHeight) >= document.documentElement.scrollHeight - 1))) break;
      height = now;
    }
    // the last element of the page (the footer's last line) ends above the bar
    const barTop = (await page.getByTestId('market-trade-bar').boundingBox())!.y;
    const end = await page.evaluate(() => {
      const last = [...document.querySelectorAll('footer *')].filter((e) => e.getBoundingClientRect().height > 0).pop() ?? document.body;
      return last.getBoundingClientRect().bottom;
    });
    expect(end, 'the page end is not under the bar').toBeLessThanOrEqual(barTop + 1);
  });
});

test.describe('desktop, 1280 px wide', () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test('there is no Buy | Sell bar', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await expect(page.getByTestId('market-trade-bar')).toBeHidden();
  });
});
