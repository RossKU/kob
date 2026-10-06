// Dictionary leaks and layout: the whole ticket + confirmation + My orders flow with no untranslated dictionary key anywhere (and the trading terms in
// English), and the same flow on a 390 px phone (no horizontal scroll, the key controls reachable, the confirmation dialog fits the screen).
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { expectNoLeakedKeys, horizontalOverflow } from '../helpers/i18n';
import { runFlow } from '../helpers/orders';
import { acknowledgeAndSign, fillFields, openReview, pickType, placedBy, readConfirm, reviewAndSign, waitReviewable } from '../helpers/ticket';

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

  test('a stop order and a limit order end to end: disclosure, confirmation, signing result, My orders and cancel', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);

    // stop order: the trigger terms and the confirmation title
    await pickType(page, 'stopMarket', 'sell');
    await fillFields(page, { amount: '2', stop: '0.023' });
    await waitReviewable(page);
    const disc = page.getByTestId('order-disclosure');
    await expect(disc).toContainText(/stop/i);
    await expectNoLeakedKeys(page, 'the stop-order disclosure');
    const stop = await openReview(page);
    expect(stop.created[0]!.title).toBe('Sell stop order');
    await expectNoLeakedKeys(page, 'the stop-order confirmation');
    await page.getByTestId('confirm-cancel').click();

    // market order
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '1' });
    await waitReviewable(page);
    const market = await openReview(page);
    expect(market.created[0]!.title).toContain('market order');
    expect(market.created[0]!.title).toContain('Buy');
    await page.getByTestId('confirm-cancel').click();

    // a limit order, signed
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.027' });
    await waitReviewable(page);
    await expectNoLeakedKeys(page, 'the limit-order disclosure');
    const confirm = await openReview(page);
    expect(confirm.created[0]!.title).toBe('Sell limit order');
    expect(confirm.heading).toMatch(/^Place/);
    await expect(page.getByTestId('confirm-wallet-notice')).toContainText('wallet');
    await expectNoLeakedKeys(page, 'the limit-order confirmation');
    const txid = await acknowledgeAndSign(page);
    await expect(page.getByTestId('tx-status')).toContainText('Accepted');
    await expectNoLeakedKeys(page, 'the signing result');
    const placed = await placedBy(mock, txid);
    const id = placed.views[0].covenant_id;
    await page.getByTestId('confirm-close').click();

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
    await page.getByTestId(`order-cancel-${id}`).click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    const cancel = await readConfirm(page);
    expect(cancel.heading).toMatch(/cancel/i);
    expect(cancel.kind).toBe('cancel');
    await expectNoLeakedKeys(page, 'the cancel confirmation');
    await page.getByTestId('confirm-cancel').click();
    await page.getByTestId(`order-cancel-${id}`).click();
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(step!.placed.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
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

  test('the market page, the ticket and the confirmation dialog fit the screen and the order can be placed', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    expect(await horizontalOverflow(page), 'market page').toBe(0);
    // key controls are reachable (visible after scrolling, clickable)
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.027', tip: '0.0005' });
    await waitReviewable(page);
    expect(await horizontalOverflow(page), 'ticket with disclosure').toBe(0);
    await expect(page.getByTestId('order-disclosure')).toBeVisible();
    await page.getByTestId('order-review').scrollIntoViewIfNeeded();
    await page.getByTestId('order-review').click();
    const dialog = page.getByTestId('confirm-screen');
    await expect(dialog).toBeVisible();
    const box = (await dialog.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390 + 1);
    expect(await horizontalOverflow(page), 'confirmation dialog').toBe(0);
    // the dialog scrolls inside itself: acknowledge and sign are reachable
    await page.getByTestId('confirm-ack').scrollIntoViewIfNeeded();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('confirm-sign')).toBeEnabled();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
    const txid = (await page.getByTestId('tx-id').getAttribute('data-value'))!;
    const placed = await placedBy(mock, txid);
    expect((await wallet!.calls())).toHaveLength(1);
    expect(await horizontalOverflow(page), 'signing result').toBe(0);
    await page.getByTestId('confirm-close').scrollIntoViewIfNeeded();
    await page.getByTestId('confirm-close').click();

    // My orders and its cancel action
    await page.getByTestId('nav-orders').click();
    const id = placed.views[0].covenant_id;
    await expect(page.getByTestId(`order-row-${id}`)).toBeVisible();
    expect(await horizontalOverflow(page), 'My orders').toBe(0);
    await page.getByTestId(`order-cancel-${id}`).scrollIntoViewIfNeeded();
    await page.getByTestId(`order-cancel-${id}`).click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    expect(await horizontalOverflow(page), 'cancel confirmation').toBe(0);
    await page.getByTestId('confirm-cancel').click();
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
    const { confirm } = await reviewAndSign(page, mock);
    expect(confirm.created[0]!.children).toHaveLength(1);
  });
});
