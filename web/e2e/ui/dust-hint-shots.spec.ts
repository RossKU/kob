// Screenshots of the dust hint of the ticket (review only, skipped without KOB_DUST_SHOTS=<dir>):
// a TWAP whose slices are worth less than half the listing minimum, the "?" flashing once, its
// help showing the line, and the same ticket with slices over the line.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType } from '../helpers/ticket';

const SHOTS = process.env.KOB_DUST_SHOTS;
test.skip(!SHOTS, 'screenshots only (KOB_DUST_SHOTS)');
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

test.use({ autoOpen: false });

async function twap(page: Page, slice: string): Promise<void> {
  await pickType(page, 'twap');
  await fillFields(page, { amount: '40', price: '0.05', sliceAmount: slice, interval: '5' });
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
}

test('the dust hint: a TWAP of 0.05 KAS slices, its "?" and its help; slices over the line flash nothing', async ({ appPage: page, mock }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'light'));
  await fund(mock, 'alice', { tokens: 50n });
  await openMarket(page, mock);
  const ticket = page.getByTestId('order-ticket');
  const help = page.getByTestId('order-help-toggle');

  // over the line: 20 tokens a slice at 0.05 KAS is 1 KAS
  await twap(page, '20');
  await expect(help).not.toHaveClass(/tk-flash/);
  await ticket.screenshot({ path: join(SHOTS!, 'dust-hint-before.png') });

  // 1 token a slice at 0.05 KAS is 0.05 KAS: dust, the "?" flashes once
  await page.getByTestId('field-sliceAmount').fill('1');
  await expect(help).toHaveClass(/tk-flash-1/);
  await ticket.screenshot({ path: join(SHOTS!, 'dust-hint-ticket.png') });

  // the help shows the line under the slice field
  await page.waitForTimeout(1_500);
  await help.click();
  await expect(help).toHaveAttribute('aria-pressed', 'true');
  await expect(ticket).toContainText('is not shown in the public book');
  // typing on while it stays dust: no new flash
  await page.getByTestId('field-sliceAmount').fill('2');
  await expect(help).toHaveClass(/tk-flash-1/);
  await ticket.screenshot({ path: join(SHOTS!, 'dust-hint-open.png') });
});
