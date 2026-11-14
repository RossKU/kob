// The notifications on a phone (below 640 px): one at a time, at the top just under the header, over the page; a "+N" chip counts the ones waiting and opens
// the bell. While a flow's card (waiting for the wallet, or done) is on screen no new toast shows, and none is replayed after it (founder 2026-11-14:
// the bell holds them); a toast outside a flow closes by itself after a few seconds. A desktop is
// covered by header.spec.ts and manage.spec.ts (top right under the header, stacked).
import { test, expect } from '../fixtures';
import { openOrders, seedOrders } from '../helpers/orders';
import { confirmUntil } from '../helpers/ticket';

test.use({ autoOpen: false });

test('a phone: the flow card is the notification shown; no toast is queued behind it or replayed after it; a toast outside a flow is compact and closes by itself', async ({ appPage: page, mock, wallet }) => {
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
  // nothing waits behind the card, and the done card goes by itself after 4 s without a toast after it
  const chip = page.getByTestId('toast-more');
  await expect(chip).toHaveCount(0);
  await expect(page.getByTestId('toast-ok')).toHaveCount(0);
  await expect(card).toHaveCount(0, { timeout: 8_000 });
  await page.waitForTimeout(500);
  await expect(page.getByTestId('toast-ok')).toHaveCount(0);
  // a toast outside a flow (a payment, with the notifications on) shows alone at the top and closes by itself (nothing in the page moves)
  await page.getByTestId('notify-bell').click();
  await page.getByTestId('notify-enable').click();
  await page.getByTestId('notify-panel').getByRole('button', { name: 'Close' }).click();
  // the first poll stores the baseline: a payment after it is a new one
  const pk = (await mock.balance('alice')).pubkey;
  await expect.poll(() => page.evaluate((k) => { try { return JSON.parse(localStorage.getItem(`kob.notify.testnet-10.${k}`) ?? 'null')?.snaps != null; } catch { return false; } }, pk), { timeout: 30_000 }).toBe(true);
  const heightBefore = await page.evaluate(() => document.documentElement.scrollHeight);
  // the pointer off the toast's place: a finger or the pointer on a phone's toast holds its timer
  await page.mouse.move(195, 760);
  await mock.giveKas('alice', 3n * 100_000_000n);
  const toast = page.getByTestId('toast-ok');
  await expect(toast).toBeVisible({ timeout: 30_000 });
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
