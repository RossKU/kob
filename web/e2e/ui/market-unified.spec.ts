// One market page: the title bar shows `BASE / QUOTE` with a drop-down on each side (KAS and the tokens); choosing the other asset changes the market in
// place (same layout, URL follows, back works). And the flip is complete: everything on the page follows the displayed orientation.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';
import { connectWallet, fund, listExkcc } from '../helpers/env';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);

/** The example registry with EXKCC listed and EXUSD (6 decimals) as a second listed token, matching the mock pair seed. */
async function listBoth(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.status = 'listed';
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional quote token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

const box = async (page: Page, id: string) => (await page.getByTestId(id).boundingBox())!;
const pairOf = (page: Page) => page.getByTestId('market-pair');
const num = (s: string | null | undefined): number => Number((s ?? '').replace(/[,%+\s]/g, ''));
const decimalsOf = (x: string): number => (x.includes('.') ? x.split('.')[1]!.length : 0);

test.describe('one market page, a pair selector', () => {
  test('there is no "Trade against" control; BASE / QUOTE is the title and the quote selector lists KAS and the other tokens', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await listBoth(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByText('Trade against')).toHaveCount(0);
    await expect(page.getByText('Choose a quote token...')).toHaveCount(0);
    await expect(page.getByTestId('pair-quote-select')).toHaveCount(0);
    const quote = page.getByTestId('market-quote-select');
    await expect(quote).toHaveValue('kas');
    const values = await quote.locator('option').evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value));
    expect(values[0]).toBe('kas');
    expect(values).toContain(QUOTE);
    expect(values).not.toContain(base);
    // the base side lists the tokens (never the quote)
    const baseValues = await page.getByTestId('market-base-select').locator('option').evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value));
    expect(baseValues).toContain(base);
    expect(baseValues).toContain(QUOTE);
    expect(baseValues).not.toContain('kas');
  });

  test('choosing a quote changes the market in place: same layout, the URL follows, back and forward work, both ways round', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await fund(mock, 'alice');
    await listBoth(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    await connectWallet(page);
    await expect(page.getByTestId('price-chart')).toBeVisible();
    const chart0 = await box(page, 'chart-section');
    const book0 = await box(page, 'book-section');
    const ticket0 = await box(page, 'order-ticket');
    const title0 = await box(page, 'market-pair');
    const stats0 = await box(page, 'market-stats');

    await page.getByTestId('market-quote-select').selectOption(QUOTE);
    await expect(page).toHaveURL(new RegExp(`#/market/${base}/${QUOTE}$`));
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    await expect(page.getByTestId('pair-book')).toBeVisible();
    // the same unified order ticket, now with EXUSD as its quote
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await expect(page.getByTestId('order-balances')).toContainText('EXUSD');
    // same places: the title, the book left, the chart in the middle, the ticket right
    const chart1 = await box(page, 'chart-section');
    const book1 = await box(page, 'pair-book-section');
    const ticket1 = await box(page, 'order-ticket');
    const title1 = await box(page, 'market-pair');
    const stats1 = await box(page, 'market-stats');
    expect(Math.abs(stats1.height - stats0.height)).toBeLessThan(2);
    expect(Math.abs(chart1.x - chart0.x)).toBeLessThan(2);
    expect(Math.abs(chart1.width - chart0.width)).toBeLessThan(2);
    expect(Math.abs(book1.x - book0.x)).toBeLessThan(2);
    expect(Math.abs(book1.width - book0.width)).toBeLessThan(2);
    expect(Math.abs(ticket1.x - ticket0.x)).toBeLessThan(2);
    expect(Math.abs(ticket1.width - ticket0.width)).toBeLessThan(2);
    expect(Math.abs(title1.x - title0.x)).toBeLessThan(2);
    expect(Math.abs(title1.y - title0.y)).toBeLessThan(2);
    await expect(page.getByText('Trade against')).toHaveCount(0);

    // the flip control next to the pair swaps base and quote
    await page.getByTestId('market-flip').click();
    await expect(page).toHaveURL(new RegExp(`#/market/${QUOTE}/${base}$`));
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXUSD/EXKCC');
    // back / forward
    await page.goBack();
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    await page.goBack();
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    await page.goForward();
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/EXUSD');

    // the old pair link is an alias
    await page.goto(`/#/pair/${base}/${QUOTE}`);
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/EXUSD');

    // picking KAS on the quote side goes back to the token's KAS market, in the same place
    await page.getByTestId('market-quote-select').selectOption('kas');
    await expect(page).toHaveURL(new RegExp(`#/market/${base}$`));
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/KAS');
    const chart2 = await box(page, 'chart-section');
    expect(Math.abs(chart2.x - chart0.x)).toBeLessThan(2);
    expect(Math.abs(chart2.width - chart0.width)).toBeLessThan(2);
  });

  test('the inverted KAS market has the same selector: KAS / TOKEN, a quote change leads to the inverted market of the other token, picking KAS on the left flips', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await listBoth(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    await page.getByTestId('market-flip').click();
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'KAS/EXKCC');
    await expect(page.getByTestId('market-base-select')).toHaveValue('kas');
    await expect(page.getByTestId('market-quote-select')).toHaveValue(base);
    // the other token on the quote side: KAS/EXUSD = the EXUSD market, inverted
    await page.getByTestId('market-quote-select').selectOption(QUOTE);
    await expect(page).toHaveURL(new RegExp(`#/market/${QUOTE}$`));
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'KAS/EXUSD');
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-inverted', '1');
    // a token on the left with the other token on the right: the pair
    await page.getByTestId('market-base-select').selectOption(base);
    await expect(page).toHaveURL(new RegExp(`#/market/${base}/${QUOTE}$`));
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    // KAS on the quote side: the native KAS market of the base token
    await page.getByTestId('market-quote-select').selectOption('kas');
    await expect(page).toHaveURL(new RegExp(`#/market/${base}$`));
    await expect(pairOf(page)).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-inverted', '0');
  });
});

test.describe('the flip is complete: everything follows the displayed orientation', () => {
  test('price grouping exists in both orientations, merges levels and keeps every amount', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    const amounts = async (side: 'asks' | 'bids') =>
      (await page.getByTestId(side === 'asks' ? TESTID.bookAsks : TESTID.bookBids).locator('[data-testid^="book-"]').evaluateAll((els) => els.map((e) => Number((e as HTMLElement).dataset.amount)))).reduce((a, b) => a + b, 0);
    const rows = (side: 'asks' | 'bids') => page.getByTestId(side === 'asks' ? TESTID.bookAsks : TESTID.bookBids).locator('[data-testid^="book-"]');
    for (const flipped of [false, true]) {
      if (flipped) await page.getByTestId('market-flip').click();
      const group = page.getByTestId('book-group');
      await expect(group).toBeVisible();
      await group.selectOption({ index: 0 });
      await expect(rows('asks')).toHaveCount(10);
      const askAmount = await amounts('asks');
      const bidAmount = await amounts('bids');
      await group.selectOption({ index: 3 });
      await expect.poll(() => rows('asks').count()).toBeLessThan(10);
      expect(await rows('bids').count()).toBeLessThan(10);
      expect(await amounts('asks')).toBe(askAmount);
      expect(await amounts('bids')).toBe(bidAmount);
      // the option labels are steps of the shown price unit
      const labels = await group.locator('option').allInnerTexts();
      expect(labels).toHaveLength(4);
      if (flipped) expect(Number(labels[3]!.split('·')[1])).toBeGreaterThanOrEqual(0.1);
      await group.selectOption({ index: 0 });
    }
  });

  test('a click on a grouped inverted level still prefills a native order that reaches the whole group', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await listExkcc(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    await connectWallet(page);
    await page.getByTestId('market-flip').click();
    await page.getByTestId('book-group').selectOption({ index: 3 });
    const bid = page.getByTestId(TESTID.bookBids).getByTestId('book-bid-row').first();
    // displayed bids are the native asks (0.0251 ..): the grouped level's native price is at or above its highest ask
    const nativePrice = Number(await bid.getAttribute('data-native-price'));
    await bid.click();
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-side', 'buy');
    // the best displayed bid bucket is 39 tokens per KAS (native asks 0.0251 .. 0.0256 = 39.84 .. 39.06 fall into it): the buy it prefills must reach the highest of them
    await expect(bid).toHaveAttribute('data-price', '39.000');
    const nativeText = await page.getByTestId('order-price-native').innerText();
    expect(num(nativeText.match(/([0-9.]+) KAS per/)![1])).toBeGreaterThanOrEqual(0.0256 - 1e-9);
    expect(nativePrice).toBeGreaterThanOrEqual(2_560_000);
  });

  test('the book notes follow the orientation, in both', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    const foot = page.locator('.ob-foot');
    await expect(foot).toContainText('an ask prefills a buy of EXKCC, a bid prefills a sell of EXKCC');
    await page.getByTestId('market-flip').click();
    await expect(foot).toContainText('an ask prefills a buy of KAS, a bid prefills a sell of KAS');
    await expect(foot).not.toContainText('buy orders hold a KAS budget');
  });

  test('the ticket in the inverted market: the quantity shows its KAS value, the tip says what it is, the type help speaks of the displayed Buy / Sell', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await listExkcc(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    await connectWallet(page);
    const ticket = page.getByTestId('order-ticket');
    const help = page.getByTestId('order-type-help');

    // native: the wording of a token market
    await expect(page.locator('.field', { has: page.getByTestId('order-tip') }).locator('.tk-unit')).toContainText('KAS / EXKCC');
    await page.getByTestId('order-type').selectOption('close');
    await expect(help).toContainText('Sells the tokens you hold at market');
    await page.getByTestId('order-type').selectOption('twap');
    await expect(page.getByTestId('order-type').locator('option[value="twap"]')).toHaveText('TWAP (sell in slices)');
    await page.getByTestId('order-type').selectOption('limit');

    // inverted
    await page.getByTestId('market-flip').click();
    await expect(ticket).toHaveAttribute('data-inverted', '1');
    // the amount is still typed in EXKCC (the order's own token)
    await expect(page.locator('.field', { has: page.getByTestId(TESTID.orderAmount) }).locator('.tk-unit')).toContainText('EXKCC');
    await page.getByTestId(TESTID.orderAmount).fill('3');
    await page.getByTestId(TESTID.orderPrice).fill('25'); // 25 EXKCC per KAS = 0.04 KAS per EXKCC
    // 3 EXKCC at 0.04 KAS = 0.12 KAS
    await expect(page.getByTestId('order-amount-kas')).toContainText('3 EXKCC');
    await expect(page.getByTestId('order-amount-kas')).toContainText('0.12 KAS');
    await expect(page.getByTestId('order-amount-kas')).toContainText('your price');
    // the tip is an amount of KAS per token traded; its share of the KAS traded is shown
    await expect(page.locator('.field', { has: page.getByTestId('order-tip') }).locator('.tk-unit')).toContainText('KAS per EXKCC traded');
    await expect(page.locator('.field', { has: page.getByTestId('order-tip') }).locator('.tk-unit')).not.toContainText('KAS / EXKCC');
    await page.getByTestId(TESTID.orderTip).fill('0.001');
    await expect(page.getByTestId('order-tip-share')).toContainText('2.5%');
    await expect(page.getByTestId('order-balances')).toBeVisible();
    // the helps
    await page.getByTestId('order-type').selectOption('close');
    await expect(help).toContainText('Buys KAS with the EXKCC you hold');
    await expect(help).not.toContainText('Sells the tokens you hold');
    await expect(page.getByTestId('order-type').locator('option[value="twap"]')).toHaveText('TWAP (buy KAS in slices)');
    await expect(page.getByTestId('order-type').locator('option[value="dca"]')).toHaveText('DCA (sell KAS in slices)');
    await page.getByTestId('order-type').selectOption('stopMarket');
    await expect(help).toContainText('a Buy stop on a trade at or above its price, a Sell stop on a trade at or below it');
    await expect(help).not.toContainText('a fill of a sell order resting at or below your stop');
    await page.getByTestId('order-type').selectOption('limit');
    await expect(page.locator('.field', { has: page.getByTestId('order-tip') })).toContainText('A Buy receives the KAS minus the tip');
  });

  test('recent trades: the KAS amount column of the inverted view has one fixed precision, the same rule as the book', async ({ appPage: page, mock }) => {
    await mock.seedHistory();
    await listExkcc(page);
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}`);
    const col = async () => {
      const rows = page.getByTestId('trades-list').getByTestId('trade-row');
      await expect(rows.first()).toBeVisible();
      return rows.evaluateAll((els) => els.map((e) => (e.querySelectorAll('[role="cell"]')[1] as HTMLElement).innerText));
    };
    const native = await col();
    expect(new Set(native.map(decimalsOf)).size).toBe(1);
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('trades-list')).toContainText('Amount (KAS)');
    const inv = await col();
    expect(new Set(inv.map(decimalsOf)).size).toBe(1);
    // protocol v3 has no price tick (the registry's legacy tick is ignored): the column has ONE precision for every row, at most the 8 of a sompi
    expect(decimalsOf(inv[0]!)).toBeLessThanOrEqual(8);
  });
});

