// Screenshots of four wallet-run fixes (review only, skipped without KOB_FIX_SHOTS=<dir>).
// KOB_FIX_SHOTS_TAG names the files, e.g. before / after.
//
// - Unlock against the node's past median time and its refusal text.
// - A token held only in a creator lock.
// - A fresh curve in Open orders before the indexer has it.
// - A record being cancelled while the node has not answered.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption, connectWallet, gotoOrders } from '../helpers/env';
import { openOrders, runFlow, seedOrders } from '../helpers/orders';

const SHOTS = process.env.KOB_FIX_SHOTS;
const TAG = process.env.KOB_FIX_SHOTS_TAG ?? 'shot';
test.skip(!SHOTS, 'screenshots only (KOB_FIX_SHOTS)');
if (SHOTS) mkdirSync(SHOTS, { recursive: true });
const shot = (name: string) => join(SHOTS!, `${TAG}-${name}.png`);

test.use({ autoOpen: false });

async function settle(page: Page): Promise<void> {
  await page.evaluate(async () => {
    (document.activeElement as HTMLElement | null)?.blur();
    await document.fonts.ready;
  });
}

test('creator lock: Unlock by the node clock, its refusal, the program mark', async ({ appPage: page, mock }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
  await page.clock.install({ time: Date.now() });
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await page.getByTestId('issue-name').fill('Locked Coin');
  await page.getByTestId('issue-ticker').fill('LOCKD');
  await page.getByTestId('issue-supply').fill('1000000');
  // Keep everything, all locked until tomorrow 00:00 (the viewer's zone): the wallet holds the
  // token only in its lock.
  const { date, until } = await page.evaluate(() => {
    const d = new Date();
    const t = new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1);
    const p = (n: number) => String(n).padStart(2, '0');
    return { date: `${t.getFullYear()}-${p(t.getMonth() + 1)}-${p(t.getDate())}`, until: t.getTime() };
  });
  await page.getByTestId('issue-lock-choice').selectOption('date');
  await page.getByTestId('issue-lock-date').fill(date);
  await expect(page.getByTestId('issue-lock-until')).toBeVisible();
  await page.getByTestId('issue-review').click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
  const tokenId = (await page.getByTestId('issue-result-tokenId').getAttribute('data-value'))!;

  // 128 s after the lock time by the wall clock. The node's clock (TN10: 127-130 s behind) is 2 s
  // before it.
  await mock.median(until - 2_000);
  await page.clock.setSystemTime(until + 128_000);
  await page.goto('/#/portfolio');
  const row = page.getByTestId(`balance-row-${tokenId}`);
  await expect(row.getByTestId('balance-locked')).toBeVisible({ timeout: 30_000 });
  await page.waitForTimeout(2_000);
  await settle(page);
  await page.getByTestId('balances-panel').screenshot({ path: shot('unlock-early') });

  // The press while the node clock is before the lock time (a build that shows the button then;
  // else the node's clock passed and went back, as when the app switched nodes).
  const unlock = row.getByTestId(`balance-unlock-${tokenId}`);
  if (!(await unlock.isVisible())) {
    await mock.median(until + 5_000);
    await expect(unlock).toBeVisible({ timeout: 30_000 });
    await mock.median(until - 2_000);
  }
  await unlock.click();
  const banner = page.getByTestId('orders-action-error');
  await expect(banner).toBeVisible({ timeout: 30_000 });
  await settle(page);
  await page.screenshot({ path: shot('unlock-refused'), clip: { x: 0, y: 0, width: 1280, height: 520 } });
  await page.getByTestId('balances-panel').screenshot({ path: shot('unlock-refused-panel') });
});

test('a fresh curve in Open orders before the indexer has it', async ({ appPage: page, mock }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await page.getByTestId('issue-name').fill('Curve Coin');
  await page.getByTestId('issue-ticker').fill('CURV');
  await page.getByTestId('issue-supply').fill('1000000');
  await page.getByTestId('issue-after-curve').click();
  await page.getByTestId('sale-keep').fill('10');
  await page.getByTestId('issue-review').click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('issue-result-curve')).toBeVisible({ timeout: 30_000 });
  // The indexer does not have it yet: the list reads the wallet's records and the node.
  await page.route('**/v1/**', (r) => r.abort());
  await gotoOrders(page);
  const row = page.locator('[data-testid^="order-row-"]').first();
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(row.getByTestId('order-price')).toBeVisible();
  await page.waitForTimeout(1_500);
  await settle(page);
  await page.getByTestId('orders-list').screenshot({ path: shot('curve-fresh') });
});

test('a sell just cancelled, before the node answers', async ({ appPage: page, mock, wallet }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
  const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
  // The indexer's live order gets a record. Then the indexer is gone and the record is the list's
  // source.
  await openOrders(page, mock);
  await expect(page.getByTestId(`order-row-${id}`)).toHaveAttribute('data-status', 'open');
  await page.route('**/v1/**', (r) => r.abort());
  await page.reload();
  await connectWallet(page);
  await gotoOrders(page);
  await expect(page.getByTestId(`order-row-${id}`)).toHaveAttribute('data-status', 'open', { timeout: 30_000 });
  await page.getByTestId(`order-cancel-${id}`).click();
  await runFlow(page, mock, wallet, { steps: 1 });
  // A new page with a slow node: the list shows from the records before the node's verdict on the
  // cancelled order.
  await page.route('**/node/utxos', async (r) => {
    await new Promise((res) => setTimeout(res, 6_000));
    await r.continue();
  });
  await page.reload();
  await connectWallet(page);
  await gotoOrders(page);
  await expect(page.getByTestId('orders-list').or(page.getByTestId('orders-empty')).first()).toBeVisible({ timeout: 30_000 });
  await page.waitForTimeout(800);
  await settle(page);
  await page.screenshot({ path: shot('cancel-pending'), fullPage: false });
});
