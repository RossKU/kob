// The market list completed from kascov.io (src/data/kascov.ts), against the mock stack with a MOCKED kascov (e2e/helpers/kascov.ts: the real service is
// never reached): tokens of a program this build trades, named by their creators' claims and sorted after the KOB rows as "no orders"; icons only for
// bytes that hash to the committed hash; the notes where kascov disagrees with KOB; the Settings toggle; kascov down, slow-shaped or turned off by the
// deployment. Desktop and a 375 px phone (no horizontal overflow). The app under test is the built one, so its Content-Security-Policy is in force:
// the pictures are shown from blob: URLs, never from a third-party URL.
import { createHash } from 'node:crypto';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { mockKascov, OTHER_PNG, PNG, programHash, sha256, type MockKascovToken } from '../helpers/kascov';
import { chooseOption, optionLabels } from '../helpers/dropdown';

test.use({ autoOpen: false });

const ID = (n: number) => n.toString(16).padStart(2, '0').repeat(32);
const SAVED = ID(0xa1); // a saved copy that hashes to the committed hash
const MISMATCH = ID(0xa2); // the saved copy is another picture than the one committed (its hash is of other bytes than OTHER_PNG)
const NO_CORS = ID(0xa3); // no saved copy; the creator's own host is shown unchecked (an <img>, since 2026-11-03), after kascov's copy was asked for
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
    await expect(page.getByTestId(`token-depth-${SAVED}`)).toHaveText('no orders');
    // nothing of its figures is invented
    for (const f of ['last', 'change', 'volume', 'mcap', 'spread']) await expect(page.getByTestId(`token-${f}-${SAVED}`)).toHaveText('—');

    // the program decides: a pinned program on this network is listed (kascov's status does not decide it), an unpinned one is not
    for (const id of [SAVED, MISMATCH, NO_CORS, NO_IMAGE, PUBLIC_MINT, INVALID, MAINNET_ONLY]) await expect(page.getByTestId(`token-row-${id}`)).toHaveCount(1);
    await expect(page.getByTestId(`token-row-${UNPINNED}`)).toHaveCount(0);

    // KOB's rows come first and keep their ticker; the disagreement is a note, never a replacement
    const ids = await shownIds(page);
    expect(ids[0]).toBe(exkcc);
    expect(new Set(ids)).toEqual(new Set([exkcc, KOB_OTHER, SAVED, MISMATCH, NO_CORS, NO_IMAGE, PUBLIC_MINT, INVALID, MAINNET_ONLY]));
    await expect(page.getByTestId(`token-link-${exkcc}`)).toContainText('EXKCC');
    // a short badge each; the full sentence with both values is the tooltip, and a tap or the keyboard opens it as a panel (a title does not exist on a touch screen)
    const tickerBadge = page.getByTestId(`kascov-ticker-differs-${exkcc}`);
    const decimalsBadge = page.getByTestId(`kascov-decimals-differ-${exkcc}`);
    await expect(tickerBadge).toHaveText(/ticker differs$/);
    await expect(decimalsBadge).toHaveText(/decimals differ$/);
    await expect(tickerBadge).toHaveAttribute('title', /kascov.io says the ticker is FAKE; KOB shows EXKCC/);
    await expect(decimalsBadge).toHaveAttribute('title', /kascov.io says 2 decimals .* KOB uses 8/);
    await expect(page.getByTestId(`kascov-decimals-differ-detail-${exkcc}`)).toHaveCount(0);
    await decimalsBadge.click();
    await expect(decimalsBadge).toHaveAttribute('aria-expanded', 'true');
    await expect(page.getByTestId(`kascov-decimals-differ-detail-${exkcc}`)).toContainText('kascov.io says 2 decimals');
    await expect(page.getByTestId(`kascov-decimals-differ-detail-${exkcc}`)).toContainText('KOB uses 8');
    await page.keyboard.press('Escape');
    await expect(page.getByTestId(`kascov-decimals-differ-detail-${exkcc}`)).toHaveCount(0);
    await expect(decimalsBadge).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(page.getByTestId(`kascov-decimals-differ-detail-${exkcc}`)).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId(`kascov-source-${exkcc}`)).toHaveCount(0);

    // icons: the saved copy hashes to the committed hash and is shown from a blob: URL it decodes from; every other case is a letter
    const icon = page.getByTestId(`token-icon-${SAVED}`);
    await icon.scrollIntoViewIfNeeded();
    await expect(icon).toHaveAttribute('data-icon', 'image');
    const img = icon.locator('img');
    expect(await img.getAttribute('src')).toMatch(/^blob:/);
    await expect.poll(() => img.evaluate((e: HTMLImageElement) => e.naturalWidth)).toBe(1);
    for (const id of [MISMATCH, NO_IMAGE]) {
      const letter = page.getByTestId(`token-icon-${id}`);
      await letter.scrollIntoViewIfNeeded();
      await expect(letter).toHaveAttribute('data-icon', 'letter');
      await expect(letter.locator('img')).toHaveCount(0);
    }
    await expect(page.getByTestId(`token-icon-${MISMATCH}`)).toHaveText('M');
    // kascov holds no copy for NO_CORS: its creator's own address is shown as a plain <img>, labelled unchecked (the page cannot read its bytes: no CORS)
    const creator = page.getByTestId(`token-icon-${NO_CORS}`);
    await creator.scrollIntoViewIfNeeded();
    await expect(creator).toHaveAttribute('data-icon', 'creator-image');
    await expect(creator).toHaveAttribute('title', "Image from the creator's site, not checked");
    const creatorImg = creator.locator('img');
    expect(await creatorImg.getAttribute('src')).toBe(CREATOR_URL);
    expect(await creatorImg.getAttribute('referrerpolicy')).toBe('no-referrer');
    expect(await creatorImg.getAttribute('crossorigin')).toBeNull();
    await expect.poll(() => creatorImg.evaluate((e: HTMLImageElement) => e.naturalWidth)).toBe(1);
    // the mismatching copy was fetched and refused (a letter); the checked path never asks the creator's host: it is asked only after kascov had no copy
    await expect.poll(() => log.requests.includes(`https://kascov.io/img/testnet-10/${MISMATCH}`)).toBe(true);
    await expect.poll(() => log.requests.includes(`https://kascov.io/img/testnet-10/${NO_CORS}`)).toBe(true);
    await expect.poll(() => log.requests.includes(CREATOR_URL)).toBe(true);
    // the only <img> that points at a third-party URL is that creator image, and the policy was never violated
    expect(await page.locator('img[src^="http"]').evaluateAll((els) => els.map((e) => e.getAttribute('src')))).toEqual([CREATOR_URL]);
    expect(pageErrors.filter((e) => /Content.Security.Policy/i.test(e))).toEqual([]);
    expect(await overflow(page)).toBe(0);

    // the default sort is by 24 h volume: the rows with no orders stay after the KOB row in both directions of another figure column too
    await page.getByTestId('sort-depth').first().click();
    expect((await shownIds(page))[0]).toBe(exkcc);
    await page.getByTestId('sort-depth').first().click();
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
    await expect(page.getByTestId(`token-depth-${SAVED}`)).toBeHidden();
    await expect(page.getByTestId(`kascov-source-${SAVED}`)).toBeVisible();
    expect(await overflow(page)).toBe(0);
    // the narrowest rows too: the note of a disagreement does not widen the page
    await expect(page.getByTestId(`kascov-ticker-differs-${exkcc}`)).toBeVisible();
    expect(await overflow(page)).toBe(0);
    // a tap on the badge opens the full sentence inside the screen
    await page.getByTestId(`kascov-ticker-differs-${exkcc}`).click();
    const panel = page.getByTestId(`kascov-ticker-differs-detail-${exkcc}`);
    await expect(panel).toContainText('kascov.io says the ticker is FAKE; KOB shows EXKCC');
    const box = (await panel.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(375);
    expect(await overflow(page)).toBe(0);
    await page.getByTestId(`kascov-ticker-differs-${exkcc}`).click();
    await expect(panel).toHaveCount(0);
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

  test("kascov's price: a line in the price cell's tooltip of the market list and an optional line on the chart, asked for only while it is on", async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const stats = await mock.get(`/v1/stats/${exkcc}`);
    const dec = Number(stats.decimals);
    const own = Number(stats.last) / Number(stats.price_basis); // KOB's last price, KAS per whole token
    // kascov's price: 0.0125 KAS per whole token (quote_sompi over base_amount of one whole token), an older close of 0.02 before it
    const base = 10 ** dec;
    const candles = { [exkcc]: [{ t: 1_000, base, quote: 2_000_000 }, { t: 2_000, base, quote: 1_250_000 }] };
    const priced = (market: MockKascovToken['market']) => tokens(exkcc).map((k) => (k.covenant_id === exkcc ? { ...k, ...(market ? { market } : {}) } : k));
    const toggle = page.getByTestId('chart-kascov-toggle');
    const legend = page.getByTestId('chart-kascov');
    const chart = page.getByTestId('price-chart');

    // kascov does not price the token: no toggle, no candle request, no line in the tooltip
    let log = await mockKascov(page, { tokens: priced({ unpriced_reason: "no single covenant holds this token's inventory" }), candles });
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-last-${exkcc}`)).not.toHaveAttribute('title', /kascov/);
    await page.goto(`/#/market/${exkcc}`);
    await expect(chart).toHaveAttribute('data-state', 'ready');
    await expect(toggle).toHaveCount(0);
    expect(log.candleRequests).toEqual([]);

    // priced: the list's price cell keeps its text and tooltip and gains one line (the percent is against KOB's own price)
    log = await mockKascov(page, { tokens: priced({ spot_num_sompi: 1_250_000, spot_den: base }), candles });
    await page.reload(); // the app keeps the list for a while: a new page reads the new one
    await page.goto('/#/market');
    const cell = page.getByTestId(`token-last-${exkcc}`);
    await expect(cell).toHaveAttribute('title', /\nkascov: 0\.0125 KAS \([+-]\d+%\)$/);
    const pct = Number(/\(([+-]\d+)%\)$/.exec((await cell.getAttribute('title'))!)![1]);
    expect(Math.abs(pct - Math.round(((0.0125 - own) / own) * 100))).toBeLessThanOrEqual(1);
    expect(log.candleRequests).toEqual([]);

    // the market page: a toggle, off, and nothing asked of kascov's candles yet
    await page.goto(`/#/market/${exkcc}`);
    await expect(chart).toHaveAttribute('data-state', 'ready');
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');
    await expect(toggle).toHaveAttribute('title', 'kascov price (third-party)');
    await expect(chart).toHaveAttribute('data-kascov', '0');
    await expect(legend).toHaveCount(0);
    expect(log.candleRequests).toEqual([]);

    // on: the legend says the price and what it is, and the 5 minute chart asked for the 15 minute candles of this token, once
    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await expect(chart).toHaveAttribute('data-kascov', '1');
    await expect(legend).toHaveText('kascov 0.0125');
    await expect(legend).toHaveAttribute('title', "Third-party reference from kascov.io: a marginal pool price on other venues, before fees. KOB does not use it; KOB's prices come from its own book.");
    expect(log.candleRequests).toEqual([`https://kascov.io/data/testnet-10/token/${exkcc}/candles?bucket=15m&phase=all`]);

    // another interval asks for its own bucket; off stops the line and every request
    await page.getByTestId('chart-interval-1h').click();
    await expect.poll(() => log.candleRequests.length).toBe(2);
    expect(log.candleRequests[1]).toContain('bucket=1h&phase=all');
    await expect(legend).toHaveText('kascov 0.0125');
    await toggle.click();
    await expect(legend).toHaveCount(0);
    await expect(chart).toHaveAttribute('data-kascov', '0');
    await page.getByTestId('chart-interval-1d').click();
    await page.getByTestId('chart-interval-5m').click();
    await expect(chart).toHaveAttribute('data-state', 'ready');
    expect(log.candleRequests.length).toBe(2);

    // kascov answers with an error: the legend says so, nothing else breaks
    log = await mockKascov(page, { tokens: priced({ spot_num_sompi: 1_250_000, spot_den: base }), candles, candlesMode: 'http' });
    await page.reload();
    await expect(chart).toHaveAttribute('data-state', 'ready');
    await toggle.click();
    await expect(legend).toHaveText('kascov unavailable');
    await expect(chart).toHaveAttribute('data-state', 'ready');
    expect(log.candleRequests.length).toBe(1);
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

test.describe('the Settings toggle "Load token icons"', () => {
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
  test('no request to kascov, no hint, the page says so', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const log = await mockKascov(page, opts(exkcc));
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${exkcc}`)).toHaveAttribute('data-state', 'ready');
    expect(new Set(await shownIds(page))).toEqual(new Set([exkcc, KOB_OTHER]));
    await expect(page.getByTestId('kascov-unavailable')).toHaveCount(0);
    await page.goto('/#/settings');
    await expect(page.getByTestId('settings-icons-off')).toBeVisible();
    // logos from the address a creator gave (a KCC-23 declaration) need no token list: the switches stay
    await expect(page.getByTestId('settings-icons-toggle')).toBeEnabled();
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

const CORS = { 'access-control-allow-origin': '*', 'access-control-allow-headers': 'content-type', 'access-control-allow-methods': 'GET, POST, OPTIONS', 'access-control-expose-headers': 'retry-after' };

test.describe('token logos from the creator\'s address: the Settings switch, the blocklist and the report', () => {
  test('the creator-sites switch is on by default and off means a letter and no request to the creator\'s host; the master switch off disables it', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const log = await mockKascov(page, opts(exkcc));
    await page.goto('/#/settings');
    const creator = page.getByTestId('settings-icons-creator-toggle');
    await expect(creator).toBeChecked();
    await expect(page.getByTestId('settings-icons')).toContainText('that site sees your IP address and when you looked');
    await creator.uncheck();
    expect(await page.evaluate(() => localStorage.getItem('kob.icons.creator'))).toBe('0');
    await page.goto('/#/market');
    await page.getByTestId(`token-icon-${NO_CORS}`).scrollIntoViewIfNeeded();
    await expect(page.getByTestId(`token-icon-${NO_CORS}`)).toHaveAttribute('data-icon', 'letter');
    // the checked copies still show: only the creators' addresses are off
    await page.getByTestId(`token-icon-${SAVED}`).scrollIntoViewIfNeeded();
    await expect(page.getByTestId(`token-icon-${SAVED}`)).toHaveAttribute('data-icon', 'image');
    await page.waitForTimeout(500);
    expect(log.requests.includes(CREATOR_URL)).toBe(false);
    await page.goto('/#/settings');
    await page.getByTestId('settings-icons-creator-toggle').check();
    await page.getByTestId('settings-icons-toggle').uncheck();
    await expect(page.getByTestId('settings-icons-creator-toggle')).toBeDisabled();
  });

  test('a token on the blocklist shows its letter and no request is made; the list is read again and a restored token shows its logo', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const log = await mockKascov(page, opts(exkcc));
    let hidden: string[] = [NO_CORS, SAVED];
    let reads = 0;
    await page.route('**/v1/logo-blocklist', (route) => {
      reads++;
      return route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify({ hidden, updated_ms: reads }) });
    });
    await page.goto('/#/market');
    await expect.poll(() => reads).toBeGreaterThan(0);
    for (const id of [NO_CORS, SAVED]) {
      await page.getByTestId(`token-icon-${id}`).scrollIntoViewIfNeeded();
      await expect(page.getByTestId(`token-icon-${id}`)).toHaveAttribute('data-icon', 'letter');
    }
    await page.waitForTimeout(500);
    expect(log.requests.includes(CREATOR_URL)).toBe(false);
    expect(log.requests.includes(`https://kascov.io/img/testnet-10/${SAVED}`)).toBe(false);
    // the token page says so
    await page.goto(`/#/market/${NO_CORS}`);
    await expect(page.getByTestId('token-logo-note')).toContainText('Logo hidden after a report');
    await expect(page.getByTestId('logo-report-open')).toHaveCount(0);
    // the cache of the last list serves the next visit at once, even when the indexer is unreachable (it fails open only for tokens not on the list)
    expect(await page.evaluate(() => localStorage.getItem('kob.logo-blocklist.v1'))).toContain(NO_CORS);
  });

  test('an indexer without the route (404) or an unreachable one leaves every logo visible', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    await mockKascov(page, opts(exkcc));
    await page.route('**/v1/logo-blocklist', (route) => route.fulfill({ status: 404, headers: CORS, contentType: 'application/json', body: '{"error":{"code":"not_found"}}' }));
    await page.goto('/#/market');
    await page.getByTestId(`token-icon-${NO_CORS}`).scrollIntoViewIfNeeded();
    await expect(page.getByTestId(`token-icon-${NO_CORS}`)).toHaveAttribute('data-icon', 'creator-image');
  });

  test('"Report logo" in the token details: three reasons, the result, the letter at once, and the link to the GitHub form', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    await mockKascov(page, opts(exkcc));
    await page.route('**/v1/logo-blocklist', (route) => route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: '{"hidden":[],"updated_ms":0}' }));
    const posted: unknown[] = [];
    await page.route('**/v1/logo-reports', (route) => {
      if (route.request().method() === 'OPTIONS') return route.fulfill({ status: 204, headers: CORS });
      posted.push(route.request().postDataJSON());
      return route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify({ result: 'hidden', covenant_id: NO_CORS }) });
    });
    await page.goto(`/#/market/${NO_CORS}`);
    await expect(page.getByTestId('token-logo-note')).toContainText("Image from the creator's site, not checked");
    await page.getByTestId('logo-report-open').click();
    const dialog = page.getByTestId('logo-report-dialog');
    await expect(dialog).toBeVisible();
    await expect(page.getByTestId('logo-report-github')).toHaveAttribute('href', new RegExp(`^https://github.com/RossKU/kob/issues/new\\?template=logo-report.yml&covenant_id=${NO_CORS}$`));
    const reason = page.getByTestId('logo-report-reason');
    expect(await optionLabels(reason)).toEqual(['Illegal content', 'Impersonates another project or person', 'Something else']);
    // the list is in the dialog and above it; Escape and a press on the backdrop close the list, not the dialog
    await reason.click();
    await expect(dialog.getByTestId('logo-report-reason-panel')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('logo-report-reason-panel')).toHaveCount(0);
    await expect(dialog).toBeVisible();
    await expect(reason).toBeFocused();
    await reason.click();
    await page.locator('.modal-backdrop').click({ position: { x: 4, y: 4 } });
    await expect(page.getByTestId('logo-report-reason-panel')).toHaveCount(0);
    await expect(dialog).toBeVisible();
    await chooseOption(page.getByTestId('logo-report-reason'), 'illegal');
    await page.getByTestId('logo-report-send').click();
    await expect(page.getByTestId('logo-report-result')).toContainText('now hidden for everyone');
    expect(posted).toEqual([{ covenant_id: NO_CORS, reason: 'illegal' }]);
    // the logo is a letter at once, and the details say why
    await page.keyboard.press('Escape');
    await expect(page.getByTestId(`token-icon-${NO_CORS}`).first()).toHaveAttribute('data-icon', 'letter');
    await expect(page.getByTestId('token-logo-note')).toContainText('Logo hidden after a report');
  });

  test('a report the indexer cannot take says so and offers the GitHub form', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    await mockKascov(page, opts(exkcc));
    await page.route('**/v1/logo-reports', (route) => {
      if (route.request().method() === 'OPTIONS') return route.fulfill({ status: 204, headers: CORS });
      return route.fulfill({ status: 429, headers: { ...CORS, 'retry-after': '120' }, contentType: 'application/json', body: '{"error":{"code":"rate_limited"}}' });
    });
    await page.goto(`/#/market/${NO_CORS}`);
    await page.getByTestId('logo-report-open').click();
    await page.getByTestId('logo-report-send').click();
    await expect(page.getByTestId('logo-report-result')).toContainText('Try again in 120 s');
    await expect(page.getByTestId('logo-report-github')).toBeVisible();
    // not hidden by a failed report
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('token-logo-note')).toContainText("not checked");
  });
});

/** The CIDv1 (raw leaf, SHA-256) of some bytes: what `ipfs add --cid-version 1 --raw-leaves` prints for a file of one block. */
function rawCid(bytes: Buffer): string {
  const A = 'abcdefghijklmnopqrstuvwxyz234567';
  const input = Buffer.concat([Buffer.from([0x01, 0x55, 0x12, 0x20]), createHash('sha256').update(bytes).digest()]);
  let bits = 0;
  let acc = 0;
  let out = 'b';
  for (const b of input) {
    acc = (acc << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += A[(acc >> (bits - 5)) & 31];
      bits -= 5;
    }
    acc &= (1 << bits) - 1;
  }
  if (bits > 0) out += A[(acc << (5 - bits)) & 31];
  return out;
}

test.describe('an ipfs:// logo', () => {
  test('is fetched as bytes through the gateway, compared with its CID and shown from a blob: URL; bytes that are not the CID\'s are a letter', async ({ appPage: page, mock }) => {
    const exkcc = (await mock.token()).covenant_id;
    const IPFS_OK = ID(0xa9);
    const IPFS_FORGED = ID(0xaa);
    const okCid = rawCid(PNG);
    const forgedCid = rawCid(OTHER_PNG);
    const log = await mockKascov(page, {
      ...opts(exkcc),
      tokens: [
        ...tokens(exkcc),
        { covenant_id: IPFS_OK, claimed_ticker: 'IPFSOK', claimed_image: `ipfs://${okCid}`, program: MINT },
        // the gateway answers this address with a PNG that is not the one the CID names
        { covenant_id: IPFS_FORGED, claimed_ticker: 'IPFSBAD', claimed_image: `ipfs://${forgedCid}`, program: MINT },
      ],
    });
    const asked: string[] = [];
    await page.route('https://dweb.link/ipfs/**', (route) => {
      asked.push(route.request().url());
      return route.fulfill({ status: 200, contentType: 'image/png', headers: { 'access-control-allow-origin': '*' }, body: PNG });
    });
    await page.goto('/#/market');
    const ok = page.getByTestId(`token-icon-${IPFS_OK}`);
    await ok.scrollIntoViewIfNeeded();
    await expect(ok).toHaveAttribute('data-icon', 'image');
    expect(await ok.locator('img').getAttribute('src')).toMatch(/^blob:/);
    const bad = page.getByTestId(`token-icon-${IPFS_FORGED}`);
    await bad.scrollIntoViewIfNeeded();
    await expect.poll(() => asked.includes(`https://dweb.link/ipfs/${forgedCid}`)).toBe(true);
    await expect(bad).toHaveAttribute('data-icon', 'letter');
    expect(asked).toContain(`https://dweb.link/ipfs/${okCid}`);
    // the gateway is the only host asked for these: not kascov's creator route
    expect(log.requests.some((u) => u.includes('dweb.link'))).toBe(false);
  });
});
