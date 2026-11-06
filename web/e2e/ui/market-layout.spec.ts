// The token page's facts (Token details, Status and checks, logo check) use the full width of the page on 2 and 3 columns: they flow side by side under the
// whole grid (book, chart, ticket) and the activity log, which takes the full width between them, no column of the page is left empty beside them, and a 64-hex id takes one or two lines. A phone stays one column.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

const rect = (page: import('@playwright/test').Page, id: string) =>
  page.getByTestId(id).evaluate((el) => { const r = el.getBoundingClientRect(); return { left: r.left, right: r.right, top: r.top + scrollY, bottom: r.bottom + scrollY, width: r.width, height: r.height }; });

for (const width of [900, 1100, 1280, 1600, 1774]) {
  test(`${width} px: the token facts span the full width under the grid, side by side, hashes in at most two lines`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 1000 });
    await fund(mock);
    await openMarket(page, mock);
    const ticket = await rect(page, 'ticket-container');
    const chart = await rect(page, 'chart-section');
    const facts = page.getByTestId('token-details');
    // the activity log above the facts reads its rows once it is on screen (its height changes then): measured after they are in
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('activity-list').getByTestId('activity-row').first()).toBeVisible();
    await facts.scrollIntoViewIfNeeded();
    const f = await rect(page, 'token-details');
    // the full width of the page content: from the left edge of the book / chart column to the right edge of the ticket column
    expect(Math.abs(f.right - ticket.right), 'right edge = the ticket column').toBeLessThan(2);
    expect(f.left, 'left edge at or before the chart').toBeLessThanOrEqual(chart.left + 1);
    expect(f.top - ticket.bottom, 'under the ticket, with the page gap (12px)').toBeGreaterThanOrEqual(11);
    // the activity log: the full width under the grid (under the ticket, with the page gap), above the facts
    const log = await page.locator('.mkt-area-activity').evaluate((el) => { const r = el.getBoundingClientRect(); return { left: r.left, right: r.right, top: r.top + scrollY, bottom: r.bottom + scrollY }; });
    expect(Math.abs(log.right - ticket.right), 'activity: right edge = the ticket column').toBeLessThan(2);
    expect(log.left, 'activity: left edge at or before the chart').toBeLessThanOrEqual(chart.left + 1);
    expect(log.top - ticket.bottom, 'activity under the ticket').toBeGreaterThanOrEqual(11);
    expect(f.top - log.bottom, 'the facts under the activity log').toBeGreaterThanOrEqual(11);
    // the two cards are side by side (one row), the status card folded
    const cards = facts.locator(':scope > section');
    expect(await cards.count()).toBeGreaterThanOrEqual(2);
    const a = await cards.nth(0).boundingBox();
    const b = await cards.nth(1).boundingBox();
    expect(Math.abs(a!.y - b!.y), 'same row').toBeLessThan(2);
    expect(a!.x + a!.width, 'side by side').toBeLessThanOrEqual(b!.x + 1);
    // the 64-hex covenant id is one or two lines (a line is ~22 px; three on a narrow tablet)
    const id = page.getByTestId('token-covenant-id');
    expect((await id.boundingBox())!.height).toBeLessThan(width >= 1100 ? 50 : 70);
    await page.getByTestId('token-status-details').getByRole('button').first().click();
    const hash = page.getByTestId('tpl-registry-hash');
    await expect(hash).toBeVisible();
    expect((await hash.boundingBox())!.height, 'two lines at most (three on a narrow tablet)').toBeLessThan(width >= 1100 ? 50 : 70);
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBe(0);
  });
}

test('a phone (390 px) keeps one column: the cards are stacked', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await fund(mock);
  await openMarket(page, mock);
  const cards = page.getByTestId('token-details').locator(':scope > section');
  await cards.first().scrollIntoViewIfNeeded();
  const a = (await cards.nth(0).boundingBox())!;
  const b = (await cards.nth(1).boundingBox())!;
  expect(b.y).toBeGreaterThanOrEqual(a.y + a.height - 1);
  expect(Math.abs(a.x - b.x)).toBeLessThan(2);
});
