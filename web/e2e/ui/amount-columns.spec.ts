// Amounts in the order book, the pair book and the trade tapes never lose a digit to the cell edge ("56,609,95…", "0.0000000…"): a column too
// wide for its cells shows fewer decimals or K / M / B, and the exact amount is in the cell's / row's tooltip. Tokens of 0 and 18 decimals with
// huge and one-base-unit amounts (the indexer answers are rewritten on the way), on a desktop and a phone width. Mock stack.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';

test.use({ autoOpen: false });

const PAIR_QUOTE = 'e7'.repeat(32);

/** The example registry with EXKCC listed plus the extra listed tokens. */
async function listTokens(page: Page, extra: { ticker: string; covenant_id: string; decimals: number }[]): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.verified = true;
  for (const e of extra) reg.tokens.push({ ...exkcc, name: e.ticker, ...e });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

/** Rewrites a JSON answer of the mock indexer whose path matches `re`. */
async function rewrite(page: Page, re: RegExp, edit: (body: Record<string, unknown>) => void): Promise<void> {
  await page.route(re, async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    edit(body);
    await route.fulfill({ response: res, body: JSON.stringify(body) });
  });
}

/** Every numeric cell of the books and tapes on screen whose text is cut by its box (the cell is narrower than its text). */
async function cutCells(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const sel = '[data-testid=book-ask-row] > span, [data-testid=book-bid-row] > span, [data-testid=pair-level] > span:not(.pair-src), [data-testid=trade-row] > span';
    return [...document.querySelectorAll<HTMLElement>(sel)].filter((el) => el.scrollWidth > el.clientWidth + 1).map((el) => `${el.textContent} (${el.scrollWidth} > ${el.clientWidth})`);
  });
}

async function atBothWidths(page: Page, check: () => Promise<void>): Promise<void> {
  for (const width of [1280, 375]) {
    await page.setViewportSize({ width, height: 900 });
    await check();
  }
}

for (const decimals of [0, 18]) {
  test(`token book and trade tape of ${decimals} decimals: huge and one-base-unit amounts keep every digit`, async ({ appPage: page, mock }) => {
    const id = (decimals === 0 ? 'd0' : 'd1').repeat(32);
    const ticker = decimals === 0 ? 'WHOLE' : 'EIGHTEEN';
    const unit = 10n ** BigInt(decimals);
    const scale = decimals === 0 ? 1n : 1_000_000_000n;
    await mock.seed({ tokens: [{ ticker, name: ticker, covenant_id: id, program: 'KCC20Ref_8x8', extension_commitment: 'ee'.repeat(32), decimals, tick: 10_000 }] });
    await mock.seed({ book: { token: id, mid: 2_500_000, tick: 10_000, levels: 3 } });
    await listTokens(page, [{ ticker, covenant_id: id, decimals }]);
    const huge = 56_609_953n * unit + (decimals ? 1n : 0n);
    await rewrite(page, new RegExp(`/v1/books/${id}`), (b) => {
      const asks = b.asks as { amount: string }[];
      const bids = b.bids as { amount: string }[];
      asks[0]!.amount = huge.toString();
      bids[0]!.amount = '1';
      bids[1]!.amount = (12_345_678_901n * unit).toString();
    });
    const now = Date.now();
    const trade = (n: number, amount: bigint) => ({ id: n, txid: n.toString(16).padStart(64, '0'), ts: now - n * 1000, daa: 1000 - n, price: '2500000', amount: amount.toString(), quote: '1000', side: n % 2 ? 'buy' : 'sell', fills: 1, confirmations: 10, settled: true });
    await page.route(new RegExp(`/v1/trades/${id}`), (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify({ token: id, price_basis: scale.toString(), decimals, items: [trade(1, huge), trade(2, 1n), trade(3, 3n * unit)], next_cursor: null }) }),
    );
    await page.goto(`/#/market/${id}`);
    const book = page.getByTestId('order-book');
    await expect(book.getByTestId('book-ask-row').first()).toBeVisible();
    await expect(page.getByTestId('trade-row')).toHaveCount(3);
    const bigRow = book.locator(`[data-testid=book-ask-row][data-amount="${huge}"]`);
    await expect(bigRow.locator(':scope > span').nth(1)).toHaveText(/^~?56\.61M$/);
    await expect(bigRow.locator(':scope > span').nth(1)).toHaveAttribute('title', decimals ? `56,609,953.000000000000000001 ${ticker}` : `56,609,953 ${ticker}`);
    // a level the mock marks as estimated carries the "~" mark before the amount
    await expect(book.locator('[data-testid=book-bid-row][data-amount="1"] > span').nth(1)).toHaveText(decimals ? /^~?<0\.01$/ : /^~?1$/);
    await expect(page.getByTestId('trade-row').first().locator(':scope > span').nth(1)).toHaveText('56.61M');
    await expect(page.getByTestId('trade-row').first().locator(':scope > span').nth(1)).toHaveAttribute('title', decimals ? `56,609,953.000000000000000001 ${ticker}` : `56,609,953 ${ticker}`);
    await atBothWidths(page, async () => {
      expect(await cutCells(page)).toEqual([]);
    });
    // a token without decimals takes whole numbers: its amount box says so
    await expect(page.getByTestId('order-amount')).toHaveAttribute('placeholder', decimals === 0 ? '0' : '0.0');
  });
}

test('pair book and pair fills: a huge and a one-base-unit amount keep every digit', async ({ appPage: page, mock }) => {
  await mock.seed({ pair: true });
  await listTokens(page, [{ ticker: 'EXUSD', covenant_id: PAIR_QUOTE, decimals: 6 }]);
  const base = (await mock.token()).covenant_id;
  await rewrite(page, /\/v1\/pairs\/[0-9a-f]{64}\/[0-9a-f]{64}\/book/, (b) => {
    const asks = b.asks as { amount: string }[];
    const bids = b.bids as { amount: string }[];
    if (asks[0]) asks[0].amount = '5660995312345678';
    if (bids[0]) bids[0].amount = '1';
  });
  const now = Date.now();
  const fill = (n: number, a: string, bAmt: string) => ({
    id: n, txid: n.toString(16).padStart(64, '0'), ts: now - n * 1000, daa: 1000 - n, order: 'ab'.repeat(32), contract: 'KobPair', side: 'ask', amount_a: a, amount_b: bAmt,
    price: null, price_num: null, price_den: null, a_scale: 100_000_000, tip_kas: null, counterparty: 'route', price_source: 'kas', confirmations: 10, settled: true,
  });
  await page.route(/\/v1\/pairs\/[0-9a-f]{64}\/[0-9a-f]{64}\/fills/, (route) => {
    const m = /pairs\/([0-9a-f]{64})\/([0-9a-f]{64})\/fills/.exec(route.request().url())!;
    return route.fulfill({
      status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' },
      body: JSON.stringify({ base: m[1], quote: m[2], volume_24h: { amount_a: '0', amount_b: '0', fills: 2 }, items: [fill(1, '5660995312345678', '123456789012345'), fill(2, '1', '1')], next_cursor: null }),
    });
  });
  await page.goto(`/#/market/${base}/${PAIR_QUOTE}`);
  await expect(page.getByTestId('pair-level').first()).toBeVisible();
  await expect(page.getByTestId('trade-row')).toHaveCount(2);
  const big = page.locator('[data-testid=pair-level][data-amount="5660995312345678"] > span').nth(1);
  await expect(big).toHaveText('56.61M');
  await expect(big).toHaveAttribute('title', '56,609,953.12345678 EXKCC');
  await expect(page.getByTestId('trade-row').first().locator(':scope > span').first()).toHaveText('56.61M');
  await atBothWidths(page, async () => {
    expect(await cutCells(page)).toEqual([]);
  });
});
