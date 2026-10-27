// An order worth less than the executor's minimum order value (default 1 KAS) is indexed but never listed, so it never fills. The ticket says so before
// signing (a note, never a stop: no checkbox, the button stays enabled) and My orders says "Not listed by the indexer" with the reason and what to do.
// The mock indexer applies the rule like the real executor once `mock.minOrderValue(1 KAS)` is set (it is off by default). Desktop and a 375 px phone.
import { expect, test, TEST_KEYS } from '../fixtures';
import { KAS, fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';
import { fillFields, pickType, placeOrder, settlePlan } from '../helpers/ticket';
import { orderCancel, orderRow } from '../testids';

test.use({ autoOpen: false });

const VIEWPORTS = [
  { name: 'desktop 1280x800', width: 1280, height: 800 },
  { name: 'phone 375 px', width: 375, height: 812 },
] as const;

for (const vp of VIEWPORTS) {
  test.describe(vp.name, () => {
    test.use({ viewport: { width: vp.width, height: vp.height } });

    test.beforeEach(async ({ mock }) => {
      await fund(mock);
      await mock.minOrderValue(KAS);
    });

    test('the ticket notes a sell below the minimum before signing, updates live, and never stops the order', async ({ appPage: page, mock }) => {
      await openMarket(page, mock);
      await pickType(page, 'limit', 'sell');
      const note = page.getByTestId('order-note-min-value');
      const place = page.getByTestId('order-place');

      // 38 EXKCC at 0.026 KAS = 0.988 KAS: below 1 KAS
      await fillFields(page, { amount: '38', price: '0.026' });
      await settlePlan(page);
      await expect(note).toBeVisible();
      await expect(note).toContainText('worth 0.988 KAS');
      await expect(note).toContainText('minimum order value of 1 KAS');
      await expect(note).toContainText('will not fill');
      await expect(note).toContainText('cancel it from My orders');
      // a note, never a stop: no acknowledgement, the button is enabled and nothing in the notes asks for a click first
      await expect(place).toBeEnabled();
      await expect(page.getByTestId('order-notes').locator('input, button')).toHaveCount(0);
      await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
      expect(await horizontalOverflow(page), 'ticket with the note').toBe(0);

      // live: a larger amount (39 x 0.026 = 1.014 KAS) takes the note away, a smaller one brings it back
      await fillFields(page, { amount: '39' });
      await settlePlan(page);
      await expect(note).toHaveCount(0);
      await expect(place).toBeEnabled();
      await fillFields(page, { amount: '5' });
      await settlePlan(page);
      await expect(note).toContainText('worth 0.13 KAS');
      // the price counts too: 40 EXKCC at 0.025 KAS is exactly 1 KAS (listed), one base unit less is not
      await fillFields(page, { amount: '40', price: '0.025' });
      await settlePlan(page);
      await expect(note).toHaveCount(0);
      await fillFields(page, { amount: '39.99999999' });
      await settlePlan(page);
      await expect(note).toContainText('worth 0.99999999 KAS');
      await expect(place).toBeEnabled();
      expect(await horizontalOverflow(page), 'ticket with the note, after edits').toBe(0);
    });

    test('an order well above the minimum has no note', async ({ appPage: page, mock }) => {
      await openMarket(page, mock);
      await pickType(page, 'limit', 'sell');
      await fillFields(page, { amount: '100', price: '0.03' });
      await settlePlan(page);
      await expect(page.getByTestId('order-note-min-value')).toHaveCount(0);
      await expect(page.getByTestId('order-place')).toBeEnabled();
    });

    test('an order placed below the minimum is shown in My orders as not listed, with the reason and the hint; a listed order is not', async ({ appPage: page, mock }) => {
      await openMarket(page, mock);
      await pickType(page, 'limit', 'sell');
      // below: 38 x 0.026 = 0.988 KAS. The note is there and the order goes out anyway.
      await fillFields(page, { amount: '38', price: '0.026' });
      await settlePlan(page);
      await expect(page.getByTestId('order-note-min-value')).toBeVisible();
      const low = await placeOrder(page, mock);
      const lowId = low.submission.created[0]!.covenantId;
      // above: 40 x 0.0255 = 1.02 KAS
      await fillFields(page, { amount: '40', price: '0.0255' });
      await settlePlan(page);
      await expect(page.getByTestId('order-note-min-value')).toHaveCount(0);
      const ok = await placeOrder(page, mock);
      const okId = ok.submission.created[0]!.covenantId;

      // the indexer's verdicts
      expect(await mock.order(lowId)).toMatchObject({ listed: false, unlisted_reason: 'order_value_below_minimum', status: 'open' });
      expect(await mock.order(okId)).toMatchObject({ listed: true, unlisted_reason: null, status: 'open' });

      await page.goto('/#/orders');
      await expect(page.getByTestId('orders-list')).toBeVisible();
      const lowRow = page.getByTestId(orderRow(lowId));
      await expect(lowRow).toBeVisible();
      const banner = lowRow.getByTestId('order-unlisted');
      await expect(banner).toBeVisible();
      await expect(lowRow.getByTestId('order-unlisted-reason')).toHaveText(/^Not listed by the indexer: the order is worth less than the minimum order value/);
      await expect(lowRow.getByTestId('order-unlisted-hint')).toHaveText(/will not fill\. Cancel it, or replace it with a larger order\.$/);
      // the machine code is only behind "Details"
      await expect(lowRow.getByTestId('order-unlisted-code')).toContainText('order_value_below_minimum');
      await expect(banner.locator('details[open]')).toHaveCount(0);
      await expect(lowRow.getByTestId('order-unlisted-reason')).not.toContainText('order_value_below_minimum');
      // it is still cancellable
      await expect(lowRow.getByTestId(orderCancel(lowId))).toBeEnabled();
      // the listed order has no such line
      await expect(page.getByTestId(orderRow(okId))).toBeVisible();
      await expect(page.getByTestId(orderRow(okId)).getByTestId('order-unlisted')).toHaveCount(0);
      expect(await horizontalOverflow(page), 'My orders').toBe(0);
      // and the banner stays inside the card
      const card = (await lowRow.boundingBox())!;
      const inner = (await banner.boundingBox())!;
      expect(inner.x).toBeGreaterThanOrEqual(card.x - 1);
      expect(inner.x + inner.width).toBeLessThanOrEqual(card.x + card.width + 1);
    });

    test('an ended order no longer says it is not listed', async ({ appPage: page, mock }) => {
      const token = (await mock.token()).covenant_id;
      const spec = { token, side: 'ask', maker: 'alice', price: 2_600_000, amount: (38n * 100_000_000n).toString() };
      await mock.seed({ orders: [spec, { ...spec, history: 'cancelled' }] });
      const orders = await mock.orders(`maker=${TEST_KEYS.alice.pubkey}&limit=200`);
      expect(orders.filter((o) => o.listed === false && o.unlisted_reason === 'order_value_below_minimum')).toHaveLength(2);
      const live = orders.find((o) => o.status === 'open')!;
      const ended = orders.find((o) => o.status === 'cancelled')!;
      await openMarket(page, mock, { route: '/orders' });
      await expect(page.getByTestId(orderRow(live.covenant_id)).getByTestId('order-unlisted')).toBeVisible();
      await page.getByTestId('orders-tab-history').click();
      await expect(page.getByTestId(orderRow(ended.covenant_id))).toBeVisible();
      await expect(page.getByTestId(orderRow(ended.covenant_id)).getByTestId('order-unlisted')).toHaveCount(0);
    });
  });
}
