// Screenshots of the creator lock (review only, skipped without KOB_LOCK_SHOTS=<dir>): Create token
// with "Lock my share until" chosen, the token page's "Creator lock" line, and the Portfolio's
// "locked until" sub-line, from one creation run through the mock wallet and node.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption } from '../helpers/env';

const SHOTS = process.env.KOB_LOCK_SHOTS;
test.skip(!SHOTS, 'screenshots only (KOB_LOCK_SHOTS)');
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

test.use({ autoOpen: false });

test('creator lock shots', async ({ appPage: page, mock }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
  // the manual lock, which the wallet signs open (Settings, Manual unlock)
  await page.addInitScript(() => localStorage.setItem('kob.lock.manual', '1'));
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await page.getByTestId('issue-name').fill('Locked Coin');
  await page.getByTestId('issue-ticker').fill('LOCKD');
  await page.getByTestId('issue-supply').fill('1000000');
  await page.getByTestId('issue-after-ask').click();
  await page.getByTestId('sale-keep').fill('40');
  await page.getByTestId('issue-lock-choice').selectOption('180');
  await expect(page.getByTestId('issue-lock-until')).toBeVisible();
  await expect(page.getByTestId('issue-summary-lock')).toContainText('400,000 LOCKD locked until');
  await page.evaluate(() => { (document.activeElement as HTMLElement | null)?.blur(); window.scrollTo(0, 0); });
  await page.screenshot({ path: join(SHOTS!, 'lock-create.png'), fullPage: true });

  // The creation: the confirmation says locked for you. The mock wallet signs, the mock node takes
  // both steps.
  await page.getByTestId('issue-review').click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  await expect(page.getByTestId('confirm-brief-lock')).toContainText('400,000 LOCKD');
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
  const tokenId = (await page.getByTestId('issue-result-tokenId').getAttribute('data-value'))!;

  // The token page: the declared lock in the token's details.
  await page.goto(`/#/market/${tokenId}`);
  const line = page.getByTestId('token-creator-lock');
  await expect(line).toContainText('400,000 LOCKD (40% of supply) until', { timeout: 30_000 });
  await line.scrollIntoViewIfNeeded();
  await page.getByTestId('token-details').screenshot({ path: join(SHOTS!, 'lock-token-page.png') });

  // The Portfolio: the locked share as a sub-line under Available.
  await page.goto('/#/portfolio');
  const row = page.getByTestId(`balance-row-${tokenId}`);
  await expect(row.getByTestId('balance-locked')).toContainText('locked until', { timeout: 30_000 });
  await page.evaluate(async () => { await document.fonts.ready; await Promise.all(document.getAnimations().filter((a) => a.effect?.getTiming().iterations !== Infinity).map((a) => a.finished)); });
  await page.getByTestId('balances-panel').screenshot({ path: join(SHOTS!, 'lock-portfolio.png') });
});
