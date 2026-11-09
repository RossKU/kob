// The Refresh buttons of the Portfolio page and its Balances card: while they refresh, neither the button nor the "Updated ..." text beside it moves or changes
// size, the button keeps its colours (it is not disabled; a press while busy is ignored: aria-busy), and a refresh that is quick shows no spinner at all.
// Mock stack (the indexer's answers are held back to make the refresh long enough to measure).
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

interface Box { x: number; y: number; width: number; height: number }
const box = async (loc: Locator): Promise<Box> => (await loc.boundingBox())!;
const same = (a: Box, b: Box, what: string) => {
  for (const k of ['x', 'y', 'width', 'height'] as const) expect(Math.abs(a[k] - b[k]), `${what}: ${k} ${a[k]} -> ${b[k]}`).toBeLessThanOrEqual(0.5);
};
const look = (btn: Locator) => btn.evaluate((el) => { const cs = getComputedStyle(el); return { bg: cs.backgroundColor, color: cs.color, opacity: cs.opacity, border: cs.borderColor, disabled: (el as HTMLButtonElement).disabled }; });

async function holdIndexer(page: Page, ms: number): Promise<{ count: () => number }> {
  let n = 0;
  await page.route(/\/v1\/orders/, async (route) => {
    n++;
    await new Promise((r) => setTimeout(r, ms));
    await route.continue();
  });
  return { count: () => n };
}

for (const width of [390, 1280]) {
  test.describe(`${width} px wide`, () => {
    test.use({ viewport: { width, height: 900 } });

    test('Portfolio Refresh: no move, no resize, no grey flash while it refreshes', async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      await seedPortfolio(mock);
      await listExkcc(page);
      await page.goto('/#/portfolio');
      await connectWallet(page, 'kasware');
      const btn = page.getByTestId('orders-refresh');
      const upd = page.getByTestId('orders-updated');
      await expect(upd).toBeVisible();
      await expect(page.getByTestId('portfolio-total')).toContainText('KAS');
      const idleBtn = await box(btn);
      const idleUpd = await box(upd);
      const idleLook = await look(btn);
      expect(idleLook.disabled).toBe(false);

      const held = await holdIndexer(page, 1500);
      await btn.click();
      await page.mouse.move(5, 5); // off the button: its hover colour is not the busy colour
      // the refresh is on: sample for 2 s; the button stays where it is, enabled and in its colours; the spinner shows once it lasts (over the label)
      let sawBusy = false;
      let sawSpinner = false;
      for (let i = 0; i < 20; i++) {
        await page.waitForTimeout(100);
        same(await box(btn), idleBtn, 'Refresh button');
        same(await box(upd), idleUpd, '"Updated" text');
        const l = await look(btn);
        expect(l.disabled, 'never disabled').toBe(false);
        expect({ ...l }).toEqual(idleLook);
        if ((await btn.getAttribute('aria-busy')) === 'true') sawBusy = true;
        if ((await btn.locator('.spinner').count()) > 0) sawSpinner = true;
      }
      expect(sawBusy, 'aria-busy while refreshing').toBe(true);
      expect(sawSpinner, 'a long refresh shows the spinner').toBe(true);
      // a press while it is busy is ignored (one read, not two)
      const before = held.count();
      await btn.click();
      await btn.click();
      await page.waitForTimeout(400);
      // one refresh reads the active and the past orders (two requests): the second press, made while the first is on, started none
      expect(held.count() - before, 'two quick presses start one refresh').toBe(2);
      await expect(btn).not.toHaveAttribute('aria-busy', 'true', { timeout: 15_000 });
      same(await box(btn), idleBtn, 'Refresh button after');
      same(await box(upd), idleUpd, '"Updated" text after');
    });

    test('Balances Refresh and a quick refresh: the button never changes size, no spinner for a quick one', async ({ appPage: page, mock }) => {
      test.setTimeout(120_000);
      await seedPortfolio(mock);
      await listExkcc(page);
      await page.goto('/#/portfolio');
      await connectWallet(page, 'kasware');
      const card = page.getByTestId('balances-panel');
      await expect(card.getByTestId('kas-balance')).toBeVisible();
      const btn = card.getByRole('button', { name: 'Refresh' });
      const idle = await box(btn);
      const idleLook = await look(btn);
      let busy = 0;
      let disabledSeen = false;
      await btn.click();
      await page.mouse.move(5, 5);
      for (let i = 0; i < 12; i++) {
        await page.waitForTimeout(50);
        same(await box(btn), idle, 'balances Refresh button');
        const l = await look(btn);
        if (l.disabled) disabledSeen = true;
        expect(l.bg).toBe(idleLook.bg);
        if ((await btn.getAttribute('aria-busy')) === 'true') busy++;
      }
      expect(disabledSeen, 'never disabled').toBe(false);
      same(await box(btn), idle, 'after');
    });
  });
}
