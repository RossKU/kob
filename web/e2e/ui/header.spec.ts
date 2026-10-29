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

// 900 px and wider is the desktop row; on a font so wide that the row does not fit (the CI runner's fonts at 900 px) the header takes the phone layout, still ONE row
/** A wide font like the CI runner's (DejaVu Sans, close to Verdana): the header must fit, or take the phone layout, on it too. */
const wideFonts = (page: Page) =>
  page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const s = document.createElement('style');
      s.textContent = 'html, body, button, input, select { font-family: Verdana, "DejaVu Sans", sans-serif !important; }';
      document.head.append(s);
    });
  });

for (const width of [1280, 1024, 900, 880]) {
  test(`desktop ${width} px: one row, in the desktop layout or (when it does not fit) the phone layout`, async ({ appPage: page }) => {
    await wideFonts(page);
    await page.setViewportSize({ width, height: 800 });
    await page.goto('/#/market');
    await expect(page.getByTestId('wallet-connect-list')).toBeVisible();
    await expect(page.getByTestId('network-badge')).toHaveText(/^(testnet-10|tn10)$/);
    await expect(page.getByTestId('preaudit-badge')).toHaveText('pre-audit');
    await expect(page.getByTestId('status-bar')).toHaveCount(0);
    // the layout settles after the first measurement
    await expect.poll(async () => (await page.getByTestId('nav-market').count()) + (await page.getByTestId('menu-button').count())).toBe(1);
    const wide = (await page.getByTestId('nav-market').count()) === 1;
    if (width >= 880) expect(wide, `the desktop row fits ${width} px on a wide font`).toBe(true);
    const ids = wide
      ? ['brand', 'nav-market', 'nav-settings', 'status-dot', 'network-chip', 'wallet-connect-kasware', 'theme-toggle', 'notify-bell']
      : ['brand', 'network-chip', 'wallet-connect', 'notify-bell', 'menu-button'];
    const { ys, right } = await rowOf(page, ids);
    expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
    expect(right).toBeLessThanOrEqual(width);
    expect((await page.locator('header.header').boundingBox())!.height).toBeLessThan(64);
    expect(await noScroll(page)).toBe(true);
  });
}

test('desktop: a row that does not fit (wide fonts, a long translation) becomes the phone layout, still one row, and comes back when the window is wider', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto('/#/market');
  await expect(page.getByTestId('wallet-connect-kasware')).toBeVisible();
  await expect(page.getByTestId('nav-market')).toBeVisible();
  await page.addStyleTag({ content: '.header .nav a, .header .btn, .header .net-chip, .header .brand span { letter-spacing: 12px; }' });
  await expect(page.getByTestId('menu-button')).toBeVisible();
  await expect(page.getByTestId('nav-market')).toHaveCount(0);
  const { ys, right } = await rowOf(page, ['menu-button', 'brand', 'network-chip', 'wallet-connect', 'notify-bell']);
  expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
  expect(right).toBeLessThanOrEqual(1280);
  expect(await noScroll(page)).toBe(true);
  // a much wider window fits the row again
  await page.addStyleTag({ content: '.header .nav a, .header .btn, .header .net-chip, .header .brand span { letter-spacing: 0 !important; }' });
  await page.setViewportSize({ width: 1500, height: 800 });
  await expect(page.getByTestId('nav-market')).toBeVisible();
  await expect(page.getByTestId('menu-button')).toHaveCount(0);
});

test('a header that started narrow comes back to the full row when the window is widened (an unfolded phone at 880 px, then 900 and 1200)', async ({ appPage: page }) => {
  await wideFonts(page);
  await page.setViewportSize({ width: 600, height: 800 });
  await page.goto('/#/market');
  await expect(page.getByTestId('menu-button')).toBeVisible();
  await expect(page.getByTestId('nav-market')).toHaveCount(0);
  for (const width of [880, 900, 1200]) {
    await page.setViewportSize({ width, height: 800 });
    await expect(page.getByTestId('nav-market'), `the tabs at ${width} px`).toBeVisible();
    await expect(page.getByTestId('nav-settings')).toBeVisible();
    await expect(page.getByTestId('menu-button')).toHaveCount(0);
    const { ys, right } = await rowOf(page, ['brand', 'nav-market', 'status-dot', 'network-chip', 'wallet-connect-kasware', 'theme-toggle', 'notify-bell']);
    expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
    expect(right).toBeLessThanOrEqual(width);
  }
  // and narrow again
  await page.setViewportSize({ width: 500, height: 800 });
  await expect(page.getByTestId('menu-button')).toBeVisible();
});

test('a row squeezed by wide texts comes back when the room is there: tabs again after the texts are normal and the window grows', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1000, height: 800 });
  await page.goto('/#/market');
  await expect(page.getByTestId('nav-market')).toBeVisible();
  await page.addStyleTag({ content: '.header .nav a, .header .btn, .header .net-chip { letter-spacing: 12px; }' });
  await expect(page.getByTestId('menu-button')).toBeVisible();
  await page.addStyleTag({ content: '.header .nav a, .header .btn, .header .net-chip { letter-spacing: 0 !important; }' });
  // the same width: the texts are normal again and the row fits, so it is shown again without any resize
  await page.setViewportSize({ width: 1020, height: 800 });
  await expect(page.getByTestId('nav-market')).toBeVisible();
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
      const { ys, right } = await rowOf(page, ['brand', 'network-chip', 'wallet-connect', 'notify-bell', 'menu-button']);
      expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
      expect(right).toBeLessThanOrEqual(width);
      // the order of the row: the logo, the chip, Connect, the bell and, last, the menu
      const xs = (await Promise.all(['brand', 'network-chip', 'wallet-connect', 'notify-bell', 'menu-button'].map((id) => page.getByTestId(id).boundingBox()))).map((b) => b!.x);
      expect([...xs].sort((a, b) => a - b)).toEqual(xs);
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

  test('the menu closes on a tap of the tab already open, on a tap outside and on Escape (and stays for the theme toggle)', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto('/#/market');
    const button = page.getByTestId('menu-button');
    // the tab of the page already open: no navigation, the menu still closes
    await button.click();
    await expect(page.getByTestId('menu-panel')).toBeVisible();
    await page.getByTestId('nav-market').click();
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    await expect(page).toHaveURL(/#\/market$/);
    // a tap outside the menu
    await button.click();
    await expect(page.getByTestId('menu-panel')).toBeVisible();
    await page.mouse.click(20, 780);
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    // another tab: navigation and closing
    await button.click();
    await page.getByTestId('nav-orders').click();
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    await expect(page).toHaveURL(/#\/orders/);
    // Escape
    await button.click();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    // the theme toggle changes the page in place: the menu stays open so the change is seen
    await button.click();
    await page.getByTestId('theme-toggle').click();
    await expect(page.getByTestId('menu-panel')).toBeVisible();
  });

  test('Connect connects the extension at once (no sheet), the connected header shows the short address in one row (on a wide font too)', async ({ appPage: page }) => {
    await wideFonts(page);
    await page.setViewportSize({ width: 375, height: 800 });
    await page.goto('/#/market');
    await page.getByTestId('wallet-connect').click();
    await expect(page.getByTestId('wallet-sheet')).toHaveCount(0);
    await expect(page.getByTestId('wallet-address')).toContainText(/^q\w{3}…\w{4}$/);
    await expect(page.getByTestId('wallet-sheet')).toHaveCount(0);
    // the runner's fonts are wider still than Verdana: a little more letter spacing stands for them
    await page.addStyleTag({ content: '.header * { letter-spacing: 0.5px; }' });
    const { ys, right } = await rowOf(page, ['menu-button', 'brand', 'network-chip', 'wallet-connected', 'notify-bell']);
    expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
    expect(right).toBeLessThanOrEqual(375);
    expect(await noScroll(page)).toBe(true);
    // nothing sits on top of anything: each item of the row ends before the next begins
    const order = ['brand', 'network-chip', 'wallet-connected', 'notify-bell', 'menu-button'];
    const boxes = await Promise.all(order.map((id) => page.getByTestId(id).boundingBox()));
    for (let i = 0; i + 1 < order.length; i++) expect(boxes[i]!.x + boxes[i]!.width, `${order[i]} ends before ${order[i + 1]}`).toBeLessThanOrEqual(boxes[i + 1]!.x + 0.5);
    // the connected chip's disconnect button can be pressed
    await page.getByTestId('wallet-disconnect').click({ trial: true });
  });

  test('a problem is a full-width banner under the header and the menu button shows it as a dot', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 375, height: 800 });
    await page.route('**/node/**', (route) => route.abort());
    await page.goto('/#/market');
    await expect(page.getByTestId('banner-node')).toBeVisible();
    await expect(page.getByTestId('network-chip')).toHaveAttribute('data-level', 'bad');
    await expect(page.getByTestId('chip-dot')).toBeVisible();
  });
});
