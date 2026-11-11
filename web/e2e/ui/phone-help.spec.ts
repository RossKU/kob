// Field help of the ticket: off at first on every width (UI review H-03: the form is its labels and boxes, the long text of a field is its tooltip), shown by the title row's "?";
// shown, it wraps (a phone has no hover tooltip) instead of ending in an ellipsis. Mock stack.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { pickType } from '../helpers/ticket';

test.use({ autoOpen: false });

test.describe('desktop, 1280 px wide', () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test('the field help is off at first, a field keeps its long help as its tooltip, and the "?" shows it', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await expect(page.getByTestId('order-help-toggle')).toHaveAttribute('aria-pressed', 'false');
    await expect(page.getByTestId('order-type-help')).toBeHidden();
    // no hint box that holds only the help is left in the layout (an empty box would still take the field's gap)
    const boxes = await page.getByTestId('order-ticket').evaluate((root) => [...root.querySelectorAll<HTMLElement>('.field-hint')].filter((e) => e.offsetHeight > 0 && e.querySelector(':scope > .tk-help') && !e.querySelector(':scope > :not(.tk-help)')).length);
    expect(boxes).toBe(0);
    // the tooltip of the order type and of the price box is their long help
    await expect(page.getByTestId('order-type').locator('xpath=ancestor::div[@title][1]')).toHaveAttribute('title', /Rests at your price/);
    await expect(page.getByTestId('order-price').locator('xpath=ancestor::div[@title][1]')).toHaveAttribute('title', /rounded to the price step/);
    // turned on, the help stands under the fields and stays on the next visit
    await page.getByTestId('order-help-toggle').click();
    await expect(page.getByTestId('order-type-help')).toBeVisible();
    await page.reload();
    await expect(page.getByTestId('order-help-toggle')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('order-type-help')).toBeVisible();
  });
});

test.describe('phone, 390 px wide', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('field help is wrapped, never cut with an ellipsis', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'stopMarket', 'sell'); // the longest help texts
    // a phone hides the field help at first (the form fits the panel); the title row's "?" shows it
    await expect(page.getByTestId('order-type-help')).toBeHidden();
    await page.getByTestId('order-help-toggle').click();
    await expect(page.getByTestId('order-help-toggle')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('order-type-help')).toBeVisible();
    const cut = await page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>('.tk-help, .tk-sub')].map((el) => ({ text: (el.textContent ?? '').slice(0, 40), ws: getComputedStyle(el).whiteSpace, over: el.scrollWidth - el.clientWidth })).filter((r) => r.ws === 'nowrap' || r.over > 1),
    );
    expect(cut).toEqual([]);
    const help = page.getByTestId('order-type-help');
    const lines = await help.evaluate((el) => Math.round(el.getBoundingClientRect().height / parseFloat(getComputedStyle(el).lineHeight)));
    expect(lines, 'the type help takes more than one line and is shown whole').toBeGreaterThan(1);
  });
});
