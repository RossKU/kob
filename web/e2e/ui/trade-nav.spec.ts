// The header's "Trade" tab (Market | Trade | Portfolio | Activity | ...): Market is the list; Trade leads back to the market opened last (a token's KAS market or a
// token/token pair, the exact route), per network in localStorage. It is ALWAYS there (the tab bar never shifts between pages): with nothing stored it leads to
// the first market the registry lists. A stored value that is no market route, or a market that does not resolve, is dropped. Trade is the active tab on a
// market page, Market only on the list. The phone menu has it too. 360 px fit.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);
const KEY = 'kob.lastMarket.testnet-10';
const stored = (page: Page) => page.evaluate((k) => localStorage.getItem(k), KEY);

async function listPair(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional pair quote token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

test('a first visit has the Trade tab too: it leads to the first listed market, and nothing is stored until a market is opened', async ({ appPage: page, mock }) => {
  await page.goto('/#/market');
  await expect(page.getByTestId('nav-market')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).not.toHaveAttribute('aria-current', 'page');
  await expect(page.getByTestId('nav-market')).toHaveAttribute('aria-current', 'page');
  // the registry's first tradable token (the mock's EXKCC), else the list: a real market route either way
  const token = (await mock.token()).covenant_id;
  await expect(page.getByTestId('nav-trade')).toHaveAttribute('href', new RegExp('^#/market(/' + token + ')?$'));
  expect(await stored(page)).toBeNull();
  // the tab does not move between the list and another page
  const x = (await page.getByTestId('nav-trade').boundingBox())!.x;
  await page.getByTestId('nav-activity').click();
  expect((await page.getByTestId('nav-trade').boundingBox())!.x).toBeCloseTo(x, 0);
});

test('opening a market adds Trade (next to Market); Market then leads to the list and Trade back to the market', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  const token = await openMarket(page, mock, { connect: false });
  const tab = page.getByTestId('nav-trade');
  await expect(tab).toBeVisible();
  await expect(tab).toHaveText('Trade');
  await expect(tab).toHaveAttribute('aria-current', 'page');
  await expect(page.getByTestId('nav-market')).not.toHaveAttribute('aria-current', 'page');
  expect(await stored(page)).toBe(`#/market/${token}`);
  // Market | Trade | Activity: in this order
  const xs = await Promise.all(['nav-market', 'nav-trade', 'nav-activity'].map(async (id) => (await page.getByTestId(id).boundingBox())!.x));
  expect(xs[0]).toBeLessThan(xs[1]!);
  expect(xs[1]).toBeLessThan(xs[2]!);
  // the market page has the way back to the list
  await expect(page.getByTestId('market-all')).toHaveText(/All markets/);

  await page.getByTestId('nav-market').click();
  await expect(page.getByTestId('token-list-view')).toBeVisible();
  await expect(page.getByTestId('nav-market')).toHaveAttribute('aria-current', 'page');
  await expect(page.getByTestId('nav-trade')).not.toHaveAttribute('aria-current', 'page');
  await expect(page.getByTestId('nav-trade')).toBeVisible();

  await page.getByTestId('nav-trade').click();
  await expect(page).toHaveURL(new RegExp(`#/market/${token}$`));
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).toHaveAttribute('aria-current', 'page');

  // "All markets" goes to the list, and Trade is still there
  await page.getByTestId('market-all').click();
  await expect(page.getByTestId('token-list-view')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).toBeVisible();
  // other pages keep neither tab current
  await page.getByTestId('nav-activity').click();
  await expect(page.getByTestId('nav-trade')).not.toHaveAttribute('aria-current', 'page');
  await expect(page.getByTestId('nav-market')).not.toHaveAttribute('aria-current', 'page');
});

test('a shared link to a market is remembered; the tab survives a reload', async ({ appPage: page, mock }) => {
  const token = (await mock.token()).covenant_id;
  await page.goto(`/#/market/${token}?ticket=close`);
  await expect(page.getByTestId('nav-trade')).toBeVisible();
  expect(await stored(page), 'the route without its ticket preset').toBe(`#/market/${token}`);
  await page.goto('/#/activity');
  await page.reload();
  await expect(page.getByTestId('nav-trade')).toHaveAttribute('href', `#/market/${token}`);
});

test('a pair is remembered as a pair', async ({ appPage: page, mock }) => {
  await mock.seed({ pair: true });
  await fund(mock, 'alice');
  await listPair(page);
  const base = (await mock.token()).covenant_id;
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('pair-header')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).toHaveAttribute('aria-current', 'page');
  expect(await stored(page)).toBe(`#/market/${base}/${QUOTE}`);
  await page.getByTestId('nav-market').click();
  await expect(page.getByTestId('nav-trade')).toHaveAttribute('href', `#/market/${base}/${QUOTE}`);
  await page.getByTestId('nav-trade').click();
  await expect(page.getByTestId('pair-header')).toBeVisible();
  // the token's own market is the other tab's target afterwards
  await page.goto(`/#/market/${base}`);
  await expect(page.getByTestId('token-header')).toBeVisible();
  expect(await stored(page)).toBe(`#/market/${base}`);
});

test('a stored value that is no market route is dropped: Trade leads to the first listed market', async ({ appPage: page }) => {
  await page.addInitScript((k) => localStorage.setItem(k, '#/portfolio'), KEY);
  await page.goto('/#/market');
  await expect(page.getByTestId('nav-market')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).not.toHaveAttribute('href', '#/portfolio');
  expect(await stored(page)).toBeNull();
});

test('a market that no longer resolves is forgotten and Trade falls back to the first listed market', async ({ appPage: page }) => {
  const gone = '9d'.repeat(32);
  await page.addInitScript((k) => localStorage.setItem(k, `#/market/${'9d'.repeat(32)}`), KEY);
  await page.goto('/#/market');
  await expect(page.getByTestId('nav-trade')).toHaveAttribute('href', `#/market/${gone}`);
  await page.getByTestId('nav-trade').click();
  await expect(page.getByTestId('token-not-found')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).not.toHaveAttribute('href', `#/market/${gone}`);
  expect(await stored(page)).toBeNull();
  // the list is a click away, and the tab is still there
  await page.getByTestId('nav-market').click();
  await expect(page.getByTestId('token-list-view')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).toBeVisible();
});

test('without storage the app still works and the Trade tab is there', async ({ appPage: page, mock }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, 'localStorage', { get: () => { throw new Error('blocked'); } });
  });
  const token = (await mock.token()).covenant_id;
  await page.goto(`/#/market/${token}`);
  await expect(page.getByTestId('nav-market')).toBeVisible();
  await expect(page.getByTestId('nav-trade')).toBeVisible();
});

for (const width of [360, 390]) {
  test(`${width} px: Trade is in the phone menu, and the header does not overflow`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 800 });
    await openMarket(page, mock, { connect: false });
    await expect(page.getByTestId('menu-button')).toBeVisible();
    await expect(page.getByTestId('nav-trade')).toHaveCount(0); // closed: the tabs are inside the menu
    await page.getByTestId('menu-button').click();
    const tab = page.getByTestId('nav-trade');
    await expect(tab).toBeVisible();
    await expect(tab).toHaveAttribute('aria-current', 'page');
    const box = (await tab.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(width);
    // the header never overflows; the page below it is checked on the normal fonts only (a market title on the wide ones is its own matter, pair-header.spec.ts)
    expect(await page.locator('header.header').evaluate((h) => h.scrollWidth <= window.innerWidth)).toBe(true);
    if (process.env.KOB_WIDE_FONTS !== '1') expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    const header = (await page.locator('header.header').boundingBox())!;
    expect(header.height).toBeLessThan(64);
    // from the list, the menu's Trade leads back
    await page.getByTestId('menu-panel').getByTestId('nav-market').click();
    await expect(page.getByTestId('token-list-view')).toBeVisible();
    await page.getByTestId('menu-button').click();
    await page.getByTestId('nav-trade').click();
    await expect(page.getByTestId('order-ticket')).toBeVisible();
  });
}

// the extra tab must not break the one-row header at the widths where the tabs are shown (wide fonts: KOB_WIDE_FONTS=1 makes the row take the phone layout first)
for (const width of [1280, 1024, 900, 880]) {
  test(`${width} px: the header with the Trade tab is one row and does not scroll sideways`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 800 });
    await openMarket(page, mock, { connect: false });
    await expect.poll(async () => (await page.getByTestId('nav-trade').count()) + (await page.getByTestId('menu-button').count())).toBeGreaterThan(0);
    const wide = (await page.getByTestId('nav-trade').count()) === 1;
    const ids = wide ? ['brand', 'nav-market', 'nav-trade', 'nav-settings', 'network-chip', 'theme-toggle', 'notify-bell'] : ['brand', 'network-chip', 'notify-bell', 'menu-button'];
    const ys = await Promise.all(ids.map(async (id) => {
      const b = (await page.getByTestId(id).boundingBox())!;
      return b.y + b.height / 2;
    }));
    expect(Math.max(...ys) - Math.min(...ys)).toBeLessThan(6);
    expect((await page.locator('header.header').boundingBox())!.height).toBeLessThan(64);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });
}
