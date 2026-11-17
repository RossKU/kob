// Portfolio > Balances: the header and every row share one set of column tracks, whatever buttons a row has (none, Merge all, Cancel all, both, Convert to plain),
// so each figure column ends on one right edge (within 1 px) at 1280 and 1024 px; a phone keeps its cards. Screenshots go to the test's output. Mock stack.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });

const EXT = 'ee'.repeat(32);
const MERGE = '5a'.repeat(32); // two plain UTXOs: Merge all only
const CONVERT = '5b'.repeat(32); // one plain, one with a borrow setting: Convert to plain
const PLAIN = '5c'.repeat(32); // one plain UTXO: no buttons
const EXTRA = [['EXSTD', MERGE], ['EXCNV', CONVERT], ['EXONE', PLAIN]] as const;
const NUM_COLS = ['available', 'inOrders', 'strays', 'kasLocked', 'value'];

async function registry(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  Object.assign(exkcc, { verified: true });
  for (const [ticker, id] of EXTRA) reg.tokens.push({ ...exkcc, ticker, name: ticker, covenant_id: id, template_id: 'kcc20-ref-3x3', extension_commitment: EXT });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

async function seed(mock: MockClient): Promise<void> {
  await seedPortfolio(mock);
  await mock.seed({ tokens: EXTRA.map(([ticker, id]) => ({ ticker, covenant_id: id, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3' })) });
  for (const k of [1n, 2n]) await mock.giveTokens('alice', k * TOKEN, { token: MERGE, carrier: 2n * KAS });
  await mock.giveTokens('alice', 2n * TOKEN, { token: CONVERT, carrier: 2n * KAS });
  await mock.giveTokens('alice', 3n * TOKEN, { token: CONVERT, carrier: 2n * KAS, borrow_scheme: 1 });
  await mock.giveTokens('alice', 7n * TOKEN, { token: PLAIN, carrier: 2n * KAS });
}

async function open(page: Page, mock: MockClient): Promise<void> {
  await seed(mock);
  await registry(page);
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId(`orders-merge-token-${MERGE}`)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId(`orders-convert-token-${CONVERT}`)).toBeVisible();
  await expect(page.locator('[data-testid^="orders-cancel-token-"]').first()).toBeVisible();
  // the fonts in, the rows risen in (.ml-in)
  await page.evaluate(async () => { await document.fonts.ready; await Promise.all(document.getAnimations().filter((a) => a.effect?.getTiming().iterations !== Infinity).map((a) => a.finished)); });
}

/** Per figure column: the right edges of the header cell and of every row's cell, and of every row's figure (its first line), in px. */
const edges = (page: Page) =>
  page.getByTestId('token-balances-table').evaluate((table, cols) => {
    const firstLineRight = (cell: Element): number => {
      const range = document.createRange();
      range.selectNodeContents(cell.firstChild ?? cell);
      const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0);
      const top = Math.min(...rects.map((r) => r.top));
      return Math.max(...rects.filter((r) => r.top < top + 4).map((r) => r.right));
    };
    const out: Record<string, { cells: number[]; figures: number[] }> = {};
    for (const k of cols) {
      const head = table.querySelector(`.dt-head .dt-h-${k}`)!;
      const cells = Array.from(table.querySelectorAll(`.dt-body .dt-row > .dt-c-${k}`));
      out[k] = { cells: [head, ...cells].map((c) => c.getBoundingClientRect().right), figures: cells.map(firstLineRight) };
    }
    return out;
  }, NUM_COLS);

const spread = (xs: number[]): number => Math.max(...xs) - Math.min(...xs);
const shot = (page: Page, name: string) => page.screenshot({ path: test.info().outputPath(`${name}.png`), fullPage: true });

for (const width of [1280, 1152, 1024]) {
  test(`${width} px: every row and the header share the column tracks; each figure column ends on one edge`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 1000 });
    await open(page, mock);
    const table = page.getByTestId('token-balances-table');
    await shot(page, `balances-${width}`);
    // a font as wide as the CI runner's (DejaVu Sans) needs more than 1024 px for the KAS figure: there the table is cards, as it was before
    if (width < 1100 && (await table.getAttribute('data-fold')) === 'cards') test.skip(true, 'cards at this width with this font');
    await expect(table).not.toHaveAttribute('data-fold', 'cards');
    // one grid for all: the same tracks on the header and on each row
    const tracks = await table.locator('.dt-head, .dt-body .dt-row').evaluateAll((els) => els.map((e) => getComputedStyle(e).gridTemplateColumns));
    expect(new Set(tracks).size, tracks.join('\n')).toBe(1);
    for (const [k, e] of Object.entries(await edges(page))) {
      expect(spread(e.cells), `${k}: cell right edges ${e.cells}`).toBeLessThanOrEqual(1);
      expect(spread(e.figures), `${k}: figure right edges ${e.figures}`).toBeLessThanOrEqual(1);
    }
    // the buttons stay inside their cell
    for (const act of await table.locator('.dt-c-act').all()) {
      const cell = (await act.boundingBox())!;
      for (const b of await act.locator('.btn').all()) {
        const bb = (await b.boundingBox())!;
        expect(bb.x).toBeGreaterThanOrEqual(cell.x - 1);
        expect(bb.x + bb.width).toBeLessThanOrEqual(cell.x + cell.width + 1);
      }
    }
  });
}

test('390 px: the balances stay cards', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await open(page, mock);
  await expect(page.getByTestId('token-balances-table')).toHaveAttribute('data-fold', 'cards');
  await shot(page, 'balances-390');
});
