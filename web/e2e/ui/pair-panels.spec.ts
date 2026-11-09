// A token pair page has the same panels as a KAS market, in the same places: the 24 h strip, the chart, the depth chart of the pair book, the pair's
// fills and the facts of BOTH tokens. The founder price rule: prices come only from the two KAS markets (last, change, high, low, the chart), the
// fills of pair orders are VOLUME only (the volume tiles and the activity log's fill rows, which have amounts of both tokens, the counterparty and the time, and no
// price column). The flip (base <-> quote) applies to all of it. Mock stack: the pair seed (EXKCC/EXUSD with resting pair orders and an EXUSD KAS
// book), a seeded history of both KAS markets (the pair candles), and simulated fills of a resting pair ask and a resting pair bid.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);
const num = (s: string | null | undefined): number => Number((s ?? '').replace(/[,%+≥\s]/g, ''));

/** The example registry with EXKCC listed and a second listed KCC-20, EXUSD (6 decimals), matching the mock pair seed. */
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

/** Pair seed + the history of both KAS markets; `fills`: simulate one fill of a resting pair ask and one of a resting pair bid. Returns the EXKCC covenant id. */
async function setup(page: Page, mock: MockClient, fills: boolean): Promise<string> {
  await mock.seed({ pair: true });
  await mock.seedHistory({});
  await mock.seedHistory({ token: QUOTE, mid: 50_000_000, tick: 100_000, seed: 2 }); // 0.5 KAS per EXUSD
  await listPair(page);
  const base = (await mock.token()).covenant_id;
  if (fills) {
    const orders = await mock.orders('status=active&limit=200');
    const ask = orders.find((o) => o.contract === 'KobPair' && o.pair?.side === 'ask' && o.pair?.price === '50500')!;
    const bid = orders.find((o) => o.contract === 'KobPair' && o.pair?.side === 'bid')!;
    expect(ask, 'a seeded pair ask').toBeTruthy();
    expect(bid, 'a seeded pair bid').toBeTruthy();
    await mock.fill(ask.covenant_id);
    await mock.fill(bid.covenant_id);
  }
  return base;
}

const tile = (page: Page, id: string) => page.getByTestId(id).locator('.mkt-stat-value').first();
const PRICE_TILES = ['stat-last', 'stat-change', 'stat-high', 'stat-low'];
const VOLUME_TILES = ['stat-volume', 'stat-quote-volume', 'stat-trades'];

async function expectFigures(page: Page, ids: string[]): Promise<void> {
  // a figure (digits), never the dash and never the loading skeleton
  for (const id of ids) await expect(tile(page, id), id).toHaveText(/\d/, { timeout: 15_000 });
}

test('pair page: KAS-derived prices, pair fills as volume only, depth, both tokens\' facts, the same places as a KAS market; flip follows', async ({ appPage: page, mock }) => {
  const base = await setup(page, mock, true);
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('pair-page')).toHaveAttribute('data-base', base);

  // the header: line 1 = icon and fingerprint of A, icon and fingerprint of B, the pair text, the swap button, all one line (full layout: pair-header.spec.ts)
  const at = (id: string) => page.getByTestId('pair-page').getByTestId(id).first().boundingBox();
  const [icon, fp, iconB, sel, flipBtn] = await Promise.all([`token-icon-${base}`, `fingerprint-${base}`, `token-icon-${QUOTE}`, 'market-pair', 'market-flip'].map(at));
  expect(icon!.x).toBeLessThan(fp!.x);
  expect(fp!.x + fp!.width).toBeLessThanOrEqual(iconB!.x + 1);
  expect(iconB!.x + iconB!.width).toBeLessThanOrEqual(sel!.x + 1);
  expect(sel!.x + sel!.width).toBeLessThanOrEqual(flipBtn!.x + 1);
  expect(Math.min(sel!.y + sel!.height, icon!.y + icon!.height) - Math.max(sel!.y, icon!.y)).toBeGreaterThan(0);
  expect(fp!.height).toBeLessThan(icon!.height - 4);
  expect(Math.abs(fp!.y + fp!.height / 2 - (icon!.y + icon!.height / 2))).toBeLessThan(1.5);

  // 24 h strip: prices from the two KAS markets, the volumes and the fill count from the pair's own fills (2: the ask and the bid)
  await expectFigures(page, [...PRICE_TILES, ...VOLUME_TILES]);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-volume-source', 'fills');
  await expect(tile(page, 'stat-trades')).toHaveText('2');
  await expect(page.getByTestId('stat-volume')).toContainText('24h volume (EXKCC)');
  await expect(page.getByTestId('stat-quote-volume')).toContainText('24h volume (EXUSD)');
  const high = num(await tile(page, 'stat-high').innerText());
  const low = num(await tile(page, 'stat-low').innerText());
  expect(high).toBeGreaterThan(low);
  expect(low).toBeGreaterThan(0);
  // the chart is the pair price derived from the two KAS markets
  await expect(page.getByTestId('chart-unit')).toHaveText('EXUSD per EXKCC');
  await expect(page.getByTestId('pair-chart-note')).toContainText('derived from EXKCC/KAS and EXUSD/KAS');

  // depth chart of the pair book: both sides, in pair units
  const depth = page.getByTestId('depth-section');
  await expect(depth.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
  await expect(depth.getByTestId('depth-chart')).toContainText('EXKCC');
  await expect(depth.locator('.bid-area')).toHaveCount(1);
  await expect(depth.locator('.ask-area')).toHaveCount(1);

  // the pair's fills in the activity log (preset to the pair): amounts of both tokens and how each was filled, newest first; NO price (a pair fill
  // never makes a price)
  const trades = page.getByTestId('activity');
  await trades.scrollIntoViewIfNeeded();
  await expect(trades.locator('.act-scope')).toHaveText('EXKCC/EXUSD');
  const rows = trades.locator('[data-testid=activity-row][data-kind=fill]');
  await expect(rows).toHaveCount(2);
  const sides = await rows.evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.side));
  expect([...sides].sort()).toEqual(['buy', 'sell']);
  for (const r of await rows.all()) {
    await expect(r).toHaveAttribute('data-market', `${base}/${QUOTE}`);
    await expect(r.locator('.act-c-px')).toHaveText('—');
    await expect(r.locator('.act-c-who')).toContainText(/via KAS route|netting|from inventory/);
    // one whole EXKCC each (the mock fills one token by default), about 0.05 EXUSD each
    const a = await r.locator('.act-c-amt b').innerText();
    const b = await r.locator('.act-sub').innerText();
    expect(a).toMatch(/ EXKCC$/);
    expect(b).toMatch(/ EXUSD$/);
    expect(num(a.replace(' EXKCC', ''))).toBe(1);
    expect(Math.abs(num(b.replace(' EXUSD', '')) - 0.05)).toBeLessThan(0.002);
  }

  // the facts of both tokens: covenant id, decimals, price scale, template verification
  const facts = page.getByTestId('pair-token-details');
  await expect(facts).toHaveCount(2);
  await expect(facts.nth(0)).toHaveAttribute('data-token', base);
  await expect(facts.nth(1)).toHaveAttribute('data-token', QUOTE);
  await expect(facts.nth(0).getByTestId('token-covenant-id')).toContainText(base);
  await expect(facts.nth(1).getByTestId('token-covenant-id')).toContainText(QUOTE);
  await expect(facts.nth(1).getByTestId('token-scale')).toContainText('per 1 EXUSD (6 decimals)');
  await expect(facts.nth(0).getByTestId('token-program')).toContainText(/KCC-20 (.+) · template matched$/);
  await expect(facts.nth(0).getByTestId('token-status-details')).toBeVisible();
  // no box and no bullet sentences: one line of badges per token (the sentence is each badge's tooltip, the program and decimals the chip's)
  await expect(page.getByTestId('pair-details')).not.toContainText(/Not in the token registry|Full covenant id|Token program:/);
  await expect(page.getByTestId('pair-details').locator('.banner')).toHaveCount(0);
  await expect(page.getByTestId('pair-details').getByTestId('badge-family')).toHaveCount(2);
  await expect(facts.nth(1).getByTestId('token-program')).toContainText(/KCC-20 (.+) · template matched$/);
  await expect(facts.nth(1).getByTestId('token-status-details')).toBeVisible();
  // the same places as a KAS market: depth under the chart, the activity log under the grid, the facts below everything
  const box = async (id: string) => (await page.getByTestId(id).boundingBox())!;
  expect((await box('depth-section')).y).toBeGreaterThan((await box('chart-section')).y);
  expect((await box('activity')).y).toBeGreaterThan((await box('depth-section')).y + (await box('depth-section')).height - 1);
  expect((await box('pair-details')).y).toBeGreaterThan((await box('activity')).y + (await box('activity')).height - 1);

  // flip: base and quote swap (EXUSD/EXKCC); every panel follows. The page shows both orientations of the pair as one market, so the fills of the
  // EXKCC/EXUSD orders are still counted, while the prices are the inverse range of the same KAS markets
  await page.getByTestId('market-flip').click();
  await expect(page).toHaveURL(new RegExp(`#/market/${QUOTE}/${base}$`));
  await expect(page.getByTestId('pair-page')).toHaveAttribute('data-base', QUOTE);
  await expect(page.getByTestId('stat-volume')).toContainText('24h volume (EXUSD)');
  await expect(page.getByTestId('stat-quote-volume')).toContainText('24h volume (EXKCC)');
  await expectFigures(page, PRICE_TILES);
  await expect(tile(page, 'stat-trades')).toHaveText('2');
  const flipHigh = num(await tile(page, 'stat-high').innerText());
  const flipLow = num(await tile(page, 'stat-low').innerText());
  expect(flipHigh).toBeGreaterThan(flipLow);
  expect(flipLow).toBeGreaterThan(0);
  expect(flipHigh * low).toBeGreaterThan(0.5);
  expect(flipHigh * low).toBeLessThan(2);
  await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('depth-chart')).toContainText('EXUSD');
  // the log follows the page: the same two fills, turned round to EXUSD/EXKCC
  await trades.scrollIntoViewIfNeeded();
  await expect(trades.locator('.act-scope')).toHaveText('EXUSD/EXKCC');
  await expect(trades.getByTestId('activity-loading')).toHaveCount(0);
  const flipped = trades.locator('[data-testid=activity-row][data-kind=fill]');
  await expect(flipped).toHaveCount(2);
  for (const r of await flipped.all()) await expect(r).toHaveAttribute('data-market', `${QUOTE}/${base}`);
  const flippedFacts = page.getByTestId('pair-token-details');
  await expect(flippedFacts.nth(0)).toHaveAttribute('data-token', QUOTE);
  await expect(flippedFacts.nth(1)).toHaveAttribute('data-token', base);

  // the market list's row of the favourite pair, favoured as EXUSD/EXKCC, counts the fills of the EXKCC/EXUSD orders too (one market, not two): the volume is in
  // EXKCC (this row's quote) and the two fills are there
  await page.getByTestId('pair-favorite').click();
  await page.goto('/#/market');
  await page.getByTestId('token-list-favorites').click();
  const row = page.getByTestId(`pair-row-${QUOTE}-${base}`);
  await expect(row).toHaveAttribute('data-state', 'ready');
  await expect(row.getByTestId(`pair-volume-${QUOTE}-${base}`)).toHaveAttribute('title', /EXKCC · 2 pair fills in 24 h$/);
});

test('pair page without a pair fill: the prices still come from the two KAS markets, the volume is zero and the activity log has no fill', async ({ appPage: page, mock }) => {
  const base = await setup(page, mock, false);
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expectFigures(page, PRICE_TILES);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-volume-source', 'fills');
  await expect(tile(page, 'stat-trades')).toHaveText('0');
  await page.getByTestId('activity').scrollIntoViewIfNeeded();
  await expect(page.getByTestId('activity').getByTestId('activity-loading')).toHaveCount(0);
  await expect(page.getByTestId('activity').locator('[data-testid=activity-row][data-kind=fill]')).toHaveCount(0);
  await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('pair-token-details')).toHaveCount(2);
});
