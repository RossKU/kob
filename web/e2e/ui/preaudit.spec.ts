// The open review notice (the specification is not final, no review shows the code is free of bugs): a header badge and the footer's disclaimer on every page, and an entry of the bell's current status
// whose dismissal survives a reload. Info: it colours nothing. Nothing blocks trading.
import { closeBell, expect, statusEntry, test } from '../fixtures';

test.describe('open review notice', () => {
  test.use({ appPath: '/#/market' });

  test('the footer leads to the disclaimer: every section, and the line of this network', async ({ appPage: page }) => {
    await page.getByTestId('footer-disclaimer').click();
    await expect(page.getByTestId('disclaimer-page')).toBeVisible();
    await expect(page).toHaveTitle(/^Disclaimer - /);
    await expect(page.getByTestId('disclaimer-network')).toHaveText('This site runs on Kaspa testnet-10. Test tokens have no value.');
    for (const k of ['audit', 'custody', 'tokens', 'market', 'services', 'advice', 'duty', 'donations', 'warranty']) await expect(page.getByTestId(`disclaimer-${k}`)).toBeVisible();
    await expect(page.getByTestId('disclaimer-audit')).toContainText('Open review');
    await expect(page.getByTestId('disclaimer-audit')).toContainText('no review can show the code is free of bugs');
    await expect(page.getByTestId('disclaimer-audit')).not.toContainText('audit');
    await expect(page.getByTestId('disclaimer-warranty')).toContainText('ISC License');
  });

  test('footer line, header badge and the entry of the bell; the dismissal is remembered across a reload', async ({ appPage: page }) => {
    const footer = page.getByTestId('footer-preaudit');
    await expect(footer).toBeVisible();
    // the shared footer's disclaimer (the landing page's wording, for the configured network) and the app's non-custodial statement
    await expect(footer).toContainText('Testnet · open review.');
    await expect(footer).toContainText('The specification is not final, and no review can show the code is free of bugs.');
    await expect(footer).toContainText('Not financial advice');
    await expect(footer).toContainText('publishes software and information only');
    await expect(footer).toContainText('non-custodial');
    await expect(footer.getByRole('link', { name: 'Security policy' })).toHaveAttribute('href', /SECURITY\.md$/);
    await expect(page.getByTestId('preaudit-badge')).toHaveText('open review');

    const banner = await statusEntry(page, 'banner-preaudit');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('Open review: the specification is not final, and no review can show the code is free of bugs.');
    await expect(banner.getByRole('link', { name: 'Security policy' })).toHaveAttribute('href', /SECURITY\.md$/);
    await banner.getByRole('button', { name: 'Dismiss' }).click();
    await expect(banner).toHaveCount(0);
    expect(await page.evaluate(() => localStorage.getItem('kob.preAudit.dismissed.v1'))).toBe('1');
    await closeBell(page);

    await page.reload();
    await expect(page.getByTestId('main')).toBeVisible();
    await expect(await statusEntry(page, 'banner-preaudit')).toHaveCount(0);
    await closeBell(page);
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
