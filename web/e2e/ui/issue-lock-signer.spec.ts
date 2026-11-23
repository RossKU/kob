// Create token, "Lock my share until": a wallet that has not shown it signs script inputs (the
// unlock's coin is one) gets a warning under the choice, because the unlock would then need the CLI
// and the private key. A wallet that signs them (KasWare) gets none. The warning is only for a
// manual lock (Settings, "Manual unlock"): the default automatic lock needs no signature. Mock stack.
import { mkdirSync } from 'node:fs';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption } from '../helpers/env';

test.use({ autoOpen: false });

async function lockForm(page: import('@playwright/test').Page, wallet: string): Promise<void> {
  await page.goto('/#/create');
  await clickConnectOption(page, wallet);
  await page.getByTestId('issue-name').fill('Locked Coin');
  await page.getByTestId('issue-ticker').fill('LOCKD');
  await page.getByTestId('issue-supply').fill('1000000');
  await page.getByTestId('issue-lock-choice').selectOption('180');
  await expect(page.getByTestId('issue-lock-until')).toBeVisible();
}

test.describe('a wallet not proven to sign the unlock', () => {
  test.use({ walletId: 'kastle', covenantChecked: false });
  test('is warned under "Lock my share until" for a manual lock, and not without a lock', async ({ appPage: page, mock }) => {
    await page.addInitScript(() => localStorage.setItem('kob.lock.manual', '1'));
    await mock.giveKas('alice', 500n * KAS);
    await lockForm(page, 'wallet-connect-kastle');
    const dir = process.env.KOB_SHOTS;
    if (dir) {
      mkdirSync(dir, { recursive: true });
      await page.waitForTimeout(300);
      await page.getByTestId('issue-lock').scrollIntoViewIfNeeded();
      await page.getByTestId('issue-lock').locator('xpath=..').screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-issue-lock-signer.png` });
    }
    const note = page.getByTestId('issue-lock-signer');
    await expect(note).toBeVisible();
    await expect(note).toContainText('This wallet has not shown that it can sign the unlock');
    await page.getByTestId('issue-lock-choice').selectOption('none');
    await expect(note).toHaveCount(0);
  });

  test('is not warned for the default automatic lock, which nobody signs', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await lockForm(page, 'wallet-connect-kastle');
    await expect(page.getByTestId('issue-lock-until')).toContainText('opens automatically');
    await expect(page.getByTestId('issue-lock-signer')).toHaveCount(0);
  });
});

test('KasWare signs the unlock: no warning', async ({ appPage: page, mock }) => {
  await mock.giveKas('alice', 500n * KAS);
  await lockForm(page, 'wallet-connect-kasware');
  await expect(page.getByTestId('issue-lock-signer')).toHaveCount(0);
});
