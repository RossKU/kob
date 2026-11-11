// The header's network chip is a control: it opens a small panel with the two networks (the current one marked), and choosing the other one saves the network
// (like Settings) and reloads. The deployment's servers are for its own network, so the other network starts without them; a deployment can pin its network
// (`lockNetwork`: no switch) and a browser with saved servers is told why it cannot switch.
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';

test.use({ autoOpen: false });

const stored = (page: Page) => page.evaluate(() => JSON.parse(localStorage.getItem('kob.settings') ?? 'null'));
const badge = (page: Page) => page.getByTestId('network-badge');
const noScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);

/** Choose a network in the panel: the page reloads on it. */
async function choose(page: Page, network: 'mainnet' | 'testnet-10') {
  await page.getByTestId('network-switch').click();
  await expect(page.getByTestId('network-switch-panel')).toBeVisible();
  await Promise.all([page.waitForEvent('load'), page.getByTestId(`network-option-${network}`).click()]);
}

test('the chip is a button with a name; it opens the panel with both networks, the current one marked, the notes, and a click on the current one changes nothing', async ({ appPage: page }) => {
  await page.goto('/#/market');
  const button = page.getByTestId('network-switch');
  await expect(button).toBeVisible();
  await expect(button).toHaveAttribute('aria-haspopup', 'true');
  await expect(button).toHaveAttribute('aria-expanded', 'false');
  await expect(page.getByRole('button', { name: 'Network: testnet-10. Change network' })).toBeVisible();
  // the chip's own words are still in it
  await expect(badge(page)).toHaveAttribute('data-network', 'testnet-10');
  await expect(page.getByTestId('preaudit-badge')).toHaveText('open review');
  await expect(page.getByTestId('network-switch-panel')).toHaveCount(0);

  await button.click();
  await expect(button).toHaveAttribute('aria-expanded', 'true');
  const panel = page.getByTestId('network-switch-panel');
  await expect(panel).toBeVisible();
  await expect(page.getByTestId('network-option-testnet-10')).toHaveAttribute('aria-current', 'true');
  await expect(page.getByTestId('network-option-testnet-10')).toContainText('current');
  await expect(page.getByTestId('network-option-mainnet')).not.toHaveAttribute('aria-current', 'true');
  await expect(page.getByTestId('network-option-mainnet')).toBeEnabled();
  await expect(page.getByTestId('network-switch-note')).toContainText('Open review');
  await expect(page.getByTestId('network-switch-reload')).toContainText('reloads the page');
  await expect(page.getByTestId('network-switch-reload')).toContainText('real funds');
  // the deployment has an indexer, which is for testnet-10: the other network starts without it, and the panel says so
  await expect(page.getByTestId('network-switch-servers')).toContainText('no indexer');

  // the current network again: nothing is saved
  await page.getByTestId('network-option-testnet-10').click();
  expect(await stored(page)).toBeNull();
  await expect(panel).toBeVisible();

  await page.keyboard.press('Escape');
  await expect(panel).toHaveCount(0);
  await expect(button).toBeFocused();
});

test('keyboard: Enter opens the panel, Tab reaches the options, Enter on the other network switches', async ({ appPage: page }) => {
  await page.goto('/#/market');
  await page.getByTestId('network-switch').focus();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('network-switch-panel')).toBeVisible();
  await page.keyboard.press('Tab');
  await expect(page.getByTestId('network-option-mainnet')).toBeFocused();
  await Promise.all([page.waitForEvent('load'), page.keyboard.press('Enter')]);
  await expect(badge(page)).toHaveAttribute('data-network', 'mainnet');
  expect(await stored(page)).toEqual({ network: 'mainnet' });
});

test('switching to mainnet and back: the setting is saved, the page reloads into the network, and the testnet servers are not used on mainnet', async ({ appPage: page, mock }) => {
  await page.goto('/#/market');
  await expect(badge(page)).toHaveAttribute('data-network', 'testnet-10');

  await choose(page, 'mainnet');
  expect(await stored(page)).toEqual({ network: 'mainnet' });
  await expect(badge(page)).toHaveAttribute('data-network', 'mainnet');
  await expect(page.getByTestId('network-switch')).toHaveAccessibleName('Network: mainnet. Change network');
  // none of the deployment's servers is in force: Settings (advanced) shows them empty, the network is mainnet
  await page.goto('/#/settings');
  await page.getByTestId('settings-servers-fold').getByRole('button').first().click();
  await expect(page.getByTestId('settings-indexer-url')).toHaveValue('');
  await expect(page.getByTestId('settings-node-url')).toHaveValue('');
  await expect(page.getByTestId('settings-registry-url')).toHaveValue('./registry/tokens.json');
  await expect(page.getByTestId('settings-network')).toContainText('mainnet');
  // the panel on mainnet: the current one is mainnet, and no note about losing servers (there are none to lose)
  await page.getByTestId('network-switch').click();
  await expect(page.getByTestId('network-option-mainnet')).toHaveAttribute('aria-current', 'true');
  await expect(page.getByTestId('network-switch-servers')).toHaveCount(0);

  // and back: the deployment's own servers apply again
  await Promise.all([page.waitForEvent('load'), page.getByTestId('network-option-testnet-10').click()]);
  expect(await stored(page)).toEqual({ network: 'testnet-10' });
  await expect(badge(page)).toHaveAttribute('data-network', 'testnet-10');
  await page.getByTestId('settings-servers-fold').getByRole('button').first().click();
  await expect(page.getByTestId('settings-indexer-url')).toHaveValue(mock.url);
  await expect(page.getByTestId('settings-registry-url')).toHaveValue(`${mock.url}/registry/tokens.json`);
});

test.describe('the deployment pins its network', () => {
  test.use({ appConfig: { lockNetwork: true } });
  test('the chip offers no choice: its panel has the status lines and the notice, no networks to pick', async ({ appPage: page }) => {
    await page.goto('/#/market');
    await expect(page.getByTestId('network-chip')).toBeVisible();
    await expect(badge(page)).toHaveAttribute('data-network', 'testnet-10');
    await expect(page.getByTestId('network-switch')).toHaveCount(0);
    await page.getByTestId('network-status').click();
    await expect(page.getByTestId('network-status-panel')).toBeVisible();
    await expect(page.getByTestId('status-node')).toBeVisible();
    await expect(page.getByTestId('network-switch-note')).toContainText('Open review');
    await expect(page.getByTestId('network-option-mainnet')).toHaveCount(0);
    await expect(page.getByTestId('network-switch-panel')).toHaveCount(0);
  });
});

test('servers saved in this browser: the switch says why it is off and points to Settings', async ({ appPage: page, mock }) => {
  await page.addInitScript((url) => {
    try {
      if (!localStorage.getItem('kob.settings')) localStorage.setItem('kob.settings', JSON.stringify({ indexerUrl: url }));
    } catch {
      /* no storage */
    }
  }, mock.url);
  await page.goto('/#/market');
  await page.getByTestId('network-switch').click();
  await expect(page.getByTestId('network-switch-blocked')).toContainText('indexerUrl');
  await expect(page.getByTestId('network-option-mainnet')).toBeDisabled();
  await expect(page.getByTestId('network-option-testnet-10')).toHaveAttribute('aria-current', 'true');
  await page.getByTestId('network-switch-settings').click();
  await expect(page).toHaveURL(/#\/settings$/);
  await expect(page.getByTestId('network-switch-panel')).toHaveCount(0);
  expect(await stored(page)).toEqual({ indexerUrl: mock.url });
});

test('a phone: the chip (the short form with its status dot) opens the panel under the header, inside the window, and switches', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/#/market');
  await expect(badge(page)).toHaveText('tn10');
  await expect(page.getByTestId('menu-button')).toBeVisible();
  await expect(page.getByTestId('chip-dot')).toBeVisible();
  await page.getByTestId('network-switch').click();
  const panel = page.getByTestId('network-switch-panel');
  await expect(panel).toBeVisible();
  // the panel rises in over ~160 ms (the app's motion): measured when it has arrived
  await panel.evaluate((el) => Promise.all(el.getAnimations().map((x) => x.finished)));
  const box = (await panel.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(390);
  const header = (await page.locator('header.header').boundingBox())!;
  expect(box.y).toBeGreaterThanOrEqual(header.y + header.height - 2);
  expect(await noScroll(page)).toBe(true);
  // the menu and the chip's panel never stay open together
  await page.getByTestId('menu-button').click();
  await expect(page.getByTestId('menu-panel')).toBeVisible();
  await expect(panel).toHaveCount(0);
  await expect(page.getByTestId('menu-network')).toBeVisible();
  await page.getByTestId('network-switch').click();
  await expect(page.getByTestId('menu-panel')).toHaveCount(0);
  await Promise.all([page.waitForEvent('load'), page.getByTestId('network-option-mainnet').click()]);
  await expect(badge(page)).toHaveAttribute('data-network', 'mainnet');
  await expect(badge(page)).toHaveText('mainnet');
  expect(await stored(page)).toEqual({ network: 'mainnet' });
  expect(await noScroll(page)).toBe(true);
});
