// The automatic creator lock on the mock stack: Create token locks the whole share by default as an
// automatic lock (1 KAS lock coin in the genesis), the Portfolio says it opens automatically, and
// once the node's past median time is past the date "Release now" sends the release, which nobody
// signs: the wallet is never asked, and the tokens come back free. With KOB_LOCK_AUTO_SHOTS=<dir>
// it also saves the screenshots of the steps.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption, connectWallet } from '../helpers/env';
import { runFlow } from '../helpers/orders';

const SHOTS = process.env.KOB_LOCK_AUTO_SHOTS;
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

test.use({ autoOpen: false });

/** After a reload the wallet may need its Connect again. */
async function connected(page: Page): Promise<void> {
  const chip = page.getByTestId('wallet-address');
  if (!(await chip.isVisible({ timeout: 5_000 }).catch(() => false))) {
    await chip.waitFor({ state: 'visible', timeout: 5_000 }).catch(() => undefined);
    if (!(await chip.isVisible())) await connectWallet(page);
  }
}

async function shot(page: Page, name: string, testId?: string): Promise<void> {
  if (!SHOTS) return;
  await page.evaluate(async () => {
    (document.activeElement as HTMLElement | null)?.blur();
    await document.fonts.ready;
  });
  if (testId) await page.getByTestId(testId).screenshot({ path: join(SHOTS, `${name}.png`) });
  else await page.screenshot({ path: join(SHOTS, `${name}.png`), fullPage: true });
}

test('automatic creator lock: created by default, opens automatically, Release now with no signature', async ({ appPage: page, mock, wallet }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
  await page.clock.install({ time: Date.now() });
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await page.getByTestId('issue-name').fill('Auto Coin');
  await page.getByTestId('issue-ticker').fill('AUTOL');
  await page.getByTestId('issue-supply').fill('1000000');
  // Keep everything, all locked until tomorrow 00:00 (the viewer's zone).
  const { date, until } = await page.evaluate(() => {
    const d = new Date();
    const t = new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1);
    const p = (n: number) => String(n).padStart(2, '0');
    return { date: `${t.getFullYear()}-${p(t.getMonth() + 1)}-${p(t.getDate())}`, until: t.getTime() };
  });
  await page.getByTestId('issue-lock-choice').selectOption('date');
  await page.getByTestId('issue-lock-date').fill(date);
  await expect(page.getByTestId('issue-lock-until')).toContainText('opens automatically');
  // no "the wallet may not sign the unlock" warning: nothing is signed to open it
  await expect(page.getByTestId('issue-lock-signer')).toHaveCount(0);
  await expect(page.getByTestId('issue-summary-lock')).toContainText('1,000,000 AUTOL locked until');
  await expect(page.getByTestId('issue-summary-lock')).toContainText('opens automatically');
  await expect(page.getByTestId('issue-summary-lock-coin')).toContainText('1 KAS');
  await page.evaluate(() => window.scrollTo(0, 0));
  await shot(page, 'lock-auto-create');

  await page.getByTestId('issue-review').click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  await expect(page.getByTestId('confirm-brief-lock')).toContainText('opens automatically');
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
  const tokenId = (await page.getByTestId('issue-result-tokenId').getAttribute('data-value'))!;

  // Before the date: "opens automatically after", no button.
  await page.goto('/#/portfolio');
  const row = page.getByTestId(`balance-row-${tokenId}`);
  await expect(row.getByTestId('balance-locked')).toContainText('opens automatically after', { timeout: 30_000 });
  await expect(row.getByTestId(`balance-release-${tokenId}`)).toHaveCount(0);
  await expect(row.getByTestId(`balance-unlock-${tokenId}`)).toHaveCount(0);
  await shot(page, 'lock-auto-portfolio-before', 'balances-panel');

  // After the date by the node's clock: Release now.
  await mock.median(until + 5_000);
  await page.clock.setSystemTime(until + 128_000);
  await page.reload();
  await connected(page);
  const release = page.getByTestId(`balance-release-${tokenId}`);
  await expect(release).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId(`balance-unlock-cli-${tokenId}`)).toHaveCount(0);
  await shot(page, 'lock-auto-portfolio-open', 'balances-panel');
  const asked = (await wallet!.calls()).length;
  await release.click();
  const [sent] = await runFlow(page, mock, null, { steps: 1 });
  // nobody signed: the wallet was never asked
  expect((await wallet!.calls()).length).toBe(asked);
  // the release spent the coin and the locked tokens of the genesis
  expect(sent!.submission.txid).toMatch(/^[0-9a-f]{64}$/);

  // The tokens are back, free: no lock line any more.
  await page.reload();
  await connected(page);
  const after = page.getByTestId(`balance-row-${tokenId}`);
  await expect(after).toBeVisible({ timeout: 30_000 });
  await expect(after.getByTestId('balance-locked')).toHaveCount(0, { timeout: 30_000 });
  await expect(after).toContainText('1,000,000');
  await shot(page, 'lock-auto-portfolio-after', 'balances-panel');
});
