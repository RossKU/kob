// The token page's header is the market bar (e2e/ui/market-bar.spec.ts): the identity block and the pair selector, the flip, the star and the 24 h figures in ONE row, then Live, and nothing else
// unless there is a warning: no title block, no covenant id, no name, no chips. A normal token shows no status; the High risk chip (with its reason on a tap) is the one chip that stays. The
// covenant id (whole, with a copy button), the name, where its facts come from and the program chip (whose tooltip names the program and the decimals) are in the token's details card, once. A real
// warning stays in the page as a one-line note.
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';

const ID = '3165ab67'.padEnd(64, '9');

test.use({ autoOpen: false });

async function seedOpenToken(page: Page, mock: { seed(s: Record<string, unknown>): Promise<unknown> }, over: Record<string, unknown> = {}): Promise<void> {
  await mock.seed({
    tokens: [{ ticker: '', covenant_id: ID, program: 'KCC20PublicMint', template_id: 'KCC20PublicMint', extension_commitment: 'ee'.repeat(32), powers: ['public-mint'], decimals: null, order_scale: 100_000_000, ...over }],
    orders: [{ token: ID, side: 'ask', maker: 'alice', amount: '4500000000000', price: 1_000_000 }],
  });
  await page.route(/\/v1\/tokens(\?.*)?$/, async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    for (const t of body.tokens) if (t.covenant_id === ID) Object.assign(t, { warnings: ['genesis_unverified'] });
    await route.fulfill({ response: res, json: body });
  });
}

for (const [name, size] of [['desktop', { width: 1280, height: 900 }], ['phone', { width: 375, height: 800 }]] as const) {
  test.describe(`token header, ${name}`, () => {
    test.use({ viewport: size });

    test('no blue box, no bullet sentences, no program line, no disclosure; the id, the name and the kind chip are in the details; only a risk chip would be in the bar', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock);
      await page.goto(`/#/market/${ID}`);
      const header = page.getByTestId('token-header');
      await expect(page.getByTestId('order-book')).toBeVisible();
      await expect(header.locator('.banner')).toHaveCount(0);
      for (const gone of ['token-open-caution', 'open-token-decimals', 'token-program-note', 'token-more', 'token-fact-text-genesis_unverified', 'token-fact-text-public-mint', 'token-short-id', 'token-meta']) await expect(header.getByTestId(gone)).toHaveCount(0);
      await expect(header.locator('details')).toHaveCount(0);
      await expect(header).not.toContainText(/Its creation was not checked|lets anyone create more|Not in the token registry|Token program:/);
      // the identity block, then the pair text, then the swap button right after it
      const [icon, fp, pair, flip] = await Promise.all([`token-icon-${ID}`, `fingerprint-${ID}`, 'market-pair', 'market-flip'].map((id) => header.getByTestId(id).boundingBox()));
      expect(icon!.x).toBeLessThan(fp!.x);
      expect(fp!.x + fp!.width).toBeLessThan(pair!.x);
      expect(pair!.x + pair!.width).toBeLessThanOrEqual(flip!.x + 1);
      // the fingerprint is 60 % of the icon and centred on it
      expect(Math.abs(fp!.height / icon!.height - 0.6)).toBeLessThan(0.03);
      expect(Math.abs(fp!.y + fp!.height / 2 - (icon!.y + icon!.height / 2))).toBeLessThan(1.5);
      // a normal token shows no status: no fact chip at all, no High risk chip, and the kind chip is not in the bar
      await expect(header.locator('[data-testid^="token-fact-"]')).toHaveCount(0);
      await expect(header.getByTestId('badge-high-risk')).toHaveCount(0);
      await expect(header.getByTestId('badge-family')).toHaveCount(0);
      await expect(header).not.toContainText(/Genesis not verified|Public mint/);
      // the covenant id is said once, whole, with a copy button, in the details; the kind chip is there too, its tooltip names the program and the decimals
      const details = page.getByTestId('token-details');
      await expect(details.getByTestId('token-covenant-id')).toHaveText(ID);
      await expect(details.getByTestId('token-covenant-id')).toHaveAttribute('title', ID);
      await expect(details.getByRole('button', { name: /copy/i }).first()).toBeVisible();
      const chip = details.getByTestId('badge-family');
      await expect(chip).toBeVisible();
      await expect(chip).toHaveAttribute('title', /Token program: KCC20PublicMint \(reviewed\), as pinned in this build\./);
      await expect(chip).toHaveAttribute('title', /whole tokens of 8 decimals/);
      // the facts are one tap away, in the details
      await page.getByTestId('token-status-details').getByRole('button').first().click();
      await expect(page.getByTestId('status-fact-public-mint')).toContainText(/anyone create more/);
      await expect(page.getByTestId('status-fact-genesis_unverified')).toContainText(/not checked against its token program/);
    });

    test('the trading content starts right below the bar', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock);
      await page.goto(`/#/market/${ID}`);
      await expect(page.getByTestId('order-book')).toBeVisible();
      const header = (await page.getByTestId('token-header').boundingBox())!;
      const bar = (await page.getByTestId('market-bar').boundingBox())!;
      const grid = (await page.locator('.mkt-grid').boundingBox())!;
      // the bar is the header (one row of 56 to 64 px; a phone's first row and its figures under it), the grid follows it at the page gap
      expect(header.height).toBeLessThan(size.width < 600 ? 135 : 70);
      expect(grid.y - (bar.y + bar.height)).toBeLessThan(20);
    });

    test('a real warning is one line in the page', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock, { powers: ['freeze'] });
      await page.goto(`/#/market/${ID}`);
      await expect(page.getByTestId('token-header').getByTestId('powers-warning')).toBeVisible();
      // and the one High risk chip, in the bar, its reason on a tap
      await expect(page.getByTestId('market-bar').getByTestId('badge-high-risk')).toHaveAttribute('data-reasons', 'issuer_control');
      await page.getByTestId('token-header').getByTestId('high-risk-toggle').click();
      await expect(page.getByTestId('high-risk-reasons')).toContainText('freeze or seize balances');
    });
  });
}
