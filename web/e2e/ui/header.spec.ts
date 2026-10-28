// The header is ONE row, on a desktop (900 px and wider) and on a phone alike. Desktop: the logo, the tabs, a status dot, the chip "testnet-10 · pre-audit",
// "Connect wallet" and "Private key", the theme toggle and the bell. Phone: the logo, a menu button (the tabs, the theme toggle, the network and status
// explanation), the shortened chip, one "Connect" button (a sheet with Wallet and Private key) and the bell. A problem is also a banner, never only the dot.
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';

test.use({ autoOpen: false });

const rowOf = async (page: Page, ids: string[]): Promise<{ ys: number[]; right: number }> => {
  const boxes = await Promise.all(ids.map((id) => page.getByTestId(id).boundingBox()));
  expect(boxes.every((b) => b !== null), `all of ${ids.join(', ')} are shown`).toBe(true);
  return { ys: boxes.map((b) => b!.y + b!.height / 2), right: Math.max(...boxes.map((b) => b!.x + b!.width)) };
};
const noScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);

for (const width of [1280, 1024, 900]) {
  test(`desktop ${width} px: one row`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height: 800 });
    await page.goto('/#/market');
    await expect(page.getByTestId('wallet-connect-list')).toBeVisible();
    const { ys, right } = await rowOf(page, ['brand', 'nav-market', 'nav-settings', 'status-dot', 'network-chip', 'wallet-connect-kasware', 'wallet-connect-local-key', 'theme-toggle']);
    expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
    expect(right).toBeLessThanOrEqual(width);
    expect((await page.locator('header.header').boundingBox())!.height).toBeLessThan(64);
    await expect(page.getByTestId('network-badge')).toHaveText('testnet-10');
    await expect(page.getByTestId('preaudit-badge')).toHaveText('pre-audit');
    await expect(page.getByTestId('menu-button')).toHaveCount(0);
    await expect(page.getByTestId('status-bar')).toHaveCount(0);
    expect(await noScroll(page)).toBe(true);
  });
}

test('desktop: texts much wider than the row (a long translation, wide fonts) wrap to a second line, nothing overlaps', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto('/#/market');
  await expect(page.getByTestId('wallet-connect-list')).toBeVisible();
  await page.addStyleTag({ content: '.header .nav a, .header .btn, .header .net-chip, .header .brand span { letter-spacing: 6px; }' });
  const ids = ['brand', 'nav-market', 'nav-orders', 'nav-create', 'nav-settings', 'status-dot', 'network-chip', 'wallet-connect-kasware', 'wallet-connect-local-key', 'theme-toggle'];
  const boxes = await Promise.all(ids.map((id) => page.getByTestId(id).boundingBox()));
  for (let i = 0; i < ids.length; i++) {
    for (let j = i + 1; j < ids.length; j++) {
      const [a, b] = [boxes[i]!, boxes[j]!];
      const apart = a.x + a.width <= b.x + 0.5 || b.x + b.width <= a.x + 0.5 || a.y + a.height <= b.y + 0.5 || b.y + b.height <= a.y + 0.5;
      expect(apart, `${ids[i]} and ${ids[j]} do not overlap`).toBe(true);
    }
  }
  await page.getByTestId('wallet-connect-local-key').click({ trial: true });
});

test('desktop: the status popover is keyboard accessible and closes on Escape and outside', async ({ appPage: page }) => {
  await page.goto('/#/market');
  const dot = page.getByTestId('status-dot');
  await dot.focus();
  await page.keyboard.press('Enter');
  await expect(dot).toHaveAttribute('aria-expanded', 'true');
  await expect(page.getByTestId('status-node')).toContainText('Node: reachable');
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('status-popover')).toHaveCount(0);
  await expect(dot).toBeFocused();
  await dot.click();
  await expect(page.getByTestId('status-popover')).toBeVisible();
  await page.getByTestId('brand').click({ position: { x: 5, y: 5 } });
  await expect(page.getByTestId('status-popover')).toHaveCount(0);
});

test('desktop: Connect wallet connects the one extension at once; the connected chip shows the address and the buttons are gone', async ({ appPage: page }) => {
  await page.goto('/#/market');
  await page.getByTestId('wallet-connect-kasware').click();
  await expect(page.getByTestId('wallet-address')).toContainText(/^kaspatest:q/);
  await expect(page.getByTestId('wallet-connect-list')).toHaveCount(0);
  const { ys } = await rowOf(page, ['brand', 'wallet-connected', 'theme-toggle']);
  expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
});

test.describe('phone', () => {
  for (const width of [375, 360]) {
    test(`${width} px: one row, a menu, a short chip, one Connect button, no horizontal scroll`, async ({ appPage: page }) => {
      await page.setViewportSize({ width, height: 800 });
      await page.goto('/#/market');
      await expect(page.getByTestId('wallet-connect')).toBeVisible();
      const { ys, right } = await rowOf(page, ['menu-button', 'brand', 'network-chip', 'wallet-connect', 'notify-bell']);
      expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
      expect(right).toBeLessThanOrEqual(width);
      expect((await page.locator('header.header').boundingBox())!.height).toBeLessThan(56);
      await expect(page.getByTestId('network-badge')).toHaveText('tn10');
      await expect(page.getByTestId('wallet-connect')).toHaveText('Connect');
      for (const gone of ['nav-market', 'theme-toggle', 'status-dot', 'wallet-connect-local-key', 'wallet-install-hint']) await expect(page.getByTestId(gone)).toHaveCount(0);
      expect(await noScroll(page)).toBe(true);
    });
  }

  test('the menu holds the tabs, the theme toggle, the network explanation and the status; it closes on navigation and on Escape', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 375, height: 800 });
    await page.goto('/#/market');
    const button = page.getByTestId('menu-button');
    await expect(button).toHaveAttribute('aria-expanded', 'false');
    await button.click();
    await expect(button).toHaveAttribute('aria-expanded', 'true');
    for (const id of ['nav-market', 'nav-orders', 'nav-create', 'nav-settings', 'theme-toggle']) await expect(page.getByTestId('menu-panel').getByTestId(id)).toBeVisible();
    await expect(page.getByTestId('menu-network')).toContainText('Network: testnet-10');
    await expect(page.getByTestId('menu-network')).toContainText('no external security review');
    await expect(page.getByTestId('status-node')).toContainText('Node: reachable');
    await expect(page.getByTestId('status-indexer')).toContainText('following');
    // the panel is as wide as the screen and does not scroll the page sideways
    const panel = (await page.getByTestId('menu-panel').boundingBox())!;
    expect(panel.x).toBeGreaterThanOrEqual(0);
    expect(panel.x + panel.width).toBeLessThanOrEqual(375);
    expect(await noScroll(page)).toBe(true);
    // a tab opens its page and closes the menu
    await page.getByTestId('nav-settings').click();
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    await expect(page).toHaveURL(/#\/settings/);
    // Escape closes it and the focus returns to the button
    await button.click();
    await expect(page.getByTestId('menu-panel')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    await expect(button).toBeFocused();
    // the theme toggle in the menu works
    await button.click();
    const before = await page.getByTestId('theme-toggle').getAttribute('data-theme');
    await page.getByTestId('theme-toggle').click();
    await expect(page.getByTestId('theme-toggle')).not.toHaveAttribute('data-theme', before!);
  });

  test('Connect opens a sheet: Wallet connects the extension, the connected header shows the short address in one row', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 375, height: 800 });
    await page.goto('/#/market');
    await page.getByTestId('wallet-connect').click();
    const sheet = page.getByTestId('wallet-sheet');
    await expect(sheet.getByRole('button', { name: 'Wallet' })).toBeVisible();
    await expect(sheet.getByRole('button', { name: 'Private key' })).toBeVisible();
    await sheet.getByRole('button', { name: 'Wallet' }).click();
    await expect(page.getByTestId('wallet-address')).toContainText(/^q\w{3}…\w{4}$/);
    await expect(page.getByTestId('wallet-sheet')).toHaveCount(0);
    const { ys, right } = await rowOf(page, ['menu-button', 'brand', 'network-chip', 'wallet-connected', 'notify-bell']);
    expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
    expect(right).toBeLessThanOrEqual(375);
    expect(await noScroll(page)).toBe(true);
  });

  test('a problem is a full-width banner under the header and the menu button shows it as a dot', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 375, height: 800 });
    await page.route('**/node/**', (route) => route.abort());
    await page.goto('/#/market');
    await expect(page.getByTestId('banner-node')).toBeVisible();
    await expect(page.getByTestId('menu-button')).toHaveAttribute('data-level', 'bad');
  });
});
