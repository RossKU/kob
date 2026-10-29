// The ticket's amount slider, the pro style (founder, 2026-10-29): the width of the ticket, tick marks at 0 / 25 / 50 / 75 / 100 % with their labels in one row
// directly under the track and aligned to the ticks (the labels are the buttons), a magnetic snap within 3 % of a tick while dragging, a bubble with the value
// above the thumb while dragging, the filled track in the colour of the side, a 44 px thumb on a phone, the keyboard (arrows 1 %, Page Up / Down 25 %).
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType } from '../helpers/ticket';

test.use({ autoOpen: false });

async function openSlider(page: Page, mock: Parameters<typeof fund>[0], side: 'buy' | 'sell'): Promise<void> {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'limit', side);
  await fillFields(page, { price: side === 'sell' ? '0.03' : '0.0245' });
  await expect(page.getByTestId('order-amount-range')).toBeEnabled();
}

/** Where the thumb's centre is for `pct` percent: the thumb is `thumb` px wide and travels the track less its own width. */
const xAt = (box: { x: number; width: number }, thumb: number, pct: number): number => box.x + thumb / 2 + (box.width - thumb) * (pct / 100);

for (const [name, width] of [['desktop', 1280], ['phone', 390]] as const) {
  test.describe(`${name} ${width} px`, () => {
    test.use({ viewport: { width, height: 900 } });

    test('full width, ticks and labels in one aligned row, the labels set the amount', async ({ appPage: page, mock }) => {
      await openSlider(page, mock, 'sell');
      const slider = page.getByTestId('order-amount-slider');
      const range = page.getByTestId('order-amount-range');
      const sb = (await slider.boundingBox())!;
      const tb = (await page.getByTestId('order-ticket').boundingBox())!;
      // the width of the ticket (its padding on both sides)
      expect(sb.width).toBeGreaterThan(tb.width - 40);
      expect(sb.x).toBeGreaterThanOrEqual(tb.x);
      expect(sb.x + sb.width).toBeLessThanOrEqual(tb.x + tb.width);
      const rb = (await range.boundingBox())!;
      const thumb = parseFloat(await slider.evaluate((el) => getComputedStyle(el).getPropertyValue('--thumb')));
      expect(thumb).toBe(width < 760 ? 44 : 18);
      // five labels, one row, each centred under its tick, below the track
      const labels = await Promise.all([0, 25, 50, 75, 100].map((v) => page.getByTestId(`order-amount-pct-${v}`).boundingBox()));
      const ys = labels.map((b) => Math.round(b!.y));
      expect(new Set(ys).size).toBe(1);
      expect(labels[0]!.y).toBeGreaterThanOrEqual(rb.y + rb.height - thumb / 2);
      [0, 25, 50, 75, 100].forEach((v, i) => {
        const centre = labels[i]!.x + labels[i]!.width / 2;
        expect(Math.abs(centre - xAt(rb, thumb, v)), `label ${v}`).toBeLessThan(2);
      });
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
      // the labels replace the separate % buttons: a click sets the amount
      await page.getByTestId('order-amount-pct-50').click();
      await expect(slider).toHaveAttribute('data-pct', '50');
      await expect(page.getByTestId('order-amount')).toHaveValue('50');
      await page.getByTestId('order-amount-pct-100').click();
      await expect(slider).toHaveAttribute('data-pct', '100');
      await page.getByTestId('order-amount-pct-0').click();
      await expect(page.getByTestId('order-amount')).toHaveValue('');
    });

    test('dragging: it snaps to a tick within 3 %, a bubble reads the value, the track is the side\'s colour', async ({ appPage: page, mock }) => {
      await openSlider(page, mock, 'sell');
      const slider = page.getByTestId('order-amount-slider');
      const range = page.getByTestId('order-amount-range');
      const bubble = page.getByTestId('order-amount-bubble');
      await expect(bubble).toBeHidden();
      await page.getByTestId('order-amount-pct-50').click();
      const rb = (await range.boundingBox())!;
      const thumb = parseFloat(await slider.evaluate((el) => getComputedStyle(el).getPropertyValue('--thumb')));
      const cy = rb.y + rb.height / 2;
      await page.mouse.move(xAt(rb, thumb, 50), cy);
      await page.mouse.down();
      // 27 % is within 3 % of the 25 % tick: it snaps there
      await page.mouse.move(xAt(rb, thumb, 27), cy, { steps: 6 });
      await expect(slider).toHaveAttribute('data-pct', '25');
      await expect(bubble).toBeVisible();
      await expect(bubble).toHaveText('25%');
      // 40 % is far from every tick: it stays
      await page.mouse.move(xAt(rb, thumb, 40), cy, { steps: 6 });
      await expect(slider).toHaveAttribute('data-pct', '40');
      await expect(bubble).toHaveText('40%');
      // 73 % is within 3 % of 75 %
      await page.mouse.move(xAt(rb, thumb, 73), cy, { steps: 6 });
      await expect(slider).toHaveAttribute('data-pct', '75');
      await page.mouse.up();
      await expect(bubble).toBeHidden();
      await expect(page.getByTestId('order-amount')).toHaveValue('75');

      // the filled track: green for a buy, red for a sell (the slider's --slider-fill is what the track is painted with)
      const colours = () =>
        slider.evaluate((el) => {
          const of = (v: string) => {
            const probe = document.createElement('span');
            probe.style.color = v;
            el.append(probe);
            const c = getComputedStyle(probe).color;
            probe.remove();
            return c;
          };
          return { fill: of('var(--slider-fill)'), buy: of('var(--buy)'), sell: of('var(--sell)') };
        });
      let c = await colours();
      expect(c.fill).toBe(c.sell);
      await page.getByTestId('order-side-buy').click();
      await expect(slider).toHaveAttribute('data-side', 'buy');
      await fillFields(page, { price: '0.0245' });
      await expect(range).toBeEnabled();
      c = await colours();
      expect(c.fill).toBe(c.buy);
      expect(c.buy).not.toBe(c.sell);
    });

    test('keyboard: arrows 1 %, Page Up / Page Down 25 %, Home / End', async ({ appPage: page, mock }) => {
      await openSlider(page, mock, 'sell');
      const slider = page.getByTestId('order-amount-slider');
      const range = page.getByTestId('order-amount-range');
      await page.getByTestId('order-amount-pct-50').click();
      await range.focus();
      await page.keyboard.press('ArrowRight');
      await expect(slider).toHaveAttribute('data-pct', '51');
      await page.keyboard.press('ArrowLeft');
      await page.keyboard.press('ArrowLeft');
      await expect(slider).toHaveAttribute('data-pct', '49');
      await page.keyboard.press('PageUp');
      await expect(slider).toHaveAttribute('data-pct', '74');
      await page.keyboard.press('PageDown');
      await page.keyboard.press('PageDown');
      await expect(slider).toHaveAttribute('data-pct', '24');
      await page.keyboard.press('End');
      await expect(slider).toHaveAttribute('data-pct', '100');
      await page.keyboard.press('PageUp');
      await expect(slider).toHaveAttribute('data-pct', '100');
      await page.keyboard.press('Home');
      await expect(page.getByTestId('order-amount')).toHaveValue('');
    });
  });
}
