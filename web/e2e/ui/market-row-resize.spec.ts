// The drag handle under the trading row (founder, 2026-11-11): on three columns with a fine pointer a horizontal separator under book | chart | ticket sets the row's height. Never below the ticket's
// fit (the ticket has no empty room), the chart's box takes the extra height, the room under the ticket card is the "Market trades" panel (the recent fills, from the tape the page already reads), the
// choice is kept over a reload, a double-click or Enter resets, ArrowUp / ArrowDown move it 16 px. No handle on a phone or a two-column window. Mock stack, the token page of EXKCC.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

test.use({ autoOpen: false });

const handle = (page: Page) => page.getByTestId('market-row-resize');
const chartBox = async (page: Page) => (await page.locator('.chart-box').boundingBox())!.height;
const rowH = (page: Page) => page.getByTestId('book-section').evaluate((el) => el.getBoundingClientRect().height);
const num = async (page: Page, attr: string) => Number(await handle(page).getAttribute(attr));

async function open(page: Page, mock: import('../fixtures').MockClient, width: number, height: number): Promise<void> {
  await page.setViewportSize({ width, height });
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await fund(mock);
  await openMarket(page, mock);
  await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
  // the page settles once the ticket has measured its form (--tk-fit)
  await expect.poll(() => page.locator('.mkt-grid').evaluate((el) => (el as HTMLElement).style.getPropertyValue('--tk-fit'))).toMatch(/px$/);
  await page.waitForTimeout(400);
}

/** The ticket's form has no empty band: from the bottom of the Advanced options box to the top of the pinned footer at most 24 px. */
async function expectNoGap(page: Page, label: string): Promise<void> {
  const band = await page.evaluate(() => document.querySelector('.tk-foot')!.getBoundingClientRect().top - document.querySelector('[data-testid="order-advanced"]')!.getBoundingClientRect().bottom);
  expect(band, `${label}: the ticket's empty band`).toBeLessThanOrEqual(24);
  expect(band).toBeGreaterThanOrEqual(0);
}

async function drag(page: Page, dy: number): Promise<void> {
  const b = (await handle(page).boundingBox())!;
  const x = b.x + b.width / 2;
  const y = b.y + b.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  for (let i = 1; i <= 10; i++) await page.mouse.move(x, y + (dy * i) / 10);
  await page.mouse.up();
}

for (const [width, height] of [[1280, 900], [1920, 1080]] as const) {
  test(`${width}x${height}: dragging the handle down 200 px grows the chart by about 200, the ticket keeps no gap, Market trades appears, the height is kept over a reload, a double-click resets`, async ({ appPage: page, mock }) => {
    await open(page, mock, width, height);
    await expect(handle(page)).toBeVisible();
    await expect(handle(page)).toHaveAttribute('role', 'separator');
    await expect(handle(page)).toHaveAttribute('aria-orientation', 'horizontal');
    const h0 = await rowH(page);
    const c0 = await chartBox(page);
    const place0 = (await page.getByTestId('order-place').boundingBox())!;
    expect(await num(page, 'aria-valuenow'), 'the value is the row').toBeCloseTo(h0, 0);
    expect(await num(page, 'aria-valuemin'), 'the least height is the default').toBeLessThanOrEqual(h0 + 1);
    // by default the room under the ticket is none: no Market trades panel is drawn
    await expect(page.getByTestId('market-trades')).toBeHidden();
    await expectNoGap(page, 'default');

    await drag(page, 200);
    expect(await rowH(page) - h0, 'the row grew by the drag').toBeGreaterThan(190);
    expect(await rowH(page) - h0).toBeLessThanOrEqual(205);
    expect((await chartBox(page)) - c0, 'the chart takes it').toBeGreaterThan(190);
    expect(await num(page, 'aria-valuenow')).toBeCloseTo(await rowH(page), 0);
    // the ticket card keeps its height and its button: no empty room in it; the room under it is the Market trades panel
    await expectNoGap(page, 'dragged');
    const place1 = (await page.getByTestId('order-place').boundingBox())!;
    expect(Math.abs(place1.y - place0.y), 'the place button stays where it was').toBeLessThanOrEqual(1);
    const trades = page.getByTestId('market-trades');
    await expect(trades).toBeVisible();
    await expect(trades.getByTestId('market-trade-row').first()).toBeVisible();
    expect(await trades.getByTestId('market-trade-row').count()).toBeGreaterThan(2);
    const t = (await trades.boundingBox())!;
    const tk = (await page.getByTestId('order-ticket').boundingBox())!;
    expect(t.y, 'under the ticket card').toBeGreaterThanOrEqual(tk.y + tk.height);
    expect(Math.abs(t.y + t.height - ((await page.getByTestId('book-section').boundingBox())!.y + (await rowH(page)))), 'ends with the row').toBeLessThanOrEqual(1);
    // a row: the time, the price coloured by the side, the amount
    const first = trades.getByTestId('market-trade-row').first();
    await expect(first.locator('span').first()).toHaveText(/^\d\d:\d\d:\d\d$/);
    await expect(first).toHaveAttribute('data-side', /^(buy|sell)$/);
    await expect(first.locator('span').nth(1)).toHaveClass(/^(buy|sell)$/);
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBe(0);

    // kept over a reload
    const dragged = await rowH(page);
    expect(await page.evaluate(() => localStorage.getItem('kob.market.rowH'))).not.toBeNull();
    await page.reload();
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
    await expect.poll(async () => Math.abs((await rowH(page)) - dragged)).toBeLessThanOrEqual(1);
    await expect(page.getByTestId('market-trades')).toBeVisible();

    // a double-click resets it
    await handle(page).dblclick();
    // (the default of the reloaded page: its least height; the page is not signed in after a reload, so its form can be a line taller than before)
    await expect.poll(async () => Math.abs((await rowH(page)) - (await num(page, 'aria-valuemin')))).toBeLessThanOrEqual(1);
    expect(await chartBox(page)).toBeLessThan(c0 + 30);
    expect(await page.evaluate(() => localStorage.getItem('kob.market.rowH'))).toBeNull();
    await expect(page.getByTestId('market-trades')).toBeHidden();
  });
}

test('1280x900: the keyboard moves the row 16 px a step, never under the ticket\'s fit, Enter resets; Expand composes with the height and dragging works while expanded', async ({ appPage: page, mock }) => {
  await open(page, mock, 1280, 900);
  const h0 = await rowH(page);
  await handle(page).focus();
  // up from the default: the least height, nothing changes
  await page.keyboard.press('ArrowUp');
  expect(Math.abs((await rowH(page)) - h0)).toBeLessThanOrEqual(1);
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await expect.poll(async () => (await rowH(page)) - h0).toBeCloseTo(32, 0);
  expect(await num(page, 'aria-valuenow')).toBeCloseTo(h0 + 32, 0);
  await page.keyboard.press('ArrowUp');
  await expect.poll(async () => (await rowH(page)) - h0).toBeCloseTo(16, 0);
  await expectNoGap(page, 'a keyboard step');
  await page.keyboard.press('End');
  expect(await rowH(page)).toBeLessThanOrEqual((await num(page, 'aria-valuemax')) + 1);
  await page.keyboard.press('Home');
  await expect.poll(async () => Math.abs((await rowH(page)) - h0)).toBeLessThanOrEqual(1);
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect.poll(async () => Math.abs((await rowH(page)) - h0)).toBeLessThanOrEqual(1);

  // Expand: the book gives way, the chart spans its column; the chosen height still rules and a drag still works
  await page.getByTestId('chart-expand').click();
  await expect(page.getByTestId('book-section')).toBeHidden();
  await drag(page, 120);
  const grown = await page.getByTestId('chart-section').evaluate((el) => el.getBoundingClientRect().height);
  await page.getByTestId('chart-expand').click();
  await expect(page.getByTestId('book-section')).toBeVisible();
  expect(Math.abs((await rowH(page)) - grown), 'the height is the same expanded or not').toBeLessThanOrEqual(1);
  expect(await rowH(page) - h0).toBeGreaterThan(110);
});

test('no handle on a phone, on two columns, or with a coarse pointer', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await fund(mock);
  await openMarket(page, mock);
  await expect(page.getByTestId('order-ticket')).toBeAttached();
  await expect(handle(page)).toHaveCount(0);
  await page.setViewportSize({ width: 1000, height: 900 });
  await expect(handle(page)).toHaveCount(0);
  await expect(page.getByTestId('market-trades')).toBeHidden();
});

test.describe('a coarse pointer at 1280', () => {
  test.use({ hasTouch: true, viewport: { width: 1280, height: 900 } });
  test('has no handle', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await expect(handle(page)).toHaveCount(0);
  });
});
