// Connect clicked before the extension has injected itself (audit 2026-10-29 round 2, R5): "no wallet detected" is not taken from a list that has not been
// filled yet. The click waits for the first look for wallets (or a short timeout), then decides: with the extension there it connects to it, and the private
// key entry stays closed; without one it opens the private key entry as before.
import { test, expect, TEST_KEYS } from '../fixtures';
import { buildWalletConfig } from '../wallet';
import { mockWalletInitScript } from '../../src/testing/mock-wallets';

test.use({ walletId: null, autoOpen: false });

test('a click on Connect before the extension injects connects to the extension, not to the private key entry', async ({ appPage: page }) => {
  const config = buildWalletConfig({ wallet: 'kasware', secretKey: TEST_KEYS.alice.secretKey });
  // the extension is not there when the page starts; the test lets it appear (as real extensions do, a moment after the page's scripts) right after the click
  await page.addInitScript({ content: `window.__injectWallet = () => { ${mockWalletInitScript(config)} };` });
  await page.goto('/');
  await page.getByTestId('wallet-connect-local-key').click(); // the button as it reads while no wallet is known yet
  await page.evaluate(() => (window as unknown as { __injectWallet: () => void }).__injectWallet());
  await expect(page.getByTestId('wallet-address')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId('signer-overlay')).toHaveCount(0);
  await expect(page.getByTestId('signer-frame')).toHaveCount(0);
});

test('without any extension the click opens the private key entry once the first look is over', async ({ appPage: page }) => {
  await page.goto('/');
  await page.getByTestId('wallet-connect-local-key').click();
  await expect(page.getByTestId('signer-overlay')).toBeVisible({ timeout: 15_000 });
});
