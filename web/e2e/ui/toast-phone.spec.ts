// The notifications on a phone (below 640 px): one at a time, at the top just under the header, over the page; a "+N" chip counts the ones waiting and opens
// the bell; a flow's card (waiting for the wallet, or done) is the one shown while it is there; a success closes by itself after a few seconds. A desktop is
// covered by header.spec.ts and manage.spec.ts (bottom right, stacked).
import { test, expect } from '../fixtures';
import { openOrders, seedOrders } from '../helpers/orders';
import { confirmUntil } from '../helpers/ticket';

test.use({ autoOpen: false });

test('a phone: the flow card is the notification shown, the success toast waits behind it as "+1" (the chip opens the bell), then shows alone and closes by itself', async ({ appPage: page, mock, wallet }) => {
  await page.setViewportSize({ width: 390, height: 800 });
  const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
  await openOrders(page, mock);
  await page.getByTestId(`order-cancel-${id}`).click();
  const card = page.getByTestId('tx-flow');
  await expect(card).toBeVisible();
  const header = (await page.locator('header.header').boundingBox())!;
  const box = (await card.boundingBox())!;
  expect(box.y, 'under the header (sub-pixel rounding allowed)').toBeGreaterThanOrEqual(header.y + header.height - 1);
  expect(box.y, 'just under the header').toBeLessThanOrEqual(header.y + header.height + 16);
  expect(box.x, 'left gutter').toBeGreaterThanOrEqual(11.5);
  expect(box.x, 'left gutter').toBeLessThanOrEqual(12.5);
  expect(390 - (box.x + box.width), 'right gutter').toBeLessThanOrEqual(12.5);
  await confirmUntil(page, page.getByTestId('flow-done'));
  await expect(page.getByTestId('flow-done')).toBeVisible();
  // the success toast is queued behind the card
  const chip = page.getByTestId('toast-more');
  await expect(chip).toHaveText('+1');
  await expect(page.getByTestId('toast-ok')).toHaveCount(0);
  const heightBefore = await page.evaluate(() => document.documentElement.scrollHeight);
  await chip.click();
  await expect(page.getByTestId('notify-panel')).toBeVisible();
  await page.keyboard.press('Escape');
  // the done card goes by itself after 4 s; the toast then shows alone, at the top, and closes by itself too (nothing in the page moved)
  await expect(card).toHaveCount(0, { timeout: 8_000 });
  const toast = page.getByTestId('toast-ok');
  await expect(toast).toBeVisible();
  await expect(chip).toHaveCount(0);
  // (it slides down from under the header first)
  await expect.poll(async () => (await toast.boundingBox())!.y, { message: 'under the header' }).toBeGreaterThanOrEqual(header.y + header.height - 1);
  const tb = (await toast.boundingBox())!;
  expect(tb.height, 'compact: two lines at most').toBeLessThanOrEqual(80);
  const close = (await toast.getByRole('button', { name: 'Dismiss' }).boundingBox())!;
  expect(close.width).toBeGreaterThanOrEqual(32);
  expect(close.height).toBeGreaterThanOrEqual(32);
  expect(await page.evaluate(() => document.documentElement.scrollHeight)).toBe(heightBefore);
  await expect(toast).toHaveCount(0, { timeout: 8_000 });
});
