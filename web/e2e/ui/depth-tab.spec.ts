// Below 1200 px the depth chart has no panel of its own: it is the "Depth" tab of the price chart's panel (a "Price | Depth" switch in the panel's header, the
// choice remembered in this browser). The price chart is hidden behind it, never unmounted, so its size and its interval stay; from 1200 px there is no switch and
// the depth chart is the panel under the chart. Mock stack.
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { listExkcc } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';

async function open(page: Page, mock: MockClient): Promise<void> {
  await mock.seedHistory({ hours: 48, trades: 600, seed: 7 });
  await listExkcc(page);
  const tok = await mock.token();
  await page.goto(`/#/market/${tok.covenant_id}`);
  await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
}

const chartBox = (page: Page) => page.locator('[data-testid="price-chart"] .chart-box').boundingBox();

for (const width of [390, 850]) {
  test.describe(`${width} px`, () => {
    test.use({ viewport: { width, height: 900 } });

    test('the depth chart is a tab of the price panel, keeps the price chart as it was, and is remembered', async ({ appPage: page, mock }) => {
      await open(page, mock);
      const view = page.getByTestId('chart-view');
      await expect(view).toHaveAttribute('role', 'radiogroup');
      await expect(page.getByTestId('chart-view-price')).toHaveAttribute('aria-checked', 'true');
      await expect(page.getByTestId('depth-section')).toHaveCount(0);
      await expect(page.getByTestId('depth-chart')).toHaveCount(0);

      // a state the price chart must keep: another interval
      await page.getByTestId('chart-interval-1h').click();
      await expect(page.getByTestId('price-chart')).toHaveAttribute('data-interval', '1h');
      await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'ready');
      const before = (await chartBox(page))!;

      await page.getByTestId('chart-view-depth').click();
      await expect(page.getByTestId('chart-view-depth')).toHaveAttribute('aria-checked', 'true');
      const depth = page.getByTestId('depth-chart');
      await expect(depth).toHaveAttribute('data-state', 'ready');
      await expect(depth.locator('svg')).toBeVisible();
      await expect(depth.locator('.bid-area')).toHaveCount(1);
      // the depth chart fills the box of the price chart; the price chart's tools are not offered meanwhile
      const d = (await depth.locator('.depth-box').boundingBox())!;
      const stage = (await page.locator('.chart-stage').boundingBox())!;
      expect(Math.abs(d.width - stage.width)).toBeLessThanOrEqual(1);
      expect(d.height).toBeGreaterThan(before.height - 80); // the box less the legend under it
      expect(d.height).toBeLessThanOrEqual(before.height + 1);
      await expect(page.getByTestId('chart-intervals')).toBeHidden();
      await expect(page.getByTestId('chart-tools')).toBeHidden();
      expect(Math.abs((await chartBox(page))!.height - before.height)).toBeLessThanOrEqual(1);
      expect(await horizontalOverflow(page), 'nothing scrolls sideways').toBe(0);

      // back: the same chart, the same interval, the same size
      await page.getByTestId('chart-view-price').click();
      await expect(page.getByTestId('depth-chart')).toHaveCount(0);
      await expect(page.getByTestId('chart-intervals')).toBeVisible();
      await expect(page.getByTestId('price-chart')).toHaveAttribute('data-interval', '1h');
      const after = (await chartBox(page))!;
      expect(Math.abs(after.width - before.width) + Math.abs(after.height - before.height)).toBeLessThanOrEqual(1);
      await expect(page.locator('[data-testid="price-chart"] canvas').first()).toBeVisible();

      // the last choice is remembered across a reload
      await page.getByTestId('chart-view-depth').click();
      await page.reload();
      await expect(page.getByTestId('chart-view-depth')).toHaveAttribute('aria-checked', 'true');
      await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
      await expect(page.getByTestId('chart-intervals')).toBeHidden();
      await page.getByTestId('chart-view-price').click();
      await page.reload();
      await expect(page.getByTestId('chart-view-price')).toHaveAttribute('aria-checked', 'true');
      await expect(page.getByTestId('depth-chart')).toHaveCount(0);
    });
  });
}

test.describe('1280 px', () => {
  test.use({ viewport: { width: 1280, height: 900 } });

  test('no tab: the depth chart is a panel of its own under the chart', async ({ appPage: page, mock }) => {
    await open(page, mock);
    await expect(page.getByTestId('chart-view')).toHaveCount(0);
    const depth = page.getByTestId('depth-section');
    await expect(depth.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
    const c = (await page.getByTestId('chart-section').boundingBox())!;
    const d = (await depth.boundingBox())!;
    expect(d.y).toBeGreaterThan(c.y + c.height - 1);
    await expect(page.getByTestId('chart-intervals')).toBeVisible();
  });
});
