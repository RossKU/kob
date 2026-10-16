// Dictionary leaks and layout: the whole ticket + inline status + My orders flow with no untranslated dictionary key anywhere (and the trading terms in
// English), and the same flow on a 390 px phone (no horizontal scroll, the key controls reachable, the status lines fit the screen).
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { expectNoLeakedKeys, horizontalOverflow } from '../helpers/i18n';
import { runFlow } from '../helpers/orders';
import { expectWalletSigned, fillFields, pickType, placedBy, placeOrder, readDisclosure, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

test.describe('English text', () => {
  test('the ticket: every order type has a name, help and fields, and no dictionary key leaks', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await expect(page.getByTestId('order-ticket')).toContainText('Order ticket');
    const names = await page.getByTestId('order-type').locator('option').allTextContents();
    expect(names).toHaveLength(18);
    for (const term of ['Limit', 'Market', 'Stop (market)', 'Stop-limit', 'Trailing stop', 'Take-profit', 'OCO', 'IFD', 'IFO', 'Repeat', 'TWAP', 'DCA']) {
      expect(names.join('|'), `order type list has ${term}`).toContain(term);
    }
    const types = await page.getByTestId('order-type').locator('option').evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value));
    for (const type of types) {
      await pickType(page, type);
      await expect(page.getByTestId('order-type-help')).not.toBeEmpty();
      await page.getByTestId('order-advanced').locator('summary').click(); // the advanced fields are checked too
      await expectNoLeakedKeys(page, `the ${type} ticket`);
      await page.getByTestId('order-advanced').locator('summary').click();
    }
    await expectNoLeakedKeys(page, 'the market page');
  });

  test('a stop order and a limit order end to end: disclosure, inline status, My orders and cancel', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);

    // stop order: the trigger terms
    await pickType(page, 'stopMarket', 'sell');
    await fillFields(page, { amount: '2', stop: '0.023' });
    await waitReviewable(page);
    const disc = page.getByTestId('order-disclosure');
    await expect(disc).toContainText(/stop/i);
    expect((await readDisclosure(page)).summary).toContain('Sell 2 EXKCC');
    await expectNoLeakedKeys(page, 'the stop-order disclosure');
    await expect(page.getByTestId('order-place')).toHaveText('Sell Stop (market)');

    // market order
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '1' });
    await waitReviewable(page);
    expect((await readDisclosure(page)).summary).toContain('Buy 1 EXKCC');
    await expect(disc).toContainText(/market/i);
    await expectNoLeakedKeys(page, 'the market-order disclosure');

    // a limit order, placed with one click
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.027' });
    await waitReviewable(page);
    await expectNoLeakedKeys(page, 'the limit-order disclosure');
    await expect(page.getByTestId('order-place')).toHaveText('Sell Limit');
    const placed = await placeOrder(page, mock);
    await expectWalletSigned(wallet!, placed);
    await expect(page.getByTestId('order-status').getByTestId('tx-status')).toContainText('Accepted');
    await expectNoLeakedKeys(page, 'the order status');
    const id = placed.views[0].covenant_id;

    // My orders
    await page.getByTestId('nav-orders').click();
    await expect(page.locator('h1')).toHaveText('My orders');
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toBeVisible();
    await expect(page.getByTestId(`order-type-${id}`)).toHaveText('Limit (GTC)');
    await expect(row.getByTestId('order-status')).toHaveText('open');
    await expect(page.getByTestId(`order-cancel-${id}`)).toHaveText('Cancel');
    await expectNoLeakedKeys(page, 'My orders');

    // cancel
    await page.getByTestId(`order-cancel-${id}`).click(); // no confirmation: the flow starts and the wallet is asked at once
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(step!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
    await expect(page.getByTestId('flow-done')).toBeVisible();
    await expectNoLeakedKeys(page, 'the finished flow');
    await page.getByTestId('orders-tab-history').click();
    await expect(page.getByTestId(`order-row-${id}`).getByTestId('order-status')).toHaveText('cancelled');
  });

  test('every page of the app is free of leaked keys', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    for (const route of ['/market', '/orders', '/issue', '/settings']) {
      await page.goto(`/#${route}`);
      await expect(page.locator('main, [data-testid$="-view"], .container').first()).toBeVisible();
      await expectNoLeakedKeys(page, route);
    }
  });
});

test.describe('phone, 390 px wide', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('the market page, the ticket and the status lines fit the screen and the order can be placed', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    expect(await horizontalOverflow(page), 'market page').toBe(0);
    // key controls are reachable (visible after scrolling, clickable)
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.027', tip: '0.0005' });
    await waitReviewable(page);
    expect(await horizontalOverflow(page), 'ticket with disclosure').toBe(0);
    await expect(page.getByTestId('order-disclosure')).toBeVisible();
    await page.getByTestId('order-place').scrollIntoViewIfNeeded();
    await page.getByTestId('order-place').click();
    // the confirmed status line under the button stays inside the screen
    const status = page.getByTestId('order-status');
    await expect(status).toBeVisible();
    await expect(status.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
    const box = (await status.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390 + 1);
    expect(await horizontalOverflow(page), 'order status').toBe(0);
    const txid = (await status.getByTestId('tx-id').getAttribute('data-value'))!;
    const placed = await placedBy(mock, txid);
    expect(await wallet!.calls()).toHaveLength(1);

    // My orders and its cancel action: the flow starts at once
    await page.getByTestId('nav-orders').click();
    const id = placed.views[0].covenant_id;
    await expect(page.getByTestId(`order-row-${id}`)).toBeVisible();
    expect(await horizontalOverflow(page), 'My orders').toBe(0);
    await page.getByTestId(`order-cancel-${id}`).scrollIntoViewIfNeeded();
    await page.getByTestId(`order-cancel-${id}`).click();
    await runFlow(page, mock, wallet, { steps: 1 });
    expect(await horizontalOverflow(page), 'cancel flow').toBe(0);
    const flow = (await page.getByTestId('flow-status').boundingBox())!;
    expect(flow.x).toBeGreaterThanOrEqual(0);
    expect(flow.x + flow.width).toBeLessThanOrEqual(390 + 1);
  });

  test('every page of the app has no horizontal scroll on a phone', async ({ appPage: page, mock }) => {
    await fund(mock);
    const tokenId = await openMarket(page, mock);
    for (const route of ['/market', '/orders', '/issue', '/settings']) {
      await page.goto(`/#${route}`);
      await expect(page.getByTestId('nav-market')).toBeVisible();
      expect(await horizontalOverflow(page), route).toBe(0);
    }
    // a heavy ticket: the if-done order with all its fields and the disclosure
    await page.goto(`/#/market/${tokenId}`);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await pickType(page, 'repeatIfo', 'buy');
    // a 1 EXKCC minimum fill keeps the repeat merge tip at 0.01 KAS per token (see cases.ts, repeat section)
    await fillFields(page, { amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04', 'exit.stop': '0.023' });
    await waitReviewable(page);
    expect(await horizontalOverflow(page), 'repeat IFO ticket').toBe(0);
    const placed = await placeOrder(page, mock);
    expect(placed.submission.created).toHaveLength(1);
    expect(placed.submission.created[0]!.kind).toBe('KobIfdBid');
    expect(await horizontalOverflow(page), 'repeat IFO ticket with its status').toBe(0);
  });
});
