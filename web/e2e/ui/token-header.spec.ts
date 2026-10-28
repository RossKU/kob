// The token page header is compact so the trading content comes first: the identity block and the pair selector, one line (short id with a copy button,
// name, where its facts come from, Live), one line of fact badges, and a collapsed "Token details" that holds the sentences, the program, the decimals
// and the full covenant id. A real warning is never in the disclosure.
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';

const ID = '3165ab67'.padEnd(64, '9');
const SHORT = '3165ab67…99999999';

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

    test('no blue box and no duplicate id: one short id with a copy button, one line of badges, a collapsed "Token details"', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock);
      await page.goto(`/#/market/${ID}`);
      const header = page.getByTestId('token-header');
      await expect(page.getByTestId('order-book')).toBeVisible();
      // the box of an open-list token is gone from the flow: its facts are in the closed disclosure
      await expect(header.locator('.banner')).toHaveCount(0);
      await expect(page.getByTestId('token-open-caution')).toBeHidden();
      await expect(page.getByTestId('token-program-note')).toBeHidden();
      // the covenant id is said once, short, with a copy button (the title for screen readers is not drawn)
      await expect(header.getByTestId('token-short-id')).toHaveText(SHORT);
      await expect(header.getByTestId('token-short-id-copy')).toBeVisible();
      await expect(header.getByTestId('token-meta').getByText(SHORT)).toHaveCount(1);
      // the facts are badges on one line; their sentences wait in the details
      const genesis = header.getByTestId('token-fact-genesis_unverified');
      const mint = header.getByTestId('token-fact-public-mint');
      await expect(genesis).toHaveText('Genesis not verified');
      await expect(mint).toHaveText('Public mint');
      const [g, m] = [await genesis.boundingBox(), await mint.boundingBox()];
      expect(Math.abs(g!.y - m!.y)).toBeLessThan(4);
      await expect(header.getByTestId('token-fact-text-genesis_unverified')).toBeHidden();
      // the disclosure is closed, then opens with everything the box and the bullets said
      const more = header.getByTestId('token-more');
      await expect(more).not.toHaveAttribute('open', '');
      await more.locator('summary').click();
      await expect(header.getByTestId('token-fact-text-genesis_unverified')).toContainText('not checked against its token program');
      await expect(header.getByTestId('token-fact-text-public-mint')).toContainText('anyone create more');
      await expect(header.getByTestId('token-program')).toContainText('KCC20PublicMint (reviewed)');
      await expect(header.getByTestId('open-token-decimals')).toContainText('amounts are whole tokens of 8 decimals');
      await expect(header.getByTestId('open-token-decimals')).toContainText('the price step is 1 sompi');
      await expect(header.getByTestId('token-more-id')).toContainText(ID);
    });

    test('the trading content starts right below the header', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock);
      await page.goto(`/#/market/${ID}`);
      await expect(page.getByTestId('order-book')).toBeVisible();
      const header = (await page.getByTestId('token-header').boundingBox())!;
      const stats = (await page.getByTestId('market-stats').boundingBox())!;
      // the header is at most the identity block, the pair selector and two short lines (the tall preamble was about 440 px on a phone, 270 on a desktop)
      expect(header.height).toBeLessThan(size.width < 600 ? 280 : 190);
      expect(stats.y - (header.y + header.height)).toBeLessThan(40);
    });

    test('a real warning is one line in the page, never behind the disclosure', async ({ appPage: page, mock }) => {
      await seedOpenToken(page, mock, { powers: ['freeze'] });
      await page.goto(`/#/market/${ID}`);
      const warning = page.getByTestId('token-header').getByTestId('powers-warning');
      await expect(warning).toBeVisible();
      await expect(page.getByTestId('token-more')).not.toHaveAttribute('open', '');
    });
  });
}
