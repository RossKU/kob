// Taps land where the finger is: the Portfolio page and a market page with its panel, at 390, 640, 720 and 1280 px.
//  1. layout shifts: while the mock pushes fills and balance changes for ~10 s, the layout-shift entries (no recent input) of everything above the Activity panel add up
//     to less than 0.02, and the tabs, the Rows control and the Refresh button stay where they are (page coordinates, sampled all along);
//  2. tap targets: at the centre of each control (the tabs, Rows, Refresh, Cancel, Copy, the order lines' toggle, the pager; on a market page the ticket's
//     Buy / Sell, its slider, the book's rows) `document.elementFromPoint` is the control itself or inside it: no overlay takes its taps.
// Mock stack.
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TOKEN, listExkcc, connectWallet } from '../helpers/env';
import { seedOrders } from '../helpers/orders';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

const WIDTHS = [390, 640, 720, 1280];
const CLS_LIMIT = 0.02;

interface Shift { value: number; at: number; nodes: string[] }

/** Starts recording the layout shifts (without recent input) the page makes; `window.__shifts` holds them. */
const RECORD = () => {
  const w = window as unknown as { __shifts: Shift[]; __recording: boolean };
  w.__shifts = [];
  w.__recording = true;
  const describe = (n: Node | null): string => {
    if (!(n instanceof Element)) return 'gone';
    const inAct = !!n.closest('[data-testid="activity"]');
    return `${inAct ? 'ACTIVITY ' : ''}${n.tagName.toLowerCase()}${n.getAttribute('data-testid') ? `[${n.getAttribute('data-testid')}]` : ''}.${String(n.getAttribute('class') ?? '').split(' ')[0]}`;
  };
  new PerformanceObserver((list) => {
    if (!w.__recording) return;
    for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean; startTime: number; sources?: { node: Node | null }[] }[]) {
      if (e.hadRecentInput) continue;
      w.__shifts.push({ value: e.value, at: Math.round(e.startTime), nodes: (e.sources ?? []).map((s) => describe(s.node)) });
    }
  }).observe({ type: 'layout-shift', buffered: false });
};

/** Where the tabs, the Rows control and the Refresh button are, in page coordinates. */
const WATCHED = ['[role="tab"]', '[data-testid="portfolio-rows-box"]', '[data-testid="orders-refresh"]'];
const positions = (page: Page, rootTestId: string) =>
  page.evaluate(
    ([sel, root]) => {
      const el = document.querySelector(`[data-testid="${root}"]`)!;
      const out: Record<string, string> = {};
      sel!.split('|').forEach((s) => el.querySelectorAll(s).forEach((e, i) => {
        const r = e.getBoundingClientRect();
        out[`${s}#${i}`] = [r.left + scrollX, r.top + scrollY, r.width, r.height].map((v) => Math.round(v * 2) / 2).join(',');
      }));
      return out;
    },
    [WATCHED.join('|'), rootTestId] as const,
  );

/** For 10 s: fills arrive, balances change, the page may refresh by itself; returns the position samples and the shifts. */
async function churn(page: Page, mock: import('../fixtures').MockClient, orderId: string, rootTestId: string) {
  await page.evaluate(RECORD);
  const samples: Record<string, string>[] = [];
  const t0 = Date.now();
  let n = 0;
  while (Date.now() - t0 < 10_000) {
    if (n % 3 === 0) await mock.fill(orderId, TOKEN / 4n).catch(() => undefined);
    if (n % 6 === 2) await mock.giveKas('alice', 123_456_789n + BigInt(n) * 1_000_003n, 1).catch(() => undefined);
    samples.push(await positions(page, rootTestId));
    await page.waitForTimeout(250);
    n++;
  }
  const shifts = await page.evaluate(() => (window as unknown as { __shifts: Shift[] }).__shifts);
  return { samples, shifts };
}

const above = (shifts: Shift[]): Shift[] => shifts.filter((s) => s.nodes.length === 0 || s.nodes.some((n) => !n.startsWith('ACTIVITY')));

for (const width of WIDTHS) {
  test.describe(`${width} px wide`, () => {
    test.use({ viewport: { width, height: 900 } });

    for (const where of ['portfolio', 'market'] as const) {
      test(`${where}: while fills and balances change, nothing above the Activity panel shifts and the tabs, Rows and Refresh stay put`, async ({ appPage: page, mock }) => {
        test.setTimeout(240_000);
        await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
        await seedPortfolio(mock);
        const [big] = await seedOrders(mock, [{ side: 'ask', price: 2_950_000, tokens: 400 }]);
        await listExkcc(page);
        const token = await mock.token();
        await page.goto(where === 'portfolio' ? '/#/portfolio/open' : `/#/market/${token.covenant_id}`);
        await connectWallet(page, 'kasware');
        const root = where === 'portfolio' ? 'orders-view' : 'market-orders';
        await expect(page.getByTestId(root)).toBeVisible();
        await expect(page.locator('[data-testid^="order-row-"]').first()).toBeVisible();
        if (where === 'market') await page.getByTestId('market-orders').scrollIntoViewIfNeeded();
        await page.waitForTimeout(2500); // the page settles (rows rising in, late icons, the node's answers)
        const { samples, shifts } = await churn(page, mock, big!, root);
        const cls = above(shifts).reduce((a, s) => a + s.value, 0);
        console.log(`CLS ${where} ${width}px: above Activity ${cls.toFixed(4)}, all shifts ${shifts.length} (${shifts.reduce((a, s) => a + s.value, 0).toFixed(4)}); fills seen: ${(await page.getByTestId('orders-tab-fills').textContent())}`);
        expect.soft(cls, `layout shift ${cls.toFixed(4)} above the Activity panel: ${JSON.stringify(above(shifts))}`).toBeLessThan(CLS_LIMIT);
        const first = samples[0]!;
        // a position is held to the half pixel; a size to a pixel (a tab's count may take a digit more: its neighbour to the right moves by that much)
        const moved = (a: string, b: string, k: string) => {
          const [x1, y1, w1, h1] = a.split(',').map(Number);
          const [x2, y2, w2, h2] = b.split(',').map(Number);
          // a tab that grows pushes only the tabs after it: its left edge is not held, the line it stands on is
          return Math.abs(y1! - y2!) > 0.5 || Math.abs(h1! - h2!) > 1 || (!k.startsWith('[role="tab"]') && (Math.abs(x1! - x2!) > 0.5 || Math.abs(w1! - w2!) > 1)) || (k.startsWith('[role="tab"]') && Math.abs(w1! - w2!) > 12);
        };
        for (const k of Object.keys(first)) {
          const bad = samples.findIndex((s) => moved(first[k]!, s[k]!, k));
          expect.soft(bad, `${k} moved at sample ${bad}: ${first[k]} -> ${samples[bad]?.[k]}`).toBe(-1);
        }
      });

      test(`${where}: the centre of every control is the control (no overlay takes its taps)`, async ({ appPage: page, mock }) => {
        test.setTimeout(240_000);
        await page.addInitScript(() => localStorage.setItem('kob.theme', 'dark'));
        await seedPortfolio(mock);
        await listExkcc(page);
        const token = await mock.token();
        await page.goto(where === 'portfolio' ? '/#/portfolio' : `/#/market/${token.covenant_id}`);
        await connectWallet(page, 'kasware');
        const rootId = where === 'portfolio' ? 'orders-view' : 'market-orders';
        const panel = page.getByTestId(rootId);
        await expect(panel).toBeVisible();
        const covered: string[] = [];
        /** Scrolls each matching control to the middle of the window and asks what is at its centre. */
        const check = async (scope: string, selector: string, label: string, max = 6) => {
          const n = Math.min(await page.locator(`${scope} ${selector}`).count(), max);
          for (let i = 0; i < n; i++) {
            const el = page.locator(`${scope} ${selector}`).nth(i);
            if (!(await el.isVisible())) continue;
            await el.evaluate((e) => e.scrollIntoView({ block: 'center', inline: 'nearest' }));
            await page.waitForTimeout(60);
            const r = await el.evaluate((e) => {
              const b = e.getBoundingClientRect();
              if (b.width === 0 || b.height === 0) return 'empty';
              const top = document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2);
              if (top && e.contains(top)) return 'ok';
              return `covered by ${top?.tagName.toLowerCase()}.${String(top?.getAttribute('class') ?? '').split(' ')[0]}[${top?.getAttribute('data-testid') ?? ''}]`;
            });
            if (r !== 'ok' && r !== 'empty') covered.push(`${label} #${i}: ${r}`);
          }
        };
        const tabs: [string, string][] = [['balances', 'balances'], ['active', 'open'], ['history', 'history'], ['fills', 'fills']];
        for (const [id] of tabs) {
          await check(`[data-testid="${rootId}"]`, '[role="tab"]', 'tab');
          await panel.getByTestId(`orders-tab-${id}`).click();
          await expect(panel.getByTestId(`orders-tab-${id}`)).toHaveAttribute('aria-selected', 'true');
          await page.waitForTimeout(400);
          const s = `[data-testid="${rootId}"]`;
          await check(s, '[data-testid="portfolio-rows-box"] button', `${id}: Rows`);
          await check(s, '[data-testid="orders-refresh"]', `${id}: Refresh`);
          await check(s, '[data-testid^="order-cancel-"]', `${id}: Cancel`);
          await check(s, '[data-testid^="orders-cancel-"]', `${id}: Cancel all / token`);
          await check(s, '.dt-chev', `${id}: order toggle`);
          await check(s, '.copy-btn', `${id}: Copy`);
          await check(s, '[data-testid$="-older"], [data-testid$="-newer"]', `${id}: pager`);
          await check(s, '[data-testid="orders-export-fills"]', `${id}: Export`);
        }
        if (where === 'market') {
          await check('[data-testid="token-page"]', '[data-testid="order-side-buy"], [data-testid="order-side-sell"]', 'ticket Buy / Sell');
          await check('[data-testid="token-page"]', 'input[type="range"]', 'ticket slider');
          await check('[data-testid="token-page"]', '[data-testid="book-ask-row"]', 'book ask row', 4);
          await check('[data-testid="token-page"]', '[data-testid="book-bid-row"]', 'book bid row', 4);
        }
        expect(covered, covered.join('\n')).toEqual([]);
      });
    }
  });
}
