// The signing stepper (src/ui/kit/Stepper.tsx) mid-flow, in both themes and on a phone: Create token (Prepare, then Create token) on its
// confirmation screen, and Merge all (Merge 1 of 3 ...) in its flow card. The wallet holds the second request open, so the screen is caught
// with one step done and the next one current. An ordered list with aria-current on the current step; nothing runs past the window's width.
// Screenshots go to `KOB_STEPPER_SHOTS`, else the test's output folder.
import { mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, clickConnectOption, connectWallet } from '../helpers/env';

const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);
const SHOTS = process.env.KOB_STEPPER_SHOTS;
if (SHOTS) mkdirSync(SHOTS, { recursive: true });
const shot = (page: Page, name: string) => page.screenshot({ path: SHOTS ? join(SHOTS, `${name}.png`) : test.info().outputPath(`${name}.png`) });

async function registryWithStd(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  Object.assign(exkcc, { verified: true });
  reg.tokens.push({ ...exkcc, ticker: 'EXSTD', name: 'Example standard token', covenant_id: STD, template_id: 'kcc20-ref-3x3', extension_commitment: EXT });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}
const seedStd = (mock: MockClient) => mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3' }] });

const noOverflow = async (page: Page, width: number) => expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
const inside = async (page: Page, testid: string, width: number) => {
  const box = (await page.getByTestId(testid).boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(width);
};

test.use({ autoOpen: false, walletOptions: { walletNodeLagMs: 600_000 } });

const VARIANTS = [
  { name: 'desktop-light', theme: 'light', width: 1280, height: 800 },
  { name: 'desktop-dark', theme: 'dark', width: 1280, height: 800 },
  { name: 'phone', theme: 'light', width: 390, height: 844 },
] as const;

for (const v of VARIANTS) {
  test.describe(v.name, () => {
    test.beforeEach(async ({ page }) => {
      await page.setViewportSize({ width: v.width, height: v.height });
      await page.addInitScript((th) => localStorage.setItem('kob.theme', th), v.theme);
    });

    test('Create token mid-flow: Prepare done, Create token current', async ({ appPage: page, mock, wallet }) => {
      await mock.giveKas('alice', 500n * KAS);
      await page.goto('/#/create');
      await clickConnectOption(page, 'wallet-connect-kasware');
      await expect(page.locator('html')).toHaveAttribute('data-theme', v.theme);
      await page.getByTestId('issue-name').fill('Step Coin');
      await page.getByTestId('issue-ticker').fill('STEP');
      await page.getByTestId('issue-supply').fill('1000');
      await page.getByTestId('issue-review').click();
      await page.getByTestId('confirm-ack').check();
      await page.getByTestId('confirm-sign').click();
      // the first approval answers at once; the second one is held open
      await wallet!.waitForCalls(1, 20_000);
      await wallet!.configure({ delayMs: 120_000 });
      const stepper = page.getByTestId('create-stepper');
      await expect(stepper).toHaveAttribute('role', 'list');
      await expect(stepper).toHaveClass(/stepper-row/);
      await expect(page.getByTestId('create-step-prep')).toHaveAttribute('data-state', 'done', { timeout: 30_000 });
      const genesis = page.getByTestId('create-step-genesis');
      await expect(genesis).toHaveAttribute('aria-current', 'step');
      await expect(genesis).toContainText('Create token');
      await expect(genesis).toContainText('Sign in your wallet', { timeout: 30_000 });
      await expect(page.getByTestId('create-step-prep')).not.toHaveAttribute('aria-current', /.*/);
      await stepper.scrollIntoViewIfNeeded();
      await noOverflow(page, v.width);
      await inside(page, 'create-stepper', v.width);
      await shot(page, `create-token-${v.name}`);
    });

    test('Merge all mid-flow: Merge 1 of 3 done, Merge 2 of 3 current', async ({ appPage: page, mock, wallet }) => {
      test.setTimeout(120_000);
      await seedStd(mock);
      for (let k = 1; k <= 7; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
      await registryWithStd(page);
      await page.goto('/#/portfolio');
      await connectWallet(page, 'kasware');
      await expect(page.locator('html')).toHaveAttribute('data-theme', v.theme);
      await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveText(/^Merge all \(7\): /, { timeout: 15_000 });
      await page.getByTestId(`orders-merge-token-${STD}`).click();
      await expect(page.getByTestId('merge-all-summary')).toContainText('3 transactions', { timeout: 30_000 });
      await page.getByTestId('merge-all-confirm').click();
      await wallet!.waitForCalls(1, 30_000);
      await wallet!.configure({ delayMs: 120_000 });
      const stepper = page.getByTestId('flow-progress');
      await expect(stepper).toHaveAttribute('role', 'list');
      await expect(stepper).toHaveClass(/stepper-row/);
      await expect(page.getByTestId('flow-step-0')).toHaveAttribute('data-state', 'submitted', { timeout: 60_000 });
      const second = page.getByTestId('flow-step-1');
      await expect(second).toHaveAttribute('aria-current', 'step', { timeout: 60_000 });
      await expect(second).toContainText('Merge 2 of 3');
      await expect(second).toHaveAttribute('data-state', 'sending', { timeout: 60_000 });
      await expect(second).toContainText('Sign in your wallet', { timeout: 60_000 });
      await expect(page.getByTestId('flow-step-2')).toHaveAttribute('data-state', 'pending');
      await noOverflow(page, v.width);
      await inside(page, 'tx-flow', v.width);
      await shot(page, `merge-all-${v.name}`);
    });
  });
}
