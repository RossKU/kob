// Screenshots of the chromatic curve shape (review only, skipped without KOB_CHROMATIC_SHOTS=<dir>): Create token with "Sell on a curve"
// and the Chromatic shape chosen, the "After creation" section with its graph and the whole page.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { test, expect } from '../fixtures';
import { KAS, clickConnectOption } from '../helpers/env';

const SHOTS = process.env.KOB_CHROMATIC_SHOTS;
test.skip(!SHOTS, 'screenshots only (KOB_CHROMATIC_SHOTS)');
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

test.use({ autoOpen: false });

test('chromatic curve shots', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await page.getByTestId('issue-name').fill('Chroma Coin');
  await page.getByTestId('issue-ticker').fill('CHRM');
  await page.getByTestId('issue-supply').fill('1000000');
  await page.getByTestId('issue-after-curve').click();
  await page.getByTestId('sale-keep').fill('10');
  await page.getByTestId('sale-shape-chromatic').click();
  // the Octaves field takes the Levels slot, the Multiple slot shows the levels and the top
  await expect(page.getByTestId('sale-levels')).toHaveCount(0);
  await expect(page.getByTestId('sale-multiple')).toHaveCount(0);
  await expect(page.getByTestId('sale-octaves-range')).toContainText('25, top 4× first');
  await page.getByTestId('sale-octaves').fill('3');
  await expect(page.getByTestId('sale-octaves-range')).toContainText('37, top 8× first');
  await expect(page.getByTestId('sale-graph')).toBeVisible();
  await page.evaluate(() => { (document.activeElement as HTMLElement | null)?.blur(); window.scrollTo(0, 0); });
  await page.evaluate(async () => { await document.fonts.ready; await Promise.all(document.getAnimations().filter((a) => a.effect?.getTiming().iterations !== Infinity).map((a) => a.finished.catch(() => undefined))); });
  await page.getByTestId('issue-after').screenshot({ path: join(SHOTS!, 'chromatic-create.png') });
  await page.screenshot({ path: join(SHOTS!, 'chromatic-create-page.png'), fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(1280);
});
