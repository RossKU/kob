// An order worth less than the executor's minimum order value (default 1 KAS) is indexed but never listed, so it never fills. The ticket says nothing of it before
// signing in no note at all (founder, 2026-11-02: the ticket shows only what blocks an order, so nothing on it appears or moves; the button stays enabled) the final confirmation says it in one line, and My orders says "Not listed by the indexer" with the reason and what to do.
// The mock indexer applies the rule like the real executor once `mock.minOrderValue(1 KAS)` is set (it is off by default). Desktop and a 375 px phone.
import { expect, test, TEST_KEYS } from '../fixtures';
import { KAS, fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';
import { cancelReceipt, clickPlaceOnly, confirmReceipt, fillFields, pickType, placeOrder, readReceipt, settlePlan, waitSubmitted } from '../helpers/ticket';
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

    test('the ticket shows no note for a sell below the minimum (nothing appears or moves), and the order is never stopped', async ({ appPage: page, mock }) => {
      await openMarket(page, mock);
      await pickType(page, 'limit', 'sell');
      const note = page.getByTestId('order-note-min-value');
      const place = page.getByTestId('order-place');
      const slot = page.getByTestId('order-issues');

      // 38 EXKCC at 0.026 KAS = 0.988 KAS: below 1 KAS. No note, no reason (the order can be placed), the button is where it was
      await fillFields(page, { amount: '39', price: '0.026' });
      await settlePlan(page);
      await expect(place).toBeEnabled();
      const before = (await place.boundingBox())!;
      await fillFields(page, { amount: '38' });
      await settlePlan(page);
      await expect(note).toHaveCount(0);
      await expect(page.getByTestId('order-notes')).toHaveCount(0);
      await expect(place).toBeEnabled();
      await expect(slot).not.toContainText(/S/);
      expect(await place.boundingBox()).toEqual(before);
      expect(await horizontalOverflow(page), 'ticket').toBe(0);
      // the price counts too: still no note either side of the limit
      await fillFields(page, { amount: '40', price: '0.025' });
      await settlePlan(page);
      await expect(note).toHaveCount(0);
      await fillFields(page, { amount: '39.99999999' });
      await settlePlan(page);
      await expect(note).toHaveCount(0);
      await expect(place).toBeEnabled();
      expect(await place.boundingBox()).toEqual(before);
    });

    test('the final confirmation notes an order below the minimum in one line (worked out when it opens), a listed order has none, and Continue still places it', async ({ appPage: page, mock }) => {
      await openMarket(page, mock);
      await pickType(page, 'limit', 'sell');
      const notes = page.getByTestId('confirm-order-notes');
      // 38 x 0.026 = 0.988 KAS: below 1 KAS. The ticket stays quiet; the confirmation says it once, under its summary
      await fillFields(page, { amount: '38', price: '0.026' });
      await settlePlan(page);
      await clickPlaceOnly(page);
      await readReceipt(page);
      await expect(page.getByTestId('confirm-order-note')).toHaveCount(1);
      await expect(notes).toContainText('Worth 0.988 KAS');
      await expect(notes).toContainText('will not be listed, so it will not fill');
      expect(await horizontalOverflow(page), 'confirmation').toBe(0);
      await cancelReceipt(page);
      // 40 x 0.0255 = 1.02 KAS: listed, no note
      await fillFields(page, { amount: '40', price: '0.0255' });
      await settlePlan(page);
      await clickPlaceOnly(page);
      await readReceipt(page);
      await expect(notes).toHaveCount(0);
      await cancelReceipt(page);
      // back below: the note is there again, and Continue places the order (a note never blocks it)
      await fillFields(page, { amount: '38', price: '0.026' });
      await settlePlan(page);
      await clickPlaceOnly(page);
      await readReceipt(page);
      await expect(notes).toContainText('Worth 0.988 KAS');
      await confirmReceipt(page);
      await waitSubmitted(page);
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
      // below: 38 x 0.026 = 0.988 KAS. The ticket says nothing and the order goes out anyway.
      await fillFields(page, { amount: '38', price: '0.026' });
      await settlePlan(page);
      await expect(page.getByTestId('order-note-min-value')).toHaveCount(0);
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

      await page.goto('/#/portfolio/open');
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
      await openMarket(page, mock, { route: '/portfolio/open' });
      await expect(page.getByTestId(orderRow(live.covenant_id)).getByTestId('order-unlisted')).toBeVisible();
      await page.getByTestId('orders-tab-history').click();
      await expect(page.getByTestId(orderRow(ended.covenant_id))).toBeVisible();
      await expect(page.getByTestId(orderRow(ended.covenant_id)).getByTestId('order-unlisted')).toHaveCount(0);
    });
  });
}
