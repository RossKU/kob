// The connected wallet's own tokens in the market list. The indexer lists only registry tokens and
// tokens that have had an order placed, so a token just created, or one in a wallet with no order
// ever placed, had no row there and its holder could not find its market. Each such token now has a
// row in that wallet's own list, marked "Held", named by its verified KCC-23 declaration, with "—"
// for its figures; its market takes the first order. Nobody else's list changes. The list's filters
// are one menu, on a desktop and on a 390 px phone. Against the mock stack.
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { clickConnectOption, connectWallet, disconnectWallet, fund, listExkcc } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';
import { listFilterOn, openListFilter, toggleListFilter } from '../helpers/list-filter';
import { fillFields, pickType, placeOrder, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

const KAS = 100_000_000n;
const EXT = 'ee'.repeat(32);
const MINE = 'a9'.repeat(32); // KCC20Ref, held by alice, no order, no declaration
const BOBS = 'b8'.repeat(32); // KCC20Ref, held by bob only
const SHOTS = process.env.KOB_HELD_SHOTS ?? '';

const tokenRows = (page: Page) => page.locator('[data-testid^="token-row-"]:not([data-testid="token-row-skeleton"])');
const rowIds = (page: Page) => tokenRows(page).evaluateAll((els) => els.map((e) => e.getAttribute('data-testid')!.replace('token-row-', '')));

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  await page.waitForTimeout(800);
  await page.screenshot({ path: join(SHOTS, `${name}.png`) });
}

async function seedHoldings(mock: MockClient): Promise<string> {
  await mock.seed({
    tokens: [
      { ticker: '', covenant_id: MINE, program: 'KCC20Ref', extension_commitment: EXT, decimals: null, hidden: true },
      { ticker: '', covenant_id: BOBS, program: 'KCC20Ref', extension_commitment: EXT, decimals: null, hidden: true },
    ],
  });
  await mock.giveTokens('alice', 700_000n, { token: MINE });
  await mock.giveTokens('bob', 500_000n, { token: BOBS });
  await fund(mock, 'alice');
  return (await mock.token()).covenant_id as string;
}

/**
 * Creates a token in the app (KCC-23: a declaration, then the genesis) as alice, who holds all of
 * it; returns its covenant id.
 */
async function createToken(page: Page, mock: MockClient): Promise<string> {
  await mock.giveKas('alice', 500n * KAS);
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await page.getByTestId('issue-name').fill('Held Example Coin');
  await page.getByTestId('issue-ticker').fill('HELDX');
  await page.getByTestId('issue-decimals').fill('2');
  await page.getByTestId('issue-supply').fill('1000');
  await expect(page.getByTestId('issue-holder-0-amount')).toHaveValue('1000');
  await page.getByTestId('issue-review').click();
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
  return (await page.getByTestId('issue-result-tokenId').getAttribute('data-value'))!;
}

test('a token just created is in its holder\'s list, marked Held and named by its declaration; its market takes the first order', async ({ appPage: page, mock }) => {
  await listExkcc(page);
  const id = await createToken(page, mock);
  // the indexer lists no order of it, so its token list does not name it
  expect((await mock.tokens()).map((t) => t.covenant_id)).not.toContain(id);

  await page.goto('/#/market');
  const row = page.getByTestId(`token-row-${id}`);
  await expect(row).toBeVisible({ timeout: 20_000 });
  await expect(row).toHaveAttribute('data-source', 'held');
  await expect(row).toHaveAttribute('data-held', '1');
  await expect(page.getByTestId(`token-held-${id}`)).toHaveText('Held');
  // named by its verified declaration, as its page names it
  await expect(page.getByTestId(`token-link-${id}`)).toHaveText('HELDX');
  await expect(page.getByTestId(`token-name-${id}`)).toContainText('Held Example Coin');
  // no order is known: every figure reads "—"
  for (const f of ['last', 'change', 'volume', 'mcap', 'spread', 'depth']) await expect(page.getByTestId(`token-${f}-${id}`)).toHaveText('—');
  await shot(page, 'list-held-row-1280');

  // the Held filter keeps the wallet's tokens only
  await toggleListFilter(page, 'token-list-held');
  await expect(page.getByTestId('token-list-filter-count')).toHaveText('1');
  expect(await rowIds(page)).toEqual([id]);
  await toggleListFilter(page, 'token-list-held');
  await expect(page.getByTestId('token-list-filter-count')).toHaveCount(0);

  // its market opens from the row, and its ticket takes the first order: a sell of what the wallet
  // holds
  await page.getByTestId(`token-link-${id}`).click();
  await expect(page.getByTestId('token-page')).toBeVisible();
  await expect(page.getByTestId('token-not-found')).toHaveCount(0);
  await expect(page.getByTestId('order-ticket')).toBeVisible({ timeout: 30_000 });
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '10', price: '0.5' });
  await waitReviewable(page);
  const placed = await placeOrder(page, mock);
  expect(placed.submission.created).toHaveLength(1);
  const mine = await mock.ordersOf('alice');
  expect(mine.find((o: { token: string }) => o.token === id)).toMatchObject({ side: 1, status: 'open' }); // side 1: an ask
});

test('the rows are only in the holder\'s own view; a token without a declaration is nameless and follows "show nameless"', async ({ appPage: page, mock }) => {
  const exkcc = await seedHoldings(mock);
  await listExkcc(page);
  await page.goto('/#/market');
  await expect(page.getByTestId(`token-row-${exkcc}`)).toBeVisible();

  // without a wallet: no Held filter, no mark, and the token no list names is found by nothing, a
  // search by its id included
  await expect((await openListFilter(page)).getByTestId('token-list-held')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await expect(page.getByTestId(`token-held-${exkcc}`)).toHaveCount(0);
  await page.getByTestId('token-search').fill(MINE);
  await expect(page.getByTestId('token-list-empty')).toBeVisible();
  await page.getByTestId('token-search').fill('');

  // alice connects: the listed token she holds is marked; hers without a name waits for "show
  // nameless"; bob's is never there
  await connectWallet(page);
  await expect(page.getByTestId(`token-held-${exkcc}`)).toHaveText('Held', { timeout: 20_000 });
  await expect(page.getByTestId(`token-row-${MINE}`)).toHaveCount(0);
  await toggleListFilter(page, 'token-list-nameless');
  const row = page.getByTestId(`token-row-${MINE}`);
  await expect(row).toBeVisible();
  await expect(row).toHaveAttribute('data-source', 'held');
  await expect(page.getByTestId(`token-link-${MINE}`)).toHaveText('Unnamed token');
  await expect(page.getByTestId(`token-held-${MINE}`)).toHaveText('Held');
  await expect(page.getByTestId(`token-row-${BOBS}`)).toHaveCount(0);

  // Held and "show nameless" together: the wallet's two tokens
  await toggleListFilter(page, 'token-list-held');
  await expect(page.getByTestId('token-list-filter-count')).toHaveText('2');
  expect((await rowIds(page)).sort()).toEqual([exkcc, MINE].sort());
  // without "show nameless" the Held view keeps the named one
  await toggleListFilter(page, 'token-list-nameless');
  expect(await rowIds(page)).toEqual([exkcc]);
  await toggleListFilter(page, 'token-list-nameless');

  // the wallet leaves: its rows, its marks and its filter go with it
  await disconnectWallet(page);
  await expect(page.getByTestId(`token-row-${MINE}`)).toHaveCount(0);
  await expect(page.getByTestId(`token-held-${exkcc}`)).toHaveCount(0);
  await expect(page.getByTestId('token-list-filter-count')).toHaveText('1');
  expect(await listFilterOn(page, 'token-list-nameless')).toBe(true);
  await expect((await openListFilter(page)).getByTestId('token-list-held')).toHaveCount(0);
  await page.keyboard.press('Escape');
});

for (const vp of [
  { name: 'desktop 1280x800', tag: '1280', width: 1280, height: 800 },
  { name: 'phone 390 px', tag: '390', width: 390, height: 844 },
] as const) {
  test.describe(vp.name, () => {
    test.use({ viewport: { width: vp.width, height: vp.height } });

    test('the filter menu: every filter in one place, the number that are on, the keys, and no sideways scroll', async ({ appPage: page, mock }) => {
      const exkcc = await seedHoldings(mock);
      await listExkcc(page);
      await page.goto('/#/market');
      await connectWallet(page);
      await expect(page.getByTestId(`token-held-${exkcc}`)).toBeVisible({ timeout: 20_000 });
      const face = page.getByTestId('token-list-filter');
      await expect(face).toHaveText(/^Filter/);
      await expect(face).toHaveAttribute('aria-haspopup', 'menu');

      const panel = await openListFilter(page);
      await expect(face).toHaveAttribute('aria-expanded', 'true');
      const items = panel.getByRole('menuitemcheckbox');
      await expect(items).toHaveText(['Favorites', 'Held', 'Show tokens without a name (1)']);
      await expect(items.first()).toBeFocused();
      // the panel stands in the window, and the page does not scroll sideways
      const box = (await panel.boundingBox())!;
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.width).toBeLessThanOrEqual(vp.width);
      expect(await horizontalOverflow(page)).toBe(0);
      await shot(page, `filter-menu-${vp.tag}`);

      // the keys: the arrows move, Escape closes and gives the focus back
      await page.keyboard.press('ArrowDown');
      await expect(panel.getByTestId('token-list-held')).toBeFocused();
      await page.keyboard.press('Escape');
      await expect(panel).toHaveCount(0);
      await expect(face).toBeFocused();

      // two filters on: the button says so, the entries are checked, Clear switches them off
      await toggleListFilter(page, 'token-list-held');
      await toggleListFilter(page, 'token-list-favorites');
      await expect(page.getByTestId('token-list-filter-count')).toHaveText('2');
      await expect(face).toHaveAttribute('aria-label', 'Filter the list: 2 filters on');
      const again = await openListFilter(page);
      await expect(again.getByTestId('token-list-held')).toHaveAttribute('aria-checked', 'true');
      await expect(again.getByTestId('token-list-favorites')).toHaveAttribute('aria-checked', 'true');
      await expect(again.getByTestId('token-list-nameless')).toHaveAttribute('aria-checked', 'false');
      await again.getByTestId('token-list-filter-clear').click();
      await expect(page.getByTestId('token-list-filter-count')).toHaveCount(0);
      await expect(page.getByTestId(`token-row-${exkcc}`)).toBeVisible();
      expect(await horizontalOverflow(page)).toBe(0);
    });
  });
}
