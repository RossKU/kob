// A token pair page has the same panels as a KAS market, in the same places: the 24 h strip, the chart, the depth chart of the pair book, the pair's
// fills and the facts of BOTH tokens. The founder price rule: prices come only from the two KAS markets (last, change, high, low, the chart), the
// fills of pair orders are VOLUME only (the volume tiles and the fills list, which has amounts of both tokens, the counterparty and the time, and no
// price column). The flip (base <-> quote) applies to all of it. Mock stack: the pair seed (EXKCC/EXUSD with resting pair orders and an EXUSD KAS
// book), a seeded history of both KAS markets (the pair candles), and simulated fills of a resting pair ask and a resting pair bid.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);
const num = (s: string | null | undefined): number => Number((s ?? '').replace(/[,%+≥\s]/g, ''));
const decimalsOf = (x: string): number => (x.includes('.') ? x.split('.')[1]!.length : 0);

/** The example registry with EXKCC listed and a second listed KCC-20, EXUSD (6 decimals), matching the mock pair seed. */
async function listPair(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.status = 'listed';
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

  // the pair's fills: amounts of both tokens, the counterparty and the time, newest first; NO price column (a pair fill never makes a price)
  const trades = page.getByTestId('trades-section');
  const rows = trades.getByTestId('trade-row');
  await expect(rows).toHaveCount(2);
  await expect(trades.getByTestId('trades-list')).toHaveAttribute('data-source', 'pair');
  await expect(trades).toContainText('Pair fills');
  await expect(trades).toContainText('Amount (EXKCC)');
  await expect(trades).toContainText('Amount (EXUSD)');
  await expect(trades).toContainText('Filled via');
  await expect(trades).not.toContainText('Price');
  await expect(page.getByTestId('pair-trades-note')).toContainText('Pair fills: volume only');
  const sides = await rows.evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.side));
  expect([...sides].sort()).toEqual(['buy', 'sell']);
  const counterparties = await rows.evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.counterparty));
  for (const c of counterparties) expect(['route', 'netting', 'inventory']).toContain(c);
  const cells = async () => rows.evaluateAll((els) => els.map((e) => [...e.querySelectorAll('[role="cell"]')].map((c) => (c as HTMLElement).innerText)));
  const nat = await cells();
  expect(nat[0]).toHaveLength(4);
  expect(new Set(nat.map((r) => decimalsOf(r[0]!))).size).toBe(1);
  expect(new Set(nat.map((r) => decimalsOf(r[1]!))).size).toBe(1);
  // one whole EXKCC each (the mock fills one token by default), about 0.05 EXUSD each
  for (const r of nat) {
    expect(num(r[0])).toBe(1);
    expect(Math.abs(num(r[1]) - 0.05)).toBeLessThan(0.002);
  }

  // the facts of both tokens: covenant id, decimals, price scale, template verification
  const facts = page.getByTestId('pair-token-details');
  await expect(facts).toHaveCount(2);
  await expect(facts.nth(0)).toHaveAttribute('data-token', base);
  await expect(facts.nth(1)).toHaveAttribute('data-token', QUOTE);
  await expect(facts.nth(0).getByTestId('token-covenant-id')).toContainText(base);
  await expect(facts.nth(1).getByTestId('token-covenant-id')).toContainText(QUOTE);
  await expect(facts.nth(1).getByTestId('token-scale')).toContainText('per 1000000 base units');
  await expect(facts.nth(0).getByTestId('template-verification')).toBeVisible();
  await expect(facts.nth(1).getByTestId('template-verification')).toBeVisible();
  // the same places as a KAS market: depth and trades under the chart, the facts below everything
  const box = async (id: string) => (await page.getByTestId(id).boundingBox())!;
  expect((await box('depth-section')).y).toBeGreaterThan((await box('chart-section')).y);
  expect((await box('trades-section')).y).toBeGreaterThan((await box('chart-section')).y);
  expect((await box('pair-details')).y).toBeGreaterThan((await box('depth-section')).y + (await box('depth-section')).height - 1);

  // flip: base and quote swap (another pair: EXUSD/EXKCC); every panel follows. The fills above are of EXKCC/EXUSD orders: an oriented pair view
  // never lists them inverted, so the flipped pair has no fill and no volume, while its prices are the inverse range of the same KAS markets
  await page.getByTestId('market-flip').click();
  await expect(page).toHaveURL(new RegExp(`#/market/${QUOTE}/${base}$`));
  await expect(page.getByTestId('pair-page')).toHaveAttribute('data-base', QUOTE);
  await expect(page.getByTestId('stat-volume')).toContainText('24h volume (EXUSD)');
  await expect(page.getByTestId('stat-quote-volume')).toContainText('24h volume (EXKCC)');
  await expectFigures(page, PRICE_TILES);
  await expect(tile(page, 'stat-trades')).toHaveText('0');
  const flipHigh = num(await tile(page, 'stat-high').innerText());
  const flipLow = num(await tile(page, 'stat-low').innerText());
  expect(flipHigh).toBeGreaterThan(flipLow);
  expect(flipLow).toBeGreaterThan(0);
  expect(flipHigh * low).toBeGreaterThan(0.5);
  expect(flipHigh * low).toBeLessThan(2);
  await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('depth-chart')).toContainText('EXUSD');
  await expect(trades).toContainText('Amount (EXUSD)');
  await expect(page.getByTestId('trades-empty')).toBeVisible();
  const flippedFacts = page.getByTestId('pair-token-details');
  await expect(flippedFacts.nth(0)).toHaveAttribute('data-token', QUOTE);
  await expect(flippedFacts.nth(1)).toHaveAttribute('data-token', base);
});

test('pair page without a pair fill: the prices still come from the two KAS markets, the volume is zero and the fills list says what it contains', async ({ appPage: page, mock }) => {
  const base = await setup(page, mock, false);
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expectFigures(page, PRICE_TILES);
  await expect(page.getByTestId('market-stats')).toHaveAttribute('data-volume-source', 'fills');
  await expect(tile(page, 'stat-trades')).toHaveText('0');
  await expect(page.getByTestId('trades-empty')).toBeVisible();
  await expect(page.getByTestId('pair-trades-note')).toContainText('Pair fills: volume only');
  await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');
  await expect(page.getByTestId('pair-token-details')).toHaveCount(2);
});
