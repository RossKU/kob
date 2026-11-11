// The app's motion (founder, 2026-11-11: subtle, fast, restrained): the sliding mark of a segmented control (the selected pill) and of a tab strip (the underline)
// sits under the selected option on the first paint, after a click and after a resize, and takes no room (the control's box never changes); with reduced motion
// the transitions are gone. The mark is a transform + width on an absolutely positioned element (kit/slide-indicator.ts). Mock stack.
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { connectWallet, listExkcc, openMarket } from '../helpers/env';

test.use({ autoOpen: false });

type Box = { x: number; y: number; width: number; height: number };

const box = async (el: Locator): Promise<Box> => {
  const b = await el.boundingBox();
  if (!b) throw new Error('no box');
  return b;
};

/** The mark of `group` against the option: the pill covers it, the underline lies on its bottom edge (no transition under way: the box is the place it was sent to). */
async function expectMarkUnder(group: Locator, option: Locator, kind: 'pill' | 'underline'): Promise<void> {
  const mark = group.locator(':scope > .slide-ind');
  await expect
    .poll(
      async () => {
        const [m, o] = await Promise.all([box(mark), box(option)]);
        const dx = Math.max(Math.abs(m.x - o.x), Math.abs(m.width - o.width));
        const dy = kind === 'pill' ? Math.max(Math.abs(m.y - o.y), Math.abs(m.height - o.height)) : Math.abs(m.y + m.height - (o.y + o.height));
        return Math.max(dx, dy) <= 1;
      },
      { message: `the mark of ${kind} is under the selected option`, timeout: 5_000 },
    )
    .toBe(true);
}

const everyTransitionOff = (page: Page, selector: string) =>
  page.locator(selector).first().evaluate((el) => getComputedStyle(el).transitionDuration.split(',').every((d) => parseFloat(d) === 0));

test.describe('segmented control: the selected pill', () => {
  test('is under the selected side on the first paint, after a click and after a resize; the control keeps its box', async ({ appPage: page, mock }) => {
    await openMarket(page, mock);
    const side = page.getByTestId('order-side-buy').locator('xpath=..');
    const [a, b] = [page.getByTestId('order-side-buy'), page.getByTestId('order-side-sell')];
    await expect(a).toBeVisible();
    // whichever side the ticket opens on: the first paint has the mark under it already (no waiting: it has not slid in from anywhere)
    const startsOnBuy = (await a.getAttribute('aria-checked')) === 'true';
    const [first, second] = startsOnBuy ? [a, b] : [b, a];
    const m0 = await box(side.locator(':scope > .slide-ind'));
    const b0 = await box(first);
    expect(Math.abs(m0.x - b0.x)).toBeLessThanOrEqual(1);
    expect(Math.abs(m0.width - b0.width)).toBeLessThanOrEqual(1);
    const before = await box(side);

    await second.click();
    await expect(second).toHaveAttribute('aria-checked', 'true');
    await expectMarkUnder(side, second, 'pill');
    const colour = await side.locator(':scope > .slide-ind').evaluate((e) => getComputedStyle(e).backgroundColor);
    // the sell side is the red pill, the buy side the green one: the mark's colour follows the selection
    await expect(side).toHaveAttribute('data-tone', second === b ? 'sell' : 'buy');
    await first.click();
    await expectMarkUnder(side, first, 'pill');
    await expect(side).toHaveAttribute('data-tone', first === b ? 'sell' : 'buy');
    await expect.poll(() => side.locator(':scope > .slide-ind').evaluate((e) => getComputedStyle(e).backgroundColor)).not.toBe(colour);

    // a resize: the pill follows its segment at once
    await second.click();
    for (const width of [900, 600, 390, 1280]) {
      await page.setViewportSize({ width, height: 900 });
      await expect(second).toHaveAttribute('aria-checked', 'true');
      await expectMarkUnder(side, second, 'pill');
    }
    // the mark takes no room: the group's box is what it was
    const after = await box(side);
    expect(Math.abs(after.height - before.height)).toBeLessThanOrEqual(0.5);
  });
});

test.describe('tab strip: the active underline', () => {
  test('lies under the active tab after a click and after a resize', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    const token = await mock.token();
    await page.goto(`/#/market/${token.covenant_id}`);
    await connectWallet(page, 'kasware');
    const panel = page.getByTestId('market-orders');
    await expect(panel).toBeVisible();
    const strip = panel.getByRole('tablist');
    const tab = (id: string) => panel.getByTestId(`orders-tab-${id}`);
    await expectMarkUnder(strip, strip.locator('[aria-selected="true"]'), 'underline');
    const before = await box(strip);
    for (const id of ['history', 'fills', 'balances', 'active']) {
      await tab(id).click();
      await expect(tab(id)).toHaveAttribute('aria-selected', 'true');
      await expectMarkUnder(strip, tab(id), 'underline');
    }
    await tab('fills').click();
    for (const width of [800, 390, 1280]) {
      await page.setViewportSize({ width, height: 900 });
      await expect(tab('fills')).toHaveAttribute('aria-selected', 'true');
      await expectMarkUnder(strip, tab('fills'), 'underline');
    }
    await page.setViewportSize({ width: 1280, height: 720 });
    await expect(strip).toBeVisible();
    // the mark takes no room: the strip is as high as before (its width is the page's)
    await expect.poll(async () => Math.abs((await box(strip)).height - before.height)).toBeLessThanOrEqual(0.5);
  });
});

test.describe('motion is subtle, and reduced motion turns it off', () => {
  test('the pill, the buttons and the rows ease; with `prefers-reduced-motion: reduce` they do not', async ({ appPage: page, mock }) => {
    await page.emulateMedia({ reducedMotion: 'no-preference' });
    await openMarket(page, mock);
    const side = page.getByTestId('order-side-buy').locator('xpath=..');
    await expect(side).toBeVisible();
    expect(await everyTransitionOff(page, '[data-testid="order-side-buy"]')).toBe(false);
    // a slide (a click on the other side) has the stylesheet's transition on; the first paint and a resize have it off
    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-side-buy').click();
    // the durations are the quick ones (100-200 ms)
    const ms = await side.locator(':scope > .slide-ind').evaluate((e) => Math.max(...getComputedStyle(e).transitionDuration.split(',').map((d) => parseFloat(d) * 1000)));
    expect(ms).toBeGreaterThanOrEqual(100);
    expect(ms).toBeLessThanOrEqual(200);

    await page.emulateMedia({ reducedMotion: 'reduce' });
    for (const selector of ['[data-testid="order-side-buy"]', '.slide-ind', '.btn', '.tabs button']) {
      if ((await page.locator(selector.trim()).count()) === 0) continue;
      expect(await everyTransitionOff(page, selector.trim()), selector).toBe(true);
    }
    // and a pressed button does not shrink
    const btn = page.locator('.btn:not(:disabled)').first();
    await btn.hover();
    await page.mouse.down();
    expect(await btn.evaluate((e) => getComputedStyle(e).transform)).toBe('none');
    await page.mouse.up();
  });
});
