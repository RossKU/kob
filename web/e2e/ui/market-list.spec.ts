// The market list (`#/market`) against the mock stack: the 24 h figures of each token's KAS market (`/v1/stats`: last price, change, KAS volume,
// spread, depth within 2 % of the mid, market cap), a favourite pair (price implied through the two KAS markets, 24 h pair volume, `/v1/pairs` for its order counts), sorting by
// every column (24 h volume first; mouse and keyboard), search over tokens and pairs, a row whose figures the indexer cannot give, and the phone layout.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { seedFavorites } from '../helpers/env';

// every test opens the list itself (after its registry / route stubs are in place)
test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);
/** the default seed's token (EXKCC of the example registry) */
const EXKCC = 'e5'.repeat(32);

/** The example registry with EXKCC listed and a second listed KCC-20, EXUSD (6 decimals), matching the mock pair seed. */
async function listBoth(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, tick: 100_000, display: { description: 'Fictional quote token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

/** Both tokens listed, the pair seed (EXUSD's KAS book, live EXKCC/EXUSD pair orders) and two EXUSD trades worth 75 KAS: EXUSD out-trades EXKCC. */
async function twoMarkets(page: Page, mock: MockClient): Promise<string> {
  await seedFavorites(page, { pairs: [{ base: EXKCC, quote: QUOTE }] });
  await mock.seed({ pair: true });
  await mock.seed({
    trades: [
      { token: QUOTE, ago_ms: 3_600_000, legs: [{ side: 'ask', price: 50_000_000, amount: 100_000_000 }] },
      { token: QUOTE, ago_ms: 60_000, legs: [{ side: 'bid', price: 50_200_000, amount: 50_000_000 }] },
    ],
  });
  await listBoth(page);
  const exkcc = (await mock.token()).covenant_id;
  await page.goto('/#/market');
  await expect(page.getByTestId(`token-row-${QUOTE}`)).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId(`token-row-${exkcc}`)).toHaveAttribute('data-state', 'ready');
  return exkcc;
}

/** The covenant ids of the token rows in their shown order: EXKCC and EXUSD only (the example registry lists tokens the mock does not trade). */
const rowIds = async (page: Page) =>
  (await page.locator('[data-testid="token-table"] tbody tr').evaluateAll((trs) => trs.map((tr) => (tr.getAttribute('data-testid') ?? '').replace('token-row-', ''))))
    .filter((id) => id === QUOTE || id === EXKCC);

test('a token row shows its last price in KAS, the 24 h change, KAS volume, spread and the depth within 2 % (order counts in its tooltip), with exact figures in the tooltips; no supply, no market cap', async ({ appPage: page, mock }) => {
  const id = (await mock.token()).covenant_id;
  await page.goto('/#/market');
  const row = page.getByTestId(`token-row-${id}`);
  await expect(row).toHaveAttribute('data-state', 'ready');
  const stats = await mock.get(`/v1/stats/${id}`);
  // the seeded fills of the last minutes: last 0.025 KAS per EXKCC at the decimals of the registry tick; the exact price and the unit in the tooltip
  const last = String(Number(stats.last) / 1e8);
  await expect(row.getByTestId(`token-last-${id}`)).toHaveText(new RegExp(`^${last.replace('.', '\\.')}0*$`));
  await expect(row.getByTestId(`token-last-${id}`)).toHaveAttribute('title', `${last} (KAS per EXKCC)`);
  await expect(row.getByTestId(`token-change-${id}`)).toHaveText(/^[+-]?\d+\.\d\d%$/);
  await expect(row.getByTestId(`token-volume-${id}`)).toHaveText((Number(stats.quote_volume_24h) / 1e8).toFixed(2));
  await expect(row.getByTestId(`token-volume-${id}`)).toHaveAttribute('title', new RegExp(`KAS · ${stats.trades_24h} trades in 24 h$`));
  await expect(row.getByTestId(`token-spread-${id}`)).toHaveText(/^\d+\.\d\d%$/);
  await expect(row.getByTestId(`token-spread-${id}`)).toHaveAttribute('title', /^Best bid 0\.0\d+ · best ask 0\.0\d+ \(KAS per EXKCC\)$/);
  // the executor's `depth_2pct_quote` (sompi): the KAS the book offers within 2 % of its mid
  expect(stats.depth_2pct_quote).toMatch(/^\d+$/);
  await expect(row.getByTestId(`token-depth-${id}`)).toHaveText((Number(stats.depth_2pct_quote) / 1e8).toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 }));
  await expect(row.getByTestId(`token-depth-${id}`)).toHaveAttribute('title', new RegExp(`KAS · ${stats.open_asks} asks · ${stats.open_bids} bids$`));
  // a token with no supply KOB can vouch for has no market cap
  await expect(row.getByTestId(`token-mcap-${id}`)).toHaveText('—');
  // the list opens by 24 h volume, most traded first
  await expect(page.getByTestId('sort-volume').locator('xpath=..')).toHaveAttribute('aria-sort', 'descending');
  // pairs are reached by the picker and the favourites: no table of them
  await expect(page.getByTestId('pair-table')).toHaveCount(0);
});

test('sorting: 24 h volume first, every header sorts (click and keyboard), search keeps the order; the table of favourite pairs follows the same order', async ({ appPage: page, mock }) => {
  const exkcc = await twoMarkets(page, mock);
  expect(await rowIds(page)).toEqual([QUOTE, exkcc]);
  await expect(page.getByTestId(`token-volume-${QUOTE}`)).toHaveText('75.10');

  // a click on the name sorts A-Z, a second click Z-A
  await page.getByTestId('sort-market').first().click();
  await expect(page.getByTestId('sort-market').first().locator('xpath=..')).toHaveAttribute('aria-sort', 'ascending');
  expect(await rowIds(page)).toEqual([exkcc, QUOTE]);
  await page.getByTestId('sort-market').first().click();
  expect(await rowIds(page)).toEqual([QUOTE, exkcc]);

  // keyboard: Tab to a header button and Enter / Space; the price (EXUSD 0.502 KAS > EXKCC 0.0251 KAS), descending first, then ascending
  await page.getByTestId('sort-last').first().focus();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('sort-last').first().locator('xpath=..')).toHaveAttribute('aria-sort', 'descending');
  expect(await rowIds(page)).toEqual([QUOTE, exkcc]);
  await page.keyboard.press('Space');
  expect(await rowIds(page)).toEqual([exkcc, QUOTE]);
  await expect(page.getByTestId('sort-volume').first().locator('xpath=..')).toHaveAttribute('aria-sort', 'none');

  // the favourite pair EXKCC/EXUSD: its price implied through both KAS markets (EXKCC's ~0.025 EXUSD per KAS's 0.502 KAS), the depth near its mid (live pair orders in the tooltip), no spread
  await page.getByTestId('token-list-favorites').click();
  const pairId = `${exkcc}-${QUOTE}`;
  const pair = page.getByTestId(`pair-row-${pairId}`);
  await expect(pair).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId(`pair-link-${pairId}`)).toHaveText(`EXKCC (${exkcc.slice(0, 4)}…${exkcc.slice(-4)}) / EXUSD (e7e7…e7e7)`);
  await expect(page.getByTestId(`pair-link-${pairId}`)).toHaveAttribute('href', `#/market/${exkcc}/${QUOTE}`);
  await expect(pair.getByTestId(`pair-last-${pairId}`)).toHaveAttribute('title', /^0\.0\d+ \(EXUSD per EXKCC\)$/);
  await expect(pair.getByTestId(`pair-depth-${pairId}`)).toHaveText(/^≥?[\d,.]+[KMBT]?$/);
  await expect(pair.getByTestId(`pair-depth-${pairId}`)).toHaveAttribute('title', /EXUSD · 3 asks · 2 bids · 1 conditional/);
  // a pair has no supply: no market cap column
  await expect(page.getByTestId('pair-table').getByTestId('sort-mcap')).toHaveCount(0);
  await expect(pair.getByTestId(`pair-spread-${pairId}`)).toHaveText('—');
  await expect(pair.getByTestId(`pair-volume-${pairId}`)).toHaveText('0.00');
  await expect(pair.getByTestId(`pair-volume-${pairId}`)).toHaveAttribute('title', /EXUSD · 0 pair fills in 24 h$/);
  // the pair table's headers carry the same sort state
  await expect(page.getByTestId('pair-table').getByTestId('sort-last').locator('xpath=..')).toHaveAttribute('aria-sort', 'ascending');

  // search: a token matches its pairs; BASE/QUOTE matches the pair; nothing else
  await page.getByTestId('token-search').fill('exusd');
  await expect(pair).toBeVisible();
  await page.getByTestId('token-search').fill('exkcc/exusd');
  await expect(pair).toBeVisible();
  await page.getByTestId('token-search').fill('exusd/exkcc');
  await expect(pair).toHaveCount(0);
  await expect(page.getByTestId('token-list-empty')).toBeVisible();
  await page.getByTestId('token-search').fill('');
  await expect(pair).toBeVisible();

  // a pair link opens the pair page
  await page.getByTestId(`pair-link-${pairId}`).click();
  await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
});

test('a token whose 24 h figures the indexer cannot give shows — without breaking the list; the other rows keep theirs', async ({ appPage: page, mock }) => {
  await mock.seed({ pair: true });
  await listBoth(page);
  const exkcc = (await mock.token()).covenant_id;
  await seedFavorites(page, { pairs: [{ base: exkcc, quote: QUOTE }] });
  await page.route(`**/v1/stats/${QUOTE}`, (route) => route.fulfill({ status: 500, contentType: 'application/json', body: '{"error":{"code":"internal","message":"boom"}}' }));
  await page.goto('/#/market');
  const bad = page.getByTestId(`token-row-${QUOTE}`);
  await expect(bad).toHaveAttribute('data-state', 'unavailable');
  for (const k of ['last', 'change', 'volume', 'mcap', 'spread', 'depth']) {
    await expect(bad.getByTestId(`token-${k}-${QUOTE}`)).toHaveText('—');
    await expect(bad.getByTestId(`token-${k}-${QUOTE}`)).toHaveAttribute('title', 'Unavailable from the indexer');
  }
  await expect(page.getByTestId(`token-row-${exkcc}`)).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId(`token-last-${exkcc}`)).toHaveText(/^0\.0\d+$/);
  // a row without figures sorts last whatever the direction
  await page.getByTestId('sort-volume').first().click();
  expect(await rowIds(page)).toEqual([exkcc, QUOTE]);
  // the pair needs both legs for its price: — there too
  await page.getByTestId('token-list-favorites').click();
  await expect(page.getByTestId(`pair-last-${exkcc}-${QUOTE}`)).toHaveText('—');
});

test('while the figures load, each row shows placeholders and the names are already there', async ({ appPage: page, mock }) => {
  const id = (await mock.token()).covenant_id;
  let release: () => void = () => undefined;
  const gate = new Promise<void>((r) => (release = r));
  await page.route('**/v1/stats/**', async (route) => {
    await gate;
    await route.continue();
  });
  await page.goto('/#/market');
  const row = page.getByTestId(`token-row-${id}`);
  await expect(row).toHaveAttribute('data-state', 'loading');
  await expect(row.getByTestId(`token-link-${id}`)).toBeVisible();
  await expect(row.getByTestId(`token-last-${id}`).locator('.skeleton')).toBeVisible();
  release();
  await expect(row).toHaveAttribute('data-state', 'ready');
  await expect(row.getByTestId(`token-last-${id}`).locator('.skeleton')).toHaveCount(0);
});

test('a token icon has a thin ring from the theme token (light: black 12 %, dark: white 22 %) that does not change its size', async ({ appPage: page, mock }) => {
  const id = (await mock.token()).covenant_id;
  await page.goto('/#/market');
  const icon = page.getByTestId(`token-icon-${id}`);
  await expect(icon).toBeVisible();
  const sizeOf = async () => {
    const b = (await icon.boundingBox())!;
    return [b.width, b.height];
  };
  const shadow = () => icon.evaluate((el) => getComputedStyle(el).boxShadow);
  for (const [theme, ring] of [['light', 'rgba(0, 0, 0, 0.12)'], ['dark', 'rgba(255, 255, 255, 0.22)']] as const) {
    const before = await sizeOf();
    await page.evaluate((t) => document.documentElement.setAttribute('data-theme', t), theme);
    await expect.poll(shadow).toContain(ring);
    expect(await shadow()).toMatch(/ 0px 0px 0px 1px$/);
    expect(await sizeOf()).toEqual(before);
  }
});

test.describe('phone', () => {
  test.use({ viewport: { width: 375, height: 812 } });

  test('375 px: name, price and change only, no sideways scrolling (an open-list token named by 8 + 8 hex too)', async ({ appPage: page, mock }) => {
    await mock.seed({ open_token: true });
    const exkcc = await twoMarkets(page, mock);
    // a token without metadata (no ticker, no decimals) is out of the default list until asked for
    await expect(page.getByTestId(`token-link-${'0f'.repeat(32)}`)).toHaveCount(0);
    await page.getByTestId('token-list-nameless').click();
    await expect(page.getByTestId(`token-link-${'0f'.repeat(32)}`)).toHaveText('Unnamed token');
    await expect(page.getByTestId(`token-hash-${'0f'.repeat(32)}`)).toHaveText('0f0f0f0f0f0f…0f0f0f0f0f0f');
    const row = page.getByTestId(`token-row-${exkcc}`);
    await expect(row.getByTestId(`token-link-${exkcc}`)).toBeVisible();
    await expect(row.getByTestId(`token-last-${exkcc}`)).toBeVisible();
    await expect(row.getByTestId(`token-change-${exkcc}`)).toBeVisible();
    for (const k of ['volume', 'mcap', 'spread', 'depth']) await expect(row.getByTestId(`token-${k}-${exkcc}`)).toBeHidden();
    // the identity block stays on the left, the shortened id under the name
    await expect(row.getByTestId(`identity-${exkcc}`)).toBeVisible();
    await expect(row.getByTestId(`token-hash-${exkcc}`)).toBeVisible();
    await expect(page.getByTestId('sort-volume').first()).toBeHidden();
    const overflow = await page.evaluate(() => {
      const wraps = [...document.querySelectorAll('.ml-table')].map((w) => w.scrollWidth - w.clientWidth);
      return { page: document.documentElement.scrollWidth - window.innerWidth, tables: Math.max(...wraps) };
    });
    expect(overflow.page).toBeLessThanOrEqual(0);
    expect(overflow.tables).toBeLessThanOrEqual(0);
    // sorting still works from the visible headers
    await page.getByTestId('sort-change').first().click();
    await expect(page.getByTestId('sort-change').first().locator('xpath=..')).toHaveAttribute('aria-sort', 'descending');
    // the favourite pair's table keeps its headers
    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId('pair-table').getByTestId('sort-last')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(0);
  });
});
