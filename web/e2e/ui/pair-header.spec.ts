// The two market headers are the same market bar (founder, 2026-11-11; e2e/ui/market-bar.spec.ts has its size and the figures).
// A token pair: [icon A][fingerprint A] / [icon B][fingerprint B], the tickers ("TETH / TUSD", the pair selector), the swap button, the star and the figures in one row at 1280 and 880 px, one
// row of the pair, the star and Live with the figures under it on a phone; the name, the id and the program chip of each token are in its details card. A token's KAS market: the identity block, the
// pair and the swap, the figures, Live. Checked at 1280, 880, 390 and 360 px, also with the wide fonts (KOB_WIDE_FONTS=1: no row breaks the page's width).
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);

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

const box = async (loc: ReturnType<Page['getByTestId']>) => (await loc.first().boundingBox())!;

for (const width of [1280, 880, 390, 360]) {
  test(`${width} px: the pair header is icons and fingerprints, the tickers, the swap and the star on one row of the bar; each token's name and program are in its details`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 900 });
    await mock.seed({ pair: true });
    await fund(mock, 'alice');
    await listPair(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    const header = page.getByTestId('pair-header');
    await expect(header).toBeVisible();
    const [iconA, fpA, iconB, fpB, flip, pair, star, live] = await Promise.all([
      box(header.getByTestId(`token-icon-${base}`)), box(header.getByTestId(`fingerprint-${base}`)), box(header.getByTestId(`token-icon-${QUOTE}`)), box(header.getByTestId(`fingerprint-${QUOTE}`)),
      box(header.getByTestId('market-flip')), box(header.getByTestId('market-pair')), box(header.getByTestId('pair-favorite')), box(header.getByTestId('pair-live')),
    ]);
    // left to right: icon A, fingerprint A, icon B, fingerprint B, the tickers, the swap button, the star, all on one row
    expect(iconA.x).toBeLessThan(fpA.x);
    expect(fpA.x + fpA.width).toBeLessThanOrEqual(iconB.x + 1);
    expect(iconB.x).toBeLessThan(fpB.x);
    expect(fpB.x + fpB.width).toBeLessThanOrEqual(pair.x + 1);
    expect(pair.x + pair.width).toBeLessThanOrEqual(flip.x + 1);
    expect(flip.x + flip.width).toBeLessThanOrEqual(star.x + star.width);
    const bar = await box(header.getByTestId('market-bar'));
    expect(flip.x + flip.width).toBeLessThanOrEqual(bar.x + bar.width + 1);
    expect(live.x + live.width).toBeLessThanOrEqual(bar.x + bar.width + 1);
    // the pictures are 32 px on a desktop and 24 on a phone (two blocks in the first row)
    expect(iconA.height).toBeCloseTo(width >= 760 ? 32 : 24, 0);
    // the tickers share the row of the pictures
    for (const b of [pair, fpA, iconB, fpB, flip, star]) expect(Math.abs(b.y + b.height / 2 - (iconA.y + iconA.height / 2))).toBeLessThan(iconA.height / 2 + 4);
    // the fingerprint is 60 % of the icon
    expect(Math.abs(fpA.height / iconA.height - 0.6)).toBeLessThan(0.03);
    expect(Math.abs(fpB.height / iconB.height - 0.6)).toBeLessThan(0.03);
    await expect(header.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    // no line per token in the header any more: the name, the id and the program chip are in each token's details card
    await expect(header.getByTestId('pair-token-meta')).toHaveCount(0);
    await expect(header.getByTestId('token-name')).toHaveCount(0);
    const facts = page.getByTestId('pair-token-details');
    await expect(facts).toHaveCount(2);
    for (const [i, id, name] of [[0, base, null], [1, QUOTE, 'Example USD (fictional)']] as const) {
      const d = facts.nth(i);
      await expect(d.getByTestId('token-covenant-id')).toContainText(id);
      if (name) await expect(d.getByTestId('token-name')).toContainText(name);
      await expect(d.getByTestId('badge-family')).toHaveCount(1);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    // the selector is still there (market-unified.spec.ts drives it)
    await expect(header.getByTestId('market-base-select')).toBeAttached();
    if (width !== 880) await header.screenshot({ path: test.info().outputPath(`pair-header-${width}.png`) });
  });

  test(`${width} px: a token's KAS market header is the identity block, the pair, the swap and the figures in the bar`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    const header = page.getByTestId('token-header');
    await expect(header).toBeVisible();
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    const id = await header.getByTestId('token-title').getAttribute('data-covenant-id');
    const [icon, fp, pair, flip, live, stats, bar] = await Promise.all([
      box(header.getByTestId(`token-icon-${id}`)), box(header.getByTestId(`fingerprint-${id}`)), box(header.getByTestId('market-pair')), box(header.getByTestId('market-flip')),
      box(header.getByTestId('live-indicator')), box(header.getByTestId('market-stats')), box(header.getByTestId('market-bar')),
    ]);
    // the identity block at the left, then the pair and the swap; Live at the bar's right end; the figures beside them (a phone: under)
    expect(icon.x).toBeLessThan(fp.x);
    expect(fp.x + fp.width).toBeLessThanOrEqual(pair.x + 1);
    expect(pair.x + pair.width).toBeLessThanOrEqual(flip.x + 1);
    expect(live.x + live.width).toBeLessThanOrEqual(bar.x + bar.width + 1);
    if (width >= 760) expect(bar.height).toBeLessThanOrEqual(64);
    else expect(stats.y, 'a phone: the figures are a row under the pair').toBeGreaterThanOrEqual(pair.y + pair.height - 1);
    expect(Math.abs(fp.height / icon.height - 0.6)).toBeLessThan(0.03);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  });
}
