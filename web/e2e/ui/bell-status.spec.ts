// The current status of the bell: what holds now, listed above the notifications, colouring the bell without opening it. Live state, not history: an entry
// comes and goes with its condition, is not stored and Clear does not touch it. It does not depend on the notifications being on or a wallet being connected.
// The one exception stays on the page: a wallet on another network.
import type { Page } from '@playwright/test';
import { closeBell, expect, openBell, statusEntry, test, TEST_KEYS } from '../fixtures';
import { connectWallet } from '../helpers/env';

const bell = (page: Page) => page.getByTestId('notify-bell');
const shot = (name: string) => (process.env.KOB_SHOT_DIR ? `${process.env.KOB_SHOT_DIR}/${name}.png` : undefined);
// (the mock stack needs `features.test`: test mode is a warning of its own, so without a problem the bell here is amber with the open review notice beside it)
const cssColor = (page: Page, variable: string) =>
  page.evaluate((v) => {
    const probe = document.createElement('i');
    probe.style.color = `var(${v})`;
    document.body.append(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  }, variable);
const noPageScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);

test.describe('without a wallet, notifications off (the defaults)', () => {
  test.use({ walletId: null, autoOpen: false });

  test('the node goes down: the bell turns red and counts it, the dialog lists it above the rest, and it leaves by itself when the node returns', async ({ appPage: page }) => {
    test.setTimeout(90_000);
    await page.goto('/#/market');
    await expect(page.getByTestId('status-dot')).toHaveAttribute('data-node-level', 'ok');
    await expect(bell(page)).toHaveAttribute('data-status-tone', 'warn');
    await expect(bell(page)).toHaveAttribute('aria-label', 'Notifications. Current status: 2');

    await page.route('**/node/**', (route) => route.abort());
    await expect(bell(page)).toHaveAttribute('data-status-tone', 'error', { timeout: 40_000 });
    await expect(bell(page)).toHaveAttribute('aria-label', 'Notifications. Current status: 3');
    await expect(page.getByTestId('notify-status-live')).toHaveText('Current status: 3');
    await page.mouse.move(1, 1);
    await expect.poll(() => bell(page).evaluate((b) => getComputedStyle(b).color)).toBe(await cssColor(page, '--bad'));
    await page.screenshot({ path: shot('bell-red-closed') });
    // nothing of it on the page
    await expect(page.getByTestId('banner-node')).toHaveCount(0);

    const entry = await statusEntry(page, 'banner-node');
    await expect(entry).toContainText('No connection to a Kaspa node');
    await expect(entry).toContainText('cannot be reached');
    await page.screenshot({ path: shot('bell-red-open') });
    // the worst first, and all of it above the notifications
    await expect(page.getByTestId('notify-status').locator('.banner').first()).toHaveAttribute('data-testid', 'banner-node');
    const status = (await page.getByTestId('notify-status').boundingBox())!;
    const history = (await page.getByTestId('notify-connect').boundingBox())!;
    expect(status.y + status.height).toBeLessThanOrEqual(history.y);

    await page.unroute('**/node/**');
    await expect(entry).toHaveCount(0, { timeout: 40_000 });
    await expect(bell(page)).toHaveAttribute('data-status-tone', 'warn');
    await expect(bell(page)).toHaveAttribute('data-status-count', '2');
    await closeBell(page);
  });

  test('the status is readable with the notifications off and no wallet; the dialog says both as before', async ({ appPage: page }) => {
    await page.goto('/#/market');
    await expect(bell(page)).toBeVisible();
    await expect(await statusEntry(page, 'banner-preaudit')).toBeVisible();
    await expect(page.getByTestId('notify-disabled')).toBeVisible();
    await expect(page.getByTestId('notify-connect')).toBeVisible();
  });

  test('a dismissed entry keeps the focus in the dialog, and info colours nothing', async ({ appPage: page }) => {
    await page.goto('/#/market');
    const entry = await statusEntry(page, 'banner-preaudit');
    await entry.getByRole('button', { name: 'Dismiss' }).click();
    await expect(entry).toHaveCount(0);
    await expect(bell(page)).toHaveAttribute('aria-label', 'Notifications. Current status: 1');
    await expect.poll(() => page.evaluate(() => !!document.activeElement?.closest('[data-testid="notify-panel"]'))).toBe(true);
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('notify-panel')).toHaveCount(0);
    // the tones without an alarm (the mock stack always has the test-mode warning): the bell keeps its plain colour
    await page.mouse.move(1, 1);
    for (const tone of ['info', 'none']) {
      const color = await bell(page).evaluate((b, v) => {
        b.setAttribute('data-status-tone', v);
        return getComputedStyle(b).color;
      }, tone);
      expect(color).not.toBe(await cssColor(page, '--bad'));
      expect(color).not.toBe(await cssColor(page, '--warn'));
    }
  });
});

test.describe('in test mode, with what is saved in this browser', () => {
  test.use({ walletId: null, autoOpen: false });

  test('a warning turns the bell amber; a link to Settings closes the dialog; 1280 and 390 px: the dialog scrolls and the page does not', async ({ appPage: page, mock }) => {
    await page.addInitScript(() => {
      localStorage.setItem('kob.settings', JSON.stringify({ extraIndexerUrls: ['https://a-very-long-further-indexer-host-name-for-the-phone-layout.testnet.example.invalid'] }));
    });
    await mock.setHealth('following', 1300); // 130 s behind: a warning
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto('/#/market');
    await expect(bell(page)).toHaveAttribute('data-status-tone', 'warn');
    await expect.poll(() => bell(page).evaluate((b) => getComputedStyle(b).color)).toBe(await cssColor(page, '--warn'));
    await expect(await statusEntry(page, 'banner-test-mode')).toBeVisible();
    await expect(page.getByTestId('banner-stored-extra-indexers')).toBeVisible();
    await expect(page.getByTestId('banner-indexer')).toBeVisible();
    await page.screenshot({ path: shot('bell-1280') });
    await closeBell(page);
    await page.screenshot({ path: shot('bell-closed-1280') });

    await page.setViewportSize({ width: 390, height: 500 });
    await openBell(page);
    expect(await noPageScroll(page)).toBeLessThanOrEqual(0);
    const dialog = page.getByTestId('notify-panel');
    const box = (await dialog.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390);
    for (const b of await page.getByTestId('notify-status').locator('.banner').all()) {
      const inner = (await b.boundingBox())!;
      expect(inner.x + inner.width).toBeLessThanOrEqual(box.x + box.width);
    }
    await expect(dialog.getByTestId('banner-preaudit')).toBeVisible();
    // taller than the screen: the last line is reached by scrolling the dialog
    expect(box.height).toBeGreaterThan(500);
    const link = page.getByTestId('notify-settings-link');
    await link.scrollIntoViewIfNeeded();
    await expect(link).toBeInViewport();
    await page.evaluate(() => document.querySelector('.modal-backdrop')?.scrollTo({ top: 0 }));
    await page.screenshot({ path: shot('bell-390') });

    await page.getByTestId('banner-stored-extra-indexers').getByRole('link', { name: 'Settings' }).click();
    await expect(page.getByTestId('notify-panel')).toHaveCount(0);
    await expect(page.getByTestId('settings-view')).toBeVisible();
  });
});

test.describe('with a wallet', () => {
  test('Clear empties the notifications, not the status', async ({ appPage: page, mock }) => {
    void mock;
    const key = `kob.notify.testnet-10.${TEST_KEYS.alice.pubkey.toLowerCase()}`;
    await page.evaluate(([k, v]) => localStorage.setItem(k, v), [key, JSON.stringify({ v: 1, snaps: null, payments: null, items: [{ id: 'fill-1', kind: 'fill', side: 'sell', params: { amount: '100000000' }, at: Date.now(), read: false }], seen: [], invoices: [] })] as const);
    await page.reload();
    if (!(await page.getByTestId('wallet-address').isVisible())) await connectWallet(page);
    await expect(page.getByTestId('notify-unread')).toHaveText('1');
    const count = await bell(page).getAttribute('data-status-count');
    await expect(bell(page)).toHaveAttribute('aria-label', `Notifications, 1 unread. Current status: ${count}`);
    await openBell(page);
    await page.getByTestId('notify-clear').click();
    await expect(page.getByTestId('notify-empty')).toBeVisible();
    await expect(page.getByTestId('banner-test-mode')).toBeVisible();
    await expect(bell(page)).toHaveAttribute('data-status-count', count!);
    // nothing of the status was written to the stored notifications
    const stored = await page.evaluate((k) => localStorage.getItem(k), key);
    expect(stored).not.toContain('banner');
    expect(JSON.parse(stored!).items).toEqual([]);
  });

  test.describe('on the wrong network', () => {
    test.use({ walletOptions: { network: 'mainnet', allowNetworkSwitch: false } });

    for (const width of [1280, 390]) {
      test(`${width} px: the mismatch stays at the top of the page as one line, a press unfolds it, and it is not in the dialog`, async ({ appPage: page }) => {
        await page.setViewportSize({ width, height: 800 });
        await page.goto('/#/market');
        await connectWallet(page);
        const banner = page.getByTestId('main').getByTestId('banner-network-mismatch');
        await expect(banner).toBeVisible();
        await expect(banner).toContainText('Wrong wallet network: trading is blocked');
        const line = (await banner.boundingBox())!;
        expect(line.height, 'one line').toBeLessThan(36);
        expect(await noPageScroll(page)).toBeLessThanOrEqual(0);
        await page.screenshot({ path: shot(`mismatch-${width}`) });
        // the whole text, one press away
        await banner.locator('summary').click();
        await expect(banner.getByText('Switch the network in your wallet settings')).toBeVisible();
        expect((await banner.boundingBox())!.height).toBeGreaterThan(line.height);
        expect(await noPageScroll(page)).toBeLessThanOrEqual(0);
        await openBell(page);
        await expect(page.getByTestId('notify-panel').getByTestId('banner-network-mismatch')).toHaveCount(0);
      });
    }
  });
});
