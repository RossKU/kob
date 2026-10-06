// Which registry is this app using? The mock serves the EXAMPLE registry, which is not the registry pinned in this build (registry/tokens.json):
// the app must say so persistently and never call its tokens official. The footer and the settings panel show the identity of the loaded list.
import { readFileSync } from 'node:fs';
import { expect, test } from '../fixtures';

test.describe('registry identity and indexer trust', () => {
  test('a registry that is not the build-pinned default: dot on the network badge, full text in its tooltip, no banner on testnet, identity in the footer', async ({ appPage }) => {
    const badge = appPage.getByTestId('network-badge');
    await expect(badge).toHaveText('testnet-10');
    await expect(appPage.getByTestId('registry-dot')).toBeVisible();
    // testnet: indicator + tooltip only, no banner
    await expect(appPage.getByTestId('banner-custom-registry')).toHaveCount(0);
    const tip = appPage.getByTestId('registry-tip');
    await expect(tip).toBeHidden();
    // hover shows the full text: title, source, sha256, pinned default, "check who published this list"
    await badge.hover();
    await expect(tip).toBeVisible();
    await expect(tip).toContainText('Non-default token registry');
    await expect(tip).toContainText('not the one pinned');
    await expect(tip).toContainText('sha256');
    await expect(tip).toContainText('pinned default');
    await expect(tip).toContainText('Check who published this list');
    await appPage.mouse.move(0, 400);
    await expect(tip).toBeHidden();
    const footer = appPage.getByTestId('footer-registry');
    await expect(footer).toHaveAttribute('data-default', 'false');
    await expect(footer).toContainText('sha256');
    await expect(footer).toContainText('testnet-10');
    // the indicator follows the user to every page
    await appPage.goto('/#/orders');
    await expect(appPage.getByTestId('registry-dot')).toBeVisible();
    await expect(appPage.getByTestId('banner-custom-registry')).toHaveCount(0);
    await expect(appPage.locator('[data-testid^="badge-official"]')).toHaveCount(0);
  });

  test('the tooltip opens with the keyboard (focus), closes on Escape, and a tap pins it until the next tap', async ({ appPage }) => {
    const badge = appPage.getByTestId('network-badge');
    const tip = appPage.getByTestId('registry-tip');
    await expect(badge).toHaveAttribute('aria-describedby', /.+/);
    await expect(tip).toBeHidden();
    await badge.focus();
    await expect(tip).toBeVisible();
    await appPage.keyboard.press('Escape');
    await expect(tip).toBeHidden();
    await badge.blur();
    // tap / click pins it open even when the pointer leaves
    await badge.click();
    await appPage.mouse.move(0, 400);
    await expect(tip).toBeVisible();
    await expect(badge).toHaveAttribute('aria-expanded', 'true');
    // a tap elsewhere closes it
    await appPage.mouse.click(0, 400);
    await expect(tip).toBeHidden();
    // a second tap on the badge closes a pinned tooltip
    await badge.click();
    await badge.click();
    await appPage.mouse.move(0, 400);
    await expect(tip).toBeHidden();
  });

  test.describe('on mainnet', () => {
    // the mock registry is a testnet-10 list: serve a mainnet copy of it (still not the build-pinned default)
    test.use({ appConfig: { network: 'mainnet' }, autoOpen: false });
    test('a compact one-line notice with the details in the same tooltip', async ({ appPage }) => {
      const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
      reg.network = 'mainnet';
      await appPage.route('**/registry/tokens.json', (route) =>
        route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
      );
      await appPage.goto('/');
      const banner = appPage.getByTestId('banner-custom-registry');
      await expect(banner).toBeVisible();
      // short and one line: the long text is in the (hidden) tooltip, not in the banner
      await expect(banner).toContainText('Custom token registry — details');
      await expect(appPage.getByTestId('banner-custom-registry-tip')).toBeHidden();
      expect((await banner.boundingBox())!.height).toBeLessThan(48);
      await expect(appPage.getByTestId('registry-dot')).toBeVisible();
      await appPage.getByTestId('banner-custom-registry-details').click();
      const tip = appPage.getByTestId('banner-custom-registry-tip');
      await expect(tip).toBeVisible();
      await expect(tip).toContainText('not the one pinned');
      await expect(tip).toContainText('Check who published this list');
    });
  });

  test('the settings panel shows source, network, exact sha256, the pinned default and the single-indexer status', async ({ appPage }) => {
    await appPage.goto('/#/settings');
    const panel = appPage.getByTestId('settings-trust');
    await expect(panel).toBeVisible();
    await expect(panel.getByTestId('registry-default-flag')).toContainText('non-default');
    await expect(panel.getByTestId('registry-sha256')).toBeVisible();
    await expect(panel.getByTestId('registry-pin')).toHaveText(/^[0-9a-f]{64}$/);
    // one indexer configured: nothing to cross-check against
    await expect(panel.getByTestId('indexer-trust')).toHaveAttribute('data-trust', 'single');
    await expect(panel.getByTestId('indexer-trust')).toContainText('single indexer, unverified');
  });

  test('the token page names the registry next to the badges', async ({ appPage, mock }) => {
    const tok = await mock.token();
    await appPage.goto(`/#/market/${tok.covenant_id}`);
    await expect(appPage.getByTestId('token-registry-note')).toContainText('non-default registry');
  });
});
