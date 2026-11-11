// The tables of the Portfolio never paint a cell over its neighbour: at every width from 760 to 1600 px (steps of 40), with long values seeded (a date like
// "11/10/26, 10:38 PM", a Filled cell of "1,960.83737795 / 1,960.83737795 <hex name> 100%", a hex-named token, "expired (IOC/FOK)"), no two cells of a row of
// Open orders, Order history or Fills have overlapping contents, and each cell holds its own content. Also the panel on a market page. Mock stack.
// KOB_SHOTS=<dir> saves screenshots at KOB_SHOT_WIDTHS (KOB_SHOT_TAG prefixes the names).
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { lateTimeZone, seedLongValues } from '../helpers/portfolio-long';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false, timezoneId: lateTimeZone() });

const WIDTHS = [...Array.from({ length: 22 }, (_, i) => 760 + 40 * i), 1024].sort((a, b) => a - b); // 760 ... 1600, and a tablet's 1024
const SHOT_WIDTHS = new Set((process.env.KOB_SHOT_WIDTHS ?? '').split(',').filter(Boolean).map(Number));

/** Rows (`.or-line`, or a table row of the fills) whose cells overlap, or whose cells hold more than their box, as sentences. */
async function overlaps(root: Locator, where: string): Promise<string[]> {
  return root.evaluate((el, w) => {
    const bad: string[] = [];
    const rowsOf = [...el.querySelectorAll<HTMLElement>('.dt-row')];
    const name = (c: Element) => (c.getAttribute('class') ?? c.tagName).split(' ')[0] || c.tagName;
    const text = (c: Element, n: number) => (c.textContent ?? '').trim().slice(0, n);
    for (const row of rowsOf) {
      const cells = [...row.children].filter((c) => (c as HTMLElement).offsetParent !== null) as HTMLElement[];
      const boxes = cells.map((c) => {
        const range = document.createRange();
        range.selectNodeContents(c);
        let rects = [...range.getClientRects()].filter((r) => r.width > 0.5 && r.height > 0.5);
        // a whole transaction id ends with an ellipsis where its cell has no room: the text is clipped by its box, so the box (the id up to its ellipsis, and the Copy button) is what is painted
        const whole = c.querySelector('.copy-ellipsis');
        if (whole) rects = [...whole.querySelectorAll('a, code, button')].filter((e) => !(e.tagName === 'CODE' && e.parentElement?.tagName === 'A')).map((e) => e.getBoundingClientRect()).filter((r) => r.width > 0.5 && r.height > 0.5);
        return { c, rects };
      });
      for (const { c, rects } of boxes) {
        const cb = c.getBoundingClientRect();
        for (const r of rects) if (r.left < cb.left - 1 || r.right > cb.right + 1) bad.push(`${w}: "${text(c, 40)}" (${name(c)}) is wider than its cell (${Math.round(r.left)}..${Math.round(r.right)} in ${Math.round(cb.left)}..${Math.round(cb.right)})`);
      }
      for (let i = 0; i < boxes.length; i++) {
        for (let j = i + 1; j < boxes.length; j++) {
          for (const a of boxes[i]!.rects) {
            for (const b of boxes[j]!.rects) {
              const dx = Math.min(a.right, b.right) - Math.max(a.left, b.left);
              const dy = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
              if (dx > 1 && dy > 1) bad.push(`${w}: "${text(boxes[i]!.c, 30)}" (${name(boxes[i]!.c)}) overlaps "${text(boxes[j]!.c, 30)}" (${name(boxes[j]!.c)}) by ${Math.round(dx)}x${Math.round(dy)}px`);
            }
          }
        }
      }
    }
    return [...new Set(bad)];
  }, where);
}

async function shot(page: Page, name: string): Promise<void> {
  const dir = process.env.KOB_SHOTS;
  if (!dir) return;
  await page.waitForTimeout(1200);
  await page.screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'overlap'}-${name}.png`, fullPage: true });
}

for (const width of WIDTHS) {
  test(`no cell paints over a neighbour at ${width} px`, async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width, height: 1000 });
    await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
    await seedPortfolio(mock);
    await seedLongValues(mock);
    await listExkcc(page);
    await page.goto('/#/portfolio');
    await connectWallet(page, 'kasware');
    const found: string[] = [];
    const root = page.getByTestId('orders-view');
    const shots = SHOT_WIDTHS.has(width);
    for (const [id, panel] of [['active', 'open'], ['history', 'history'], ['fills', 'fills']] as const) {
      await page.getByTestId(`orders-tab-${id}`).click();
      await expect(page.getByTestId(`portfolio-panel-${panel}`)).toBeVisible();
      if (id === 'fills') await expect(page.getByTestId('wallet-fill-row').first()).toBeVisible();
      else await expect(page.locator('[data-testid^="order-row-"]').first()).toBeVisible();
      await page.waitForTimeout(900); // the rows rise in
      found.push(...(await overlaps(root, `page ${id}`)));
      if (shots) await shot(page, `${width}-page-${id}`);
    }
    // the same on a market page's panel (its width is the page's less the columns beside it)
    const token = await mock.token();
    await page.goto(`/#/market/${token.covenant_id}`);
    const mine = page.getByTestId('market-orders');
    await expect(mine).toBeVisible();
    // the panel shows this market only by default: the hex-named token's orders are on another market's page
    await mine.getByTestId('market-orders-scope-all').click();
    for (const [id, panel] of [['active', 'open'], ['history', 'history'], ['fills', 'fills']] as const) {
      await mine.getByTestId(`orders-tab-${id}`).click();
      await expect(mine.getByTestId(`portfolio-panel-${panel}`)).toBeVisible();
      if (id === 'fills') await expect(mine.getByTestId('wallet-fill-row').first()).toBeVisible();
      else await expect(mine.locator('[data-testid^="order-row-"]').first()).toBeVisible();
      await page.waitForTimeout(900);
      found.push(...(await overlaps(mine, `market ${id}`)));
      if (shots) {
        await mine.scrollIntoViewIfNeeded();
        await shot(page, `${width}-market-${id}`);
      }
    }
    expect(found, found.join('\n')).toEqual([]);
  });
}
