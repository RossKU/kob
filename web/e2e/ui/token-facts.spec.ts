// Facts about a token (matcher.md 1.2a: "Wallets MUST show the warnings wherever the token is shown"). A normal token shows NO status: "Genesis not verified" (the
// check could not run) is no chip, its sentence is in the token's details. What lets someone create more of the token or freeze / seize balances (a live mint
// authority, a mint authority, freeze, seize, a failed genesis check) is ONE "High risk" chip with its reason opened on a tap (a touch screen has no tooltip);
// the other facts (public mint, blacklist) stay neutral chips with their sentence as the tooltip. Facts, not a rating.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { connectWallet, fund, listExkcc } from '../helpers/env';

const GENESIS = { txid: 'ab'.repeat(32), daa_score: 10, outputs: [1, 2], supply: 1000, minter_outputs: [], live_minters: [], checked_at_daa: 20, source: 'test' };

/** The example registry with EXKCC listed; `verifiedGenesis`: its genesis record says clean (the check `kob registry verify-genesis` makes). */
async function listWith(page: Page, verifiedGenesis: boolean): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  for (const tk of reg.tokens) {
    if (tk.ticker !== 'EXKCC') continue;
    tk.verified = true;
    if (verifiedGenesis) Object.assign(tk, { genesis_verified: true, genesis: GENESIS });
  }
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

/** The indexer says what the executor says of a token whose registry entry has a live mint authority and whose program can mint and blacklist. */
async function indexerFacts(page: Page): Promise<void> {
  await page.route(/\/v1\/tokens(\?.*)?$/, async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    for (const t of body.tokens) Object.assign(t, { powers: ['public-mint', 'blacklist'], warnings: ['genesis_unverified', 'live_mint_authority'] });
    await route.fulfill({ response: res, json: body });
  });
}

test.use({ autoOpen: false });

test.describe('token facts', () => {
  test('a token whose genesis is not verified is a normal token: no chip in the header, none at the ticket, its sentence in the details', async ({ appPage: page, mock }) => {
    await fund(mock);
    await listExkcc(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await connectWallet(page);
    const header = page.getByTestId('token-header');
    await expect(page.getByTestId('token-page')).toBeVisible();
    await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveCount(0);
    await expect(header.getByTestId('badge-high-risk')).toHaveCount(0);
    await expect(header).not.toContainText('Genesis not verified');
    // the ticket's facts panel has nothing to say either
    await expect(page.getByTestId('ticket-token-facts')).toHaveCount(0);
    // the sentence is reachable: the token's details open on a tap
    const details = page.getByTestId('token-status-details');
    await details.getByRole('button').first().click();
    await expect(details.getByTestId('status-fact-genesis_unverified')).toContainText(/Genesis not verified.*not checked against its token program/);
    await expect(details.getByTestId('status-risk-live_mint_authority')).toHaveCount(0);
  });

  test('the warnings and powers the indexer reports: a live mint authority is the one High risk chip (its reason on a tap); public mint and blacklist are no chip, they are in the details', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    await indexerFacts(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    const header = page.getByTestId('token-header');
    // by default nothing but the High risk chip: no fact chip at all ("Genesis not verified", public mint, blacklist are statuses)
    await expect(header.locator('[data-testid^="token-fact-"]')).toHaveCount(0);
    await expect(header.getByTestId('badge-high-risk')).toHaveAttribute('data-reasons', 'live_mint_authority');
    await expect(header.getByTestId('high-risk-toggle')).toHaveClass(/badge-warn/);
    await expect(page.getByTestId('high-risk-reasons')).toHaveCount(0);
    await header.getByTestId('high-risk-toggle').click();
    await expect(page.getByTestId('high-risk-reasons')).toContainText('more of this token can be created');
    await header.getByTestId('high-risk-toggle').click();
    await expect(page.getByTestId('high-risk-reasons')).toHaveCount(0);
    // the other sentences are in the details, behind one tap
    const details = page.getByTestId('token-status-details');
    await details.getByRole('button').first().click();
    await expect(details.getByTestId('status-fact-public-mint')).toContainText(/anyone create more/);
    await expect(details.getByTestId('status-fact-blacklist')).toContainText(/block addresses/);
    await expect(details.getByTestId('status-risk-live_mint_authority')).toContainText(/more of this token can be created/);
    // the market list: the High risk chip on the row, small and on one line with the covenant id; every fact sentence is in the legend above the list, behind one tap
    await page.goto('/#/market');
    const row = page.getByTestId(`token-row-${id}`);
    await expect(row.getByTestId('badge-high-risk')).toBeVisible();
    await expect(row.getByTestId('token-fact-live_mint_authority')).toHaveCount(0);
    await expect(row.locator('[data-testid^="token-fact-"]')).toHaveCount(0);
    // a tap on the chip opens the reason (not cut by the row), a second tap closes it
    await row.getByTestId('high-risk-toggle').click();
    await expect(page.getByTestId('high-risk-reasons')).toBeVisible();
    await expect(page.getByTestId('high-risk-reasons')).toContainText('more of this token can be created');
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('high-risk-reasons')).toHaveCount(0);
    await expect(row.locator('summary, [data-testid^="token-fact-text-"]')).toHaveCount(0);
    // the legend is collapsed behind one info control; the attribution stays as a short label
    await expect(page.getByTestId('legend-attribution')).toHaveText('names via kascov');
    await expect(page.getByTestId('legend-body')).toHaveCount(0);
    await page.getByTestId('legend-toggle').click();
    await expect(page.getByTestId('legend-fact-public-mint')).toBeHidden();
    await page.getByTestId('legend-badges').locator('summary').click();
    await expect(page.getByTestId('legend-fact-public-mint')).toContainText('anyone create more');
    // the id is the second line of the row's text and the badges the third, on their own line (never wrapped)
    await expect(row.locator('.ml-line2').filter({ has: page.getByTestId(`token-hash-${id}`) })).toHaveCount(1);
    await expect(row.locator('.ml-line3').filter({ has: page.getByTestId('badge-high-risk') })).toHaveCount(1);
  });

  test('a token whose registry entry verified its genesis (no live mint authority) has no fact of its own and no status', async ({ appPage: page, mock }) => {
    await listWith(page, true);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('token-header')).toBeVisible();
    await expect(page.getByTestId('token-title')).toBeVisible();
    await expect(page.getByTestId('token-facts')).toHaveCount(0);
    await expect(page.getByTestId('token-fact-genesis_unverified')).toHaveCount(0);
  });

  test('on a phone the header chips stay inside the screen', async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await listExkcc(page);
    await indexerFacts(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('token-header').getByTestId('badge-high-risk')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });

  test('a list row with every fact on a 375 px screen: the badges wrap inside the table, nothing scrolls sideways', async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width: 375, height: 812 });
    await mock.seed({ open_token: true });
    await listExkcc(page);
    await page.route(/\/v1\/tokens(\?.*)?$/, async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      for (const t of body.tokens) Object.assign(t, { powers: ['public-mint', 'blacklist', 'mint-authority'], warnings: ['genesis_unverified', 'live_mint_authority'] });
      await route.fulfill({ response: res, json: body });
    });
    await page.goto('/#/market');
    await page.getByTestId('token-list-nameless').click();
    await expect(page.getByTestId('token-table')).toBeVisible();
    const risk = page.getByTestId('token-table').getByTestId('badge-high-risk').first();
    await expect(risk).toBeVisible();
    await risk.getByTestId('high-risk-toggle').click();
    const panel = (await page.getByTestId('high-risk-reasons').boundingBox())!;
    expect(panel.x).toBeGreaterThanOrEqual(0);
    expect(panel.x + panel.width).toBeLessThanOrEqual(375);
    await expect(page.getByTestId('high-risk-reasons')).toContainText('more of this token can be created');
    await page.keyboard.press('Escape');
    const overflow = await page.evaluate(() => ({
      page: document.documentElement.scrollWidth - window.innerWidth,
      tables: Math.max(...[...document.querySelectorAll('.ml-table')].map((w) => w.scrollWidth - w.clientWidth)),
    }));
    expect(overflow.page).toBeLessThanOrEqual(0);
    expect(overflow.tables).toBeLessThanOrEqual(0);
  });
});
