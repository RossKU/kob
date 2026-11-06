// The activity log (ui/activity): the Activity page (#/activity, every market or the one in its URL) and the same log on the token and pair pages,
// against the mock indexer: one row per transaction, newest first, each row a link to the transaction on tn10.kaspa.stream (new tab, noopener);
// fills and placements (phase 1), the Vis filters and their persistence, the market filter in the URL, live rows, pause and the "+N new" hold,
// a phone layout without horizontal scroll. `KOB_ACTIVITY_SHOTS=<dir>` also saves screenshots (1440 and 390 px, dark and light).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { connectWallet, listExkcc, showActivity } from '../helpers/env';

test.use({ autoOpen: false });

const SHOTS = process.env.KOB_ACTIVITY_SHOTS ?? '';
const QUOTE = 'e7'.repeat(32);
const TX_URL = /^https:\/\/tn10\.kaspa\.stream\/transactions\/[0-9a-f]{64}$/;

/** The example registry with EXKCC and a second listed KCC-20, EXUSD (6 decimals), matching the mock pair seed. */
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

/** The default book with a little history (fills) of EXKCC; returns its covenant id. */
async function seedToken(page: Page, mock: MockClient): Promise<string> {
  await mock.seedHistory({ hours: 2, trades: 30, seed: 3 });
  await listExkcc(page);
  return (await mock.token()).covenant_id as string;
}

/** The pair seed with one fill of a resting pair ask and one of a resting pair bid; returns the EXKCC covenant id. */
async function seedPair(page: Page, mock: MockClient): Promise<string> {
  await mock.seed({ pair: true });
  await mock.seedHistory({ hours: 2, trades: 20, seed: 4 });
  await listPair(page);
  const base = (await mock.token()).covenant_id as string;
  const orders = await mock.orders('status=active&limit=200');
  const ask = orders.find((o) => o.contract === 'KobPair' && o.pair?.side === 'ask' && o.pair?.price === '50500')!;
  const bid = orders.find((o) => o.contract === 'KobPair' && o.pair?.side === 'bid')!;
  await mock.fill(ask.covenant_id);
  await mock.fill(bid.covenant_id);
  return base;
}

const rows = (page: Page) => page.getByTestId('activity-list').getByTestId('activity-row');
const attrs = (page: Page, name: string) => rows(page).evaluateAll((els, n) => els.map((e) => (e as HTMLElement).getAttribute(n) ?? ''), name);

async function shot(page: Page, name: string, theme: 'dark' | 'light', fullPage = true): Promise<void> {
  if (!SHOTS) return;
  await page.evaluate((th) => document.documentElement.setAttribute('data-theme', th), theme);
  // a phone: the screen with the log at its top
  if (!fullPage) await page.getByTestId('activity').first().evaluate((e) => { e.scrollIntoView({ block: 'start' }); window.scrollBy(0, -72); });
  await page.mouse.move(0, 0);
  await page.waitForTimeout(400);
  await page.screenshot({ path: join(SHOTS, `impl-${name}-${theme}.png`), fullPage });
}

test.describe('activity page', () => {
  test.use({ viewport: { width: 1440, height: 1000 } });

  test('every market, newest first, one row per transaction; each row opens the transaction on tn10.kaspa.stream in a new tab', async ({ appPage: page, mock }) => {
    await seedToken(page, mock);
    await page.goto('/#/activity');
    await expect(page.getByTestId('nav-activity')).toHaveAttribute('aria-current', 'page');
    await expect(page).toHaveTitle(/^Activity - KOB/);
    await expect(rows(page).first()).toBeVisible();
    const kinds = await attrs(page, 'data-kind');
    expect(kinds).toContain('fill');
    expect(kinds).toContain('placed');
    // one row per transaction
    const txids = await attrs(page, 'data-txid');
    expect(new Set(txids).size).toBe(txids.length);
    // every row is a real link: keyboard and middle click work, a new tab without the opener
    const first = rows(page).first();
    expect(await first.evaluate((e) => e.tagName)).toBe('A');
    await expect(first).toHaveAttribute('href', TX_URL);
    await expect(first).toHaveAttribute('target', '_blank');
    await expect(first).toHaveAttribute('rel', 'noopener noreferrer');
    expect(await first.getAttribute('href')).toContain(txids[0]!);
    await expect(first).toHaveAttribute('aria-label', /opens tn10\.kaspa\.stream in a new tab/);
    // the columns: market icons, short tx, wallet, amount (+ KAS value), price, confirmations, time
    const fill = rows(page).and(page.locator('[data-kind="fill"]')).first();
    // the whole id (the column shows as much as fits, then an ellipsis), whole in its title too
    await expect(fill.locator('.act-c-tx')).toHaveText(/^[0-9a-f]{64}$/);
    await expect(fill.locator('.act-c-tx')).toHaveAttribute('title', /^[0-9a-f]{64}$/);
    await expect(fill.locator('.act-c-amt')).toContainText('EXKCC');
    await expect(fill.locator('.act-c-amt')).toContainText('KAS');
    await expect(fill.locator('.act-c-px')).toHaveText(/^0\.0\d+$/);
    await expect(fill.locator('.act-c-time')).toHaveText(/^\d{2}(:\d{2}:\d{2}|-\d{2} \d{2}:\d{2})$/);
    // the owners of the filled orders are looked up: a short address (the full one in the title)
    await expect(fill.getByTestId('activity-wallet')).toHaveText(/^q[0-9a-z]{40,}/, { timeout: 15_000 });
    await expect(fill.getByTestId('activity-wallet')).toHaveAttribute('title', /kaspatest:/);
    // the legend under the table: phase 1 kinds and the confirmation marks
    await expect(page.getByTestId('activity-legend')).toContainText('fill, taker bought');
    await expect(page.getByTestId('activity-legend')).toContainText('placed (order owner)');
    await expect(page.getByTestId('activity-legend')).toContainText('settled');
    // the page's own log has no "All activity" link
    await expect(page.getByTestId('activity-all-link')).toHaveCount(0);
    await shot(page, 'activity-1440', 'dark');
    await shot(page, 'activity-1440', 'light');
  });

  test('Vis: kinds (phase 1: placed and fills), the quick switch, "mine only"; remembered per viewer', async ({ appPage: page, mock }) => {
    await seedToken(page, mock);
    await page.goto('/#/activity');
    await expect(rows(page).first()).toBeVisible();
    // Orders = signed by the order owner (placements); Executions = fills
    await page.getByTestId('activity-actor-orders').click();
    await expect.poll(async () => [...new Set(await attrs(page, 'data-kind'))]).toEqual(['placed']);
    await page.getByTestId('activity-actor-executions').click();
    await expect.poll(async () => [...new Set(await attrs(page, 'data-kind'))]).toEqual(['fill']);
    await page.getByTestId('activity-actor-all').click();
    // the Vis panel offers only what the current API lists, and says what comes with the indexer update
    await page.getByTestId('activity-vis').click();
    const panel = page.getByTestId('activity-vis-panel');
    await expect(panel.getByRole('checkbox')).toHaveCount(3);
    await expect(panel.locator('label')).toHaveText(['Placed orders', 'Fills', 'Mine only: connect a wallet first']);
    await expect(panel).toContainText('need an update of the indexer');
    await shot(page, 'activity-vis', 'dark', false);
    await page.getByTestId('activity-vis-fill').uncheck();
    await expect.poll(async () => [...new Set(await attrs(page, 'data-kind'))]).toEqual(['placed']);
    await expect(page.getByTestId('activity-kind-fill')).toHaveAttribute('aria-pressed', 'false');
    // remembered: after a reload the fills stay hidden
    await page.reload();
    await expect(rows(page).first()).toBeVisible();
    expect([...new Set(await attrs(page, 'data-kind'))]).toEqual(['placed']);
    expect(JSON.parse((await page.evaluate(() => localStorage.getItem('kob.activity.v1')))!).page.kinds.fill).toBe(false);
    await page.getByTestId('activity-kind-fill').click();
    await expect.poll(async () => (await attrs(page, 'data-kind')).includes('fill')).toBe(true);
    // mine only: the connected wallet's orders (alice has none in the seed: the list empties); disabled without a wallet
    await expect(page.getByTestId('activity-mine')).toBeDisabled();
    await connectWallet(page, 'kasware');
    await expect(page.getByTestId('activity-mine')).toBeEnabled();
    await page.getByTestId('activity-mine').click();
    await expect(page.getByTestId('activity-empty')).toHaveText('Nothing matches the filters.');
    await page.getByTestId('activity-mine').click();
    await expect(rows(page).first()).toBeVisible();
  });

  test('the market filter is in the URL: a token page and the Activity page share links', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto('/#/activity');
    await expect(rows(page).first()).toBeVisible();
    // every market: the pair's fills and EXKCC's KAS book
    await expect.poll(async () => new Set(await attrs(page, 'data-market')).has(`${base}/${QUOTE}`)).toBe(true);
    await page.getByTestId('activity-market').selectOption(base);
    await expect(page).toHaveURL(new RegExp(`#/activity\\?market=${base}$`));
    await expect.poll(async () => [...new Set(await attrs(page, 'data-market'))]).toEqual([base]);
    await page.goto(`/#/activity?market=${base}/${QUOTE}`);
    await expect(page.getByTestId('activity-market')).toHaveValue(`${base}/${QUOTE}`);
    await expect.poll(async () => [...new Set(await attrs(page, 'data-market'))]).toEqual([`${base}/${QUOTE}`]);
    await page.getByTestId('activity-market').selectOption('all');
    await expect(page).toHaveURL(/#\/activity$/);
  });

  test('live: a new fill arrives at the top; paused or pointed at, it is held behind "+N new" until released', async ({ appPage: page, mock }) => {
    const token = await seedToken(page, mock);
    await page.goto('/#/activity');
    await expect(page.getByTestId('activity-live')).toHaveAttribute('data-mode', 'live');
    await expect(rows(page).first()).toBeVisible();
    const asks = (await mock.ordersOf('maker', 'active')).filter((o: { side: number; token: string }) => o.side === 1 && o.token === token);
    // live
    const before = (await attrs(page, 'data-txid'))[0];
    await mock.fill(asks[0]!.covenant_id);
    await expect.poll(async () => (await attrs(page, 'data-txid'))[0], { timeout: 15_000 }).not.toBe(before);
    await expect(rows(page).first()).toHaveAttribute('data-kind', 'fill');
    await expect(rows(page).first()).toHaveAttribute('data-side', 'buy');
    // paused: the new row waits, counted
    await page.getByTestId('activity-pause').click();
    await expect(page.getByTestId('activity-live')).toHaveAttribute('data-mode', 'held');
    const top = (await attrs(page, 'data-txid'))[0];
    await mock.fill(asks[1]!.covenant_id);
    await expect(page.getByTestId('activity-new')).toHaveText('+1 new', { timeout: 15_000 });
    expect((await attrs(page, 'data-txid'))[0]).toBe(top);
    await shot(page, 'activity-paused', 'dark', false);
    await page.getByTestId('activity-new').click();
    await expect(page.getByTestId('activity-new')).toHaveCount(0);
    await expect.poll(async () => (await attrs(page, 'data-txid'))[0]).not.toBe(top);
    await expect(page.getByTestId('activity-pause')).toHaveAttribute('aria-pressed', 'false');
    // pointed at: held while the pointer is over the list, released when it leaves
    const top2 = (await attrs(page, 'data-txid'))[0];
    await rows(page).nth(2).hover();
    await mock.fill(asks[2]!.covenant_id);
    await expect(page.getByTestId('activity-new')).toHaveText('+1 new', { timeout: 15_000 });
    expect((await attrs(page, 'data-txid'))[0]).toBe(top2);
    await page.mouse.move(5, 5);
    await expect.poll(async () => (await attrs(page, 'data-txid'))[0]).not.toBe(top2);
    await expect(page.getByTestId('activity-new')).toHaveCount(0);
  });
});

test.describe('the log on the token and pair pages', () => {
  test.use({ viewport: { width: 1440, height: 1000 } });

  test('token page: the same log, preset to the token, full width under the exchange grid; the depth chart has the freed cell', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto(`/#/market/${base}`);
    const log = page.getByTestId('token-page').getByTestId('activity');
    // under the token page the log reads nothing until it is on screen
    await showActivity(page);
    await expect(log.getByTestId('activity-row').first()).toBeVisible();
    await expect(log.getByTestId('activity-market')).toHaveValue(base);
    await expect.poll(async () => [...new Set(await attrs(page, 'data-market'))]).toEqual([base]);
    await expect(log.getByTestId('activity-all-link')).toHaveAttribute('href', `#/activity?market=${base}`);
    // layout: under the chart, the depth chart and the book, above the facts; as wide as the grid; the depth chart spans the two middle columns
    const box = async (id: string) => (await page.getByTestId(id).first().boundingBox())!;
    const [grid, act, depth, chart, details] = [(await page.locator('.mkt-grid').boundingBox())!, await box('activity'), await box('depth-section'), await box('chart-section'), (await page.locator('.mkt-area-details').boundingBox())!];
    expect(act.y).toBeGreaterThan(depth.y + depth.height - 1);
    expect(details.y).toBeGreaterThan(act.y + act.height - 1);
    expect(Math.abs(act.width - grid.width)).toBeLessThan(2);
    expect(Math.abs(depth.width - chart.width)).toBeLessThan(2);
    // the viewer can widen the filter: every market (the pair's fills appear), and back
    await log.getByTestId('activity-market').selectOption('all');
    await expect.poll(async () => new Set(await attrs(page, 'data-market')).has(`${base}/${QUOTE}`)).toBe(true);
    await expect(log.getByTestId('activity-all-link')).toHaveAttribute('href', '#/activity');
    await log.getByTestId('activity-market').selectOption(base);
    await expect.poll(async () => [...new Set(await attrs(page, 'data-market'))]).toEqual([base]);
    await shot(page, 'token-1440', 'dark');
    await shot(page, 'token-1440', 'light');
    // the link opens the Activity page with the same filter
    await log.getByTestId('activity-all-link').click();
    await expect(page).toHaveURL(new RegExp(`#/activity\\?market=${base}$`));
    await expect(page.getByTestId('activity-page').getByTestId('activity-market')).toHaveValue(base);
  });

  test('pair page: the pair\'s fills (both sides, A and B amounts, no price: a pair fill never makes one), its placements', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto(`/#/market/${base}/${QUOTE}`);
    const log = page.getByTestId('pair-page').getByTestId('activity');
    await showActivity(page);
    await expect(log.getByTestId('activity-market')).toHaveValue(`${base}/${QUOTE}`);
    const fills = log.locator('[data-testid="activity-row"][data-kind="fill"]');
    await expect(fills).toHaveCount(2);
    expect([...(await fills.evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.side)))].sort()).toEqual(['buy', 'sell']);
    for (const r of await fills.all()) {
      await expect(r).toHaveAttribute('data-market', `${base}/${QUOTE}`);
      await expect(r.locator('.act-c-px')).toHaveText('—');
      await expect(r.locator('.act-c-amt b')).toContainText('EXKCC');
      await expect(r.locator('.act-sub')).toContainText('EXUSD');
      await expect(r).toHaveAttribute('href', TX_URL);
      // two token icons: A / B
      await expect(r.locator('.act-c-mk .tk-icon')).toHaveCount(2);
    }
    await expect(log.locator('[data-testid="activity-row"][data-kind="placed"]').first()).toBeVisible();
    await expect(log.getByTestId('activity-all-link')).toHaveAttribute('href', `#/activity?market=${base}/${QUOTE}`);
    await shot(page, 'pair-1440', 'dark');
    await shot(page, 'pair-1440', 'light');
  });
});

test.describe('activity on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('two-line rows (icon, market and tx over the wallet; amount over the time), no horizontal scroll', async ({ appPage: page, mock }) => {
    const base = await seedPair(page, mock);
    await page.goto('/#/activity');
    await expect(rows(page).first()).toBeVisible();
    const overflow = async () => page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(await overflow()).toBe(0);
    const r = rows(page).first();
    await expect(r.locator('.act-c-tx')).toBeHidden();
    await expect(r.locator('.act-c-px')).toBeHidden();
    await expect(r.locator('.act-txm')).toBeVisible();
    const [mk, who, amt, time] = await Promise.all(['.act-c-mk', '.act-c-who', '.act-c-amt', '.act-c-time'].map(async (s) => (await r.locator(s).boundingBox())!));
    expect(who.y).toBeGreaterThan(mk.y + mk.height - 2);
    expect(time.y).toBeGreaterThan(amt.y + 4);
    expect(amt.x).toBeGreaterThan(mk.x);
    await shot(page, 'activity-390', 'dark', false);
    await shot(page, 'activity-390', 'light', false);
    await page.goto(`/#/market/${base}`);
    await showActivity(page);
    await expect(page.getByTestId('activity').getByTestId('activity-row').first()).toBeVisible();
    expect(await overflow()).toBe(0);
    await shot(page, 'token-390', 'dark', false);
    await shot(page, 'token-390', 'light', false);
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await showActivity(page);
    await expect(page.getByTestId('activity').getByTestId('activity-row').first()).toBeVisible();
    expect(await overflow()).toBe(0);
    await shot(page, 'pair-390', 'dark', false);
  });
});

// the table fits its card at every width: narrower cards drop the pie, then the Wallet column, then take two lines per row; never a sideways scroll
const WIDTHS = [390, 760, 887, 1000, 1100, 1280, 1440, 1774];
test('the log fits its card at every width, on the Activity page and under a token page (no element past the card, no page scroll sideways)', async ({ appPage: page, mock }) => {
  test.setTimeout(120_000);
  const token = await seedPair(page, mock);
  const outside = () =>
    page.evaluate(() => {
      const card = document.querySelector('[data-testid="activity"]')!.getBoundingClientRect();
      const bad: string[] = [];
      for (const e of document.querySelectorAll('[data-testid="activity"] *')) {
        const r = e.getBoundingClientRect();
        if (r.width === 0 || (e as HTMLElement).closest('.hp-panel')) continue;
        if (r.right > card.right + 1 || r.left < card.left - 1) bad.push(`${e.tagName}.${String((e as HTMLElement).className).slice(0, 30)} ${Math.round(r.left)}-${Math.round(r.right)} card ${Math.round(card.left)}-${Math.round(card.right)}`);
      }
      return { bad: bad.slice(0, 5), scrollX: document.documentElement.scrollWidth - window.innerWidth };
    });
  for (const route of ['/#/activity', `/#/market/${token}`]) {
    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(route);
      await showActivity(page);
      await expect(rows(page).first()).toBeVisible();
      await expect.poll(outside, { message: `${route} at ${width} px` }).toEqual({ bad: [], scrollX: 0 });
    }
  }
});

// live updates move nothing on the page: rows sit at fixed heights and slide by a transform, a row is never re-created, the tools keep their place
test('20 live updates: no layout shift (CLS 0), rows kept (not re-created), the tools still', async ({ appPage: page, mock }) => {
  test.setTimeout(120_000);
  const token = await seedToken(page, mock);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`/#/market/${token}`);
  await showActivity(page);
  await expect(rows(page)).toHaveCount(20);
  await page.mouse.move(2, 2);
  await page.waitForTimeout(1500);
  // the tools within the log's card (the book above changes height as its orders fill: the whole card may move, its contents must not)
  const tools = async () => { const [t, c] = [(await page.locator('.act-tools').boundingBox())!, (await page.getByTestId('activity').boundingBox())!]; return { x: t.x - c.x, y: t.y - c.y }; };
  const toolsBefore = await tools();
  await page.evaluate(() => {
    const w = window as unknown as { __cls: { value: number; log: number; sources: string[] } };
    w.__cls = { value: 0, log: 0, sources: [] };
    new PerformanceObserver((list) => {
      for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean; sources?: { node?: Node | null }[] }[]) {
        if (e.hadRecentInput) continue;
        w.__cls.value += e.value;
        const log = document.querySelector('[data-testid="activity"]');
        for (const s of e.sources ?? []) {
          const n = s.node as HTMLElement | null;
          if (n && log?.contains(n)) w.__cls.log += e.value;
          w.__cls.sources.push(`${n && log?.contains(n) ? 'log:' : ''}${n?.nodeName ?? '?'}.${n?.className?.toString().slice(0, 40) ?? ''}[${(n?.textContent ?? '').slice(0, 20)}]`);
        }
      }
    }).observe({ type: 'layout-shift', buffered: false });
    // every row now on screen is marked: a re-created row would lose the mark
    for (const r of document.querySelectorAll('[data-testid="activity-row"]')) (r as unknown as { __kept: boolean }).__kept = true;
  });
  const marked = await attrs(page, 'data-txid');
  const orders = (await mock.ordersOf('maker', 'active')).filter((o: { token: string }) => o.token === token).slice(0, 20);
  expect(orders.length).toBeGreaterThanOrEqual(20);
  const first = (await attrs(page, 'data-txid'))[0];
  for (const o of orders) {
    await mock.fill(o.covenant_id);
    await page.waitForTimeout(250);
  }
  // the newest rows came in above it (it moved down, or off the 20 rows shown)
  await expect.poll(async () => { const i = (await attrs(page, 'data-txid')).indexOf(first!); return i === -1 || i >= 10; }, { timeout: 30_000 }).toBe(true);
  await page.waitForTimeout(1500);
  const cls = await page.evaluate(() => (window as unknown as { __cls: { value: number; log: number; sources: string[] } }).__cls);
  console.log(`CLS over 20 updates: page ${cls.value.toFixed(4)}, from the activity log ${cls.log.toFixed(4)} (shifted: ${[...new Set(cls.sources)].join(', ') || 'none'})`);
  // nothing in the log shifts; the page's total stays far under the "good" threshold (0.1) though the order book above changes as its orders fill
  expect(cls.log).toBe(0);
  expect(cls.value).toBeLessThan(0.05);
  // rows still on screen are the same nodes
  const kept = await page.evaluate((ids) => ids.filter((id) => { const r = document.querySelector(`[data-testid="activity-row"][data-txid="${id}"]`); return r && !(r as unknown as { __kept?: boolean }).__kept; }), marked);
  expect(kept, 're-created rows').toEqual([]);
  const toolsAfter = await tools();
  expect(Math.abs(toolsAfter.x - toolsBefore.x) + Math.abs(toolsAfter.y - toolsBefore.y)).toBeLessThan(1);
});
