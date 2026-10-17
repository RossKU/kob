// The bell lists what it counts (TN10 2026-10-17: a badge "5" opened a dialog that only said the notifications were off). Notifications that were
// collected while they were on stay listed after they are turned off; "off" only means nothing new is collected, said in one line with its switch.
import { test, expect, TEST_KEYS } from '../fixtures';
import { connectWallet, openMarket } from '../helpers/env';

test('notifications off: the bell still lists what its badge counts, newest first, with their time and a link; the switch is one line', async ({ appPage: page, mock }) => {
  const key = `kob.notify.testnet-10.${TEST_KEYS.alice.pubkey.toLowerCase()}`;
  const now = Date.now();
  const items = [1, 2, 3, 4, 5].map((n) => ({ id: `fill-${n}`, kind: 'fill', side: n % 2 ? 'sell' : 'buy', params: { amount: '100000000' }, at: now - n * 60_000, read: false }));
  await openMarket(page, mock);
  // the browser holds five notifications collected earlier; they are off now
  await page.evaluate(([k, v]) => {
    localStorage.setItem(k, v);
    localStorage.setItem('kob.notify.settings', JSON.stringify({ enabled: false, browser: false, kinds: {} }));
  }, [key, JSON.stringify({ v: 1, snaps: null, payments: null, items, seen: [], invoices: [] })] as const);
  await page.reload();
  if (!(await page.getByTestId('wallet-address').isVisible())) await connectWallet(page);
  await expect(page.getByTestId('notify-unread')).toHaveText('5');
  await page.getByTestId('notify-bell').click();
  const panel = page.getByTestId('notify-panel');
  await expect(panel.getByTestId('notify-disabled')).toContainText('Notifications are off: nothing new is collected.');
  await expect(panel.getByTestId('notify-enable')).toBeVisible();
  const listed = panel.getByTestId('notify-list').locator('li');
  await expect(listed).toHaveCount(5);
  // newest first, each with its time and a link to My orders
  const times = await listed.locator('time').evaluateAll((ts) => ts.map((x) => Date.parse(x.getAttribute('datetime') ?? '')));
  expect(times).toEqual([...times].sort((a, b) => b - a));
  await expect(listed.first().locator('a')).toHaveAttribute('href', '#/orders');
  await expect(listed.first()).toContainText('Order filled');
  // read: the badge goes
  await panel.getByTestId('notify-mark-read').click();
  await expect(page.getByTestId('notify-unread')).toHaveCount(0);
});

test('notifications off and nothing collected: one line says so with its switch, "No notifications yet.", no list buttons', async ({ appPage: page, mock }) => {
  await openMarket(page, mock);
  await expect(page.getByTestId('notify-unread')).toHaveCount(0);
  await page.getByTestId('notify-bell').click();
  const panel = page.getByTestId('notify-panel');
  await expect(panel.getByTestId('notify-disabled')).toHaveText(/^Notifications are off: nothing new is collected\.\s*Turn on$/);
  expect(((await panel.innerText()).match(/Notifications are off/g) ?? []).length, 'said once').toBe(1);
  await expect(panel.getByTestId('notify-empty')).toHaveText('No notifications yet.');
  await expect(panel.getByTestId('notify-mark-read')).toHaveCount(0);
  await expect(panel.getByTestId('notify-clear')).toHaveCount(0);
  // turned on: the off line goes, the empty line stays
  await panel.getByTestId('notify-enable').click();
  await expect(panel.getByTestId('notify-disabled')).toHaveCount(0);
  await expect(panel.getByTestId('notify-empty')).toHaveText('No notifications yet.');
});
