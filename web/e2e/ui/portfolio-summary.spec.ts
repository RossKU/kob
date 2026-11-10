// The four summary cards over the Portfolio's tabs (Total value, Available, In your orders, Open orders), on the Portfolio page and in a market page's panel: at every
// width from 360 to 1600 px (steps of 40) the value row of each card is one line (number and unit side by side, nothing broken), the cards are one height with their
// labels and values on common lines, the Total reads to two decimals without a "≈" and carries an "Estimate (?)" tag whose popover opens by a click and by the keyboard
// (and holds the note about tokens without a price, which is not in the card any more). Mock stack.
// KOB_SHOTS=<dir> saves a picture of the strip at every width (KOB_SHOT_TAG prefixes the names, KOB_THEME picks dark / light).
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

const WIDTHS = Array.from({ length: 24 }, (_, i) => 360 + 40 * i).concat(590); // 360 ... 1280 in steps of 40 (the sweep goes on in the next list), and 590
const WIDE = Array.from({ length: 8 }, (_, i) => 1320 + 40 * i); // 1320 ... 1600
const ALL = [...WIDTHS, ...WIDE].sort((a, b) => a - b);

/** What is wrong with the strip in `root`, as sentences. */
async function problems(root: Locator, where: string): Promise<string[]> {
  return root.evaluate((el, w) => {
    const bad: string[] = [];
    const strip = el.querySelector<HTMLElement>('[data-testid="portfolio-summary"]')!;
    const cards = [...strip.querySelectorAll<HTMLElement>('.pf-stat')];
    if (cards.length !== 4) bad.push(`${w}: ${cards.length} cards`);
    for (const c of cards) {
      const v = c.querySelector<HTMLElement>('.pf-v')!;
      const cs = getComputedStyle(v);
      const lh = cs.lineHeight === 'normal' ? 1.25 * parseFloat(cs.fontSize) : parseFloat(cs.lineHeight);
      if (v.getBoundingClientRect().height > 1.3 * lh + 0.5) bad.push(`${w}: the value "${v.textContent}" is on several lines`);
      if (v.scrollWidth > v.clientWidth + 1) bad.push(`${w}: the value "${v.textContent}" is cut (${v.scrollWidth} in ${v.clientWidth})`);
      for (const part of v.querySelectorAll<HTMLElement>('.dt-val, .dt-unit')) {
        const r = part.getBoundingClientRect();
        if (r.height > 1.3 * lh + 0.5) bad.push(`${w}: "${part.textContent}" is on several lines`);
      }
      const k = c.querySelector<HTMLElement>('.pf-k')!;
      if (k.scrollWidth > k.clientWidth + 1) bad.push(`${w}: the label "${k.textContent}" is cut`);
    }
    // one height, the labels and the values on common lines (cards of one row)
    const rows = new Map<number, HTMLElement[]>();
    for (const c of cards) {
      const top = Math.round(c.getBoundingClientRect().top);
      rows.set(top, [...(rows.get(top) ?? []), c]);
    }
    for (const [, row] of rows) {
      const hs = new Set(row.map((c) => Math.round(c.getBoundingClientRect().height)));
      if (hs.size > 1) bad.push(`${w}: the cards are of heights ${[...hs]}`);
      const ks = new Set(row.map((c) => Math.round(c.querySelector('.pf-k')!.getBoundingClientRect().top)));
      if (ks.size > 1) bad.push(`${w}: the labels are at ${[...ks]}`);
      const vs = new Set(row.map((c) => Math.round(c.querySelector('.pf-v')!.getBoundingClientRect().top)));
      if (vs.size > 1) bad.push(`${w}: the values are at ${[...vs]}`);
    }
    // no empty space under the value: the card ends where the value does (a line of padding)
    for (const c of cards) {
      const free = c.getBoundingClientRect().bottom - c.querySelector('.pf-v')!.getBoundingClientRect().bottom;
      if (free > 16) bad.push(`${w}: ${Math.round(free)}px of empty space under a value`);
    }
    return bad;
  }, where);
}

async function shot(page: Page, root: Locator, name: string): Promise<void> {
  const dir = process.env.KOB_SHOTS;
  if (!dir) return;
  await root.getByTestId('portfolio-summary').screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'sum'}-${name}.png` });
  void page;
}

for (const width of ALL) {
  test(`summary cards at ${width} px: one line each, one height, the Estimate tag`, async ({ appPage: page, mock }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width, height: 900 });
    const theme = process.env.KOB_THEME ?? 'dark';
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), theme);
    await seedPortfolio(mock);
    await listExkcc(page);
    await page.goto('/#/portfolio');
    await connectWallet(page, 'kasware');
    const found: string[] = [];

    const root = page.getByTestId('orders-view');
    await expect(page.getByTestId('portfolio-total')).toContainText('KAS');
    await page.waitForTimeout(500);
    found.push(...(await problems(root, `page`)));
    await shot(page, root, `${theme}-${width}-page`);
    // the Total: two decimals, no "≈"; Available and In your orders keep their exact amounts; the note about unpriced tokens is not in the card
    const total = (await page.getByTestId('portfolio-total').locator('.dt-val').textContent())!;
    expect(total, 'the total reads to two decimals').toMatch(/^\d{1,3}(,\d{3})*\.\d{2}$/);
    expect(await page.getByTestId('portfolio-summary').textContent()).not.toContain('≈');
    await expect(page.getByTestId('portfolio-unpriced')).toHaveCount(0);

    // the tag: a button of its own, its popover opens by a click and by the keyboard, closes by Escape, and says it is an estimate
    const tag = page.getByTestId('portfolio-estimate');
    await expect(tag).toBeVisible();
    await expect(tag).toContainText('Estimate');
    const detail = page.getByTestId('portfolio-estimate-detail');
    await expect(detail).toHaveCount(0);
    await tag.click();
    await expect(detail).toBeVisible();
    await expect(detail).toContainText('estimate');
    const box = (await detail.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(width);
    await tag.click();
    await expect(detail).toHaveCount(0);
    await tag.focus();
    await page.keyboard.press('Enter');
    await expect(detail).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(detail).toHaveCount(0);
    await tag.focus();
    await page.keyboard.press('Space');
    await expect(detail).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(detail).toHaveCount(0);
    expect(await page.evaluate(() => document.activeElement?.getAttribute('data-testid'))).toBe('portfolio-estimate');

    // a market page's panel: the same cards
    const token = await mock.token();
    await page.goto(`/#/market/${token.covenant_id}`);
    const mine = page.getByTestId('market-orders');
    await expect(mine.getByTestId('portfolio-total')).toContainText('KAS');
    await page.waitForTimeout(500);
    found.push(...(await problems(mine, `market`)));
    await mine.scrollIntoViewIfNeeded();
    await shot(page, mine, `${theme}-${width}-market`);
    await expect(mine.getByTestId('portfolio-estimate')).toBeVisible();
    await mine.getByTestId('portfolio-estimate').click();
    await expect(mine.getByTestId('portfolio-estimate-detail').or(page.getByTestId('portfolio-estimate-detail')).first()).toBeVisible();
    await page.keyboard.press('Escape');
    expect(found, found.join('\n')).toEqual([]);
  });
}
