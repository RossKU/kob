// Screenshots of Create token with its "After creation" choice (desktop light and dark, phone). `KOB_CURVE_SHOTS`: the folder;
// `KOB_CURVE_PHASE=before` takes the page as it was before the choice existed (nothing is selected).
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption } from '../helpers/env';

const SHOTS = process.env.KOB_CURVE_SHOTS;
const PHASE = process.env.KOB_CURVE_PHASE === 'before' ? 'before' : 'after';
if (SHOTS) mkdirSync(SHOTS, { recursive: true });
const shot = (page: Page, name: string) => page.screenshot({ path: SHOTS ? join(SHOTS, `${name}.png`) : test.info().outputPath(`${name}.png`), fullPage: true });

test.use({ autoOpen: false });

const VARIANTS = [
  { name: 'light', theme: 'light', width: 1280, height: 900 },
  { name: 'dark', theme: 'dark', width: 1280, height: 900 },
  { name: 'phone', theme: 'light', width: 390, height: 844 },
] as const;

for (const v of VARIANTS) {
  test(`Create token shots (${v.name})`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width: v.width, height: v.height });
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), v.theme);
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await clickConnectOption(page, 'wallet-connect-kasware');
    await page.getByTestId('issue-name').fill('Curve Coin');
    await page.getByTestId('issue-ticker').fill('CURV');
    await page.getByTestId('issue-supply').fill('1000000');
    if (PHASE === 'after') {
      await page.getByTestId('issue-after-curve').click();
      await page.getByTestId('sale-keep').fill('10');
      await expect(page.getByTestId('sale-split')).toContainText('100,000');
      await expect(page.getByTestId('sale-graph')).toBeVisible();
    }
    await expect(page.getByTestId('issue-summary')).toBeVisible();
    await shot(page, `create-${PHASE}-${v.name}`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(v.width);
  });
}
