// Drives the market list's filter menu (ui/market/ListFilterMenu.tsx): its entries exist only while
// it is open, in a panel outside the list's card, and keep the test ids the separate buttons had
// (`token-list-favorites`, `token-list-hidden`, `token-list-nameless`, `token-list-held`). A choice
// closes the menu.
import type { Page } from '@playwright/test';
import { expect } from '../fixtures';

/** Opens the menu (when it is not open yet) and returns its panel. */
export async function openListFilter(page: Page) {
  const panel = page.getByTestId('token-list-filter-panel');
  if (!(await panel.isVisible())) await page.getByTestId('token-list-filter').click();
  await expect(panel).toBeVisible();
  return panel;
}

/**
 * Switches one filter of the menu, as a user does: open the menu, press the entry; the menu is
 * closed after.
 */
export async function toggleListFilter(page: Page, testid: string): Promise<void> {
  const panel = await openListFilter(page);
  await panel.getByTestId(testid).click();
  await expect(panel).toHaveCount(0);
}

/** Whether a filter is on, read in the menu (which is closed again after). */
export async function listFilterOn(page: Page, testid: string): Promise<boolean> {
  const panel = await openListFilter(page);
  const on = (await panel.getByTestId(testid).getAttribute('aria-checked')) === 'true';
  await page.keyboard.press('Escape');
  await expect(panel).toHaveCount(0);
  return on;
}
