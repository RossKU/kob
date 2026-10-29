// The market list completed from kascov.io (src/data/kascov.ts), against the mock stack with a MOCKED kascov (e2e/helpers/kascov.ts: the real service is
// never reached): tokens of a program this build trades, named by their creators' claims and sorted after the KOB rows as "no orders"; icons only for
// bytes that hash to the committed hash; the notes where kascov disagrees with KOB; the Settings toggle; kascov down, slow-shaped or turned off by the
// deployment. Desktop and a 375 px phone (no horizontal overflow). The app under test is the built one, so its Content-Security-Policy is in force:
// the pictures are shown from blob: URLs, never from a third-party URL.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { mockKascov, OTHER_PNG, PNG, programHash, sha256, type MockKascovToken } from '../helpers/kascov';

test.use({ autoOpen: false });

const ID = (n: number) => n.toString(16).padStart(2, '0').repeat(32);
const SAVED = ID(0xa1); // a saved copy that hashes to the committed hash
const MISMATCH = ID(0xa2); // the saved copy is another picture than the one committed (its hash is of other bytes than OTHER_PNG)
const NO_CORS = ID(0xa3); // no saved copy; the creator's own host sends no CORS header
const NO_IMAGE = ID(0xa4); // no image at all
const UNPINNED = ID(0xa5); // a program this build does not pin
const PUBLIC_MINT = ID(0xa6); // KCC20PublicMint
const INVALID = ID(0xa7); // kascov calls it invalid: its status never decides the listing (only a `verified` one shows the chip)
const MAINNET_ONLY = ID(0xa8); // pinned but not enabled on... (KForge: testnet-10 only, so this one IS listed on testnet-10)
/** the second token the mock stack lists (its registry), with KOB's own ticker */
const KOB_OTHER = ID(0xe6);
const CREATOR_URL = 'https://storage.googleapis.com/kaspa-rocket-prod-uploads/no-cors.png';

const ROCKET = programHash('KaspaRocketToken11458');
const MINT = programHash('KCC20PublicMint');

const tokens = (exkcc: string): MockKascovToken[] => [
  { covenant_id: SAVED, claimed_ticker: 'SAVED', claimed_name: 'Saved Icon Token', claimed_decimals: 3, claimed_image_hash: sha256(PNG), program: ROCKET },
  { covenant_id: MISMATCH, claimed_ticker: 'MISS', claimed_name: 'Wrong Picture', claimed_image_hash: sha256(Buffer.from('the picture its creator committed to')), program: ROCKET },
  { covenant_id: NO_CORS, claimed_ticker: 'NOCORS', claimed_image: CREATOR_URL, claimed_image_hash: sha256(Buffer.from('a picture only its own host has')), program: MINT },
  { covenant_id: NO_IMAGE, claimed_ticker: 'PLAIN', program: MINT },
  { covenant_id: UNPINNED, claimed_ticker: 'GHOST', claimed_name: 'Not A Pinned Program', program: 'ee'.repeat(32) },
  { covenant_id: PUBLIC_MINT, claimed_ticker: 'MINTY', claimed_name: 'Public Mint Token', program: MINT },
  { covenant_id: INVALID, claimed_ticker: 'BAD', claimed_name: 'Called Invalid', status: 'invalid', program: MINT },
  { covenant_id: MAINNET_ONLY, claimed_ticker: 'FORGE', claimed_name: 'Testnet Only Program', program: programHash('KForgeToken1785') },
  // a token KOB knows: KOB's ticker and decimals stand, the disagreement is a note
  { covenant_id: exkcc, claimed_ticker: 'FAKE', claimed_name: 'Not The Name', claimed_decimals: 2 },
];

const opts = (exkcc: string) => ({ tokens: tokens(exkcc), images: { [SAVED]: PNG, [MISMATCH]: OTHER_PNG }, hosts: { [CREATOR_URL]: PNG } });

const overflow = (page: Page) => page.evaluate(() => Math.max(0, document.documentElement.scrollWidth - window.innerWidth));
const shownIds = (page: Page) =>
  page.locator('[data-testid="token-table"] tbody tr').evaluateAll((trs) => trs.map((tr) => (tr.getAttribute('data-testid') ?? '').replace('token-row-', '')));

test.describe('with kascov.io', () => {
  test('desktop: names, "no orders" rows after the KOB rows, icons only for bytes that match their hash, notes where kascov disagrees', async ({ appPage: page, mock, pageErrors }) => {
    const exkcc = (await mock.token()).covenant_id;
    const log = await mockKascov(page, opts(exkcc));
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto('/#/market');

    // a token of a pinned program that no KOB source names: a "no orders" row named by its creator's claim, with the source said
    const row = page.getByTestId(`token-row-${SAVED}`);
    await expect(row).toHaveAttribute('data-source', 'kascov');
    await expect(page.getByTestId(`token-link-${SAVED}`)).toHaveText('SAVED');
    await expect(page.getByTestId(`token-hash-${SAVED}`)).toHaveText('a1a1a1a1a1a1…a1a1a1a1a1a1');
    await expect(page.getByTestId(`token-name-${SAVED}`)).toContainText('Saved Icon Token');
    // the source is a small marker; the sentence is its tooltip and the legend above the list says it once
    await expect(page.getByTestId(`kascov-source-${SAVED}`)).toHaveText('via kascov');
    await expect(page.getByTestId(`kascov-source-${SAVED}`)).toHaveAttribute('title', 'name and ticker from kascov.io (declared by their creator)');
    await page.getByTestId('legend-toggle').click();
    await expect(page.getByTestId('legend-source')).toContainText('name and ticker from kascov.io (declared by their creator)');
    await expect(page.getByTestId('token-table')).not.toContainText('declared by their creator');
    await expect(page.getByTestId(`token-orders-${SAVED}`)).toHaveText('no orders');
    // nothing of its figures is invented
    for (const f of ['last', 'change', 'volume', 'spread']) await expect(page.getByTestId(`token-${f}-${SAVED}`)).toHaveText('—');

    // the program decides: a pinned program on this network is listed (kascov's status does not decide it), an unpinned one is not
    for (const id of [SAVED, MISMATCH, NO_CORS, NO_IMAGE, PUBLIC_MINT, INVALID, MAINNET_ONLY]) await expect(page.getByTestId(`token-row-${id}`)).toHaveCount(1);
    await expect(page.getByTestId(`token-row-${UNPINNED}`)).toHaveCount(0);

    // KOB's rows come first and keep their ticker; the disagreement is a note, never a replacement
    const ids = await shownIds(page);
    expect(ids[0]).toBe(exkcc);
    expect(new Set(ids)).toEqual(new Set([exkcc, KOB_OTHER, SAVED, MISMATCH, NO_CORS, NO_IMAGE, PUBLIC_MINT, INVALID, MAINNET_ONLY]));
    await expect(page.getByTestId(`token-link-${exkcc}`)).toContainText('EXKCC');
    await expect(page.getByTestId(`kascov-ticker-differs-${exkcc}`)).toHaveText('(kascov.io says FAKE)');
    await expect(page.getByTestId(`kascov-decimals-differ-${exkcc}`)).toContainText('kascov.io says 2 decimals, KOB uses 8');
    await expect(page.getByTestId(`kascov-source-${exkcc}`)).toHaveCount(0);

    // icons: the saved copy hashes to the committed hash and is shown from a blob: URL it decodes from; every other case is a letter
    const icon = page.getByTestId(`token-icon-${SAVED}`);
    await icon.scrollIntoViewIfNeeded();
    await expect(icon).toHaveAttribute('data-icon', 'image');
    const img = icon.locator('img');
    expect(await img.getAttribute('src')).toMatch(/^blob:/);
    await expect.poll(() => img.evaluate((e: HTMLImageElement) => e.naturalWidth)).toBe(1);
    for (const id of [MISMATCH, NO_CORS, NO_IMAGE]) {
      const letter = page.getByTestId(`token-icon-${id}`);
      await letter.scrollIntoViewIfNeeded();
      await expect(letter).toHaveAttribute('data-icon', 'letter');
      await expect(letter.locator('img')).toHaveCount(0);
    }
    await expect(page.getByTestId(`token-icon-${MISMATCH}`)).toHaveText('M');
    await expect(page.getByTestId(`token-icon-${NO_CORS}`)).toHaveText('N');
    // the mismatching copy was fetched and refused; the creator's host (no CORS) was tried after kascov had no copy, and refused by the browser
    await expect.poll(() => log.requests.includes(`https://kascov.io/img/testnet-10/${MISMATCH}`)).toBe(true);
    await expect.poll(() => log.requests.includes(`https://kascov.io/img/testnet-10/${NO_CORS}`)).toBe(true);
    await expect.poll(() => log.requests.includes(CREATOR_URL)).toBe(true);
    // no <img> ever points at a third-party URL, and the policy was never violated
    expect(await page.locator('img[src^="http"]').count()).toBe(0);
    expect(pageErrors.filter((e) => /Content.Security.Policy/i.test(e))).toEqual([]);
    expect(await overflow(page)).toBe(0);

    // the default sort is by 24 h volume: the rows with no orders stay after the KOB row in both directions of another figure column too
    await page.getByTestId('sort-orders').first().click();
    expect((await shownIds(page))[0]).toBe(exkcc);
    await page.getByTestId('sort-orders').first().click();
    expect((await shownIds(page))[0]).toBe(exkcc);
    // search reaches the kascov names
    await page.getByTestId('token-search').fill('public mint');
    await expect(page.getByTestId(`token-row-${PUBLIC_MINT}`)).toHaveCount(1);
    await expect(page.getByTestId(`token-row-${SAVED}`)).toHaveCount(0);
  });

  test('a 375 px phone: no horizontal overflow, the row says "no orders" under its name', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    await mockKascov(page, opts(exkcc));
    await page.setViewportSize({ width: 375, height: 800 });
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${SAVED}`)).toBeVisible();
    await page.getByTestId(`token-icon-${SAVED}`).scrollIntoViewIfNeeded();
    await expect(page.getByTestId(`token-icon-${SAVED}`)).toHaveAttribute('data-icon', 'image');
    await expect(page.getByTestId(`token-no-orders-narrow-${SAVED}`)).toBeVisible();
    await expect(page.getByTestId(`token-orders-${SAVED}`)).toBeHidden();
    await expect(page.getByTestId(`kascov-source-${SAVED}`)).toBeVisible();
    expect(await overflow(page)).toBe(0);
    // the narrowest rows too: the note of a disagreement does not widen the page
    await expect(page.getByTestId(`kascov-ticker-differs-${exkcc}`)).toBeVisible();
    expect(await overflow(page)).toBe(0);
  });

  test('the token page names a token KOB has no ticker for from kascov, with its icon, and the facts stay KOB\'s', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    await mockKascov(page, opts(exkcc));
    await page.goto(`/#/market/${exkcc}`);
    // EXKCC has its own ticker: kascov's FAKE is a note on the header, never the title
    await expect(page.getByTestId('token-title')).toContainText('EXKCC');
    await expect(page.getByTestId(`kascov-ticker-differs-${exkcc}`)).toBeVisible();
    await expect(page.getByTestId(`token-icon-${exkcc}`)).toHaveAttribute('data-icon', 'letter');
  });
});

test.describe('refresh policy', () => {
  test('fetched when the list is shown, again only after 2 minutes, never while the tab is hidden, and not at all on another page', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    await page.addInitScript(() => {
      Object.defineProperty(document, 'hidden', { configurable: true, get: () => (window as unknown as { __hidden?: boolean }).__hidden === true });
    });
    await page.clock.install();
    const log = await mockKascov(page, opts(exkcc));
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${SAVED}`)).toBeVisible();
    expect(log.tokenRequests).toBe(1);
    await page.clock.fastForward(100_000);
    expect(log.tokenRequests).toBe(1);
    await page.clock.fastForward(30_000);
    await expect.poll(() => log.tokenRequests).toBe(2);
    // the same file again (generated_at_ms unchanged): the row is still there, nothing was lost
    await expect(page.getByTestId(`token-row-${SAVED}`)).toBeVisible();

    // the tab goes to the background: no timer fires a request
    await page.evaluate(() => {
      (window as unknown as { __hidden: boolean }).__hidden = true;
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await page.clock.fastForward(600_000);
    expect(log.tokenRequests).toBe(2);
    // back in the foreground with a stale list: fetched at once
    await page.evaluate(() => {
      (window as unknown as { __hidden: boolean }).__hidden = false;
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await expect.poll(() => log.tokenRequests).toBe(3);

    // another page does not poll
    await page.goto('/#/settings');
    await expect(page.getByTestId('settings-icons')).toBeVisible();
    await page.clock.fastForward(600_000);
    expect(log.tokenRequests).toBe(3);
  });
});

test.describe('the Settings toggle "Load token icons from third-party hosts"', () => {
  test('on by default; off: letters, no request to any image host, names stay; the choice survives a reload', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const log = await mockKascov(page, opts(exkcc));
    await page.goto('/#/settings');
    const toggle = page.getByTestId('settings-icons-toggle');
    await expect(toggle).toBeChecked();
    await expect(page.getByTestId('settings-icons-source')).toContainText('https://kascov.io/data');
    await toggle.uncheck();
    expect(await page.evaluate(() => localStorage.getItem('kob.icons'))).toBe('0');

    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${SAVED}`)).toBeVisible();
    await expect(page.getByTestId(`token-name-${SAVED}`)).toContainText('Saved Icon Token');
    await expect(page.getByTestId(`token-icon-${SAVED}`)).toHaveAttribute('data-icon', 'letter');
    // give a (wrongly) started fetch time to show itself
    await page.waitForTimeout(700);
    expect(log.requests.filter((u) => !u.endsWith('/tokens.json'))).toEqual([]);

    await page.reload();
    await page.goto('/#/settings');
    await expect(page.getByTestId('settings-icons-toggle')).not.toBeChecked();

    // on again: the picture comes
    await page.getByTestId('settings-icons-toggle').check();
    expect(await page.evaluate(() => localStorage.getItem('kob.icons'))).toBeNull();
    await page.goto('/#/market');
    await page.getByTestId(`token-icon-${SAVED}`).scrollIntoViewIfNeeded();
    await expect(page.getByTestId(`token-icon-${SAVED}`)).toHaveAttribute('data-icon', 'image');
  });
});

for (const mode of ['down', 'shape', 'http'] as const) {
  test.describe(`kascov.io ${{ down: 'unreachable', shape: 'answering a changed shape', http: 'answering 503' }[mode]}`, () => {
    test('the list is what it was: KOB rows only, a small hint, nothing thrown', async ({ appPage: page, mock }) => {
      const exkcc = (await mock.token()).covenant_id;
      const log = await mockKascov(page, { ...opts(exkcc), tokensMode: mode });
      await page.setViewportSize({ width: 375, height: 800 });
      await page.goto('/#/market');
      await expect(page.getByTestId(`token-row-${exkcc}`)).toHaveAttribute('data-state', 'ready');
      await expect(page.getByTestId('kascov-unavailable')).toHaveText(/unavailable/);
      // the label beside the legend control says it too, where it is seen
      await expect(page.getByTestId('legend-attribution')).toHaveAttribute('data-names', 'unavailable');
      await expect(page.getByTestId('legend-attribution')).toHaveText('names via kascov unavailable: trying again');
      expect(new Set(await shownIds(page))).toEqual(new Set([exkcc, KOB_OTHER]));
      await expect(page.getByTestId(`token-link-${exkcc}`)).toContainText('EXKCC');
      await expect(page.getByTestId(`kascov-ticker-differs-${exkcc}`)).toHaveCount(0);
      expect(log.tokenRequests).toBe(1);
      expect(await overflow(page)).toBe(0);
    });
  });
}

test.describe('the deployment turns the token names off (tokenNamesUrl: "")', () => {
  test.use({ appConfig: { tokenNamesUrl: '' } });
  test('no request to kascov, no hint, the toggle is disabled and says so', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const log = await mockKascov(page, opts(exkcc));
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${exkcc}`)).toHaveAttribute('data-state', 'ready');
    expect(new Set(await shownIds(page))).toEqual(new Set([exkcc, KOB_OTHER]));
    await expect(page.getByTestId('kascov-unavailable')).toHaveCount(0);
    await page.goto('/#/settings');
    await expect(page.getByTestId('settings-icons-off')).toBeVisible();
    await expect(page.getByTestId('settings-icons-toggle')).toBeDisabled();
    expect(log.requests).toEqual([]);
  });
});

test.describe('the deployment points the token names at another host (tokenNamesUrl)', () => {
  test.use({ appConfig: { tokenNamesUrl: 'https://names.example.org/kascov/data' } });
  test('the data set and the saved images come from that host, not kascov.io', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const log = await mockKascov(page, { ...opts(exkcc), origin: 'https://names.example.org/kascov' });
    const seen: string[] = [];
    page.on('request', (r) => seen.push(r.url()));
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${SAVED}`)).toBeVisible();
    await page.getByTestId(`token-icon-${SAVED}`).scrollIntoViewIfNeeded();
    await expect(page.getByTestId(`token-icon-${SAVED}`)).toHaveAttribute('data-icon', 'image');
    expect(log.requests).toContain('https://names.example.org/kascov/data/testnet-10/tokens.json');
    expect(log.requests).toContain(`https://names.example.org/kascov/img/testnet-10/${SAVED}`);
    expect(seen.filter((u) => u.startsWith('https://kascov.io/'))).toEqual([]);
  });
});
