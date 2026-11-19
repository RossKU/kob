// A creator-hosted logo loads in a wallet view only for a token the user acquired (an order of
// theirs names it) or chose to show ("Show logo", kept per token in this browser).
// A token dusted to the address shows its letter and makes no request to the creator's host.
// Market views load logos as usual.
// Mock stack, kascov mocked (e2e/helpers/kascov.ts).
import { expect, test } from '../fixtures';
import { connectWallet, TOKEN } from '../helpers/env';
import { mockKascov, PNG } from '../helpers/kascov';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

const DUST = 'd5'.repeat(32);
const OWN_URL = 'https://assets.example/own.png';
const DUST_URL = 'https://assets.example/dust.png';

test('wallet views: an acquired token loads its creator-hosted logo, a dusted one waits for "Show logo"', async ({ appPage: page, mock }) => {
  const own = (await mock.token()).covenant_id;
  await seedPortfolio(mock);
  await mock.seed({ tokens: [{ ticker: 'DUST', covenant_id: DUST, program: 'KCC20Ref', template_id: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null, order_scale: 100_000_000 }] });
  await mock.giveTokens('alice', 5n * TOKEN, { token: DUST });
  const log = await mockKascov(page, {
    tokens: [
      { covenant_id: own, claimed_ticker: 'OWN', claimed_image: OWN_URL },
      { covenant_id: DUST, claimed_ticker: 'DUST', claimed_image: DUST_URL },
    ],
    hosts: { [OWN_URL]: PNG, [DUST_URL]: PNG },
  });
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  await page.getByTestId('orders-tab-balances').click();

  const ownIcon = page.getByTestId(`token-icon-${own}`).first();
  await expect(ownIcon).toHaveAttribute('data-icon', 'creator-image');
  const dust = page.getByTestId(`token-icon-${DUST}`).first();
  await expect(dust).toHaveAttribute('data-icon', 'withheld');
  await expect(dust).toHaveAttribute('role', 'button');
  await page.waitForTimeout(500);
  expect(log.requests).toContain(OWN_URL);
  expect(log.requests).not.toContain(DUST_URL);

  // One click shows it and remembers the choice for the token.
  await dust.click();
  await expect(dust).toHaveAttribute('data-icon', 'creator-image');
  await expect.poll(() => log.requests.includes(DUST_URL)).toBe(true);
  expect(await page.evaluate(() => localStorage.getItem('kob.logo.shown'))).toContain(DUST);
  await page.reload();
  await connectWallet(page, 'kasware');
  await page.getByTestId('orders-tab-balances').click();
  await expect(page.getByTestId(`token-icon-${DUST}`).first()).toHaveAttribute('data-icon', 'creator-image');
});

test('market views are not gated', async ({ appPage: page, mock }) => {
  await mock.seed({ tokens: [{ ticker: 'DUST', covenant_id: DUST, program: 'KCC20Ref', template_id: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null, order_scale: 100_000_000 }] });
  const log = await mockKascov(page, { tokens: [{ covenant_id: DUST, claimed_ticker: 'DUST', claimed_image: DUST_URL }], hosts: { [DUST_URL]: PNG } });
  await page.goto('/#/market');
  const icon = page.getByTestId(`token-icon-${DUST}`);
  await icon.scrollIntoViewIfNeeded();
  await expect(icon).toHaveAttribute('data-icon', 'creator-image');
  expect(log.requests).toContain(DUST_URL);
});
