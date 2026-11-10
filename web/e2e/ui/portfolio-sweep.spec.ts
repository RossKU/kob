// The Portfolio panel at every width from 360 to 1280 px in steps of 40 (the layouts follow the width the panel has, not a few device widths): the Portfolio
// page's four tabs and a market page's panel (its width differs from the page's). At each width, on each tab: no horizontal overflow of the page or inside the
// panel, no summary figure on two lines or cut, the transaction of a fill on one line, the market of every order at the same x, the tab row on one line.
// KOB_SHOTS=<dir> saves a screenshot of every tab and of the market panel at KOB_SHOT_WIDTHS (comma list; KOB_SHOT_TAG prefixes the names). Mock stack.
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { PF_ID, seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

// 360 ... 1280 in steps of 40, and the two common phones (390, 430)
const WIDTHS = [...Array.from({ length: 24 }, (_, i) => 360 + 40 * i), 390, 430].sort((a, b) => a - b);
const SHOT_WIDTHS = new Set((process.env.KOB_SHOT_WIDTHS ?? '').split(',').filter(Boolean).map(Number));

/** What is wrong with the panel `root` as drawn now, as a list of sentences (empty: nothing). */
async function problems(page: Page, root: Locator, where: string, wholePage = true): Promise<string[]> {
  const out: string[] = await root.evaluate((el, [w, whole]) => {
    const bad: string[] = [];
    const doc = document.documentElement;
    // (a market page's own header may be wider than a 360 px screen in a wide font: only the panel is asked about there)
    if (whole && doc.scrollWidth > window.innerWidth) bad.push(`${w}: the page is ${doc.scrollWidth - window.innerWidth}px wider than the window`);
    const r = el.getBoundingClientRect();
    if (el.scrollWidth > el.clientWidth + 1) bad.push(`${w}: the panel overflows sideways by ${el.scrollWidth - el.clientWidth}px`);
    for (const x of el.querySelectorAll<HTMLElement>('.dt, .dt-row, .pf-strip, .pf-stat')) {
      if (x.scrollWidth > x.clientWidth + 1) bad.push(`${w}: .${x.className.split(' ')[0]} overflows by ${x.scrollWidth - x.clientWidth}px`);
      const b = x.getBoundingClientRect();
      if (b.right > r.right + 1 || b.left < r.left - 1) bad.push(`${w}: .${x.className.split(' ')[0]} sticks out of the panel`);
    }
    // the summary figures: whole, one line (a figure is never broken across lines, never cut)
    for (const x of el.querySelectorAll<HTMLElement>('.pf-v, .pf-v .dt-val, .pf-v .dt-unit')) {
      const cs = getComputedStyle(x);
      const lh = cs.lineHeight === 'normal' ? 1.25 * parseFloat(cs.fontSize) : parseFloat(cs.lineHeight);
      const isFigure = x.matches('.pf-v .dt-val') || !x.querySelector('span');
      if (x.scrollWidth > x.clientWidth + 1) bad.push(`${w}: figure "${x.textContent}" is cut`);
      if (isFigure && x.getBoundingClientRect().height > 1.3 * lh + 0.5) bad.push(`${w}: figure "${x.textContent}" is on two lines (${x.getBoundingClientRect().height}px of ${lh}px lines)`);
    }
    // the tab row: one line
    const tops = [...el.querySelectorAll<HTMLElement>('[role="tab"]')].map((t) => t.offsetTop);
    if (new Set(tops).size > 1) bad.push(`${w}: the tabs are on several lines (${tops})`);
    // a transaction: one line
    for (const x of el.querySelectorAll<HTMLElement>('[data-testid="wallet-fill-tx"]')) {
      const cs = getComputedStyle(x);
      if (x.getBoundingClientRect().height > 1.6 * parseFloat(cs.lineHeight === 'normal' ? String(1.25 * parseFloat(cs.fontSize)) : cs.lineHeight)) bad.push(`${w}: a transaction is on several lines`);
    }
    // the orders: the icon and the market of every line at one x; the status at one x (its right edge in a card, its left edge in the one-line grid)
    const lines = [...el.querySelectorAll<HTMLElement>('.or-line')];
    if (lines.length > 1) {
      const xs = (sel: string, edge: 'left' | 'right') => new Set(lines.map((l) => Math.round(l.querySelector(sel)!.getBoundingClientRect()[edge])));
      if (xs('.or-market-name', 'left').size > 1) bad.push(`${w}: the market is at ${[...xs('.or-market-name', 'left')]} across the orders`);
      if (xs('.or-market', 'left').size > 1) bad.push(`${w}: the icon is at ${[...xs('.or-market', 'left')]} across the orders`);
      if (xs('.or-status', 'right').size > 1 && xs('.or-status', 'left').size > 1) bad.push(`${w}: the status is at ${[...xs('.or-status', 'right')]} / ${[...xs('.or-status', 'left')]} across the orders`);
      for (const n of el.querySelectorAll<HTMLElement>('.or-market-name')) if (n.scrollWidth > n.clientWidth + 1) bad.push(`${w}: market "${n.textContent}" is cut`);
    }
    return bad;
  }, [where, wholePage] as const);
  void page;
  return out;
}

async function shot(page: Page, name: string): Promise<void> {
  const dir = process.env.KOB_SHOTS;
  if (!dir) return;
  await page.waitForTimeout(1200); // the rows rise in with an animation
  await page.screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'sweep'}-${name}.png`, fullPage: true });
}

for (const width of WIDTHS) {
  test(`Portfolio at ${width} px: no overflow, whole figures, aligned orders, one-line tabs and transactions`, async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
    await seedPortfolio(mock);
    await listExkcc(page);
    await page.goto('/#/portfolio');
    await connectWallet(page, 'kasware');
    const shots = SHOT_WIDTHS.has(width);
    const found: string[] = [];

    // the Portfolio page: every tab
    const root = page.getByTestId('orders-view');
    await expect(page.getByTestId('portfolio-total')).toContainText('KAS');
    const tabs: [string, string][] = [['balances', 'balances'], ['active', 'open'], ['history', 'history'], ['fills', 'fills']];
    for (const [id, panel] of tabs) {
      await page.getByTestId(`orders-tab-${id}`).click();
      await expect(page.getByTestId(`portfolio-panel-${panel}`)).toBeVisible();
      if (id === 'balances') await expect(page.getByTestId('kas-balance')).toBeVisible();
      if (id === 'active') await expect(page.locator('[data-testid^="order-row-"]').first()).toBeVisible();
      if (id === 'history') await expect(page.locator('[data-testid^="order-row-"]').first()).toBeVisible();
      if (id === 'fills') await expect(page.getByTestId('wallet-fill-row').first()).toBeVisible();
      found.push(...(await problems(page, root, `page ${id}`)));
      if (shots) await shot(page, `${width}-page-${id}`);
    }

    // a market page's panel: its width is the page's less the columns beside it
    const token = await mock.token();
    await page.goto(`/#/market/${token.covenant_id}`);
    const mine = page.getByTestId('market-orders');
    await expect(mine).toBeVisible();
    for (const [id, panel] of tabs) {
      await mine.getByTestId(`orders-tab-${id}`).click();
      await expect(mine.getByTestId(`portfolio-panel-${panel}`)).toBeVisible();
      if (id === 'fills') await expect(mine.getByTestId('wallet-fill-row').first()).toBeVisible();
      if (id === 'active' || id === 'history') await expect(mine.locator('[data-testid^="order-row-"]').first()).toBeVisible();
      found.push(...(await problems(page, mine, `market ${id}`, false)));
      if (shots) {
        await mine.scrollIntoViewIfNeeded();
        await shot(page, `${width}-market-${id}`);
      }
    }
    void PF_ID;
    expect(found, found.join('\n')).toEqual([]);
  });
}
