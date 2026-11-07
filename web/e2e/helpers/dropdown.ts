// Drives the app's drop-down (ui/kit/Dropdown.tsx) through the test id of its button: the list exists only while it is open, in a panel `<id>-panel` outside the card the
// button is in, with the rows `<id>-option-<value>`. The current value is the button's `data-value`.
import type { Locator } from '@playwright/test';
import { expect } from '../fixtures';

const panelOf = async (control: Locator): Promise<Locator> => {
  const id = await control.getAttribute('data-testid');
  if (!id) throw new Error('a drop-down without a test id');
  return control.page().getByTestId(`${id}-panel`);
};

/** Opens the list and chooses, as a user does: by value or by label as shown, or by place (`{ index }`, disabled rows counted). The list is closed after. */
export async function chooseOption(control: Locator, choice: string | { index: number }): Promise<void> {
  const panel = await panelOf(control);
  await control.click();
  await expect(panel).toBeVisible();
  const rows = panel.getByRole('option');
  // a value first: "day" must not find the "90 days" of another row's label
  const exact = typeof choice === 'string' ? panel.locator(`[role="option"][data-value="${choice}"]`) : null;
  const row = typeof choice !== 'string' ? rows.nth(choice.index) : (await exact!.count()) > 0 ? exact!.first() : rows.filter({ hasText: choice }).first();
  await row.click();
  await expect(panel).toHaveCount(0);
}

export interface ShownOption {
  value: string;
  label: string;
  disabled: boolean;
}

/** What the list offers, in order; it is left closed. A click sent to the button, not a press: this reads the list, also of a button something stands over. */
export async function readOptions(control: Locator): Promise<ShownOption[]> {
  const panel = await panelOf(control);
  await control.dispatchEvent('click');
  await expect(panel).toBeVisible();
  const options = await panel.getByRole('option').evaluateAll((els) =>
    els.map((e) => ({ value: e.getAttribute('data-value') ?? '', label: e.querySelector('.picker-label')?.textContent ?? '', disabled: e.getAttribute('aria-disabled') === 'true' })),
  );
  await control.page().keyboard.press('Escape');
  await expect(panel).toHaveCount(0);
  return options;
}

export const optionValues = async (control: Locator): Promise<string[]> => (await readOptions(control)).map((o) => o.value);

export const optionLabels = async (control: Locator): Promise<string[]> => (await readOptions(control)).map((o) => o.label);

/** The label of one option; for `expect.poll` where the text follows another control. */
export const optionLabel = async (control: Locator, value: string): Promise<string | undefined> => (await readOptions(control)).find((o) => o.value === value)?.label;
