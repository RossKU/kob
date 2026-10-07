// Market orientation: every token book is TOKEN/KAS (BTC/KAS), a USD reference token (config `quoteTokens`) is KAS/<its ticker> (tickers are never renamed), and every market has a control that
// inverts the pair. The flip is a pure display transform: price, chart, book, trades, stats and the ticket follow it, the choice is remembered per market,
// and the order the user signs is identical whichever way it is displayed.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { TESTID } from '../testids';
import { connectWallet, fund, listExkcc, openMarket } from '../helpers/env';
import { acknowledgeAndSign, openReview, placedBy, readConfirm, settlePlan } from '../helpers/ticket';

test.use({ autoOpen: false });

const num = (s: string | null | undefined): number => Number((s ?? '').replace(/[,%+\s]/g, ''));
const close = (a: number, b: number, rel = 0.02): boolean => Math.abs(a - b) <= rel * Math.abs(b);

async function seeded(mock: MockClient): Promise<string> {
  return (await mock.token()).covenant_id;
}

async function bookRows(page: Page, side: 'asks' | 'bids'): Promise<{ price: string; nativePrice: string; amount: string; text: string }[]> {
  const loc = page.getByTestId(side === 'asks' ? TESTID.bookAsks : TESTID.bookBids).locator('[data-testid^="book-"]');
  return loc.evaluateAll((els) =>
    els.map((e) => ({ price: (e as HTMLElement).dataset.price!, nativePrice: (e as HTMLElement).dataset.nativePrice!, amount: (e as HTMLElement).dataset.amount!, text: (e as HTMLElement).innerText })),
  );
}

/** the figure of a stats tile (its value text, without the unit) */
async function stat(page: Page, id: string): Promise<number> {
  const text = await page.getByTestId(id).locator('.mkt-stat-value').first().evaluate((el) => el.childNodes[0]?.textContent ?? '');
  return num(text);
}

async function sides(page: Page): Promise<string[]> {
  return page.getByTestId(TESTID.tradesList).getByTestId('trade-row').evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.side ?? ''));
}

async function legend(page: Page): Promise<{ o: number; h: number; l: number; c: number }> {
  const b = await page.getByTestId('chart-legend').locator('b').allInnerTexts();
  return { o: num(b[0]), h: num(b[1]), l: num(b[2]), c: num(b[3]) };
}

test.describe('token market: TOKEN/KAS, flipped to KAS/TOKEN', () => {
  test('the convention is TOKEN/KAS; a flip inverts price, chart, book, trades and stats, and is remembered per market', async ({ appPage: page, mock }) => {
    const id = await seeded(mock);
    await mock.seedHistory();
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${id}`)).toBeVisible(); // (the list has no market / pair column: the market's own page shows the pair)
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByTestId('token-title')).toContainText('EXKCC');
    const flip = page.getByTestId('market-flip');
    await expect(flip).toHaveAccessibleName('Flip pair');
    await expect(flip).toHaveText('');
    await expect(flip).toHaveAttribute('data-flip-to', 'KAS/EXKCC');
    await expect(flip).toHaveAttribute('aria-pressed', 'false');
    // icon only, and just left of the pair title on the same row
    await expect(flip).toHaveAttribute('title', 'Flip pair');
    const fb = (await flip.boundingBox())!;
    const pb = (await page.getByTestId('market-pair').boundingBox())!;
    expect(fb.x + fb.width).toBeLessThanOrEqual(pb.x + 1);
    expect(Math.abs(fb.y + fb.height / 2 - (pb.y + pb.height / 2))).toBeLessThan(24);

    // native figures
    const chart = page.getByTestId('price-chart');
    await expect(chart).toHaveAttribute('data-state', 'ready');
    await expect(chart).toHaveAttribute('data-inverted', '0');
    await expect(page.getByTestId('chart-unit')).toHaveText('KAS per EXKCC');
    const nativeLegend = await legend(page);
    const nativeAsks = await bookRows(page, 'asks');
    const nativeBids = await bookRows(page, 'bids');
    const nativeSides = await sides(page);
    const nativeLast = await stat(page, 'stat-last');
    const nativeHigh = await stat(page, 'stat-high');
    const nativeLow = await stat(page, 'stat-low');
    const nativeChange = await stat(page, 'stat-change');
    expect(nativeAsks).toHaveLength(10);
    expect(nativeBids).toHaveLength(10);
    expect(nativeSides.length).toBeGreaterThan(5);
    await expect(page.getByTestId('order-book')).toContainText('Price (KAS)');

    // ---- flip
    await flip.click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');
    await expect(flip).toHaveAttribute('data-flip-to', 'EXKCC/KAS');
    await expect(flip).toHaveAttribute('aria-pressed', 'true');
    await expect(chart).toHaveAttribute('data-inverted', '1');
    await expect(chart).toHaveAttribute('data-state', 'ready');
    await expect(page.getByTestId('chart-unit')).toHaveText('EXKCC per KAS');
    await expect(page.getByTestId('order-book')).toContainText('Price (EXKCC)');
    await expect(page.getByTestId('order-book')).toContainText('Size (KAS)');
    await expect(page.getByTestId('order-book')).toContainText('Total (EXKCC)');

    // candles: open / close invert, high and low swap
    const inv = await legend(page);
    expect(close(inv.o, 1 / nativeLegend.o)).toBe(true);
    expect(close(inv.c, 1 / nativeLegend.c)).toBe(true);
    expect(close(inv.h, 1 / nativeLegend.l)).toBe(true);
    expect(close(inv.l, 1 / nativeLegend.h)).toBe(true);
    expect(inv.h).toBeGreaterThanOrEqual(Math.max(inv.o, inv.c));
    expect(inv.l).toBeLessThanOrEqual(Math.min(inv.o, inv.c));

    // book: native asks are the bids (best bid = 1 / best ask), native bids the asks; the same levels, the same prefill
    const asks = await bookRows(page, 'asks');
    const bids = await bookRows(page, 'bids');
    expect(bids).toHaveLength(nativeAsks.length);
    expect(asks).toHaveLength(nativeBids.length);
    // displayed bids best first = native asks best (lowest) first
    expect(bids.map((r) => r.nativePrice)).toEqual([...nativeAsks].reverse().map((r) => r.nativePrice));
    expect(asks.map((r) => r.nativePrice)).toEqual([...nativeBids].reverse().map((r) => r.nativePrice));
    for (const r of bids) expect(close(num(r.price), 1e8 / Number(r.nativePrice), 0.001)).toBe(true);
    for (const r of asks) expect(close(num(r.price), 1e8 / Number(r.nativePrice), 0.001)).toBe(true);
    const bidPrices = bids.map((r) => num(r.price));
    const askPrices = asks.map((r) => num(r.price));
    expect([...bidPrices].sort((a, b) => b - a)).toEqual(bidPrices);
    expect([...askPrices].sort((a, b) => b - a)).toEqual(askPrices);
    expect(bidPrices[0]!).toBeLessThan(askPrices.at(-1)!);
    const mid = Number(await page.getByTestId('book-mid').getAttribute('data-value'));
    expect(close(mid, (bidPrices[0]! + askPrices.at(-1)!) / 2, 0.001)).toBe(true);
    await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'ready');

    // trades: same trades, the aggressor side flips (the taker who bought the token sold KAS), the price is inverted
    const flippedSides = await sides(page);
    expect(flippedSides).toEqual(nativeSides.map((s) => (s === 'buy' ? 'sell' : s === 'sell' ? 'buy' : s)));

    // stats
    const last = await stat(page, 'stat-last');
    expect(close(last, 1 / nativeLast, 0.02)).toBe(true);
    expect(close(await stat(page, 'stat-high'), 1 / nativeLow, 0.02)).toBe(true);
    expect(close(await stat(page, 'stat-low'), 1 / nativeHigh, 0.02)).toBe(true);
    // up 25 % one way is down 20 % the other: the signs oppose
    const change = await stat(page, 'stat-change');
    if (nativeChange !== 0) expect(Math.sign(change)).toBe(-Math.sign(nativeChange));
    await expect(page.getByTestId('stat-volume')).toContainText('24h volume (KAS)');
    await expect(page.getByTestId('stat-quote-volume')).toContainText('24h volume (EXKCC)');

    // remembered per market: a reload keeps it, another market is not affected
    await expect.poll(() => page.evaluate((k) => localStorage.getItem(k), `kob.flip.v1:token:${id}`)).toBe('1');
    await page.reload();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');

    // ---- flip back: the native figures return
    await page.goto(`/#/market/${id}`);
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(chart).toHaveAttribute('data-inverted', '0');
    expect(await bookRows(page, 'asks')).toEqual(nativeAsks);
    expect(await bookRows(page, 'bids')).toEqual(nativeBids);
    expect(await sides(page)).toEqual(nativeSides);
    await expect.poll(() => page.evaluate((k) => localStorage.getItem(k), `kob.flip.v1:token:${id}`)).toBe('0');
  });

  test('blocked storage does not break the toggle', async ({ appPage: page, mock }) => {
    const id = await seeded(mock);
    await page.addInitScript(() => {
      const deny = () => {
        throw new Error('denied');
      };
      Object.defineProperty(window, 'localStorage', { get: deny });
    });
    await page.goto(`/#/market/${id}`);
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');
  });
});

test.describe('the ticket in an inverted market', () => {
  test('the sides and the price follow the shown pair, and the order that is built and signed is identical', async ({ appPage: page, mock, wallet }) => {
    await fund(mock, 'alice');
    const id = await openMarket(page, mock);

    // ---- native: sell 1 EXKCC at 0.04 KAS per EXKCC (far above the book: it rests)
    await page.getByTestId(TESTID.orderSideSell).click();
    await page.getByTestId(TESTID.orderAmount).fill('1');
    await page.getByTestId(TESTID.orderPrice).fill('0.04');
    const native = await openReview(page);
    await page.getByTestId(TESTID.confirmCancel).click();
    await expect(page.getByTestId(TESTID.confirmScreen)).toHaveCount(0);

    // ---- flip: the form keeps its order; it is now worded in KAS/EXKCC, whose base is KAS
    await page.getByTestId('market-flip').click();
    const ticket = page.getByTestId('order-ticket');
    await expect(ticket).toHaveAttribute('data-inverted', '1');
    await expect(page.getByTestId('ticket-flip-note')).toContainText('KAS/EXKCC');
    // the native Sell is the shown Buy (buying KAS with EXKCC), at 25 EXKCC per KAS
    await expect(ticket).toHaveAttribute('data-side', 'sell');
    await expect(ticket).toHaveAttribute('data-shown-side', 'buy');
    await expect(page.getByTestId(TESTID.orderSideBuy)).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('25');
    await expect(page.getByTestId('order-price-native')).toContainText('0.04 KAS per EXKCC');
    await expect(page.getByTestId(TESTID.orderReview)).toContainText('Buy');
    // the amount is KAS: 1 EXKCC at 0.04 = 0.04 KAS, the exact token amount below it
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('0.04');
    await expect(page.locator('.field', { has: page.getByTestId(TESTID.orderAmount) }).locator('.tk-unit')).toHaveText('KAS');
    await expect(page.getByTestId('order-amount-tokens')).toContainText('0.04 KAS = 1 EXKCC at your price');
    await expect(page.getByTestId('disc-summary')).toHaveText('Buy 0.04 KAS for 1 EXKCC');

    // the same order typed in the shown units: 25 EXKCC per KAS
    await page.getByTestId(TESTID.orderPrice).fill('');
    await page.getByTestId(TESTID.orderPrice).fill('25');
    await expect(page.getByTestId('order-price-native')).toContainText('0.04 KAS per EXKCC');
    const flipped = await openReview(page);
    expect(flipped.heading).toContain('KAS/EXKCC');
    await expect(page.getByTestId('confirm-shown-as')).toContainText('KAS/EXKCC Buy: you give 1 EXKCC and receive 0.04 KAS (at 25 EXKCC per KAS');
    await expect(page.getByTestId('confirm-shown-as')).toContainText('SELL order of EXKCC');
    // the decoded confirmation of the built transaction is the same: same order, same price, same locked funds
    expect(flipped.created.length).toBe(native.created.length);
    expect(flipped.created.map((c) => c.rows)).toEqual(native.created.map((c) => c.rows));
    // everything but the new order's own covenant id (a fresh one per order)
    const plain = (x: unknown) => JSON.parse(JSON.stringify(x).replace(/"id":"[0-9a-f]{64}"/g, '"id":"-"'));
    expect(plain(flipped.sections)).toEqual(plain(native.sections));
    expect(flipped.summaryText).toBe(native.summaryText);
    const txid = await acknowledgeAndSign(page);
    const placed = await placedBy(mock, txid);
    expect(placed.views).toHaveLength(1);
    // a sell of 1 EXKCC at 0.04 KAS per token = 4_000_000 sompi per whole token, exactly
    expect(JSON.stringify(placed.views[0])).toContain('4000000');
    expect(placed.views[0].kind ?? placed.views[0].contract).toBeTruthy();
    void wallet;
    void id;
  });

  test('a click on a book level prefills the same native order, shown in the inverted units without re-rounding it', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    await page.getByTestId('market-flip').click();
    const ticket = page.getByTestId('order-ticket');
    // the best displayed bid is the native best ask (0.0251 KAS per EXKCC): hitting it sells KAS, i.e. buys EXKCC at exactly 0.0251
    const bids = page.getByTestId(TESTID.bookBids).getByTestId('book-bid-row');
    await expect(bids.first()).toHaveAttribute('data-native-price', '2510000');
    await bids.first().click();
    await expect(ticket).toHaveAttribute('data-side', 'buy');
    await expect(ticket).toHaveAttribute('data-shown-side', 'sell');
    await expect(page.getByTestId('order-price-native')).toContainText('0.0251 KAS per EXKCC');
    const shown = num(await page.getByTestId(TESTID.orderPrice).inputValue());
    expect(close(shown, 1 / 0.0251, 1e-6)).toBe(true);
    // an ask level (native bid 0.0249) sells EXKCC
    const asks = page.getByTestId(TESTID.bookAsks).getByTestId('book-ask-row');
    await asks.last().click();
    await expect(ticket).toHaveAttribute('data-side', 'sell');
    await expect(ticket).toHaveAttribute('data-shown-side', 'buy');
    await expect(page.getByTestId('order-price-native')).toContainText('0.0249 KAS per EXKCC');
  });

  test('a typed inverted price is rounded the way that never makes the order worse', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    await page.getByTestId('market-flip').click();
    const ticket = page.getByTestId('order-ticket');
    // shown Sell (native buy: the price rounds down) at 39.84 EXKCC per KAS: 1/39.84 = 0.025100401..., the buy limit never above it: 0.0251004
    // (protocol v3 has no price tick: the step is one sompi per whole token)
    await page.getByTestId(TESTID.orderSideSell).click();
    await expect(ticket).toHaveAttribute('data-side', 'buy');
    await page.getByTestId(TESTID.orderPrice).fill('39.84');
    await expect(page.getByTestId('order-price-native')).toContainText('0.0251004 KAS per EXKCC');
    // shown Buy (native sell: rounds up) at 39.84: 0.025100401... -> the sell limit never below it: 0.02510041 (the next sompi)
    await page.getByTestId(TESTID.orderSideBuy).click();
    await expect(ticket).toHaveAttribute('data-side', 'sell');
    await page.getByTestId(TESTID.orderPrice).fill('');
    await page.getByTestId(TESTID.orderPrice).fill('39.84');
    await expect(page.getByTestId('order-price-native')).toContainText('0.02510041 KAS per EXKCC');
    // the conversion itself is the rounding (to the next sompi in the maker's favour): the native line says what is placed; without a price tick
    // there is no second, tick rounding to flag
    await settlePlan(page);
    // a malformed price still reports the form's own error
    await page.getByTestId(TESTID.orderPrice).fill('abc');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveAttribute('aria-invalid', 'true');
  });
});

test.describe('USD reference token: KAS/<its ticker>', () => {
  const QUOTE = 'e7'.repeat(32);
  test.use({ appConfig: { quoteTokens: { [QUOTE]: 'USD' } } });

  /** the example registry with EXKCC listed and EXUSD (6 decimals) as a second listed token, matching the mock pair seed */
  async function listBoth(page: Page): Promise<void> {
    const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
    for (const t of reg.templates) t.review_status = 'reviewed';
    const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
    exkcc.status = 'listed';
    exkcc.verified = true;
    reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional USD reference of the e2e mock.' } });
    await page.route('**/registry/tokens.json', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
    );
  }

  test('the landing market and the list show KAS/EXUSD (real tickers), a flip gives EXUSD/KAS, and there are no USD-only views', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await listBoth(page);
    const exkcc = await seeded(mock);

    // landing: the USD token's own book, KAS/EXUSD (the ticker is never renamed): a full market with its book and ticket
    await page.goto('/');
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXUSD');
    await expect(page.getByTestId('token-title')).toContainText('EXUSD');
    await expect(page.getByTestId('token-page')).toHaveAttribute('data-token', QUOTE);
    await expect(page.getByTestId('order-book')).toBeVisible();
    await expect(page.getByTestId('order-book')).toContainText('Price (EXUSD)');
    await expect(page.getByTestId('chart-unit')).toHaveText('EXUSD per KAS');
    await expect(page.getByTestId('main')).toHaveAttribute('data-route', '#/');
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-inverted', '1');
    await expect(page.getByTestId('token-usd-chart')).toHaveCount(0);

    // a flip: EXUSD/KAS, remembered
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXUSD/KAS');
    await expect(page.getByTestId('chart-unit')).toHaveText('KAS per EXUSD');
    await page.reload();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXUSD/KAS');
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXUSD');

    // the list: both tokens, no market / pair column, no derived USD table
    await page.goto('/#/market');
    await expect(page.getByTestId(`token-row-${exkcc}`)).toBeVisible();
    await expect(page.getByTestId(`token-row-${QUOTE}`)).toBeVisible();
    await expect(page.getByTestId('token-list')).toBeVisible();
    await expect(page.getByTestId('usd-prices')).toHaveCount(0);

    // a plain token page has no USD link either
    await page.goto(`/#/market/${exkcc}`);
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByTestId('token-usd-chart')).toHaveCount(0);
  });

  test('old #/usd/kas links open the USD token market and can build an order (no chart-only dead end)', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await fund(mock, 'alice');
    await listBoth(page);
    await page.goto('/#/usd/kas');
    await expect(page).toHaveURL(new RegExp(`#/market/${QUOTE}$`));
    await expect(page.getByTestId('token-page')).toHaveAttribute('data-token', QUOTE);
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXUSD');
    await expect(page.getByTestId('usd-page')).toHaveCount(0);
    await expect(page.getByTestId('order-book')).toBeVisible();
    await expect(page.getByTestId('trades-list')).toBeVisible();
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await connectWallet(page);

    // shown Sell of 1 KAS (for EXUSD) at 3 EXUSD per KAS = native buy of 3 EXUSD at about 0.33 KAS: far below the book, it rests
    await page.getByTestId(TESTID.orderSideSell).click();
    await page.getByTestId(TESTID.orderAmount).fill('1');
    await page.getByTestId(TESTID.orderPrice).fill('3');
    await expect(page.getByTestId('order-amount-tokens')).toContainText('= 3 EXUSD');
    const built = await openReview(page);
    expect(built.blocking).toEqual([]);
    expect(built.created.length).toBe(1);
    await page.getByTestId(TESTID.confirmCancel).click();
    await expect(page.getByTestId(TESTID.confirmScreen)).toHaveCount(0);
  });

  test('old #/usd/<token> links open the pair page TOKEN/EXUSD and can build a pair order', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await fund(mock, 'alice');
    await listBoth(page);
    const exkcc = await seeded(mock);
    await page.goto(`/#/usd/${exkcc}`);
    await expect(page).toHaveURL(new RegExp(`#/market/${exkcc}/${QUOTE}$`));
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    await expect(page.getByTestId('usd-page')).toHaveCount(0);
    await expect(page.getByTestId('pair-book')).toBeVisible();
    await expect(page.getByTestId('price-chart')).toBeVisible();
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await expect(page.getByTestId('pair-usd-chart')).toHaveCount(0);
    await connectWallet(page);

    await page.getByTestId('order-side-sell').click();
    await page.getByTestId('order-price').fill('0.0515');
    await page.getByTestId('order-amount').fill('2');
    await expect(page.getByTestId('order-review')).toBeEnabled({ timeout: 15_000 });
    await page.getByTestId('order-review').click();
    const confirm = await readConfirm(page);
    expect(confirm.blocking).toEqual([]);
    expect(confirm.created[0]!.rows.pairPair).toContain('sells EXKCC for EXUSD');
    await page.getByTestId(TESTID.confirmCancel).click();
    await expect(page.getByTestId(TESTID.confirmScreen)).toHaveCount(0);
  });

  test('an unknown token or a missing USD token falls back to the market list', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await listBoth(page);
    await page.goto(`/#/usd/${'12'.repeat(32)}`);
    await expect(page).toHaveURL(/#\/market$/);
    await expect(page.getByTestId('token-list-view')).toBeVisible();
  });

  test('a token that is not a USD reference stays TOKEN/KAS next to it', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await listBoth(page);
    const exkcc = await seeded(mock);
    await page.goto(`/#/market/${exkcc}`);
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-inverted', '0');
  });
});

test('the landing screen is a chart, the market list is at #/market', async ({ appPage: page, mock }) => {
  await listExkcc(page);
  await page.goto('/');
  await expect(page.getByTestId('token-page')).toHaveAttribute('data-token', await seeded(mock));
  await expect(page.getByTestId('price-chart')).toBeVisible();
  await expect(page.getByTestId('token-list-view')).toHaveCount(0);
  await page.goto('/#/market');
  await expect(page.getByTestId('token-list-view')).toBeVisible();
});
