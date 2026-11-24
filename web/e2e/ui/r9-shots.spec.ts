// Screenshots of the round-9 screens (review only, skipped without KOB_R9_SHOTS=<dir>): the
// confirmation with the DAA next to its dates, the same with the indexer down (the chain time
// checked against the last verified time), and the private-key frame's lines for a placement and a
// cancel (the order's bound terms next to the id the app names).
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { expect, test, TEST_KEYS } from '../fixtures';
import { fund, gotoOrders, openMarket } from '../helpers/env';
import { clickPlaceOnly, fillFields, pickType, readSignerFrame, waitReviewable } from '../helpers/ticket';

const SHOTS = process.env.KOB_R9_SHOTS;
const TAG = process.env.KOB_R9_TAG ?? 'after';
test.skip(!SHOTS, 'screenshots only (KOB_R9_SHOTS)');
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

const shot = (name: string) => join(SHOTS!, `${name}-${TAG}.png`);

test.describe('the app confirmation', () => {
  test.use({ autoOpen: false });
  test('a GTC limit sell: its expiry with the DAA, healthy and with the indexer down', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
    await fund(mock, 'alice', { tokens: 50n });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.05' });
    await clickPlaceOnly(page);
    const confirm = page.getByRole('dialog').first();
    await expect(confirm).toBeVisible({ timeout: 30_000 });
    await page.screenshot({ path: shot('r9-confirm-dates'), fullPage: false });
    await page.keyboard.press('Escape');
    // the indexer goes down: the dates are checked against the last verified time this browser saw
    await mock.setHealth('stopped');
    await page.waitForTimeout(20_000);
    await clickPlaceOnly(page).catch(() => undefined);
    await page.waitForTimeout(3_000);
    await page.screenshot({ path: shot('r9-confirm-dates-indexer-down'), fullPage: false });
  });
});

test.describe('the private-key frame', () => {
  test.use({ walletId: null, autoOpen: false });
  const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
  async function connectKey(page: Page): Promise<void> {
    await page.getByTestId('wallet-connect-local-key').click();
    await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await signer(page).getByTestId('signer-connect').click();
  }
  test('a placement and a cancel in the frame', async ({ appPage: page, mock }) => {
    test.setTimeout(300_000);
    await page.setViewportSize({ width: 1440, height: 1200 });
    await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
    await fund(mock, 'alice', { tokens: 50n });
    await openMarket(page, mock, { connect: false });
    await connectKey(page);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.05' });
    await waitReviewable(page);
    await page.getByTestId('order-place').click();
    const approve = signer(page).getByTestId('signer-approve');
    await expect(approve).toBeVisible({ timeout: 30_000 });
    await readSignerFrame(page);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-place') });
    await expect(approve).toBeEnabled({ timeout: 30_000 });
    await approve.click();
    await expect.poll(async () => (await mock.submitted(false)).length, { timeout: 60_000 }).toBeGreaterThan(0);
    await gotoOrders(page);
    await page.locator('[data-testid^="order-cancel-"]:not([data-testid="order-cancel-only"]):not([data-testid="order-cancel-blocked"])').first().click({ timeout: 60_000 });
    await expect(approve).toBeVisible({ timeout: 60_000 });
    await readSignerFrame(page);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-cancel') });
  });
});
