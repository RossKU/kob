// The lists against the mock stack: Market and the activity log turn pages of the chosen number of rows (the log reaches 100 rows back, Market every token), the log on a
// token or pair page shows that market, favourite tokens and pairs (stored per network, by covenant id), the searchable pair picker, and no table of pairs by default.
// `KOB_LISTS_SHOTS=<dir>` also saves screenshots (1280 and 390 px).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { connectWallet, fund, listExkcc, pickAsset, seedFavorites, showActivity } from '../helpers/env';
import { mockKascov, programHash, type MockKascovToken } from '../helpers/kascov';

test.use({ autoOpen: false });

const SHOTS = process.env.KOB_LISTS_SHOTS ?? '';
const QUOTE = 'e7'.repeat(32);
const TWIN = 'e8'.repeat(32);
const FAV_KEY = 'kob.favorites.v1:testnet-10';
const ID = (n: number) => n.toString(16).padStart(4, '0').repeat(16);
const MINT = programHash('KCC20PublicMint');
const many: MockKascovToken[] = Array.from({ length: 130 }, (_, i) => ({ covenant_id: ID(0x1000 + i), claimed_ticker: `T${String(i).padStart(3, '0')}`, claimed_name: `Token ${i}`, program: MINT }));

const tokenRows = (page: Page) => page.locator('[data-testid^="token-row-"]:not([data-testid="token-row-skeleton"])');
const logRows = (page: Page) => page.getByTestId('activity-list').getByTestId('activity-row');
const txids = (page: Page) => logRows(page).evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.txid ?? ''));
const rowIds = (page: Page) => tokenRows(page).evaluateAll((els) => els.map((e) => e.getAttribute('data-testid')));
const sideways = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
const stored = (page: Page) => page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? 'null'), FAV_KEY);

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  await page.mouse.move(0, 0);
  await page.waitForTimeout(1_000);
  await page.screenshot({ path: join(SHOTS, `${name}.png`) });
}

/** The example registry with EXKCC, EXUSD and a second token that also calls itself EXUSD (tickers are not unique). */
async function listTokens(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, tick: 100_000, display: { description: 'Fictional quote token of the e2e mock.' } });
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'A look-alike', covenant_id: TWIN, decimals: 6, tick: 100_000, display: { description: 'Fictional token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

async function seedPair(page: Page, mock: MockClient): Promise<string> {
  await mock.seed({ pair: true });
  await listTokens(page);
  return (await mock.token()).covenant_id as string;
}

/** More than the log reaches: EXKCC's history is well over 100 rows. */
async function seedLong(page: Page, mock: MockClient): Promise<string> {
  await mock.seedHistory({ hours: 6, trades: 160, seed: 7 });
  await listExkcc(page);
  return (await mock.token()).covenant_id as string;
}

test.describe('the log: a chosen number of rows, 100 rows deep', () => {
  test('a page shows the chosen rows; the pages end at 100 rows whatever the size; the size is kept', async ({ appPage: page, mock }) => {
    await seedLong(page, mock);
    await page.goto('/#/activity');
    await expect(page.getByTestId('activity-rows-50')).toHaveAttribute('aria-checked', 'true');
    await expect(logRows(page)).toHaveCount(50);
    await expect(page.getByTestId('activity-older')).toHaveCount(0);

    await page.getByTestId('activity-rows-20').click();
    await expect(logRows(page)).toHaveCount(20);
    await expect(page.getByTestId('activity-showing')).toContainText('1–20 of');
    await expect(page.getByTestId('activity-prev')).toBeDisabled();
    const seen: string[] = [];
    for (let i = 0; i < 5; i++) {
      await expect(logRows(page)).toHaveCount(20);
      await expect(page.getByTestId('activity-showing')).toContainText(`${i * 20 + 1}–${i * 20 + 20} of`);
      seen.push(...(await txids(page)));
      if (i < 4) await page.getByTestId('activity-next').click();
    }
    // five pages, no row twice, and nothing further
    expect(new Set(seen).size).toBe(100);
    await expect(page.getByTestId('activity-showing')).toHaveText('81–100 of 100');
    await expect(page.getByTestId('activity-next')).toBeDisabled();
    await page.getByTestId('activity-prev').click();
    await expect(page.getByTestId('activity-showing')).toHaveText('61–80 of 100');

    // another size starts at the first page
    await page.getByTestId('activity-rows-50').click();
    await expect(page.getByTestId('activity-showing')).toHaveText('1–50 of 100');
    await page.getByTestId('activity-next').click();
    await expect(page.getByTestId('activity-showing')).toHaveText('51–100 of 100');
    await expect(page.getByTestId('activity-next')).toBeDisabled();

    // 100 rows are one page, read on their own (the first read gives fewer), with nothing to turn
    await page.getByTestId('activity-rows-100').click();
    await expect(logRows(page)).toHaveCount(100);
    await expect(page.getByTestId('activity-pager')).toHaveCount(0);
    await page.reload();
    await expect(page.getByTestId('activity-rows-100')).toHaveAttribute('aria-checked', 'true');
    await expect(logRows(page)).toHaveCount(100);
  });

  test('a new row pushes the oldest off; paused it waits behind "+N new"; an older page stays put and the pill leads back', async ({ appPage: page, mock }) => {
    const token = await seedLong(page, mock);
    await page.goto('/#/activity');
    await page.getByTestId('activity-rows-100').click();
    await expect(logRows(page)).toHaveCount(100);
    await expect(page.getByTestId('activity-live')).toHaveAttribute('data-mode', 'live');
    const asks = (await mock.ordersOf('maker', 'active')).filter((o: { side: number; token: string }) => o.side === 1 && o.token === token);

    const before = await txids(page);
    await mock.fill(asks[0]!.covenant_id);
    await expect.poll(async () => (await txids(page))[0], { timeout: 15_000 }).not.toBe(before[0]);
    await expect(logRows(page)).toHaveCount(100);
    const after = await txids(page);
    expect(after).not.toContain(before[99]);
    expect(after[1]).toBe(before[0]);

    // the hold is as before
    await page.getByTestId('activity-pause').click();
    const top = (await txids(page))[0];
    await mock.fill(asks[1]!.covenant_id);
    await expect(page.getByTestId('activity-new')).toHaveText('+1 new', { timeout: 15_000 });
    await expect(logRows(page)).toHaveCount(100);
    expect((await txids(page))[0]).toBe(top);
    await page.getByTestId('activity-new').click();
    await expect.poll(async () => (await txids(page))[0]).not.toBe(top);

    // on an older page the rows stay where they are, and no row of the other pages counts as new
    await page.getByTestId('activity-rows-20').click();
    await expect(logRows(page)).toHaveCount(20);
    await page.getByTestId('activity-next').click();
    await expect(page.getByTestId('activity-showing')).toContainText('21–40 of');
    await expect(page.getByTestId('activity-live')).toHaveAttribute('data-mode', 'held');
    const second = await txids(page);
    await expect(page.getByTestId('activity-new')).toHaveCount(0);
    await mock.fill(asks[2]!.covenant_id);
    await expect(page.getByTestId('activity-new')).toHaveText('+1 new', { timeout: 15_000 });
    expect(await txids(page)).toEqual(second);
    await page.getByTestId('activity-new').click();
    await expect(page.getByTestId('activity-showing')).toContainText('1–20 of');
    await expect(page.getByTestId('activity-new')).toHaveCount(0);
    await expect(page.getByTestId('activity-live')).toHaveAttribute('data-mode', 'live');
  });

  test('a filter starts at the first page', async ({ appPage: page, mock }) => {
    await seedLong(page, mock);
    await page.goto('/#/activity');
    await page.getByTestId('activity-rows-20').click();
    await page.getByTestId('activity-next').click();
    await expect(page.getByTestId('activity-showing')).toContainText('21–40 of');
    await page.getByTestId('activity-kind-placed').click();
    await expect(page.locator('[data-testid="activity-showing"], [data-testid="activity-empty"]').first()).toBeVisible();
    await expect(page.getByTestId('activity-showing')).toContainText('1–');
    // the filter put back is the first page too, not the page left
    await page.getByTestId('activity-kind-placed').click();
    await expect(page.getByTestId('activity-showing')).toContainText('1–20 of');
  });

  test('the log on a token page: that market only, no market filter, 20 rows at first, capped the same way, its size kept apart from the Activity page', async ({ appPage: page, mock }) => {
    const token = await seedLong(page, mock);
    await page.goto(`/#/market/${token}`);
    await showActivity(page);
    const log = page.getByTestId('activity');
    await expect(log.getByTestId('activity-rows-20')).toHaveAttribute('aria-checked', 'true');
    await expect(logRows(page)).toHaveCount(20);
    await expect(log.getByTestId('activity-market')).toHaveCount(0);
    await expect(log.getByTestId('activity-all-link')).toHaveAttribute('href', '#/activity');
    expect([...new Set(await logRows(page).evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.market)))]).toEqual([token]);
    for (let i = 0; i < 4; i++) {
      await log.getByTestId('activity-next').click();
      await expect(log.getByTestId('activity-showing')).toContainText(`${(i + 1) * 20 + 1}–`);
    }
    await expect(log.getByTestId('activity-showing')).toHaveText('81–100 of 100');
    await expect(log.getByTestId('activity-next')).toBeDisabled();
    await log.getByTestId('activity-rows-100').click();
    await expect(logRows(page)).toHaveCount(100);
    await log.getByTestId('activity-all-link').click();
    await expect(page.getByTestId('activity-page')).toBeVisible();
    await expect(page.getByTestId('activity-rows-50')).toHaveAttribute('aria-checked', 'true');
  });

  test('a read further back that fails leaves the rows and stops asking', async ({ appPage: page, mock }) => {
    await seedLong(page, mock);
    let older = 0;
    await page.route(/\/v1\/(fills|orders)\?.*(before|cursor)=/, async (route) => {
      older++;
      await route.fulfill({ status: 500, contentType: 'application/json', body: '{"error":{"code":"internal","message":"boom"}}' });
    });
    await page.goto('/#/activity');
    await page.getByTestId('activity-rows-100').click();
    await expect(logRows(page).first()).toBeVisible();
    await expect.poll(() => older).toBeGreaterThan(0);
    await page.waitForTimeout(1_500);
    const after = older;
    expect(after).toBeLessThanOrEqual(12);
    await page.waitForTimeout(1_000);
    expect(older).toBe(after);
    expect(await logRows(page).count()).toBeGreaterThan(0);
  });
});

test.describe('Market: pages of the chosen size', () => {
  test('a page is exactly the chosen rows; previous, next and the position; only its rows load icons', async ({ appPage: page }) => {
    await mockKascov(page, { tokens: many });
    await page.goto('/#/market');
    await expect(tokenRows(page)).toHaveCount(20);
    await expect(page.getByTestId('token-list-showing')).toHaveText(/^1–20 of \d+$/);
    await expect(page.getByTestId('token-list-prev')).toBeDisabled();
    const first = await rowIds(page);
    await expect(page.locator('[data-testid^="token-icon-"]')).toHaveCount(20);
    await page.getByTestId('token-list-next').click();
    await expect(page.getByTestId('token-list-showing')).toHaveText(/^21–40 of \d+$/);
    await expect(tokenRows(page)).toHaveCount(20);
    await expect(page.locator('[data-testid^="token-icon-"]')).toHaveCount(20);
    expect((await rowIds(page)).filter((id) => first.includes(id))).toEqual([]);
    await page.getByTestId('token-list-prev').click();
    await expect(page.getByTestId('token-list-showing')).toHaveText(/^1–20 of \d+$/);
    expect(await rowIds(page)).toEqual(first);
    // the page does not grow with the list
    const height = await page.evaluate(() => document.documentElement.scrollHeight);
    expect(height).toBeLessThan(3_000);
  });

  test('every token stays reachable: the last page holds what is left, and no scroll adds rows', async ({ appPage: page }) => {
    await mockKascov(page, { tokens: many });
    await page.goto('/#/market');
    await page.getByTestId('token-list-rows-100').click();
    await expect(tokenRows(page)).toHaveCount(100);
    await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
    await page.waitForTimeout(300);
    await expect(tokenRows(page)).toHaveCount(100);
    await page.getByTestId('token-list-next').click();
    const text = await page.getByTestId('token-list-showing').innerText();
    const [, to, total] = text.match(/^101–(\d+) of (\d+)$/)!;
    expect(to).toBe(total);
    expect(Number(total)).toBeGreaterThan(130);
    await expect(page.getByTestId('token-list-next')).toBeDisabled();
    await expect(tokenRows(page)).toHaveCount(Number(total) - 100);
  });

  test('a new sort, search or size goes back to the first page; a page turned at the end of the list shows its start', async ({ appPage: page }) => {
    await page.setViewportSize({ width: 1280, height: 700 });
    await mockKascov(page, { tokens: many });
    await page.goto('/#/market');
    await expect(tokenRows(page)).toHaveCount(20);
    const onSecond = async () => {
      await page.getByTestId('token-list-next').click();
      await expect(page.getByTestId('token-list-showing')).toHaveText(/^21–40 of/);
    };
    const onFirst = () => expect(page.getByTestId('token-list-showing')).toHaveText(/^1–\d+ of/);
    await onSecond();
    await page.getByTestId('sort-market').first().click();
    await onFirst();
    await onSecond();
    await page.getByTestId('token-list-rows-50').click();
    await onFirst();
    await page.getByTestId('token-list-rows-20').click();
    await onSecond();
    await page.getByTestId('token-search').fill('T1');
    await onFirst();
    // a search taken back is the first page too, not the page left
    await page.getByTestId('token-search').fill('');
    await onFirst();
    // the search spans the pages
    await page.getByTestId('token-search').fill('T129');
    await expect(tokenRows(page)).toHaveCount(1);
    await page.getByTestId('token-search').fill('');
    await page.getByTestId('token-list-next').scrollIntoViewIfNeeded();
    await page.getByTestId('token-list-next').click();
    await expect(page.getByTestId('token-list-showing')).toHaveText(/^21–40 of/);
    const box = (await page.getByTestId('token-table').boundingBox())!;
    expect(box.y).toBeGreaterThan(0);
    expect(box.y).toBeLessThan(700);
  });

  test('the rows of a new page rise in and figures that arrive move none of them', async ({ appPage: page, mock }) => {
    await mockKascov(page, { tokens: many });
    await mock.seedHistory({ hours: 2, trades: 30, seed: 3 });
    await page.route('**/v1/stats/**', async (route) => {
      await new Promise((r) => setTimeout(r, 2_500));
      await route.fallback();
    });
    await page.goto('/#/market');
    await expect(tokenRows(page).first()).toBeVisible({ timeout: 2_400 });
    await page.getByTestId('token-list-next').click();
    await expect(page.getByTestId('token-list-showing')).toHaveText(/^21–40 of/);
    expect(await tokenRows(page).first().evaluate((tr) => getComputedStyle(tr).animationName)).toBe('ml-rise');
    expect(await tokenRows(page).first().evaluate((tr) => getComputedStyle(tr).animationFillMode)).toBe('backwards');
    const before = await rowIds(page);
    await page.waitForTimeout(300);
    expect(await rowIds(page)).toEqual(before);
  });
});

test.describe('favourite tokens', () => {
  test('a star on the row and on the token page, kept by covenant id per network; the Favorites view shows them and the search still works', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto('/#/market');
    const star = page.getByTestId(`token-favorite-${base}`);
    await expect(star).toHaveAttribute('aria-pressed', 'false');
    await star.click();
    await expect(star).toHaveAttribute('aria-pressed', 'true');
    expect(await stored(page)).toEqual({ tokens: [base], pairs: [] });

    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId('token-list-favorites')).toHaveAttribute('aria-pressed', 'true');
    await expect(tokenRows(page)).toHaveCount(1);
    await expect(page.getByTestId(`token-row-${base}`)).toBeVisible();
    await page.getByTestId('token-search').fill('exusd');
    await expect(page.getByTestId('token-list-empty')).toHaveText('No token matches your search.');
    await page.getByTestId('token-search').fill('');

    // the token page's star is the same favourite
    await page.getByTestId(`token-link-${base}`).click();
    const pageStar = page.getByTestId('token-page-favorite');
    await expect(pageStar).toHaveAttribute('aria-pressed', 'true');
    await pageStar.click();
    await expect(pageStar).toHaveAttribute('aria-pressed', 'false');
    expect(await stored(page)).toEqual({ tokens: [], pairs: [] });
    await pageStar.click();
    await page.reload();
    await expect(pageStar).toHaveAttribute('aria-pressed', 'true');

    // taken off in the view, the row goes
    await page.goto('/#/market');
    await page.getByTestId('token-list-favorites').click();
    await page.getByTestId(`token-favorite-${base}`).click();
    await expect(tokenRows(page)).toHaveCount(0);
    await expect(page.getByTestId('token-list-empty')).toHaveText('No favorites yet.');
    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId(`token-row-${base}`)).toBeVisible();
  });

  test('another network keeps its own favourites; a damaged record is none', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.addInitScript(([k, v]) => localStorage.setItem(k!, v!), ['kob.favorites.v1:mainnet', JSON.stringify({ tokens: ['ab'.repeat(32)], pairs: [] })]);
    await page.goto('/#/market');
    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId('token-list-empty')).toHaveText('No favorites yet.');
    await page.evaluate((k) => localStorage.setItem(k, '{not json'), FAV_KEY);
    await page.reload();
    await page.getByTestId(`token-favorite-${base}`).click();
    expect(await stored(page)).toEqual({ tokens: [base], pairs: [] });
  });

  test('a storage that throws: the stars work for the visit, nothing breaks', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.addInitScript((prefix) => {
      const get = Storage.prototype.getItem;
      const set = Storage.prototype.setItem;
      Storage.prototype.getItem = function (k: string) {
        if (k.startsWith(prefix)) throw new DOMException('blocked', 'SecurityError');
        return get.call(this, k);
      };
      Storage.prototype.setItem = function (k: string, v: string) {
        if (k.startsWith(prefix)) throw new DOMException('blocked', 'QuotaExceededError');
        set.call(this, k, v);
      };
    }, 'kob.favorites');
    await page.goto('/#/market');
    const star = page.getByTestId(`token-favorite-${base}`);
    await star.click();
    await expect(star).toHaveAttribute('aria-pressed', 'true');
    await page.getByTestId('token-list-favorites').click();
    await expect(tokenRows(page)).toHaveCount(1);
    await page.getByTestId(`token-link-${base}`).click();
    await expect(page.getByTestId('token-page-favorite')).toHaveAttribute('aria-pressed', 'true');
  });

  test('a favourite changes nothing else: no request names it, the ticket is the same', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await fund(mock, 'alice');
    await page.goto(`/#/market/${base}`);
    await connectWallet(page);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    const ticket = await page.getByTestId('order-ticket').innerText();
    const sent: string[] = [];
    page.on('request', (r) => {
      if (r.method() !== 'GET') sent.push(r.url());
    });
    await page.getByTestId('token-page-favorite').click();
    await page.waitForTimeout(500);
    expect(sent).toEqual([]);
    expect(await page.getByTestId('order-ticket').innerText()).toBe(ticket);
  });

  test('a favourite no list names keeps a row by its id, so its star can come off', async ({ appPage: page, mock }) => {
    await seedPair(page, mock);
    const gone = 'ab'.repeat(32);
    await seedFavorites(page, { tokens: [gone] });
    await page.goto('/#/market');
    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId(`token-row-${gone}`)).toBeVisible();
    await expect(page.getByTestId(`token-link-${gone}`)).toHaveAttribute('href', `#/market/${gone}`);
  });
});

test.describe('hidden tokens', () => {
  test('the eye hides a token from the list in this browser; the search still finds it; the Hidden view shows it and brings it back', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto('/#/market');
    const row = page.getByTestId(`token-row-${base}`);
    await expect(row).toBeVisible();
    await expect(page.getByTestId('token-list-hidden')).toHaveCount(0);
    // the table scrolls sideways only: a row rising in never gives it a vertical scrollbar
    expect(await page.locator('.ml-tokens').evaluate((e) => getComputedStyle(e).overflowY)).toBe('hidden');
    const before = await tokenRows(page).evaluateAll((rs) => rs.map((r) => r.getAttribute('data-testid')!));
    const below = before.slice(before.indexOf(`token-row-${base}`) + 1);
    await page.getByTestId(`token-hide-${base}`).click();
    // it leaves as rows come in, reversed: up and out, then the list drops it
    await expect(row).toHaveClass(/ml-out/);
    expect(await row.evaluate((e) => getComputedStyle(e).animationName)).toBe('ml-leave');
    await expect(row).toHaveCount(0);
    // the rows that stood below it rise in again from its place, the first of them at once
    for (const [k, id] of below.entries()) {
      await expect(page.getByTestId(id)).toHaveClass(/ml-again-a/);
      expect(await page.getByTestId(id).evaluate((e) => getComputedStyle(e).animationName)).toBe('ml-rise-a');
      if (k === 0) expect(await page.getByTestId(id).evaluate((e) => getComputedStyle(e).getPropertyValue('--i').trim())).toBe('0');
    }
    await expect(page.getByTestId('token-list-hidden')).toHaveText('Hidden (1)');
    // a search still finds a hidden token
    await page.getByTestId('token-search').fill('exkcc');
    await expect(row).toBeVisible();
    await page.getByTestId('token-search').fill('');
    await expect(row).toHaveCount(0);
    // kept by this browser
    await page.reload();
    await expect(page.getByTestId('token-list-hidden')).toHaveText('Hidden (1)');
    await expect(page.getByTestId(`token-row-${base}`)).toHaveCount(0);
    // the Hidden view lists it; its eye, pressed there, puts it back in the list
    await page.getByTestId('token-list-hidden').click();
    await expect(tokenRows(page)).toHaveCount(1);
    await expect(page.getByTestId(`token-hide-${base}`)).toHaveAttribute('aria-pressed', 'true');
    await page.getByTestId(`token-hide-${base}`).click();
    await expect(page.getByTestId('token-list-empty')).toHaveText('No hidden tokens.');
    await page.getByTestId('token-list-hidden').click();
    await expect(page.getByTestId(`token-row-${base}`)).toBeVisible();
    await expect(page.getByTestId('token-list-hidden')).toHaveCount(0);
  });
});

test.describe('the pair picker', () => {
  test('two tokens by ticker, name or id lead straight to the pair; a ticker two tokens share shows both ids; no KAS', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${base}`)).toBeVisible();
    await page.getByTestId('pair-pick-base').click();
    await expect(page.getByTestId('pair-pick-base-option-kas')).toHaveCount(0);
    await page.getByTestId('pair-pick-base-search').fill('exkcc');
    await expect(page.getByTestId(`pair-pick-base-option-${base}`)).toContainText(`EXKCC (${base.slice(0, 4)}…${base.slice(-4)})`);
    await page.getByTestId(`pair-pick-base-option-${base}`).click();
    // the second choice opens by itself, without the first token
    await expect(page.getByTestId('pair-pick-quote-panel')).toBeVisible();
    await expect(page.getByTestId(`pair-pick-quote-option-${base}`)).toHaveCount(0);
    await page.getByTestId('pair-pick-quote-search').fill('exusd');
    await expect(page.getByTestId(`pair-pick-quote-option-${QUOTE}`)).toContainText('EXUSD (e7e7…e7e7)');
    await expect(page.getByTestId(`pair-pick-quote-option-${TWIN}`)).toContainText('EXUSD (e8e8…e8e8)');
    // by name, by covenant id; nothing for a text that matches no token
    await page.getByTestId('pair-pick-quote-search').fill('look-alike');
    await expect(page.locator('[data-testid^="pair-pick-quote-option-"]')).toHaveCount(1);
    await page.getByTestId('pair-pick-quote-search').fill('zzzz');
    await expect(page.getByTestId('pair-pick-quote-empty')).toBeVisible();
    await page.getByTestId('pair-pick-quote-search').fill(TWIN);
    await page.getByTestId(`pair-pick-quote-option-${TWIN}`).click();
    await expect(page).toHaveURL(new RegExp(`#/market/${base}/${TWIN}$`));
    await expect(page.getByTestId('pair-page')).toBeVisible();
    await expect(page.getByTestId('pair-pick-quote-panel')).toHaveCount(0);
  });

  test('the keyboard: arrows and Enter choose, Escape closes and gives the focus back', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto('/#/market');
    await page.getByTestId('pair-pick-base').focus();
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('pair-pick-base-search')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('pair-pick-base-panel')).toHaveCount(0);
    await expect(page.getByTestId('pair-pick-base')).toBeFocused();
    await page.keyboard.press('Enter');
    await page.keyboard.type('exusd');
    await expect(page.locator('[data-testid^="pair-pick-base-option-"]')).toHaveCount(2);
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('pair-pick-base')).toHaveAttribute('data-value', TWIN);
    await expect(page.getByTestId('pair-pick-quote-search')).toBeFocused();
    await page.keyboard.type(base);
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(new RegExp(`#/market/${TWIN}/${base}$`));
  });

  test('every row of a picker is as high as the next, with a name line or without', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('pair-page')).toBeVisible();
    await page.getByTestId('market-base-select').click();
    const rows = page.locator('[data-testid^="market-base-select-option-"]');
    await expect.poll(() => rows.count()).toBeGreaterThan(1);
    const heights = await rows.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().height));
    expect(new Set(heights).size).toBe(1);
    expect(heights[0]).toBeGreaterThanOrEqual(44);
    await page.keyboard.press('Escape');
  });

  test('a press outside closes it; a long list is cut and the search reaches the rest', async ({ appPage: page }) => {
    await mockKascov(page, { tokens: many });
    await page.goto('/#/market');
    await expect(tokenRows(page)).toHaveCount(20);
    await page.getByTestId('pair-pick-base').click();
    await expect(page.locator('[data-testid^="pair-pick-base-option-"]')).toHaveCount(50);
    await expect(page.getByTestId('pair-pick-base-more')).toContainText('more: keep typing');
    await page.getByTestId('pair-pick-base-search').fill('T129');
    await expect(page.locator('[data-testid^="pair-pick-base-option-"]')).toHaveCount(1);
    await page.getByTestId('token-table').click({ position: { x: 5, y: 5 } });
    await expect(page.getByTestId('pair-pick-base-panel')).toHaveCount(0);
  });

  test('the title bar uses the same picker', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('pair-page')).toBeVisible();
    await page.getByTestId('market-quote-select').click();
    await page.getByTestId('market-quote-select-search').fill('look-alike');
    await page.getByTestId(`market-quote-select-option-${TWIN}`).click();
    await expect(page).toHaveURL(new RegExp(`#/market/${base}/${TWIN}$`));
    await pickAsset(page, 'market-quote-select', 'kas');
    await expect(page).toHaveURL(new RegExp(`#/market/${base}$`));
    await expect(page.getByTestId('token-page')).toBeVisible();
  });
});

test.describe('favourite pairs, and no table of pairs by default', () => {
  test('nothing is read for pairs on the list by default; only a favourite pair is, and only in its view', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    const pairReads: string[] = [];
    page.on('request', (r) => {
      if (/\/v1\/pairs/.test(r.url())) pairReads.push(new URL(r.url()).pathname);
    });
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${base}`)).toHaveAttribute('data-state', 'ready');
    await page.waitForTimeout(500);
    await expect(page.getByTestId('pair-list')).toHaveCount(0);
    await expect(page.getByTestId('pair-table')).toHaveCount(0);
    await expect(page.getByText('Token pairs')).toHaveCount(0);
    expect(pairReads).toEqual([]);

    // a favourite of two pairs the seed has one of: the figures of those two only
    await page.evaluate(([k, v]) => localStorage.setItem(k!, v!), [FAV_KEY, JSON.stringify({ tokens: [], pairs: [{ base, quote: QUOTE }] })]);
    await page.reload();
    await expect(page.getByTestId(`token-row-${base}`)).toHaveAttribute('data-state', 'ready');
    await page.waitForTimeout(500);
    expect(pairReads).toEqual([]);
    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId(`pair-row-${base}-${QUOTE}`)).toHaveAttribute('data-state', 'ready');
    // both orientations of the pair are read (they are one market: its book and its volume count the orders of QUOTE/BASE too)
    expect([...new Set(pairReads)].sort()).toEqual(
      ['/v1/pairs', `/v1/pairs/${base}/${QUOTE}/book`, `/v1/pairs/${base}/${QUOTE}/fills`, `/v1/pairs/${QUOTE}/${base}/book`, `/v1/pairs/${QUOTE}/${base}/fills`].sort(),
    );
  });

  test('a star on the pair page, kept by covenant ids, whichever way round; the Favorites view lists the pair with its figures', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    // a price needs fills of both KAS markets
    await mock.seed({ trades: [{ token: QUOTE, ago_ms: 60_000, legs: [{ side: 'bid', price: 50_200_000, amount: 50_000_000 }] }] });
    await page.goto(`/#/market/${base}/${QUOTE}`);
    const star = page.getByTestId('pair-favorite');
    await expect(star).toHaveAttribute('aria-pressed', 'false');
    await star.click();
    await expect(star).toHaveAttribute('aria-pressed', 'true');
    expect(await stored(page)).toEqual({ tokens: [], pairs: [{ base, quote: QUOTE }] });
    // the flip shows the same pair
    await page.getByTestId('market-flip').click();
    await expect(page).toHaveURL(new RegExp(`#/market/${QUOTE}/${base}$`));
    await expect(page.getByTestId('pair-favorite')).toHaveAttribute('aria-pressed', 'true');

    await page.goto('/#/market');
    await page.getByTestId('token-list-favorites').click();
    const id = `${base}-${QUOTE}`;
    const row = page.getByTestId(`pair-row-${id}`);
    await expect(row).toHaveAttribute('data-state', 'ready');
    await expect(page.getByTestId(`pair-link-${id}`)).toHaveAttribute('href', `#/market/${base}/${QUOTE}`);
    await expect(row.getByTestId(`pair-last-${id}`)).toHaveAttribute('title', /\(EXUSD per EXKCC\)$/);
    await expect(row.getByTestId(`pair-depth-${id}`)).toHaveAttribute('title', /3 asks · 2 bids · 1 conditional/);
    // pairs only: no empty token table above them
    await expect(page.getByTestId('token-table')).toHaveCount(0);
    await page.getByTestId(`pair-favorite-${id}`).click();
    await expect(row).toHaveCount(0);
    await expect(page.getByTestId('token-list-empty')).toHaveText('No favorites yet.');
    expect(await stored(page)).toEqual({ tokens: [], pairs: [] });
  });

  test('a pair with no live orders has a row too; tokens and pairs share the view', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await seedFavorites(page, { tokens: [base], pairs: [{ base, quote: TWIN }] });
    await page.goto('/#/market');
    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId(`token-row-${base}`)).toBeVisible();
    const row = page.getByTestId(`pair-row-${base}-${TWIN}`);
    await expect(row).toBeVisible();
    await expect(row).not.toHaveAttribute('data-state', 'loading');
  });
});

test.describe('a phone', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('no sideways scroll on the list, the picker, the Favorites view and the log; a star is a target a finger can hit', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await mock.seedHistory({ hours: 2, trades: 60, seed: 3 });
    await mockKascov(page, { tokens: many });
    await seedFavorites(page, { tokens: [base], pairs: [{ base, quote: QUOTE }] });
    await page.goto('/#/market');
    await expect(tokenRows(page)).toHaveCount(20);
    expect(await sideways(page)).toBeLessThanOrEqual(0);
    const star = (await page.getByTestId(`token-favorite-${base}`).boundingBox())!;
    expect(star.width).toBeGreaterThanOrEqual(44);
    expect(star.height).toBeGreaterThanOrEqual(32);
    // the star leaves the price and the change alone
    const change = (await page.getByTestId(`token-change-${base}`).boundingBox())!;
    expect(star.y).toBeGreaterThanOrEqual(change.y + change.height - 2);
    await shot(page, 'market-390');

    await page.getByTestId('pair-pick-quote').click();
    const panel = (await page.getByTestId('pair-pick-quote-panel').boundingBox())!;
    expect(panel.x).toBeGreaterThanOrEqual(0);
    expect(panel.x + panel.width).toBeLessThanOrEqual(390);
    expect(await sideways(page)).toBeLessThanOrEqual(0);
    const option = (await page.locator('[data-testid^="pair-pick-quote-option-"]').first().boundingBox())!;
    expect(option.height).toBeGreaterThanOrEqual(44);
    await shot(page, 'picker-390');
    await page.keyboard.press('Escape');

    await page.getByTestId('token-list-next').scrollIntoViewIfNeeded();
    const next = (await page.getByTestId('token-list-next').boundingBox())!;
    expect(next.height).toBeGreaterThanOrEqual(44);

    await page.getByTestId('token-list-favorites').click();
    await expect(page.getByTestId(`pair-row-${base}-${QUOTE}`)).toBeVisible();
    expect(await sideways(page)).toBeLessThanOrEqual(0);
    await shot(page, 'market-favorites-390');

    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('pair-page')).toBeVisible();
    const pairStar = (await page.getByTestId('pair-favorite').boundingBox())!;
    expect(pairStar.width).toBeGreaterThanOrEqual(44);
    expect(pairStar.height).toBeGreaterThanOrEqual(44);
    expect(await sideways(page)).toBeLessThanOrEqual(0);
    await page.getByTestId('market-base-select').click();
    expect(await sideways(page)).toBeLessThanOrEqual(0);
    await shot(page, 'pair-picker-390');
    await page.keyboard.press('Escape');

    await page.goto('/#/activity');
    await expect(logRows(page).first()).toBeVisible();
    expect(await sideways(page)).toBeLessThanOrEqual(0);
    await page.getByTestId('activity-rows-20').click();
    await expect(page.getByTestId('activity-rows')).toBeVisible();
    await shot(page, 'activity-390');
  });
});

test('at 1280 px', async ({ appPage: page, mock }) => {
  test.skip(!SHOTS, 'screenshots only');
  await page.setViewportSize({ width: 1280, height: 900 });
  const base = await seedPair(page, mock);
  await mock.seedHistory({ hours: 2, trades: 60, seed: 3 });
  await mockKascov(page, { tokens: many });
  await seedFavorites(page, { tokens: [base], pairs: [{ base, quote: QUOTE }] });
  await page.goto('/#/market');
  await expect(tokenRows(page)).toHaveCount(20);
  await shot(page, 'market-1280');
  await page.getByTestId('pair-pick-base').click();
  await shot(page, 'picker-1280');
  await page.keyboard.press('Escape');
  await page.getByTestId('token-list-favorites').click();
  await expect(page.getByTestId(`pair-row-${base}-${QUOTE}`)).toBeVisible();
  await shot(page, 'market-favorites-1280');
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('pair-page')).toBeVisible();
  await page.getByTestId('market-base-select').click();
  await shot(page, 'pair-picker-1280');
  await page.keyboard.press('Escape');
  await page.goto('/#/activity');
  await expect(logRows(page).first()).toBeVisible();
  await shot(page, 'activity-1280');
});
