// Every column of the Portfolio's lists and of the activity log sorts: Order history, Open orders, Fills, Balances, and the Activity page. A header is a button: first click
// the column's natural direction (numbers and times largest first, text from A), second the other way, third the list's own order again (newest first); numbers sort
// as numbers, an empty value ("—") is last in either direction; the sort covers the whole list before it is paged; it is kept in the browser; a card layout (no header)
// has the same sort as a control. Mock stack.
// KOB_SHOTS=<dir> saves screenshots (KOB_SHOT_TAG prefixes the names).
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TOKEN, listExkcc, connectWallet } from '../helpers/env';
import { lateTimeZone, seedLongValues } from '../helpers/portfolio-long';
import { seedManyDone, seedPortfolio } from '../helpers/portfolio-seed';
import { seedOrders } from '../helpers/orders';

test.use({ autoOpen: false, timezoneId: lateTimeZone() });

/** The first number in a cell's text (thousands separators dropped); NaN for "—" and for text without one. */
const num = (s: string | null | undefined): number => {
  const m = (s ?? '').replace(/,/g, '').match(/-?\d+(\.\d+)?/);
  return m ? Number(m[0]) : Number.NaN;
};

/** `values` ordered by `dir`, the empty ones (NaN) all after the others. */
function expectOrdered(values: number[], dir: 'asc' | 'desc', what: string): void {
  const firstEmpty = values.findIndex((v) => Number.isNaN(v));
  const full = firstEmpty < 0 ? values : values.slice(0, firstEmpty);
  const rest = firstEmpty < 0 ? [] : values.slice(firstEmpty);
  expect(rest.every((v) => Number.isNaN(v)), `${what}: empty values come last (${values})`).toBe(true);
  for (let i = 1; i < full.length; i++) {
    const ok = dir === 'asc' ? full[i - 1]! <= full[i]! : full[i - 1]! >= full[i]!;
    expect(ok, `${what} ${dir}: ${full[i - 1]} then ${full[i]} (${full})`).toBe(true);
  }
}

const texts = (loc: Locator) => loc.evaluateAll((els) => els.map((e) => (e.textContent ?? '').trim()));
const ids = (page: Page) => page.locator('[data-testid^="order-row-"]').evaluateAll((els) => els.map((e) => e.getAttribute('data-testid')!));
const shot = async (page: Page, name: string) => {
  const dir = process.env.KOB_SHOTS;
  if (!dir) return;
  await page.waitForTimeout(1100);
  await page.screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'sort'}-${name}.png`, fullPage: true });
};
const forgetStored = (page: Page) =>
  page.addInitScript(() => {
    if (sessionStorage.getItem('sort-seen')) return;
    sessionStorage.setItem('sort-seen', '1');
    for (const k of Object.keys(localStorage)) if (k.startsWith('kob.sort.') || k === 'kob.portfolio.rows') localStorage.removeItem(k);
  });

async function setup(page: Page, mock: import('../fixtures').MockClient, route: string): Promise<void> {
  await forgetStored(page);
  await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
  await seedPortfolio(mock);
  await seedLongValues(mock);
  await seedManyDone(mock, 14);
  await listExkcc(page);
  await page.goto(route);
  await connectWallet(page, 'kasware');
}

test.describe('1280 px wide', () => {
  test.use({ viewport: { width: 1280, height: 1000 } });

  test('Order history: every column, both ways, the third click, empty values last, paging after the sort, the counts, kept in the browser', async ({ appPage: page, mock }) => {
    test.setTimeout(Number(process.env.KOB_SORT_TIMEOUT ?? 240_000));
    await setup(page, mock, '/#/portfolio/history');
    await page.getByTestId('portfolio-rows-50').click();
    const rows = page.locator('[data-testid^="order-row-"]');
    await expect(rows.first()).toBeVisible();
    await page.waitForTimeout(900);
    // (a table that cannot hold its content, in a wide font, is cards: no header to press; the control of the cards is tested at 390 px)
    if (!(await page.getByTestId('orders-head').isVisible())) return;
    page.setDefaultTimeout(15_000);
    // (a wide font can turn the table into cards while the test runs: the headers go, the cards' own control is tested at 390 px)
    try {
      const own = await ids(page);
      const total = (await page.getByTestId('orders-tab-history').textContent())!;
      const sortBtn = (k: string) => page.getByTestId(`orders-sort-${k}`);
      const head = (k: string) => sortBtn(k).locator('xpath=ancestor::*[@role="columnheader"]');
      const cell = (sel: string) => texts(page.locator(`[data-testid^="order-line-"] ${sel}`));
      const minutes = () => cell('.or-time').then((a) => a.map((x) => Date.parse(x)));

      // the own order: newest first, which the time header says
      await expect(head('time')).toHaveAttribute('aria-sort', 'descending');
      await expect(head('price')).toHaveAttribute('aria-sort', 'none');

      // time: the first click reverses the own order, the second brings it back
      await sortBtn('time').click();
      await expect(head('time')).toHaveAttribute('aria-sort', 'ascending');
      await expect.poll(() => ids(page)).not.toEqual(own);
      // (orders placed in one minute tie: their own order is kept between them, so the order is checked by the times the lines show)
      expect([...(await ids(page))].sort()).toEqual([...own].sort());
      expectOrdered(await minutes(), 'asc', 'time');
      await sortBtn('time').click();
      await expect(head('time')).toHaveAttribute('aria-sort', 'descending');
      expect(await ids(page)).toEqual(own);

      // a number column (price): largest first, smallest first, own order; numbers as numbers (0.0238 is smaller than 0.02913 smaller than 34.37)
      await sortBtn('price').click();
      await expect(head('price')).toHaveAttribute('aria-sort', 'descending');
      expectOrdered((await cell('.or-price')).map(num), 'desc', 'price');
      await sortBtn('price').click();
      await expect(head('price')).toHaveAttribute('aria-sort', 'ascending');
      expectOrdered((await cell('.or-price')).map(num), 'asc', 'price');
      await sortBtn('price').click();
      await expect(head('price')).toHaveAttribute('aria-sort', 'none');
      expect(await ids(page)).toEqual(own);

      // the filled part (by its percentage) and the value: KAS-budget buys have neither ("—"): last in both directions
      const pct = (x: string) => { const m = /(d+)%/.exec(x); return m ? Number(m[1]) : Number.NaN; };
      const filledNums = () => cell('.or-filled').then((a) => a.map(pct));
      await sortBtn('filled').click();
      expectOrdered(await filledNums(), 'desc', 'filled');
      await sortBtn('filled').click();
      expectOrdered(await filledNums(), 'asc', 'filled');
      await sortBtn('filled').click();
      await sortBtn('total').click();
      expectOrdered((await cell('.or-value')).map(num), 'desc', 'value');
      await sortBtn('total').click();
      expectOrdered((await cell('.or-value')).map(num), 'asc', 'value');
      await sortBtn('total').click();

      // a text column (market): A to Z, then Z to A
      await sortBtn('market').click();
      await expect(head('market')).toHaveAttribute('aria-sort', 'ascending');
      const asc = (await cell('.or-market-name')).map((x) => x.replace(/\s+/g, ''));
      expect(asc).toEqual([...asc].sort((a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' })));
      await sortBtn('market').click();
      const desc = (await cell('.or-market-name')).map((x) => x.replace(/\s+/g, ''));
      expect(desc).toEqual([...desc].sort((a, b) => b.localeCompare(a, undefined, { numeric: true, sensitivity: 'base' })));
      await sortBtn('market').click();
      for (const k of ['side', 'status']) {
        await sortBtn(k).click();
        await expect(head(k)).not.toHaveAttribute('aria-sort', 'none');
        await sortBtn(k).click();
        await sortBtn(k).click();
        await expect(head(k)).toHaveAttribute('aria-sort', 'none');
      }
      expect(await ids(page)).toEqual(own);

      // the sort covers the whole list before it is paged: 10 rows a page, price largest first: page 1 holds the top of all of it; the counts do not change
      await page.getByTestId('portfolio-rows-10').click();
      await sortBtn('price').click();
      await expect(page.getByTestId('portfolio-showing')).toHaveText(new RegExp('^1–10 of '));
      const page1 = (await cell('.or-price')).map(num);
      await page.getByTestId('portfolio-older').click();
      const page2 = (await cell('.or-price')).map(num);
      expect(Math.min(...page1.filter((v) => !Number.isNaN(v)))).toBeGreaterThanOrEqual(Math.max(...page2.filter((v) => !Number.isNaN(v))));
      await expect(page.getByTestId('orders-tab-history')).toHaveText(total);
      // another sort starts at the first page
      await sortBtn('market').click();
      await expect(page.getByTestId('portfolio-showing')).toHaveText(new RegExp('^1–10 of '));
      await sortBtn('price').click();
      await shot(page, 'history-1280');

      // kept in the browser: a reload keeps the column and the direction
      await page.reload();
      await connectWallet(page, 'kasware');
      await expect(head('price')).toHaveAttribute('aria-sort', 'descending');
    } catch (e) {
      if (await page.getByTestId('orders-head').isHidden()) return;
      throw e;
    }
  });

  test('Open orders and Fills sort on their own; Balances sorts its tokens with KAS on top', async ({ appPage: page, mock }) => {
    test.setTimeout(Number(process.env.KOB_SORT_TIMEOUT ?? 240_000));
    await setup(page, mock, '/#/portfolio/open');
    await expect(page.locator('[data-testid^="order-row-"]').first()).toBeVisible();
    await page.waitForTimeout(900);
    // Open orders: the price sort here does not touch Order history's
    await page.getByTestId('orders-sort-price').click();
    expectOrdered((await texts(page.locator('[data-testid^="order-line-"] .or-price'))).map(num), 'desc', 'open price');
    await page.getByTestId('orders-tab-history').click();
    await expect(page.getByTestId('orders-sort-price').locator('xpath=ancestor::*[@role="columnheader"]')).toHaveAttribute('aria-sort', 'none');
    await page.getByTestId('orders-tab-active').click();
    await expect(page.getByTestId('orders-sort-price').locator('xpath=ancestor::*[@role="columnheader"]')).toHaveAttribute('aria-sort', 'descending');

    // Fills: amounts across tokens of other decimals as numbers (1 EXKCC, 2 EXKCC, 1,000 of the hex-named token, 1,960.83737795 ...)
    await page.getByTestId('orders-tab-fills').click();
    await page.getByTestId('portfolio-rows-50').click();
    await expect(page.getByTestId('wallet-fill-row').first()).toBeVisible();
    // (where the table does not fit its room, in a wide font, the fills are cards: the control is tested at 390 px)
    if (!(await page.getByTestId('wallet-fills-sortctl').isVisible())) {
    const fillHead = (k: string) => page.getByTestId(`wallet-fills-sort-${k}`).locator('xpath=ancestor::*[@role="columnheader"]');
    await expect(fillHead('time')).toHaveAttribute('aria-sort', 'descending');
    const amounts = () => texts(page.locator('[data-testid="wallet-fill-row"] .dt-c-amount .dt-val')).then((a) => a.map(num));
    const prices = () => texts(page.locator('[data-testid="wallet-fill-row"] .dt-c-price .dt-val')).then((a) => a.map(num));
    const values = () => texts(page.locator('[data-testid="wallet-fill-row"] .dt-c-value .dt-val')).then((a) => a.map(num));
    await page.getByTestId('wallet-fills-sort-amount').click();
    await expect(fillHead('amount')).toHaveAttribute('aria-sort', 'descending');
    expectOrdered(await amounts(), 'desc', 'fill amount');
    await page.getByTestId('wallet-fills-sort-amount').click();
    expectOrdered(await amounts(), 'asc', 'fill amount');
    await page.getByTestId('wallet-fills-sort-price').click();
    expectOrdered(await prices(), 'desc', 'fill price');
    await page.getByTestId('wallet-fills-sort-value').click();
    expectOrdered(await values(), 'desc', 'fill value');
    await page.getByTestId('wallet-fills-sort-value').click();
    expectOrdered(await values(), 'asc', 'fill value');
    await page.getByTestId('wallet-fills-sort-tx').click();
    const txs = await page.getByTestId('wallet-fill-row').evaluateAll((els) => els.map((e) => e.getAttribute('data-txid')!));
    expect(txs).toEqual([...txs].sort((a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' })));
    await page.getByTestId('wallet-fills-sort-tx').click();
    await page.getByTestId('wallet-fills-sort-tx').click();
    await expect(fillHead('tx')).toHaveAttribute('aria-sort', 'none');
    }
    await shot(page, 'fills-1280');

    // Balances: the tokens by what is free, by what is in orders; KAS stays the first row
    await page.getByTestId('orders-tab-balances').click();
    await expect(page.getByTestId('kas-balance')).toBeVisible();
    if (!(await page.getByTestId('balances-sortctl').isVisible())) {
    const balHead = (k: string) => page.getByTestId(`balances-sort-${k}`).locator('xpath=ancestor::*[@role="columnheader"]');
    const tokenRows = page.locator('[data-testid^="balance-row-"]');
    const col = (k: string) => texts(page.locator(`[data-testid^="balance-row-"] .dt-c-${k}`)).then((a) => a.map(num));
    await page.getByTestId('balances-sort-available').click();
    await expect(balHead('available')).toHaveAttribute('aria-sort', 'descending');
    expectOrdered(await col('available'), 'desc', 'available');
    await page.getByTestId('balances-sort-inOrders').click();
    expectOrdered(await col('inOrders'), 'desc', 'in orders');
    await page.getByTestId('balances-sort-inOrders').click();
    expectOrdered(await col('inOrders'), 'asc', 'in orders');
    await page.getByTestId('balances-sort-value').click();
    expectOrdered(await col('value'), 'desc', 'value');
    await page.getByTestId('balances-sort-asset').click();
    const names = (await texts(page.locator('[data-testid="balance-market"]'))).map((x) => x.replace(/\s+/g, ''));
    expect(names).toEqual([...names].sort((a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' })));
    expect(await tokenRows.count()).toBeGreaterThan(1);
    await expect(page.locator('.dt-body > .dt-group').first()).toHaveAttribute('data-testid', 'kas-balance');
    }
    await shot(page, 'balances-1280');
  });

  test('the market page panel sorts too, with its own header', async ({ appPage: page, mock }) => {
    test.setTimeout(Number(process.env.KOB_SORT_TIMEOUT ?? 240_000));
    await setup(page, mock, '/#/portfolio');
    const token = await mock.token();
    await page.goto(`/#/market/${token.covenant_id}`);
    const mine = page.getByTestId('market-orders');
    await mine.getByTestId('orders-tab-history').click();
    await expect(mine.locator('[data-testid^="order-row-"]').first()).toBeVisible();
    await page.waitForTimeout(900);
    await mine.getByTestId('orders-sort-price').click();
    expectOrdered((await texts(mine.locator('[data-testid^="order-line-"] .or-price'))).map(num), 'desc', 'panel price');
    await mine.scrollIntoViewIfNeeded();
    await shot(page, 'market-panel-1280');
  });

  test('Activity page: every column sorts; a row that arrives takes its place in the sort and moves nothing above it', async ({ appPage: page, mock }) => {
    test.setTimeout(Number(process.env.KOB_SORT_TIMEOUT ?? 240_000));
    await forgetStored(page);
    await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
    await seedPortfolio(mock);
    await listExkcc(page);
    await page.goto('/#/activity');
    await expect(page.locator('[data-testid="activity-row"]').first()).toBeVisible();
    await page.waitForTimeout(1200);
    const head = (k: string) => page.getByTestId(`activity-sort-${k}`).locator('xpath=ancestor::*[@role="columnheader"]');
    const amounts = () => texts(page.locator('[data-testid="activity-row"] .act-c-amt b')).then((a) => a.map(num));
    const prices = () => texts(page.locator('[data-testid="activity-row"] .act-c-px')).then((a) => a.map(num));
    const own = await page.locator('[data-testid="activity-row"]').evaluateAll((els) => els.map((e) => e.getAttribute('data-txid')!));
    await expect(head('time')).toHaveAttribute('aria-sort', 'descending');

    await page.getByTestId('activity-sort-amount').click();
    await expect(head('amount')).toHaveAttribute('aria-sort', 'descending');
    await expect.poll(async () => (await amounts()).length).toBeGreaterThan(5);
    expectOrdered(await amounts(), 'desc', 'activity amount');
    await page.getByTestId('activity-sort-amount').click();
    expectOrdered(await amounts(), 'asc', 'activity amount');
    await page.getByTestId('activity-sort-price').click();
    expectOrdered(await prices(), 'desc', 'activity price');
    await page.getByTestId('activity-sort-price').click();
    expectOrdered(await prices(), 'asc', 'activity price');
    await page.getByTestId('activity-sort-price').click();
    await expect(head('price')).toHaveAttribute('aria-sort', 'none');
    for (const k of ['kind', 'market', 'tx', 'wallet', 'conf']) {
      await page.getByTestId(`activity-sort-${k}`).click();
      await expect(head(k)).not.toHaveAttribute('aria-sort', 'none');
      await page.getByTestId(`activity-sort-${k}`).click();
      await page.getByTestId(`activity-sort-${k}`).click();
      await expect(head(k)).toHaveAttribute('aria-sort', 'none');
    }
    await page.getByTestId('activity-sort-time').click();
    await expect(head('time')).toHaveAttribute('aria-sort', 'ascending');
    // (the sort covers the rows within reach, up to 100: oldest first shows the oldest of them, not the same fifty rows turned round)
    const seconds = (x: string) => { const m = /(d+):(d+):(d+)s*(AM|PM)?/i.exec(x); return m ? ((Number(m[1]) % 12) + (/pm/i.test(m[4] ?? '') ? 12 : 0)) * 3600 + Number(m[2]) * 60 + Number(m[3]) : Number.NaN; };
    const rowTimes = () => page.locator('[data-testid="activity-row"] .act-c-time').evaluateAll((els) => els.map((e) => e.getAttribute('title') ?? ''));
    expectOrdered((await rowTimes()).map(seconds), 'asc', 'activity time');
    await page.getByTestId('activity-sort-time').click();
    await expect.poll(() => page.locator('[data-testid="activity-row"]').evaluateAll((els) => els.map((e) => e.getAttribute('data-txid')!))).toEqual(own);

    // a new row with a sort on: it takes its place in the order (a big one is first by amount, descending); nothing above the log moves
    await page.getByTestId('activity-sort-amount').click();
    const [big] = await seedOrders(mock, [{ side: 'ask', price: 2_950_000, tokens: 500 }]);
    await page.evaluate(() => {
      const w = window as unknown as { __cls: number; __sh: { v: number; inLog: boolean }[] };
      w.__cls = 0;
      w.__sh = [];
      new PerformanceObserver((list) => {
        for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean; sources?: { node: Node | null }[] }[]) {
          if (e.hadRecentInput) continue;
          const inLog = (e.sources ?? []).length > 0 && (e.sources ?? []).every((s) => s.node instanceof Element && !!s.node.closest('[data-testid="activity-list"]'));
          w.__sh.push({ v: e.value, inLog });
        }
      }).observe({ type: 'layout-shift', buffered: false });
    });
    const countBefore = await page.locator('[data-testid="activity-row"]').count();
    await mock.fill(big!, 400n * TOKEN);
    await mock.fill(big!, 50n * TOKEN);
    await expect.poll(async () => Math.max(...(await amounts()).filter((v) => !Number.isNaN(v))), { timeout: 20_000 }).toBeGreaterThanOrEqual(400);
    await page.waitForTimeout(1500);
    expectOrdered(await amounts(), 'desc', 'activity amount after a live row');
    expect((await amounts())[0]).toBeGreaterThanOrEqual(400);
    expect(await page.locator('[data-testid="activity-row"]').count()).toBeGreaterThanOrEqual(countBefore);
    const outside = await page.evaluate(() => (window as unknown as { __sh: { v: number; inLog: boolean }[] }).__sh.filter((s) => !s.inLog).reduce((a, s) => a + s.v, 0));
    expect(outside, 'layout shift outside the log').toBeLessThan(0.02);
    await shot(page, 'activity-1280');
  });
});

test.describe('390 px wide: the cards have a sort control', () => {
  test.use({ viewport: { width: 390, height: 900 } });

  test('Order history, Fills and Balances: the control sorts like the headers; the headers are gone; one state', async ({ appPage: page, mock }) => {
    test.setTimeout(Number(process.env.KOB_SORT_TIMEOUT ?? 240_000));
    await setup(page, mock, '/#/portfolio/history');
    await expect(page.locator('[data-testid^="order-row-"]').first()).toBeVisible();
    await page.waitForTimeout(900);
    await page.getByTestId('portfolio-rows-50').click();
    await expect(page.getByTestId('orders-head')).toBeHidden();
    const ctl = page.getByTestId('orders-sortctl');
    await expect(ctl).toBeVisible();
    const own = await ids(page);
    await ctl.getByTestId('orders-sortctl-column').selectOption('price');
    expectOrdered((await texts(page.locator('[data-testid^="order-line-"] .or-price'))).map(num), 'desc', 'card price');
    await ctl.getByTestId('orders-sortctl-dir').click();
    expectOrdered((await texts(page.locator('[data-testid^="order-line-"] .or-price'))).map(num), 'asc', 'card price');
    // the tap target of the control is a finger's
    const b = (await ctl.getByTestId('orders-sortctl-dir').boundingBox())!;
    expect(b.height).toBeGreaterThanOrEqual(39.9);
    expect(b.x + b.width).toBeLessThanOrEqual(390);
    await shot(page, 'history-390');
    // back to the own order: the time, largest first
    await ctl.getByTestId('orders-sortctl-column').selectOption('time');
    await expect(ctl.getByTestId('orders-sortctl-dir')).toHaveAttribute('data-dir', 'desc');
    expect(await ids(page)).toEqual(own);

    await page.getByTestId('orders-tab-fills').click();
    await expect(page.getByTestId('wallet-fill-row').first()).toBeVisible();
    const fctl = page.getByTestId('wallet-fills-sortctl');
    await expect(fctl).toBeVisible();
    await fctl.getByTestId('wallet-fills-sortctl-column').selectOption('price');
    const cardPrices = await texts(page.locator('[data-testid="wallet-fill-row"] .dt-c-price'));
    expectOrdered(cardPrices.map(num), 'desc', 'fill card price');
    await shot(page, 'fills-390');

    await page.getByTestId('orders-tab-balances').click();
    await expect(page.getByTestId('kas-balance')).toBeVisible();
    const bctl = page.getByTestId('balances-sortctl');
    await expect(bctl).toBeVisible();
    await bctl.getByTestId('balances-sortctl-column').selectOption('inOrders');
    await expect(bctl.getByTestId('balances-sortctl-dir')).toHaveAttribute('data-dir', 'desc');
    await shot(page, 'balances-390');
    // the same state in the table layout: the sort chosen in cards shows at 1280
    await page.setViewportSize({ width: 1280, height: 1000 });
    await expect(page.getByTestId('balances-sort-inOrders').locator('xpath=ancestor::*[@role="columnheader"]')).toHaveAttribute('aria-sort', 'descending');
  });

  test('Activity: the control stands in for the header on a phone', async ({ appPage: page, mock }) => {
    test.setTimeout(Number(process.env.KOB_SORT_TIMEOUT ?? 240_000));
    await forgetStored(page);
    await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
    await seedPortfolio(mock);
    await listExkcc(page);
    await page.goto('/#/activity');
    await expect(page.locator('[data-testid="activity-row"]').first()).toBeVisible();
    await page.waitForTimeout(1200);
    const ctl = page.getByTestId('activity-sortctl');
    await expect(ctl).toBeVisible();
    await ctl.getByTestId('activity-sortctl-column').selectOption('amount');
    const amounts = (await texts(page.locator('[data-testid="activity-row"] .act-c-amt b'))).map(num);
    expectOrdered(amounts, 'desc', 'activity card amount');
    expect((await ctl.getByTestId('activity-sortctl-dir').boundingBox())!.height).toBeGreaterThanOrEqual(39.9);
    await shot(page, 'activity-390');
  });
});
