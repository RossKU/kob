// A token only kascov.io names (a "no orders" row of the market list) and the pages around it (the live check of 2026-10-28: such a row led to "Token not found:
// the wallet must hold it"). The page opens from what KOB can verify: the token program from the template hash this build pins, the extension commitment from
// token outputs the indexer names and the node confirms (whoever holds them), the decimals by the rules of the app (declaration, the program's default such as
// KaspaRocket's 3, else the first order names them); the creator's name and ticker are labels, never authority. Also: the tab titles, the full covenant id
// wrapping on a phone, and the saved copy kascov.io has none of being asked for once, not on every visit. Desktop and a 375 px phone, against the mock stack
// with a MOCKED kascov.io (e2e/helpers/kascov.ts).
import type { Page } from '@playwright/test';
import { expect, test, TEST_KEYS } from '../fixtures';
import { connectWallet, fund, listExkcc } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';
import { mockKascov, programHash, sha256 } from '../helpers/kascov';
import { fillFields, pickType, placeOrder, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

const EXT = 'ee'.repeat(32);
const REF_ID = 'a9'.repeat(32); // KCC20Ref, held by bob only
const ROCKET_ID = 'b8'.repeat(32); // KaspaRocket program: 3 decimals by its convention, held by alice
const GHOST_ID = 'c7'.repeat(32); // named by kascov.io, no output of it known to the indexer
const NOBODY_ID = 'd6'.repeat(32); // named by nothing
const SHORT = (id: string) => `${id.slice(0, 8)}…${id.slice(-8)}`;

const VIEWPORTS = [
  { name: 'desktop 1280x800', width: 1280, height: 800 },
  { name: 'phone 375 px', width: 375, height: 812 },
] as const;

const kascovTokens = [
  { covenant_id: REF_ID, claimed_ticker: 'KASC', claimed_name: 'Kasc Token', claimed_decimals: 2, program: programHash('KCC20Ref') },
  { covenant_id: ROCKET_ID, claimed_ticker: 'ROCK', claimed_name: 'Rocket Token', claimed_decimals: 3, program: programHash('KaspaRocketToken11458') },
  { covenant_id: GHOST_ID, claimed_ticker: 'GHST', claimed_name: 'Ghost Token', program: programHash('KCC20Ref') },
];

async function seed(mock: Parameters<typeof fund>[0]): Promise<void> {
  await mock.seed({
    tokens: [
      { ticker: '', covenant_id: REF_ID, program: 'KCC20Ref', extension_commitment: EXT, decimals: null, hidden: true },
      { ticker: '', covenant_id: ROCKET_ID, program: 'KaspaRocketToken11458', extension_commitment: EXT, decimals: null, hidden: true },
    ],
  });
  await mock.giveTokens('bob', 500_000n, { token: REF_ID });
  await mock.giveTokens('alice', 5_000_000n, { token: ROCKET_ID });
  await fund(mock, 'alice', { tokens: 0n });
}

async function openKascov(page: Page, route: string): Promise<{ requests: string[] }> {
  await listExkcc(page);
  const log = await mockKascov(page, { tokens: kascovTokens });
  await page.goto(`/#${route}`);
  return log;
}

for (const vp of VIEWPORTS) {
  test.describe(vp.name, () => {
    test.use({ viewport: { width: vp.width, height: vp.height } });

    test.beforeEach(async ({ mock }) => {
      await seed(mock);
    });

    test('a "no orders" row opens a usable page: header from the creator and KOB, an empty book, and a first buy without any wallet holding', async ({ appPage: page, mock }) => {
      await openKascov(page, '/market');
      const link = page.getByTestId(`token-link-${REF_ID}`);
      await expect(page.getByTestId(`token-row-${REF_ID}`)).toHaveAttribute('data-source', 'kascov');
      await expect(page.getByTestId(`token-orders-${REF_ID}`)).toHaveText('no orders');
      await link.click();

      // never the dead end
      await expect(page.getByTestId('token-page')).toBeVisible();
      await expect(page.getByTestId('token-not-found')).toHaveCount(0);

      // the header: the creator's name and ticker, labelled; KOB's own program and its review; the decimals rule; the explorer link
      await expect(page.getByTestId('token-title')).toHaveText(`KASC (${SHORT(REF_ID)})`);
      await expect(page.getByTestId('token-name')).toHaveText('Kasc Token');
      await expect(page.getByTestId(`kascov-source-${REF_ID}`)).toHaveText('name and ticker from kascov.io (declared by their creator)');
      // (no family chip yet: the token has no entry; the first order opens it)
      await expect(page.getByTestId('badge-family')).toHaveCount(0);
      await expect(page.getByTestId('kascov-link')).toHaveAttribute('href', `https://kascov.io/#/testnet-10/token/${REF_ID}`);
      await expect(page.getByTestId('kascov-link')).toHaveAttribute('rel', 'noopener noreferrer');
      // unverified facts are badges, never a refusal (non-blocking)
      await expect(page.getByTestId('token-fact-genesis_unverified')).toBeVisible();

      // the units and the tab use the creator's ticker where KOB has no name of its own
      await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KASC/KAS');
      await expect(page.getByTestId('chart-unit')).toHaveText('KAS per KASC');
      expect(await page.title()).toBe('KASC - KOB');

      // an empty book
      await expect(page.getByTestId('book-empty')).toHaveText('No orders yet');

      // the token declares no decimals and has no program default: the first order names them (kascov.io's claim is shown as a claim, never prefilled)
      await expect(page.getByTestId('first-order-claim')).toContainText('kascov.io shows 2 decimals');
      await expect(page.getByTestId('first-order-decimals')).toHaveValue('');
      await page.getByTestId('first-order-decimals').fill('2');
      await page.getByTestId('first-order-use').click();
      await expect(page.getByTestId('order-ticket')).toBeVisible();
      await expect(page.getByTestId('badge-family')).toHaveAttribute('title', /2 decimals you entered/);
      expect(await horizontalOverflow(page)).toBe(0);

      // a buy: no wallet holding of the token is needed (the wallet holds none)
      await connectWallet(page);
      await pickType(page, 'limit', 'buy');
      await fillFields(page, { amount: '10', price: '0.5' });
      await waitReviewable(page);
      const placed = await placeOrder(page, mock);
      expect(placed.submission.created).toHaveLength(1);
      const mine = await mock.ordersOf('alice');
      expect(mine).toHaveLength(1);
      expect(mine[0]).toMatchObject({ token: REF_ID, side: 2, status: 'open' }); // side 2: a bid
    });

    test('a token of a program with default decimals (KaspaRocket: 3) is tradable at once; the wallet that holds it can also sell', async ({ appPage: page, mock }) => {
      await openKascov(page, '/market');
      await page.getByTestId(`token-link-${ROCKET_ID}`).click();
      await expect(page.getByTestId('token-title')).toHaveText(`ROCK (${SHORT(ROCKET_ID)})`);
      await expect(page.getByTestId('badge-family')).toHaveAttribute('title', /Token program: KaspaRocketToken11458 \(reviewed\), as pinned in this build\./);
      // no decimals asked for: the program's convention
      await expect(page.getByTestId('first-order')).toHaveCount(0);
      await expect(page.getByTestId('badge-family')).toHaveAttribute('title', /3 decimals, the KaspaRocket convention/);
      await expect(page.getByTestId('book-empty')).toHaveText('No orders yet');
      expect(await page.title()).toBe('ROCK - KOB');

      await connectWallet(page);
      await expect(page.getByTestId('order-ticket')).toBeVisible();
      // alice holds 5000 ROCK (5,000,000 base units at 3 decimals): a sell
      await pickType(page, 'limit', 'sell');
      await fillFields(page, { amount: '100', price: '0.02' });
      await waitReviewable(page);
      const placed = await placeOrder(page, mock);
      expect(placed.submission.created).toHaveLength(1);
      const mine = await mock.ordersOf(TEST_KEYS.alice.name);
      expect(mine[0]).toMatchObject({ token: ROCKET_ID, side: 1, status: 'open' }); // side 1: an ask
      expect(await horizontalOverflow(page)).toBe(0);
    });

    test('what cannot be verified is said, on the page, with the creator\'s header: never "Token not found"', async ({ appPage: page }) => {
      await openKascov(page, `/market/${GHOST_ID}`);
      await expect(page.getByTestId('token-page')).toBeVisible();
      await expect(page.getByTestId('token-not-found')).toHaveCount(0);
      await expect(page.getByTestId('token-title')).toHaveText(`GHST (${SHORT(GHOST_ID)})`);
      // the one fact it lacks, exactly
      const why = page.getByTestId('ticket-untradable');
      await expect(why).toContainText('extension commitment');
      await expect(why).toContainText('the indexer knows no token output of it for the node to confirm');
      await expect(page.getByTestId('order-ticket')).toHaveCount(0);
      await expect(page.getByTestId('book-empty')).toHaveText('No orders yet');
      expect(await page.title()).toBe('GHST - KOB');
      expect(await horizontalOverflow(page)).toBe(0);

    });

    test('a covenant id nothing names: the explanation, the full id wraps and nothing overflows', async ({ appPage: page }) => {
      await openKascov(page, `/market/${NOBODY_ID}`);
      const nf = page.getByTestId('token-not-found');
      await expect(nf).toBeVisible();
      await expect(nf).toContainText(NOBODY_ID);
      await expect(nf).toContainText('KOB found no token with the covenant id');
      await expect(page.getByTestId('token-public-reason')).toContainText('cannot be verified');
      expect(await page.title()).toBe('Token not found - KOB');
      expect(await horizontalOverflow(page)).toBe(0);
      // the same for every page that prints a full id or a long typed one
      for (const route of [`/market/${'f'.repeat(300)}`, `/market/${NOBODY_ID}/${GHOST_ID}`, `/market/${NOBODY_ID}/kas`, '/orders', '/create', '/settings']) {
        await page.goto(`/#${route}`);
        await page.waitForTimeout(300);
        expect(await horizontalOverflow(page), `overflow on #${route.slice(0, 40)}`).toBe(0);
      }
    });

    test('the tab names the page: the page, the token\'s ticker, the pair', async ({ appPage: page, mock }) => {
      await listExkcc(page);
      await mockKascov(page, { tokens: [] });
      const exkcc = (await mock.token()).covenant_id;
      const titles: Record<string, string> = {
        '/market': 'Market - KOB',
        '/orders': 'My orders - KOB',
        '/create': 'Create token - KOB',
        '/settings': 'Settings - KOB',
        [`/market/${exkcc}`]: 'EXKCC - KOB',
      };
      for (const [route, title] of Object.entries(titles)) {
        await page.goto(`/#${route}`);
        await expect.poll(() => page.title(), { message: route }).toBe(title);
      }
    });
  });
}

test('a saved copy kascov.io has none of is asked for once; the next visit does not ask again, and the icon is a letter', async ({ appPage: page }) => {
  await listExkcc(page);
  const log = await mockKascov(page, { tokens: [{ covenant_id: REF_ID, claimed_ticker: 'KASC', claimed_image_hash: sha256(Buffer.from('a picture nobody saved')), program: programHash('KCC20Ref') }] });
  const asked = () => log.requests.filter((u) => u.includes(`/img/testnet-10/${REF_ID}`)).length;
  await page.goto('/#/market');
  await expect(page.getByTestId(`token-row-${REF_ID}`)).toHaveAttribute('data-source', 'kascov');
  await page.getByTestId(`token-icon-${REF_ID}`).scrollIntoViewIfNeeded();
  await expect.poll(asked).toBe(1);
  await expect(page.getByTestId(`token-icon-${REF_ID}`)).toHaveAttribute('data-icon', 'letter');
  // a new page load of the same browser: the 404 is remembered, no request goes out
  await page.reload();
  await expect(page.getByTestId(`token-row-${REF_ID}`)).toHaveAttribute('data-source', 'kascov');
  await page.getByTestId(`token-icon-${REF_ID}`).scrollIntoViewIfNeeded();
  await page.waitForTimeout(1500);
  expect(asked()).toBe(1);
  await expect(page.getByTestId(`token-icon-${REF_ID}`)).toHaveAttribute('data-icon', 'letter');
});
