// The Support page (#/support). The donation address is a mainnet one, so on this testnet-10 build too the page shows it and reads the mainnet explorer; that explorer is
// mocked here, the real one is never reached. An empty address, injected with `__KOB_TEST_SUPPORT_ADDRESS__` (read with `features.test` on), hides the page.
import type { Page } from '@playwright/test';
import { expect, test, TEST_KEYS } from '../fixtures';

/** The published donation address (web/src/ui/support/support-model.ts, README.md). */
const SUPPORT_ADDRESS = 'kaspa:qq85cdj959gx33su28luhaymtq3yczx0jj40eufxwyxs77mk4vnpqd3m0nqqc';

test.use({ autoOpen: false });

const CORS = { 'access-control-allow-origin': '*', 'access-control-expose-headers': 'X-Next-Page-Before' };

/** One accepted payment to the donation address. */
const payment = (n: number, from: string, sompi: number) => ({
  transaction_id: n.toString(16).padStart(64, '0'),
  block_time: 1_780_000_000_000 + n,
  is_accepted: true,
  inputs: [{ previous_outpoint_address: from, previous_outpoint_amount: sompi + 10_000 }],
  outputs: [{ script_public_key_address: SUPPORT_ADDRESS, amount: sompi }],
});

function mockHistory(page: Page, rows: unknown[] | 'down') {
  return page.route(/^https:\/\/api\.kaspa\.org\/addresses\//, (route) =>
    rows === 'down' ? route.abort('failed') : route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify(rows) }),
  );
}

test.describe('with no address', () => {
  test('no footer link anywhere, and #/support is the market list', async ({ appPage: page }) => {
    await page.addInitScript(() => {
      window.__KOB_TEST_SUPPORT_ADDRESS__ = '';
    });
    await page.goto('/');
    await expect(page.getByTestId('landing')).toBeVisible();
    await expect(page.getByTestId('footer-support')).toHaveCount(0);
    await page.goto('/#/support');
    await expect(page.getByTestId('token-list-view')).toBeVisible();
    await expect(page.getByTestId('support-page')).toHaveCount(0);
    await expect(page.getByTestId('footer-support')).toHaveCount(0);
  });
});

test.describe('the page', () => {
  test('the footer of the landing page and of the app lead to it: the mainnet address on a testnet-10 site, QR code, terms, table, thanks', async ({ appPage: page }) => {
    await mockHistory(page, [payment(1, TEST_KEYS.bob.mainnetAddress, 300_000_000), payment(2, TEST_KEYS.carol.mainnetAddress, 700_000_000), payment(3, TEST_KEYS.bob.mainnetAddress, 100_000_000)]);
    await page.goto('/');
    await expect(page.getByTestId('landing')).toHaveAttribute('data-network', 'testnet-10');
    await expect(page.getByTestId('footer-support')).toHaveAttribute('href', '#/support');
    await page.goto('/#/settings');
    await page.getByTestId('footer-support').click();
    await expect(page.getByTestId('support-page')).toBeVisible();
    await expect(page).toHaveTitle(/^Support - /);

    await expect(page.getByTestId('support-volunteers')).toContainText('built and run by volunteers');
    await expect(page.getByTestId('support-address')).toHaveAttribute('data-value', SUPPORT_ADDRESS);
    await expect(page.getByTestId('support-network')).toContainText('real KAS on Kaspa mainnet');
    await expect(page.getByTestId('support-terms')).toContainText('voluntary gift');
    // the QR code keeps its white ground on a dark page
    await expect(page.getByTestId('support-qr').locator('rect')).toHaveAttribute('fill', '#fff');
    await expect(page.getByTestId('support-qr').locator('path')).toHaveAttribute('fill', '#000');

    const rows = page.getByTestId('support-contributor');
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0)).toContainText('Archguardian');
    await expect(rows.nth(0).locator('td').last()).toHaveText('7');
    await expect(rows.nth(1)).toContainText('Guardian');
    await expect(rows.nth(1).locator('td').last()).toHaveText('4');
    await expect(page.getByTestId('support-executors')).toContainText('Thank you');
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'no horizontal scroll').toBe(true);
  });

  test('a failed read shows one line, and the rest of the page stays', async ({ appPage: page }) => {
    await mockHistory(page, 'down');
    await page.goto('/#/support');
    await expect(page.getByTestId('support-contributors-failed')).toHaveText('The contributors could not be read right now.');
    await expect(page.getByTestId('support-contributors-table')).toHaveCount(0);
    await expect(page.getByTestId('support-address')).toBeVisible();
    await expect(page.getByTestId('support-qr')).toBeVisible();
    await expect(page.getByTestId('support-executors')).toBeVisible();
  });

  test('nobody yet: the table says so', async ({ appPage: page }) => {
    await mockHistory(page, []);
    await page.goto('/#/support');
    await expect(page.getByTestId('support-contributors-empty')).toHaveText('No contributions yet.');
  });
});
