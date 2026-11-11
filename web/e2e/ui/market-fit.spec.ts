// The market page fits its window with no empty room (founder, 2026-11-11): on three columns, ONE row (book | chart | ticket; the depth chart is a view of the book's panel), the chart takes the
// whole row: its box is what the ticket's row leaves over (the row is as tall as the ticket's whole Limit form, never taller), so the ticket has no empty band between its "Advanced options" box and
// its pinned button, on a short, a full-HD and a tall window alike, and its place button is on the first screen at 1280x800 and 1920x1080 (review H-01); the order book lists as
// many levels per side as its card holds (8 to 40; a phone keeps 12); the 1D chart's time axis reads dates, an intraday one 24-hour times. Mock stack: the token page of EXKCC, and the pair page.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { seedOrders } from '../helpers/orders';

const QUOTE = 'e7'.repeat(32);
const SIZES = [[1280, 800], [1920, 1080], [1280, 1400], [850, 1200]] as const;
const bottom = (page: Page, sel: string) => page.locator(sel).first().evaluate((el) => el.getBoundingClientRect().bottom + scrollY);
const top = (page: Page, sel: string) => page.locator(sel).first().evaluate((el) => el.getBoundingClientRect().top + scrollY);
const heightOf = (page: Page, sel: string) => page.locator(sel).first().evaluate((el) => el.getBoundingClientRect().height);

/** The ticket's form has no empty band: from the bottom of the Advanced options box to the top of the footer (the pinned button) at most 24 px (the form's own gap and padding). */
async function expectNoEmptyBand(page: Page): Promise<void> {
  // the form's natural height is measured once the fonts and the data are in (OrderTicket.tsx --tk-fit)
  await expect.poll(() => page.locator('.mkt-grid').evaluate((el) => (el as HTMLElement).style.getPropertyValue('--tk-fit')), { message: 'the ticket measured its form' }).toMatch(/px$/);
  await page.waitForTimeout(400);
  const band = (await top(page, '.tk-foot')) - (await bottom(page, '[data-testid="order-advanced"]'));
  expect(band, `the empty band of the ticket (${page.viewportSize()!.width}x${page.viewportSize()!.height})`).toBeLessThanOrEqual(24);
  expect(band).toBeGreaterThanOrEqual(0);
}

for (const [width, height] of SIZES) {
  test(`${width}x${height}: the ticket has no empty band; the book, the chart and the ticket end on one line; nothing moves once measured`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height });
    await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
    await fund(mock);
    await openMarket(page, mock);
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
    await expectNoEmptyBand(page);
    if (width >= 1200) {
      await expect(page.getByTestId('depth-section'), 'no panel of its own for the depth chart').toHaveCount(0);
      const chart = await bottom(page, '[data-testid="chart-section"]');
      expect(Math.abs((await bottom(page, '[data-testid="order-ticket"]')) - chart), 'the ticket ends at the chart').toBeLessThanOrEqual(1);
      expect(Math.abs((await bottom(page, '[data-testid="book-section"]')) - chart), 'the book ends at the chart').toBeLessThanOrEqual(1);
      expect(await top(page, '[data-testid="book-section"]'), 'one row: the three start together').toBeCloseTo(await top(page, '[data-testid="chart-section"]'), 0);
      expect(await top(page, '[data-testid="order-ticket"]')).toBeCloseTo(await top(page, '[data-testid="chart-section"]'), 0);
      // the chart's box is the rest of the ticket's row (floor 240 px), and the page height is the same on every window height (the row follows the form, not the window)
      expect(await heightOf(page, '.chart-box')).toBeGreaterThanOrEqual(240);
    } else {
      expect(await heightOf(page, '.chart-box'), 'two columns: a fixed chart box').toBeCloseTo(420, 0);
    }
    // no loop: the boxes are the same a moment later (the ticket's measure never feeds on the row)
    const boxes = () => page.evaluate(() => ['chart-section', 'order-ticket', 'book-section'].map((id) => document.querySelector(`[data-testid="${id}"]`)!.getBoundingClientRect().height));
    const a = await boxes();
    await page.waitForTimeout(700);
    expect(await boxes()).toEqual(a);
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBe(0);
    // H-01: on the two usual desktop windows the ticket's place button is on the first screen, on load, without scrolling
    if (width >= 1280 && height <= 1080) {
      const place = (await page.getByTestId('order-place').boundingBox())!;
      expect(place.y, 'the place button is below the top edge').toBeGreaterThanOrEqual(0);
      expect(place.y + place.height, `the place button is inside the ${width}x${height} window`).toBeLessThanOrEqual(height);
      expect(await page.evaluate(() => scrollY), 'nothing scrolled the page').toBe(0);
    }
  });
}

test('1920x1080: the order book lists as many levels as its card holds, 8 at least, 40 at most; a tall row (a long form) gives at least 14 per side', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1920, height: 1080 });
  await fund(mock);
  // 25 levels a side: the seed's 10 and 15 more
  await seedOrders(mock, [
    ...Array.from({ length: 15 }, (_, i) => ({ side: 'ask' as const, price: 2_610_000 + i * 10_000, tokens: 1 })),
    ...Array.from({ length: 15 }, (_, i) => ({ side: 'bid' as const, price: 2_390_000 - i * 10_000, tokens: 1 })),
  ]);
  await openMarket(page, mock);
  const asks = page.locator('[data-testid="book-asks"] .book-row:not(.book-blank)');
  const bids = page.locator('[data-testid="book-bids"] .book-row:not(.book-blank)');
  const slotsOf = async () => page.locator('[data-testid="book-asks"] .book-row').count();
  const check = async (label: string) => {
    // the card is full: the side holds exactly the slots that fit its height (less than one row left over), each filled while the data has levels
    const side = await heightOf(page, '[data-testid="book-asks"]');
    const slots = await slotsOf();
    expect(side - slots * 21, `${label}: less than one row left over`).toBeLessThan(21);
    expect(slots * 21 - side, `${label}: the slots fit the side`).toBeLessThanOrEqual(1);
    expect(slots).toBeGreaterThanOrEqual(8);
    expect(slots).toBeLessThanOrEqual(40);
    expect(await asks.count(), `${label}: every slot a level`).toBe(slots);
    expect(await bids.count()).toBe(slots);
    const card = await page.locator('[data-testid="book-section"]').evaluate((el) => el.getBoundingClientRect().bottom);
    const foot = await page.locator('.ob-foot').evaluate((el) => el.getBoundingClientRect().bottom);
    expect(card - foot, `${label}: the foot is at the bottom of the card`).toBeLessThanOrEqual(16);
    return slots;
  };
  await expect.poll(() => slotsOf(), { message: 'the book drew its slots' }).toBeGreaterThanOrEqual(8);
  await page.waitForTimeout(500);
  const natural = await check('the ticket row');
  // the card is as tall as the ticket's row, which is as tall as the form: a taller form (here the 840 px the form measured before its field help was folded away by default)
  // gives the book more levels, at least 14 a side
  await page.locator('.mkt-grid').evaluate((el) => (el as HTMLElement).style.setProperty('--tk-fit', '840px'));
  await expect.poll(() => slotsOf(), { message: 'the book follows the taller row' }).toBeGreaterThanOrEqual(14);
  await page.waitForTimeout(300);
  expect(await check('a 840 px row')).toBeGreaterThanOrEqual(natural);
});

test('1920x1080: fewer levels than slots keeps the asks on the spread, the blank above them; the bids hang from the spread', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1920, height: 1080 });
  await fund(mock);
  await openMarket(page, mock); // the seed has 10 levels a side
  const asks = page.locator('[data-testid="book-asks"] .book-row');
  await expect(asks.first()).toBeVisible();
  // a row tall enough for more than 10 levels a side (a long form)
  await page.locator('.mkt-grid').evaluate((el) => (el as HTMLElement).style.setProperty('--tk-fit', '840px'));
  await expect.poll(() => asks.count()).toBeGreaterThan(10);
  await page.waitForTimeout(300);
  const n = await asks.count();
  expect(n).toBeGreaterThan(10);
  expect(await page.locator('[data-testid="book-asks"] .book-row:not(.book-blank)').count()).toBe(10);
  const spread = await top(page, '[data-testid="book-spread"]');
  const lastAsk = await bottom(page, '[data-testid="book-asks"] .book-row:not(.book-blank) >> nth=-1');
  expect(spread - lastAsk, 'the best ask sits on the spread row (its margin)').toBeLessThanOrEqual(12);
  const firstBid = await top(page, '[data-testid="book-bids"] .book-row:not(.book-blank) >> nth=0');
  const spreadBottom = await bottom(page, '[data-testid="book-spread"]');
  expect(firstBid - spreadBottom, 'the best bid hangs from the spread row').toBeLessThanOrEqual(12);
});

test('a phone keeps the 12 slots of its book', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await fund(mock);
  await openMarket(page, mock);
  await expect(page.locator('[data-testid="book-asks"] .book-row').first()).toBeVisible();
  expect(await page.locator('[data-testid="book-asks"] .book-row').count()).toBe(12);
  expect(await page.locator('[data-testid="book-bids"] .book-row').count()).toBe(12);
  expect(await heightOf(page, '[data-testid="book-asks"]')).toBeCloseTo(12 * 21, 0);
});

test('1D axis: a date under every tick (not one time of day), no AM / PM anywhere; an intraday axis reads 24-hour times', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await mock.seedHistory({ hours: 1700, trades: 600, seed: 7 });
  await fund(mock);
  await openMarket(page, mock);
  const chart = page.getByTestId('price-chart');
  await expect(chart).toHaveAttribute('data-state', 'ready');
  const ticks = async () => ((await chart.getAttribute('data-axis-ticks')) ?? '').split('|').filter(Boolean);
  await page.getByTestId('chart-interval-1d').click();
  await expect(chart).toHaveAttribute('data-interval', '1d');
  await expect.poll(async () => (await ticks()).length, { message: 'the axis drew its ticks' }).toBeGreaterThanOrEqual(2);
  const day = await ticks();
  expect(new Set(day).size, `the labels are not all equal: ${day.join(' | ')}`).toBeGreaterThan(1);
  for (const l of day) expect(l, 'a day-level axis has dates, never a time of day').not.toMatch(/\d:\d|AM|PM/);
  expect(day.some((l) => /^[A-Z][a-z]{2} \d{1,2}$/.test(l) || /^[A-Z][a-z]{2}$/.test(l) || /^\d{4}$/.test(l)), `dates, months or years: ${day.join(' | ')}`).toBe(true);

  await page.getByTestId('chart-interval-5m').click();
  await expect(chart).toHaveAttribute('data-interval', '5m');
  await expect.poll(async () => (await ticks()).some((l) => /^\d{2}:\d{2}$/.test(l)), { message: 'the 5m axis has 24-hour times' }).toBe(true);
  for (const l of await ticks()) expect(l, '24-hour clock, or a date at a day start').toMatch(/^(\d{2}:\d{2}|[A-Z][a-z]{2}( \d{1,2})?|\d{4})$/);
});

/** The pair page with a pair seed (as pair-panels.spec.ts). */
async function listPair(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional pair quote token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

async function openPair(page: Page, mock: MockClient): Promise<void> {
  await mock.seed({ pair: true });
  await mock.seedHistory({});
  await mock.seedHistory({ token: QUOTE, mid: 50_000_000, tick: 100_000, seed: 2 });
  await listPair(page);
  const base = (await mock.token()).covenant_id;
  await fund(mock);
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('pair-page')).toBeVisible();
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await expect(page.getByTestId('pair-book').first()).toBeVisible();
}

test.describe('the pair page fits its window too', () => {
  test.use({ autoOpen: false });
  for (const [width, height] of [[1920, 1080], [1280, 1400]] as const) {
    test(`${width}x${height}: the ticket has no empty band, the book fills its card, the three panels end on one line`, async ({ appPage: page, mock }) => {
      await page.setViewportSize({ width, height });
      await openPair(page, mock);
      await expectNoEmptyBand(page);
      const chart = await bottom(page, '[data-testid="chart-section"]');
      // the ticket's column (the pair's has the link to the wallet's orders under the form)
      expect(Math.abs((await bottom(page, '.mkt-area-ticket')) - chart)).toBeLessThanOrEqual(1);
      expect(Math.abs((await bottom(page, '[data-testid="pair-book-section"]')) - chart)).toBeLessThanOrEqual(1);
      expect(await heightOf(page, '.chart-box')).toBeGreaterThanOrEqual(240);
      // the side of the pair book is as tall as the card gave it: the 8 slots it has at least, no level cut off (every level drawn fits the side)
      expect(await heightOf(page, '[data-testid="pair-book-asks"]')).toBeGreaterThanOrEqual(8 * 21);
      expect(await page.locator('[data-testid="pair-book-asks"]').evaluate((el) => el.scrollHeight - el.clientHeight)).toBeLessThanOrEqual(1);
    });
  }
});
