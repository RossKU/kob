// Two columns: the ticket fills the width of its column (the right edge of the chart above it) also with the field help hidden, when its text is narrower. Mock stack.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
for (const width of [850, 1100]) test(`${width} px: the ticket is as wide as its column with the field help hidden`, async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width, height: 1000 });
  await fund(mock);
  await openMarket(page, mock);
  await page.getByTestId('order-help-toggle').click();
  await expect(page.getByTestId('order-type-help')).toBeHidden();
  await page.waitForTimeout(500);
  const w = await page.evaluate(() => { const r = (s: string) => document.querySelector(s)!.getBoundingClientRect(); return { tk: Math.round(r('.tk').right), chart: Math.round(r('.mkt-area-chart').right) }; });
  expect(Math.abs(w.tk - w.chart)).toBeLessThan(2);
});
