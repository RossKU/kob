// The app's drop-down (ui/kit/Dropdown.tsx) in a page: what a screen reader is told, the keys, and that the list is whole and on screen, on a phone and at the bottom of the
// window too. What each key does in every state is unit tested (src/ui/kit/dropdown-model.test.ts); here the focus, the portal and the layout, which need a browser.
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { chooseOption, optionValues, readOptions } from '../helpers/dropdown';
import { fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';

const activeValue = (page: Page) => page.locator('.picker-option.is-active').getAttribute('data-value');

/** Scrolls the page until the button stands at the bottom of the window (a padding on top gives a button high on the page the room). */
const toBottom = (face: Locator) =>
  face.evaluate((el) => {
    document.body.style.paddingTop = `${window.innerHeight}px`;
    window.scrollBy(0, el.getBoundingClientRect().bottom - window.innerHeight + 16);
  });

/** The panel is inside the window and the last row, scrolled to, is the element a press there reaches: nothing cuts it or stands over it. */
async function expectWhole(page: Page, panel: Locator): Promise<void> {
  const size = page.viewportSize()!;
  const box = (await panel.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(size.width);
  expect(box.y + box.height).toBeLessThanOrEqual(size.height);
  const hit = await panel.getByRole('option').last().evaluate((row) => {
    row.scrollIntoView({ block: 'nearest' });
    const r = row.getBoundingClientRect();
    return row.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2));
  });
  expect(hit).toBe(true);
  expect(await horizontalOverflow(page)).toBe(0);
}

test.describe('the order type', () => {
  test.beforeEach(async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
  });

  test('a button that names its label and value, a list with groups and one chosen row, and the label opens it', async ({ appPage: page }) => {
    const face = page.getByTestId('order-type');
    const panel = page.getByTestId('order-type-panel');
    await expect(face).toHaveAttribute('aria-haspopup', 'listbox');
    await expect(face).toHaveAttribute('aria-expanded', 'false');
    await expect(face).toHaveAttribute('data-value', 'limit');
    await expect(page.getByRole('button', { name: 'Order type Limit', exact: true })).toHaveAttribute('data-testid', 'order-type');
    await expect(page.getByLabel('Order type')).toHaveAttribute('data-testid', 'order-type');

    await page.locator('label[for="order-type"]').click();
    await expect(panel).toBeVisible();
    await expect(face).toHaveAttribute('aria-expanded', 'true');
    const list = panel.getByRole('listbox', { name: 'Order type' });
    await expect(list).toBeFocused();
    await expect(face).toHaveAttribute('aria-controls', (await list.getAttribute('id'))!);
    await expect(list.getByRole('group')).toHaveCount(4);
    await expect(list.getByRole('group', { name: 'Basic' }).getByRole('option')).toHaveCount(6);
    await expect(list.getByRole('option')).toHaveCount(18);
    await expect(list.getByRole('option', { selected: true })).toHaveCount(1);
    await expect(list.getByRole('option', { selected: true })).toHaveAttribute('data-value', 'limit');
    const active = await list.getAttribute('aria-activedescendant');
    await expect(page.locator(`[id="${active}"]`)).toHaveAttribute('data-value', 'limit');
    await page.keyboard.press('Escape');
    await expect(panel).toHaveCount(0);
    await expect(face).toBeFocused();
  });

  test('the keys: arrows, Home, End, a typed word, Enter, Space, Escape and Tab; the focus comes back to the button', async ({ appPage: page }) => {
    const face = page.getByTestId('order-type');
    const panel = page.getByTestId('order-type-panel');
    const ticket = page.getByTestId('order-ticket');
    await face.focus();

    await page.keyboard.press('ArrowDown');
    await expect(panel).toBeVisible();
    expect(await activeValue(page)).toBe('limit');
    await page.keyboard.press('ArrowDown');
    expect(await activeValue(page)).toBe('market');
    await page.keyboard.press('End');
    expect(await activeValue(page)).toBe('dutch');
    await page.keyboard.press('ArrowDown');
    expect(await activeValue(page)).toBe('dutch');
    await page.keyboard.press('Home');
    expect(await activeValue(page)).toBe('limit');
    await page.keyboard.type('tw');
    expect(await activeValue(page)).toBe('twap');

    await page.keyboard.press('Escape');
    await expect(panel).toHaveCount(0);
    await expect(face).toBeFocused();
    await expect(ticket).toHaveAttribute('data-type', 'limit');

    await page.keyboard.press('Enter');
    await expect(panel).toBeVisible();
    await page.keyboard.press('End');
    await page.keyboard.press('Enter');
    await expect(ticket).toHaveAttribute('data-type', 'dutch');
    await expect(face).toHaveAttribute('data-value', 'dutch');
    await expect(face).toBeFocused();

    await page.keyboard.press('Space');
    await expect(panel).toBeVisible();
    await page.keyboard.press('Home');
    await page.keyboard.press('Space');
    await expect(ticket).toHaveAttribute('data-type', 'limit');
    await expect(face).toBeFocused();

    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('End');
    await page.keyboard.press('Tab');
    await expect(panel).toHaveCount(0);
    await expect(ticket).toHaveAttribute('data-type', 'limit');
    await expect(face).not.toBeFocused();
  });

  test('a letter on the closed button opens the list on its row and changes nothing', async ({ appPage: page }) => {
    const face = page.getByTestId('order-type');
    await face.focus();
    await page.keyboard.press('m');
    await expect(page.getByTestId('order-type-panel')).toBeVisible();
    expect(await activeValue(page)).toBe('market');
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-type', 'limit');
    await page.keyboard.press('Escape');
  });

  test('a press outside closes it and gives the focus back; a change of page closes it too', async ({ appPage: page }) => {
    const face = page.getByTestId('order-type');
    const panel = page.getByTestId('order-type-panel');
    await face.click();
    await expect(panel).toBeVisible();
    await page.mouse.click(1, 1);
    await expect(panel).toHaveCount(0);
    await expect(face).toHaveAttribute('aria-expanded', 'false');
    await face.click();
    await expect(panel).toBeVisible();
    await page.evaluate(() => { location.hash = '#/settings'; });
    await expect(panel).toHaveCount(0);
  });

  test('the panel is in the body and follows the button when the page scrolls or the window changes', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    const face = page.getByTestId('order-type');
    const panel = page.getByTestId('order-type-panel');
    await face.click();
    expect(await panel.evaluate((el) => el.parentElement === document.body)).toBe(true);
    const before = (await panel.boundingBox())!;
    await page.evaluate(() => window.scrollBy(0, 40));
    await expect.poll(async () => (await panel.boundingBox())!.y).not.toBe(before.y);
    await page.setViewportSize({ width: 390, height: 500 });
    await expect.poll(async () => (await panel.boundingBox())!.x + (await panel.boundingBox())!.width).toBeLessThanOrEqual(390);
    await page.keyboard.press('Escape');
    await expect(panel).toHaveCount(0);
  });

  for (const [name, size] of [['a phone', { width: 390, height: 844 }], ['a wide window', { width: 1280, height: 800 }]] as const) {
    test(`on ${name}: the list stays whole in the window, also with the button at the bottom of it`, async ({ appPage: page }) => {
      await page.setViewportSize(size);
      const face = page.getByTestId('order-type');
      const panel = page.getByTestId('order-type-panel');
      await face.scrollIntoViewIfNeeded();
      await face.click();
      await expectWhole(page, panel);
      await page.keyboard.press('Escape');

      await toBottom(face);
      await face.click();
      await expectWhole(page, panel);
      expect((await panel.boundingBox())!.y).toBeLessThan((await face.boundingBox())!.y);
      await page.keyboard.press('Escape');

      const group = page.getByTestId('book-group');
      await group.scrollIntoViewIfNeeded();
      await group.click();
      await expectWhole(page, page.getByTestId('book-group-panel'));
      await page.keyboard.press('Escape');
    });
  }

  test('a field of the ticket: the label names it, a choice shows the fields it brings', async ({ appPage: page }) => {
    await page.getByTestId('order-advanced').locator('summary').click();
    const lifetime = page.getByTestId('field-lifetime');
    await expect(lifetime).toHaveAttribute('data-value', 'gtc');
    expect((await readOptions(lifetime)).map((o) => o.value)).toEqual(['gtc', 'gtd', 'day']);
    await chooseOption(lifetime, 'gtd');
    await expect(lifetime).toHaveAttribute('data-value', 'gtd');
    await expect(page.getByTestId('field-lifetimeAt')).toBeVisible();
  });
});

test('Settings: the kit\'s Field keeps its label on the control', async ({ appPage: page }) => {
  await page.goto('/#/settings');
  const network = page.getByTestId('settings-network');
  await expect(network).toBeVisible();
  const id = await network.getAttribute('id');
  const label = page.locator(`label[for="${id}"]`);
  await expect(label).toHaveText('Network');
  await expect(network).toHaveAttribute('aria-labelledby', `${id}-label ${id}`);
  await expect(network).toHaveAttribute('aria-describedby', `${id}-hint`);
  await label.click();
  await expect(page.getByTestId('settings-network-panel')).toBeVisible();
  await page.keyboard.press('Escape');
  expect(await optionValues(network)).toEqual(['mainnet', 'testnet-10']);
});
