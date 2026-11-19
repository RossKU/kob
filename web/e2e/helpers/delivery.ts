// EXSTD, a listed token of the standard 3 / 3 program (KCC20Ref, the program with a top-up extra),
// for the delivery specs.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect } from '../fixtures';
import { connectWallet, fund } from './env';

/**
 * EXSTD: a listed token of the standard 3 / 3 program (KCC20Ref, the program with a top-up extra),
 * as in market-fills.spec.ts
 */
export const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);

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

export async function openStd(page: Page, mock: Parameters<typeof fund>[0]): Promise<void> {
  await fund(mock);
  await mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3' }] });
  await registryWithStd(page);
  await page.goto(`/#/market/${STD}`);
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('order-ticket')).toBeVisible();
}

