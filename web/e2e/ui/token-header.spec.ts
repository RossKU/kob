// The token page header is compact so the trading content comes first: the identity block and the pair selector, one line (short id with a copy button,
// name, where its facts come from, Live) and one line of badges (the token family, whose tooltip names the program and the decimals, and the facts, each
// with its sentence as the tooltip). No box, no bullet sentences, no disclosure. A real warning stays in the page as a one-line note.
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';

const ID = '3165ab67'.padEnd(64, '9');
const SHORT = '3165ab679999…999999999999';

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

    test('no blue box, no bullet sentences, no program line, no disclosure; one short id with a copy button; the badges on one line with their meaning as tooltips', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock);
      await page.goto(`/#/market/${ID}`);
      const header = page.getByTestId('token-header');
      await expect(page.getByTestId('order-book')).toBeVisible();
      await expect(header.locator('.banner')).toHaveCount(0);
      for (const gone of ['token-open-caution', 'open-token-decimals', 'token-program-note', 'token-more', 'token-fact-text-genesis_unverified', 'token-fact-text-public-mint']) await expect(header.getByTestId(gone)).toHaveCount(0);
      await expect(header.locator('details')).toHaveCount(0);
      await expect(header).not.toContainText(/Its creation was not checked|lets anyone create more|Not in the token registry|Token program:/);
      // the identity block, then the pair text, then the swap button right after it
      const [icon, fp, pair, flip] = await Promise.all([`token-icon-${ID}`, `fingerprint-${ID}`, 'market-pair', 'market-flip'].map((id) => header.getByTestId(id).boundingBox()));
      expect(icon!.x).toBeLessThan(fp!.x);
      if (size.width >= 600) {
        expect(fp!.x + fp!.width).toBeLessThan(pair!.x);
        expect(pair!.x + pair!.width).toBeLessThanOrEqual(flip!.x + 1);
      }
      // the fingerprint is 60 % of the icon and centred on it
      expect(Math.abs(fp!.height / icon!.height - 0.6)).toBeLessThan(0.03);
      expect(Math.abs(fp!.y + fp!.height / 2 - (icon!.y + icon!.height / 2))).toBeLessThan(1.5);
      // the covenant id is said once, short, with a copy button (the title for screen readers is not drawn)
      await expect(header.getByTestId('token-short-id')).toHaveText(SHORT);
      await expect(header.getByTestId('token-short-id-copy')).toBeVisible();
      await expect(header.getByTestId('token-meta').getByText(SHORT)).toHaveCount(1);
      // the facts are badges on one line; the sentence of each is its tooltip
      const genesis = header.getByTestId('token-fact-genesis_unverified');
      const mint = header.getByTestId('token-fact-public-mint');
      await expect(genesis).toHaveText(/^Genesis not verified(: history not available)?$/);
      await expect(mint).toHaveText('Public mint');
      await expect(genesis).toHaveAttribute('title', /not checked against its token program/);
      await expect(mint).toHaveAttribute('title', /anyone create more/);
      const [g, m] = [await genesis.boundingBox(), await mint.boundingBox()];
      // one line on a desktop; on a phone "Genesis not verified" may carry its reason (KOB's own check could not be done) and the badges wrap
      if (size.width >= 600) expect(Math.abs(g!.y - m!.y)).toBeLessThan(4);
      // the program (with its review) and the decimals are the tooltip of the family chip
      const chip = header.getByTestId('badge-family');
      await expect(chip).toHaveAttribute('title', /Token program: KCC20PublicMint \(reviewed\), as pinned in this build\./);
      await expect(chip).toHaveAttribute('title', /whole tokens of 8 decimals/);
    });

    test('the trading content starts right below the header', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock);
      await page.goto(`/#/market/${ID}`);
      await expect(page.getByTestId('order-book')).toBeVisible();
      const header = (await page.getByTestId('token-header').boundingBox())!;
      const stats = (await page.getByTestId('market-stats').boundingBox())!;
      // the header is the identity block, the pair selector and two short lines (the preamble was about 440 px on a phone, 270 on a desktop; on a phone the genesis badge may carry its reason on a second line)
      expect(header.height).toBeLessThan(size.width < 600 ? 330 : 170);
      expect(stats.y - (header.y + header.height)).toBeLessThan(40);
    });

    test('a real warning is one line in the page', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock, { powers: ['freeze'] });
      await page.goto(`/#/market/${ID}`);
      await expect(page.getByTestId('token-header').getByTestId('powers-warning')).toBeVisible();
    });
  });
}
