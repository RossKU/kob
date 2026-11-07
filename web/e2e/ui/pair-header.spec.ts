// The two market headers (founder, 2026-11-02).
// A token pair: line 1 = [icon A][fingerprint A] / [icon B][fingerprint B], the tickers ("TETH / TUSD", the pair selector) and the swap button, one line
// at every width (the identity blocks are 36 px on a phone); then one compact line per
// token (ticker, short id with copy, name, chips). A token's KAS market: compact, the identity block at the left and two lines to its right (ticker / KAS with
// the swap; then the short id with copy, the name, the Live chip, KCC-20 and the genesis chip), no row half empty. Checked at 1280, 880, 390 and 360 px.
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
  test(`${width} px: the pair header is icons and fingerprints, the tickers and the swap on one line, then one info line per token`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 900 });
    await mock.seed({ pair: true });
    await fund(mock, 'alice');
    await listPair(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    const header = page.getByTestId('pair-header');
    await expect(header).toBeVisible();
    const [iconA, fpA, iconB, fpB, flip, pair] = await Promise.all([
      box(header.getByTestId(`token-icon-${base}`)), box(header.getByTestId(`fingerprint-${base}`)), box(header.getByTestId(`token-icon-${QUOTE}`)), box(header.getByTestId(`fingerprint-${QUOTE}`)),
      box(header.getByTestId('market-flip')), box(header.getByTestId('market-pair')),
    ]);
    // line 1, left to right: icon A, fingerprint A, icon B, fingerprint B, the tickers, the swap button, all on one row
    expect(iconA.x).toBeLessThan(fpA.x);
    expect(fpA.x + fpA.width).toBeLessThanOrEqual(iconB.x + 1);
    expect(iconB.x).toBeLessThan(fpB.x);
    expect(fpB.x + fpB.width).toBeLessThanOrEqual(pair.x + 1);
    expect(pair.x + pair.width).toBeLessThanOrEqual(flip.x + 1);
    expect(flip.x + flip.width).toBeLessThanOrEqual((await box(header)).x + (await box(header)).width + 1);
    // the pictures are 64 px on a desktop and smaller than the token header's on a phone
    if (width >= 760) expect(Math.abs(iconA.height - 64)).toBeLessThan(2);
    else expect(iconA.height).toBeLessThanOrEqual(36);
    // the tickers share the line of the pictures: their boxes overlap vertically
    for (const b of [iconA, fpA, iconB, fpB]) expect(Math.min(b.y + b.height, pair.y + pair.height) - Math.max(b.y, pair.y)).toBeGreaterThan(0);
    for (const b of [pair, fpA, iconB, fpB, flip]) expect(Math.abs(b.y + b.height / 2 - (iconA.y + iconA.height / 2))).toBeLessThan(iconA.height / 2);
    // the fingerprint is 60 % of the icon
    expect(Math.abs(fpA.height / iconA.height - 0.6)).toBeLessThan(0.03);
    expect(Math.abs(fpB.height / iconB.height - 0.6)).toBeLessThan(0.03);
    await expect(header.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    // then a compact line per token: ticker, short id with copy, name, chips
    const lines = header.getByTestId('pair-token-meta');
    await expect(lines).toHaveCount(2);
    for (const [i, id, ticker, name] of [[0, base, 'EXKCC', null], [1, QUOTE, 'EXUSD', 'Example USD (fictional)']] as const) {
      const line = lines.nth(i);
      await expect(line.getByTestId('token-line-ticker')).toHaveText(ticker);
      await expect(line.getByTestId('pair-token-short-id')).toHaveAttribute('data-value', id);
      await expect(line.getByTestId('pair-token-short-id')).toHaveText(id);
      await expect(line.getByTestId('pair-token-short-id-copy')).toBeVisible();
      if (name) await expect(line.getByTestId('token-name')).toHaveText(name);
      await expect(line.getByTestId('badge-family')).toBeVisible();
      expect((await line.boundingBox())!.y).toBeGreaterThanOrEqual(iconA.y + iconA.height - 1);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    // the selector is still there (market-unified.spec.ts drives it)
    await expect(header.getByTestId('market-base-select')).toBeAttached();
    if (width !== 880) await header.screenshot({ path: test.info().outputPath(`pair-header-${width}.png`) });
  });

  test(`${width} px: a token's KAS market header is the identity block with two lines to its right, no empty row`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    const header = page.getByTestId('token-header');
    await expect(header).toBeVisible();
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    const id = await header.getByTestId('token-title').getAttribute('data-covenant-id');
    const [icon, fp, pair, flip, meta, live] = await Promise.all([
      box(header.getByTestId(`token-icon-${id}`)), box(header.getByTestId(`fingerprint-${id}`)), box(header.getByTestId('market-pair')), box(header.getByTestId('market-flip')),
      box(header.getByTestId('token-meta')), box(header.getByTestId('live-indicator')),
    ]);
    // the identity block at the left, the two lines to the right of it: line 1 = ticker / KAS and the swap, line 2 = id, name, chips
    expect(icon.x).toBeLessThan(fp.x);
    expect(fp.x + fp.width).toBeLessThanOrEqual(pair.x + 1);
    expect(pair.x + pair.width).toBeLessThanOrEqual(flip.x + 1);
    expect(meta.x).toBeGreaterThanOrEqual(fp.x + fp.width - 1);
    expect(meta.y).toBeGreaterThanOrEqual(pair.y + pair.height - 2);
    expect(live.y).toBeGreaterThanOrEqual(pair.y + pair.height - 2);
    // compact: the two lines sit beside the picture, not under it (the header is not taller than the picture plus its text)
    const h = (await header.boundingBox())!;
    expect(h.height).toBeLessThan(width < 600 ? 190 : 120);
    // no empty row: the identity block is centred on the text beside it, the text neither above nor below it by more than its own half
    const text = { top: Math.min(pair.y, meta.y), bottom: Math.max(pair.y + pair.height, meta.y + meta.height) };
    expect(Math.abs((icon.y + icon.height / 2) - (text.top + text.bottom) / 2)).toBeLessThan(Math.max(icon.height, text.bottom - text.top) / 2);
    expect(Math.abs(fp.height / icon.height - 0.6)).toBeLessThan(0.03);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  });
}
