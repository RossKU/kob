// The header and the footer run the full width of the window (the brand at the left edge, the tools at the right edge, one gutter of clamp(16px, 2.5vw, 32px));
// the page's content keeps its column (--frame-max, 1240 px). The app shell and the landing page share the frame. No sideways scroll at any width.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';

test.use({ autoOpen: false });

const gutter = (w: number) => Math.min(32, Math.max(16, w * 0.025));
const noScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);

async function checkFrame(page: Page, width: number, brandId: string, toolsSel: string): Promise<void> {
  const g = gutter(width);
  const brand = (await page.getByTestId(brandId).boundingBox())!;
  expect(brand.x, `the brand sits at the left gutter (${g} px)`).toBeGreaterThanOrEqual(g - 1);
  expect(brand.x).toBeLessThanOrEqual(g + 4);
  const tools = (await page.locator(toolsSel).last().boundingBox())!;
  expect(width - (tools.x + tools.width), 'the tools end at the right gutter').toBeGreaterThanOrEqual(g - 1);
  expect(width - (tools.x + tools.width)).toBeLessThanOrEqual(g + 4);
  const header = (await page.locator('header.header').boundingBox())!;
  expect(header.width).toBeCloseTo(width, 0);
  const footer = (await page.getByTestId('footer').boundingBox())!;
  expect(footer.width).toBeCloseTo(width, 0);
  const footBrand = (await page.locator('footer .brand').first().boundingBox())!;
  expect(footBrand.x).toBeGreaterThanOrEqual(g - 1);
  expect(footBrand.x).toBeLessThanOrEqual(g + 4);
  expect(await noScroll(page)).toBe(true);
}

for (const width of [390, 800, 1280, 1920, 2560]) {
  test(`${width} px: the app's header and footer span the window, the content keeps its column`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/#/market');
    await expect(page.getByTestId('main')).toBeVisible();
    await checkFrame(page, width, 'brand', '.header-tools');
    const main = (await page.getByTestId('main').boundingBox())!;
    expect(main.width).toBeLessThanOrEqual(1240 + 1);
    if (width >= 1920) expect(main.x).toBeGreaterThan(gutter(width) + 100);
  });

  test(`${width} px: the landing page's header and footer span the window, its sections keep their column`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/');
    await expect(page.getByTestId('landing')).toBeVisible();
    await checkFrame(page, width, 'landing-brand', '.header-tools');
    const wrap = (await page.locator('.lp-wrap').first().boundingBox())!;
    expect(wrap.width).toBeLessThanOrEqual(1240 + 1);
  });
}
