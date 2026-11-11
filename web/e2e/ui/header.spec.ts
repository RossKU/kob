// The header is ONE row, on a desktop (900 px and wider) and on a phone alike. Desktop: the logo, the tabs, the chip "● testnet-10 · open review" (the dot is the
// node and indexer status, the chip opens its lines), "Connect" (or the address chip with its menu), the theme toggle and the bell. Phone: the logo, a menu button
// (the tabs, the theme toggle, the network and status explanation), the shortened chip, one "Connect" button and the bell. A problem also colours the bell and is
// listed in its panel, never only the dot.
import type { Page } from '@playwright/test';
import { test, expect, closeBell, statusEntry } from '../fixtures';
import { connectWallet } from '../helpers/env';

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
    await expect(page.getByTestId('preaudit-badge')).toHaveText('open review');
    await expect(page.getByTestId('status-bar')).toHaveCount(0);
    // the layout settles after the first measurement
    await expect.poll(async () => (await page.getByTestId('nav-market').count()) + (await page.getByTestId('menu-button').count())).toBe(1);
    const wide = (await page.getByTestId('nav-market').count()) === 1;
    if (width >= 880) expect(wide, `the desktop row fits ${width} px on a wide font`).toBe(true);
    const ids = wide
      ? ['brand', 'nav-market', 'nav-settings', 'status-dot', 'network-chip', 'wallet-connect', 'theme-toggle', 'notify-bell']
      : ['brand', 'network-chip', 'wallet-connect', 'notify-bell', 'menu-button'];
    const { ys, right } = await rowOf(page, ids);
    expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
    expect(right).toBeLessThanOrEqual(width);
    expect((await page.locator('header.header').boundingBox())!.height).toBeLessThan(64);
    expect(await noScroll(page)).toBe(true);
  });
}

// the row's room stops growing at 1240 px (--frame-max), so 1280 px is any wider window too; narrower on so wide a font the row folds into the narrow layout
for (const [width, spare] of [[1280, 20]] as const) {
  test(`${width} px with a connected wallet on a wide font: the tabs stand 12 px apart and the wide row fits with ${spare} px to spare`, async ({ appPage: page }) => {
    await wideFonts(page);
    await page.setViewportSize({ width, height: 800 });
    await page.goto('/#/market');
    await connectWallet(page);
    await expect(page.getByTestId('nav-market')).toBeVisible();
    await expect(page.getByTestId('menu-button')).toHaveCount(0);
    // the tab after Market is Trade (always there)
    const [market, next] = await Promise.all(['nav-market', 'nav-trade'].map((id) => page.getByTestId(id).boundingBox()));
    expect(next!.x - market!.x - market!.width).toBeCloseTo(12, 0);
    // what `wideWidth` (narrow.ts) sums, against what `roomOf` gives it
    const left = await page.evaluate(() => {
      const px = (v: string) => Number.parseFloat(v) || 0;
      const row = document.querySelector<HTMLElement>('.header-inner')!;
      const cs = getComputedStyle(row);
      const width = (s: string) => row.querySelector(s)!.getBoundingClientRect().width;
      const tools = row.querySelector<HTMLElement>('.header-tools')!;
      const items = [...tools.children].reduce((n, c) => n + c.getBoundingClientRect().width, 0) + px(getComputedStyle(tools).columnGap) * (tools.children.length - 1);
      const brand = row.querySelector('.brand-mark')!.getBoundingClientRect().width;
      return row.clientWidth - px(cs.paddingLeft) - px(cs.paddingRight) - (brand + width('.nav') + items + 2 * px(cs.columnGap));
    });
    expect(left).toBeGreaterThanOrEqual(spare);
  });
}

test('desktop: a row that does not fit (wide fonts, a long translation) becomes the phone layout, still one row, and comes back when the window is wider', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto('/#/market');
  await expect(page.getByTestId('wallet-connect')).toBeVisible();
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
    const { ys, right } = await rowOf(page, ['brand', 'nav-market', 'status-dot', 'network-chip', 'wallet-connect', 'theme-toggle', 'notify-bell']);
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

test('desktop: the status dot sits inside the network chip; the chip opens the status lines, by keyboard too, and closes on Escape and outside', async ({ appPage: page }) => {
  await page.goto('/#/market');
  const chip = page.getByTestId('network-switch');
  // no orphan dot: the dot is a part of the chip's own button
  await expect(chip.getByTestId('status-dot')).toBeVisible();
  const [dotBox, chipBox] = await Promise.all([page.getByTestId('status-dot').boundingBox(), chip.boundingBox()]);
  expect(dotBox!.x).toBeGreaterThanOrEqual(chipBox!.x);
  expect(dotBox!.x + dotBox!.width).toBeLessThanOrEqual(chipBox!.x + chipBox!.width);
  await chip.focus();
  await page.keyboard.press('Enter');
  await expect(chip).toHaveAttribute('aria-expanded', 'true');
  await expect(page.getByTestId('status-popover')).toBeVisible();
  await expect(page.getByTestId('status-node')).toContainText('Node: reachable');
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('status-popover')).toHaveCount(0);
  await expect(chip).toBeFocused();
  await chip.click();
  await expect(page.getByTestId('status-popover')).toBeVisible();
  await page.getByTestId('brand').click({ position: { x: 5, y: 5 } });
  await expect(page.getByTestId('status-popover')).toHaveCount(0);
});

test('the brand leads to the landing screen (the empty hash), not to the market list', async ({ appPage: page }) => {
  await page.goto('/#/portfolio');
  await expect(page.getByTestId('brand')).toHaveAttribute('href', '#/');
});

test('desktop: Connect offers the one extension and the private key side by side (founder 2026-11-11); the extension connects, the chip shows the address and the buttons are gone', async ({ appPage: page }) => {
  await page.goto('/#/market');
  await page.getByTestId('wallet-connect').click();
  await expect(page.getByTestId('wallet-choose').getByRole('button')).toHaveText(['Connect KasWare', 'Private key']);
  // rows with each wallet's mark; the extension this browser has not got is an Install link, and the private key entry carries the one line that recommends an extension
  await expect(page.getByTestId('wallet-connect-kasware').locator('.wallet-mark')).toBeVisible();
  await expect(page.getByTestId('wallet-connect-local-key').locator('.wallet-mark')).toBeVisible();
  await expect(page.getByTestId('wallet-recommend')).toHaveText('A browser extension wallet is recommended.');
  await expect(page.getByTestId('wallet-install-kaspire')).toHaveText(/^Install Kaspire/);
  await expect(page.getByTestId('wallet-install-kaspire')).toHaveAttribute('href', /^https:\/\//);
  await expect(page.getByTestId('wallet-install-kasware')).toHaveCount(0);
  await page.getByTestId('wallet-connect-kasware').click();
  await expect(page.getByTestId('wallet-address')).toContainText(/^kaspatest:q/);
  await expect(page.getByTestId('wallet-connect-list')).toHaveCount(0);
  const { ys } = await rowOf(page, ['brand', 'wallet-connected', 'theme-toggle']);
  expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
});

test('desktop: the connected address is ONE chip that opens a menu (copy, explorer, disconnect), not three buttons beside it', async ({ appPage: page }) => {
  await page.goto('/#/market');
  await connectWallet(page);
  const chip = page.getByTestId('wallet-menu');
  // closed: the chip holds the address and nothing else to press
  await expect(page.getByTestId('wallet-connected').getByRole('button')).toHaveCount(1);
  await expect(page.getByTestId('wallet-disconnect')).toHaveCount(0);
  await expect(chip).toHaveAttribute('aria-expanded', 'false');
  await chip.click();
  await expect(chip).toHaveAttribute('aria-expanded', 'true');
  await expect(page.getByTestId('wallet-menu-address')).toHaveText(/^kaspatest:q[a-z0-9]{55,}$/);
  await expect(page.getByTestId('wallet-copy')).toHaveText('Copy address');
  await expect(page.getByTestId('wallet-explorer')).toHaveAttribute('href', /^https:\/\/tn10\.kaspa\.stream\/addresses\/kaspatest:q/);
  await expect(page.getByTestId('wallet-explorer')).toHaveAttribute('target', '_blank');
  await expect(page.getByTestId('wallet-disconnect')).toHaveText('Disconnect');
  await page.getByTestId('wallet-copy').click();
  await expect(page.getByTestId('wallet-copy')).toHaveText('Copied');
  // Escape closes it and the focus returns to the chip; a press outside closes it too
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('wallet-menu-panel')).toHaveCount(0);
  await expect(chip).toBeFocused();
  await chip.click();
  await expect(page.getByTestId('wallet-menu-panel')).toBeVisible();
  await page.mouse.click(5, 500);
  await expect(page.getByTestId('wallet-menu-panel')).toHaveCount(0);
  // Disconnect ends the session
  await chip.click();
  await page.getByTestId('wallet-disconnect').click();
  await expect(page.getByTestId('wallet-connected')).toHaveCount(0);
  await expect(page.getByTestId('wallet-connect')).toBeVisible();
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
      // the status is the dot inside the chip, on a phone too
      await expect(page.getByTestId('network-switch').getByTestId('status-dot')).toBeVisible();
      await expect(page.getByTestId('wallet-connect')).toHaveText('Connect');
      for (const gone of ['nav-market', 'theme-toggle', 'wallet-connect-local-key', 'wallet-install-hint']) await expect(page.getByTestId(gone)).toHaveCount(0);
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
    for (const id of ['nav-market', 'nav-portfolio', 'nav-create', 'nav-settings', 'theme-toggle']) await expect(page.getByTestId('menu-panel').getByTestId(id)).toBeVisible();
    await expect(page.getByTestId('menu-network')).toContainText('Network: testnet-10');
    await expect(page.getByTestId('menu-network')).toContainText('no review can show the code is free of bugs');
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
    await page.getByTestId('nav-portfolio').click();
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    await expect(page).toHaveURL(/#\/portfolio/);
    // Escape
    await button.click();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('menu-panel')).toHaveCount(0);
    // the theme toggle changes the page in place: the menu stays open so the change is seen
    await button.click();
    await page.getByTestId('theme-toggle').click();
    await expect(page.getByTestId('menu-panel')).toBeVisible();
  });

  test('Connect offers the extension and the private key (the pick, no sheet); the extension connects, the connected header shows the short address in one row (on a wide font too)', async ({ appPage: page }) => {
    await wideFonts(page);
    await page.setViewportSize({ width: 375, height: 800 });
    await page.goto('/#/market');
    await page.getByTestId('wallet-connect').click();
    await expect(page.getByTestId('wallet-sheet')).toHaveCount(0);
    await expect(page.getByTestId('wallet-choose').getByRole('button')).toHaveText(['Connect KasWare', 'Private key']);
    await page.getByTestId('wallet-connect-kasware').click();
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
    // the connected chip opens its menu, under the header and inside the window, and its Disconnect can be pressed
    await page.getByTestId('wallet-menu').click();
    const menu = (await page.getByTestId('wallet-menu-panel').boundingBox())!;
    expect(menu.x).toBeGreaterThanOrEqual(0);
    expect(menu.x + menu.width).toBeLessThanOrEqual(375);
    await page.getByTestId('wallet-disconnect').click({ trial: true });
  });

  test('a problem colours the bell, is listed in its dialog, and the network chip shows it as a dot', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 375, height: 800 });
    await page.route('**/node/**', (route) => route.abort());
    await page.goto('/#/market');
    await expect(page.getByTestId('notify-bell')).toHaveAttribute('data-status-tone', 'error');
    await expect(await statusEntry(page, 'banner-node')).toBeVisible();
    await closeBell(page);
    await expect(page.getByTestId('network-chip')).toHaveAttribute('data-level', 'bad');
    await expect(page.getByTestId('status-dot')).toHaveAttribute('data-level', 'bad');
    await expect(page.getByTestId('status-dot')).toBeVisible();
  });
});

test('desktop 900 px: the brand shows its name beside the logo when the row has room for it (no wallet connected)', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  await page.goto('/#/market');
  await expect(page.getByTestId('wallet-connect-list')).toBeVisible();
  await expect(page.locator('header.header')).toHaveAttribute('data-layout', 'wide');
  await expect.poll(async () => page.getByTestId('brand').locator('span').evaluate((e) => e.getBoundingClientRect().width)).toBeGreaterThan(20);
  expect(await noScroll(page)).toBe(true);
});

test('desktop 1440 px: the tabs sit in the true centre of the row where it has room, else in the middle between the brand and the tools; one row; Connect is the landing page\'s pill', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1440, height: 800 });
  await page.goto('/#/market');
  const header = page.locator('header.header');
  await expect(page.getByTestId('nav-market')).toBeVisible();
  // the tabs: in the true centre when the row has room for that (a desktop's fonts), else (a wider font, the CI runner's) between the brand and the tools
  const placement = async () => {
    await page.waitForTimeout(300);
    const [brand, tools, nav] = await Promise.all([page.getByTestId('brand').boundingBox(), page.locator('.header .header-tools').boundingBox(), page.locator('.header .nav').boundingBox()]);
    const left = nav!.x - (brand!.x + brand!.width);
    const right = tools!.x - (nav!.x + nav!.width);
    expect(left, 'tabs clear of the brand').toBeGreaterThan(0);
    expect(right, 'tabs clear of the tools').toBeGreaterThan(0);
    if ((await header.getAttribute('data-centred')) === '') expect(Math.abs(nav!.x + nav!.width / 2 - 720), 'the true centre').toBeLessThan(1);
    else expect(Math.abs(left - right), 'the middle of the room').toBeLessThan(2);
    expect((await header.boundingBox())!.height, 'one row').toBeLessThan(60);
  };
  await placement();
  const connect = page.getByTestId('wallet-connect');
  await expect(connect).toHaveClass(/btn-pill/);
  const s = await connect.evaluate((e) => { const c = getComputedStyle(e); return { r: parseFloat(c.borderTopLeftRadius), h: e.getBoundingClientRect().height }; });
  expect(s.r).toBeGreaterThanOrEqual(s.h / 2 - 0.5);
  // connected: the wider wallet chip, still one row
  await connect.click();
  await page.getByTestId('wallet-connect-kasware').click();
  await expect(page.getByTestId('wallet-connected')).toBeVisible();
  if (await page.getByTestId('nav-market').isVisible()) await placement();
  else expect((await header.boundingBox())!.height, 'one row').toBeLessThan(60);
});

test.describe('a touch screen (pointer: coarse)', () => {
  test.use({ hasTouch: true, isMobile: true });

  test('the header controls and the kit buttons are at least 36 px tall; the footer links 32 px', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto('/#/market');
    await connectWallet(page);
    expect(await page.evaluate(() => matchMedia('(pointer: coarse)').matches), 'the touch emulation is on').toBe(true);
    for (const id of ['network-switch', 'wallet-menu', 'notify-bell', 'menu-button']) {
      const box = (await page.getByTestId(id).boundingBox())!;
      expect(box.height, `${id} is tall enough to tap`).toBeGreaterThanOrEqual(36);
      expect(box.width, `${id} is wide enough to tap`).toBeGreaterThanOrEqual(36);
    }
    await page.getByTestId('wallet-menu').click();
    expect((await page.getByTestId('wallet-disconnect').boundingBox())!.height).toBeGreaterThanOrEqual(36);
    await page.keyboard.press('Escape');
    for (const id of ['footer-disclaimer', 'footer-security']) {
      await page.getByTestId(id).scrollIntoViewIfNeeded();
      expect((await page.getByTestId(id).boundingBox())!.height, id).toBeGreaterThanOrEqual(32);
    }
  });

  test('the toasts stand at the bottom of the window, full width minus the gutters', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto('/#/settings');
    const host = page.getByTestId('toast-host');
    const css = await host.evaluate((e) => { const c = getComputedStyle(e); return { top: c.top, bottom: parseFloat(c.bottom), left: parseFloat(c.left), right: parseFloat(c.right) }; });
    expect(css.left).toBe(16);
    expect(css.right).toBe(16);
    expect(css.bottom).toBeGreaterThanOrEqual(16);
  });
});
