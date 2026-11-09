// The ticket says why the order button is off, always (founder, 2026-11-02): for every order type and side, a disabled button says why. A REAL blocking error
// (a value that does not parse, too little balance ...) is red text in the one fixed-height slot directly above it (`order-issues`); a required field that is
// simply EMPTY (untouched, or reset after an order) is no error: the slot stays empty and the disabled button itself says what to enter ("Enter an amount"). The unit of every input row sits in one fixed-width column at
// the right end of the row, and the cost line under the amount is never an empty gap.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { optionValues } from '../helpers/dropdown';
import { fillFields, pickType, placeOrder, settlePlan, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

for (const [name, width] of [['desktop', 1280], ['phone', 390]] as const) {
  test(`${name} ${width} px: a disabled order button always shows its reason; the slot keeps its box`, async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    const slot = page.getByTestId('order-issues');
    const button = page.getByTestId('order-place');
    const types = await optionValues(page.getByTestId('order-type'));
    expect(types.length).toBeGreaterThan(12);
    let slotBox: { x: number; y: number; width: number; height: number } | null = null;
    let seenDisabled = 0;
    for (const type of types) {
      for (const side of ['buy', 'sell'] as const) {
        await pickType(page, type);
        const sideBtn = page.getByTestId(`order-side-${side}`);
        if (await sideBtn.isDisabled()) continue;
        await sideBtn.click();
        // (the amount and the price are carried from one type to the next: start from an empty form)
        await fillFields(page, { amount: '' });
        if (await page.getByTestId('order-price').count()) await fillFields(page, { price: '' });
        await settlePlan(page);
        // an empty form: the button is off and says what to enter; the slot has no red text (an empty field is not an error)
        // (a close order has no amount to give: it sells everything, and its slot is empty)
        if (await button.isDisabled()) {
          await expect(button, `${type} ${side}: the button of an empty form`).toHaveText(/^Enter /);
          await expect(slot, `${type} ${side}: no error text for an empty form`).not.toContainText(/\S/);
          seenDisabled++;
        } else await expect(slot, `${type} ${side}: nothing to say`).not.toContainText(/\S/);
        // only an amount: the first missing field is named by the button, or a real reason is in the slot
        await fillFields(page, { amount: '1' });
        await settlePlan(page);
        if (await button.isDisabled()) {
          if (/^Enter /.test((await button.textContent()) ?? '')) await expect(slot, `${type} ${side}: amount only, an empty field`).not.toContainText(/\S/);
          else await expect(slot, `${type} ${side}: amount only`).toContainText(/\S/);
        } else await expect(slot).not.toContainText(/\S/);
        // the slot, and the button under it, are where they were for every type that has the same fields above (the slot's own box never changes)
        const box = (await slot.boundingBox())!;
        slotBox ??= { x: box.x, y: 0, width: box.width, height: box.height };
        expect([box.x, box.width, box.height], `${type} ${side}: slot box`).toEqual([slotBox.x, slotBox.width, slotBox.height]);
      }
    }
    expect(seenDisabled).toBeGreaterThan(20);
  });

  test(`${name} ${width} px: an empty field is no error: the button says what to enter first, the slot is empty and keeps its box; after a successful order the reset form is calm; a real error is red in the slot`, async ({ appPage: page, mock }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    const slot = page.getByTestId('order-issues');
    const button = page.getByTestId('order-place');
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '', price: '' });
    await settlePlan(page);
    const box0 = (await slot.boundingBox())!;
    // the untouched form: "Enter an amount" on the disabled button, nothing in the slot, no red
    await expect(button).toBeDisabled();
    await expect(button).toHaveText('Enter an amount');
    await expect(slot).not.toContainText(/\S/);
    await expect(slot.locator('[data-severity="error"], .tk-reason-field')).toHaveCount(0);
    // the first missing field, in order: the price next
    await fillFields(page, { amount: '2' });
    await settlePlan(page);
    await fillFields(page, { price: '' });
    await settlePlan(page);
    await expect(button).toHaveText('Enter a price');
    await expect(slot).not.toContainText(/\S/);
    // a stop order names its stop price
    await pickType(page, 'stopLimit', 'sell');
    await fillFields(page, { amount: '2' });
    await settlePlan(page);
    await expect(button).toHaveText(/^Enter a (stop )?price$|^Enter stop/i);
    // a real error is red text in the slot (the value does not parse), and the button keeps its normal label
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: 'abc', price: '0.027' });
    await settlePlan(page);
    await expect(button).toBeDisabled();
    await expect(slot).toContainText(/\S/);
    await expect(slot.locator('.tk-reason-field')).toHaveCount(1);
    await expect(button).not.toHaveText(/^Enter /);
    // a placed order resets the amount: calm, not an error right after a success
    await fillFields(page, { amount: '2', price: '0.027' });
    await waitReviewable(page);
    await placeOrder(page, mock);
    await expect(page.getByTestId('order-amount')).toHaveValue('');
    await settlePlan(page);
    await expect(button).toHaveText('Enter an amount');
    await expect(slot).not.toContainText(/\S/);
    await expect(slot.locator('[data-severity="error"], .tk-reason-field')).toHaveCount(0);
    // the slot kept its size through all of it
    const box1 = (await slot.boundingBox())!;
    expect([box1.x, box1.width, box1.height]).toEqual([box0.x, box0.width, box0.height]);
  });

  test(`${name} ${width} px: every input row has its unit in one right-aligned column of one width; the cost line is a muted placeholder, not a gap`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    for (const type of ['limit', 'stopLimit', 'market', 'ifd']) {
      await pickType(page, type, 'buy');
      await page.getByTestId('order-advanced').locator('summary').click();
      // the rows of the form and the rows inside the Advanced box (which has its own padding) each keep one column
      for (const area of ['.tk-form > .field .tk-suffix, .tk-form > .tk-touch .tk-suffix', '.tk-advanced-body .tk-suffix']) {
        const units = page.locator(`[data-testid="order-ticket"] ${area}`);
        expect(await units.count(), `${type}: input rows with a unit in ${area}`).toBeGreaterThan(0);
        const boxes = await units.evaluateAll((els) => els.map((e) => {
          const r = e.getBoundingClientRect();
          return { right: Math.round(r.right * 2) / 2, width: Math.round(r.width * 2) / 2, align: getComputedStyle(e).textAlign };
        }));
        for (const b of boxes) {
          expect(b.width, `${type}: unit column width`).toBe(boxes[0]!.width);
          expect(b.right, `${type}: unit column right edge`).toBe(boxes[0]!.right);
          expect(b.align).toBe('right');
        }
      }
      await page.getByTestId('order-advanced').locator('summary').click();
    }
    await pickType(page, 'limit', 'buy');
    const cost = page.getByTestId('order-cost-preview');
    await expect(cost).toContainText(/\S{3,}/);
    expect((await cost.boundingBox())!.height).toBeLessThan(24);
    // the Available and cost lines under the amount: the figure ends under the box's (right-aligned) figure, the unit stands under the amount's unit
    await pickType(page, 'market', 'buy');
    await page.getByTestId('order-amount').fill('1');
    await expect(page.getByTestId('order-cost-preview').locator('.tk-line-unit')).toHaveText('KAS');
    await expect(page.getByTestId('order-available').locator('.tk-line-unit')).toHaveText('KAS');
    expect(await page.getByTestId('order-amount').evaluate((e) => getComputedStyle(e).textAlign)).toBe('right');
    const edges = await page.getByTestId('order-ticket').evaluate((root) => {
      const r = (sel: string) => root.querySelector(sel)!.getBoundingClientRect().right;
      const input = root.querySelector<HTMLInputElement>('[data-testid="order-amount"]')!;
      const pad = parseFloat(getComputedStyle(input).paddingRight) + parseFloat(getComputedStyle(input).borderRightWidth);
      const amountRow = input.closest('.field')!;
      return {
        unit: amountRow.querySelector('.tk-suffix')!.getBoundingClientRect().right,
        figure: input.getBoundingClientRect().right - pad,
        availUnit: r('[data-testid="order-available"] .tk-line-unit'),
        costUnit: r('[data-testid="order-cost-preview"] .tk-line-unit'),
        availFigure: r('[data-testid="order-available"] .tk-line-num'),
        costFigure: r('[data-testid="order-cost-preview"] .tk-line-text') - parseFloat(getComputedStyle(root.querySelector('[data-testid="order-cost-preview"] .tk-line-text')!).paddingRight),
      };
    });
    for (const k of ['availUnit', 'costUnit'] as const) expect(Math.abs(edges[k] - edges.unit), k).toBeLessThan(1);
    for (const k of ['availFigure', 'costFigure'] as const) expect(Math.abs(edges[k] - edges.figure), k).toBeLessThan(1);
  });
}
