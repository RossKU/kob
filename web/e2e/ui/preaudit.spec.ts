// The pre-audit notice (no external security audit): a header badge and the footer's disclaimer on every page, and a one-time dismissible banner whose
// dismissal survives a reload. Nothing blocks trading.
import { expect, test } from '../fixtures';

test.describe('pre-audit notice', () => {
  test.use({ appPath: '/#/market' });

  test('footer line, header badge and first-visit banner; the dismissal is remembered across a reload', async ({ appPage: page }) => {
    const footer = page.getByTestId('footer-preaudit');
    await expect(footer).toBeVisible();
    // the shared footer's disclaimer (the landing page's wording, for the configured network) and the app's non-custodial statement
    await expect(footer).toContainText('Testnet · pre-audit.');
    await expect(footer).toContainText('has not had an external security audit');
    await expect(footer).toContainText('publishes software and information only');
    await expect(footer).toContainText('non-custodial');
    await expect(footer.getByRole('link', { name: 'Security policy' })).toHaveAttribute('href', /SECURITY\.md$/);
    await expect(page.getByTestId('preaudit-badge')).toHaveText('pre-audit');

    const banner = page.getByTestId('banner-preaudit');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('Canary launch');
    await banner.getByRole('button', { name: 'Dismiss' }).click();
    await expect(banner).toHaveCount(0);

    await page.reload();
    await expect(page.getByTestId('main')).toBeVisible();
    await expect(page.getByTestId('banner-preaudit')).toHaveCount(0);
    // the permanent parts stay
    await expect(page.getByTestId('footer-preaudit')).toBeVisible();
    await expect(page.getByTestId('preaudit-badge')).toBeVisible();
  });

  test('the footer line is on the other pages too', async ({ appPage: page }) => {
    for (const hash of ['#/orders', '#/create', '#/settings']) {
      await page.goto(`/${hash}`);
      await expect(page.getByTestId('footer-preaudit')).toBeVisible();
    }
  });
});
