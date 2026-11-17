// Screenshots of Create token and its confirmation screen (desktop light and dark, phone) for review. Not a test of anything: skipped without
// KOB_CREATE_SHOTS=<dir>. `KOB_CREATE_PHASE=before` names the files for the page as it was before its help moved behind "?" (and opens no help).
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption } from '../helpers/env';

const SHOTS = process.env.KOB_CREATE_SHOTS;
const PHASE = process.env.KOB_CREATE_PHASE === 'before' ? 'before' : 'after';
test.skip(!SHOTS, 'screenshots only (KOB_CREATE_SHOTS)');
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

test.use({ autoOpen: false });

const VARIANTS = [
  { name: 'light', theme: 'light', width: 1280, height: 900 },
  { name: 'dark', theme: 'dark', width: 1280, height: 900 },
  { name: 'phone', theme: 'light', width: 390, height: 844 },
] as const;

/** The whole confirmation screen: the window is made as tall as the dialog (it scrolls inside a fixed backdrop, so a full-page shot would cut it). */
async function shotDialog(page: Page, name: string, width: number, height: number): Promise<void> {
  const h = await page.getByTestId('confirm-screen').evaluate((el) => Math.ceil(el.getBoundingClientRect().height));
  await page.setViewportSize({ width, height: Math.max(height, h + 80) });
  await page.screenshot({ path: join(SHOTS!, `${name}.png`) });
  await page.setViewportSize({ width, height });
}

for (const v of VARIANTS) {
  test(`Create token and confirmation shots (${v.name})`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width: v.width, height: v.height });
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), v.theme);
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await clickConnectOption(page, 'wallet-connect-kasware');
    await page.getByTestId('issue-name').fill('Curve Coin');
    await page.getByTestId('issue-ticker').fill('CURV');
    await page.getByTestId('issue-supply').fill('1000000');
    await page.getByTestId('issue-after-curve').click();
    await page.getByTestId('sale-keep').fill('10');
    await expect(page.getByTestId('sale-graph')).toBeVisible();
    await expect(page.getByTestId('issue-summary')).toBeVisible();
    // the top of the page, nothing focused (a full-page shot of a scrolled page draws the sticky header in the middle)
    await page.evaluate(() => { (document.activeElement as HTMLElement | null)?.blur(); window.scrollTo(0, 0); });
    await page.screenshot({ path: join(SHOTS!, `page-${PHASE}-${v.name}.png`), fullPage: true });
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(v.width);
    if (PHASE === 'after') {
      // one field's help open
      await page.getByTestId('issue-ticker-help').click();
      await expect(page.getByTestId('issue-ticker-help-panel')).toBeVisible();
      await page.screenshot({ path: join(SHOTS!, `page-help-${v.name}.png`) });
      await page.keyboard.press('Escape');
    }

    await page.getByTestId('issue-review').click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    await shotDialog(page, `confirm-${PHASE}-${v.name}`, v.width, v.height);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(v.width);
    if (PHASE === 'after') {
      await page.getByTestId('confirm-risk-help').click();
      await expect(page.getByTestId('confirm-risk-help-panel')).toBeVisible();
      await page.screenshot({ path: join(SHOTS!, `confirm-risk-help-${v.name}.png`) });
    }
  });
}
