// Facts about a token (matcher.md 1.2a: "Wallets MUST show the warnings wherever the token is shown"): the indexer's `genesis_unverified` and
// `live_mint_authority`, and the program powers that let someone create more of the token or block addresses, as neutral badges with a plain
// sentence in the token header, next to the order button, in the market list and on a pair. The sentence is printed or opens on a tap (a touch
// screen has no tooltip). Facts, not a rating: a token whose genesis the registry verified has none of its own.
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
  test('a token whose genesis is not verified: a neutral badge and its sentence in the header, a badge next to the order button that opens on a tap', async ({ appPage: page, mock }) => {
    await fund(mock);
    await listExkcc(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await connectWallet(page);
    const header = page.getByTestId('token-header');
    await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveText(/^Genesis not verified(: history not available)?$/);
    // the badge is on the header's one line of badges; its sentence is its tooltip (the ticket's badge opens it on a tap)
    await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveAttribute('title', /not checked against its token program/);
    await expect(header.getByTestId('token-fact-text-genesis_unverified')).toHaveCount(0);
    await expect(header.getByTestId('token-fact-genesis_unverified')).toHaveClass(/badge-neutral/);
    // the ticket: the badge is in the text, the sentence opens on a tap
    const ticket = page.getByTestId('ticket-token-facts');
    await expect(ticket.getByTestId('token-fact-genesis_unverified')).toBeVisible();
    await expect(ticket.getByTestId('token-fact-text-genesis_unverified')).toBeHidden();
    await ticket.locator('summary').click();
    await expect(ticket.getByTestId('token-fact-text-genesis_unverified')).toBeVisible();
    // it sits above the order button, in the ticket
    const box = async (tid: string) => (await page.getByTestId(tid).boundingBox())!;
    expect((await box('ticket-token-facts')).y).toBeLessThan((await box('order-place')).y);
    // no other fact for this token
    await expect(header.getByTestId('token-fact-live_mint_authority')).toHaveCount(0);
  });

  test('the warnings and powers the indexer reports are all shown: live mint authority, public mint, blacklist', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    await indexerFacts(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    const header = page.getByTestId('token-header');
    for (const f of ['genesis_unverified', 'live_mint_authority', 'public-mint', 'blacklist']) await expect(header.getByTestId(`token-fact-${f}`)).toBeVisible();
    // the sentences are the badges' tooltips
    await expect(header.getByTestId('token-fact-live_mint_authority')).toHaveAttribute('title', /more of this token can be created/);
    await expect(header.getByTestId('token-fact-public-mint')).toHaveAttribute('title', /anyone create more/);
    await expect(header.getByTestId('token-fact-blacklist')).toHaveAttribute('title', /block addresses/);
    // the market list: the same badges on the row, small and on one line with the covenant id; the sentence is the badge's tooltip and every sentence is
    // in the legend above the list, behind one tap (no "what is this?" on a row)
    await page.goto('/#/market');
    const row = page.getByTestId(`token-row-${id}`);
    await expect(row.getByTestId('token-fact-live_mint_authority')).toBeVisible();
    await expect(row.getByTestId('token-fact-live_mint_authority')).toHaveAttribute('title', /more of this token can be created/);
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
    await expect(row.locator('.ml-line3').filter({ has: page.getByTestId('token-fact-live_mint_authority') })).toHaveCount(1);
  });

  test('a token whose registry entry verified its genesis (no live mint authority) has no fact of its own', async ({ appPage: page, mock }) => {
    await listWith(page, true);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('token-header')).toBeVisible();
    await expect(page.getByTestId('token-title')).toBeVisible();
    await expect(page.getByTestId('token-facts')).toHaveCount(0);
    await expect(page.getByTestId('token-fact-genesis_unverified')).toHaveCount(0);
  });

  test('on a phone the badges wrap inside the screen', async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await listExkcc(page);
    await indexerFacts(page);
    const id = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('token-header').getByTestId('token-fact-blacklist')).toBeVisible();
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
    await expect(page.getByTestId('token-table').getByTestId('token-fact-live_mint_authority').first()).toBeVisible();
    const overflow = await page.evaluate(() => ({
      page: document.documentElement.scrollWidth - window.innerWidth,
      tables: Math.max(...[...document.querySelectorAll('.ml-table')].map((w) => w.scrollWidth - w.clientWidth)),
    }));
    expect(overflow.page).toBeLessThanOrEqual(0);
    expect(overflow.tables).toBeLessThanOrEqual(0);
  });
});
