// The ticket says why the order button is off, always (founder, 2026-11-02): for every order type and side, a disabled button has a visible reason in the
// one fixed-height slot directly above it (`order-issues`); an enabled one has an empty slot. The unit of every input row sits in one fixed-width column at
// the right end of the row, and the cost line under the amount is never an empty gap.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType, settlePlan } from '../helpers/ticket';

test.use({ autoOpen: false });

for (const [name, width] of [['desktop', 1280], ['phone', 390]] as const) {
  test(`${name} ${width} px: a disabled order button always shows its reason; the slot keeps its box`, async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    const slot = page.getByTestId('order-issues');
    const button = page.getByTestId('order-place');
    const types = await page.getByTestId('order-type').locator('option').evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value));
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
        // an empty form: the button is off and says what to fill in
        // (a close order has no amount to give: it sells everything, and its slot is empty)
        if (await button.isDisabled()) {
          await expect(slot, `${type} ${side}: the reason of an empty form`).toContainText(/\S/);
          seenDisabled++;
        } else await expect(slot, `${type} ${side}: nothing to say`).not.toContainText(/\S/);
        // only an amount: whatever is still missing is named, the button stays off with a sentence
        await fillFields(page, { amount: '1' });
        await settlePlan(page);
        if (await button.isDisabled()) await expect(slot, `${type} ${side}: amount only`).toContainText(/\S/);
        else await expect(slot).not.toContainText(/\S/);
        // the slot, and the button under it, are where they were for every type that has the same fields above (the slot's own box never changes)
        const box = (await slot.boundingBox())!;
        slotBox ??= { x: box.x, y: 0, width: box.width, height: box.height };
        expect([box.x, box.width, box.height], `${type} ${side}: slot box`).toEqual([slotBox.x, slotBox.width, slotBox.height]);
      }
    }
    expect(seenDisabled).toBeGreaterThan(20);
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
  });
}
