// The ticket follows the pair AS SHOWN, BASE/QUOTE: the left asset is the base, Amount counts it, the price is QUOTE per BASE and Buy / Sell
// are the base's. A token's KAS book is TOKEN/KAS on chain, so its inverted view KAS/TOKEN counts KAS and builds the native order of the same
// economic meaning (buying KAS = selling the token); a pair page flips by opening B/A and carries the order across with the side turned over.
// Every check clicks the real controls and reads what the screen shows and what the confirmation decodes from the built transaction.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';
import { connectWallet, fund, openMarket } from '../helpers/env';
import { acknowledgeAndSign, openReview, placedBy } from '../helpers/ticket';

test.use({ autoOpen: false });

const amountUnit = (page: Page) => page.locator('.field', { has: page.getByTestId(TESTID.orderAmount) }).locator('.tk-unit').first();

test.describe('a KAS book: EXKCC/KAS and KAS/EXKCC', () => {
  test('a flip keeps the order and rewords it in the shown base: Buy EXKCC = Sell KAS, the amount in KAS', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    const ticket = page.getByTestId('order-ticket');

    // native EXKCC/KAS: Buy 10 EXKCC at 0.024 KAS per EXKCC (below the book: it rests)
    await page.getByTestId(TESTID.orderSideBuy).click();
    await page.getByTestId(TESTID.orderAmount).fill('10');
    await page.getByTestId(TESTID.orderPrice).fill('0.024');
    await expect(amountUnit(page)).toHaveText('EXKCC');
    await expect(page.getByTestId('disc-summary')).toHaveText('Buy 10 EXKCC');
    const native = await openReview(page);
    await page.getByTestId(TESTID.confirmCancel).click();
    await expect(page.getByTestId(TESTID.confirmScreen)).toHaveCount(0);

    // flip: KAS/EXKCC. Buying EXKCC is selling KAS: the Sell button is on, the amount is the 0.24 KAS it costs, the price 41.67 EXKCC per KAS
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');
    await expect(ticket).toHaveAttribute('data-shown-side', 'sell');
    await expect(page.getByTestId(TESTID.orderSideSell)).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId(TESTID.orderSideBuy)).toHaveAttribute('aria-checked', 'false');
    await expect(amountUnit(page)).toHaveText('KAS');
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('0.24');
    await expect(page.getByTestId('order-amount-tokens')).toContainText('0.24 KAS = 10 EXKCC at your price');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('41.666667');
    await expect(page.locator('.field', { has: page.getByTestId(TESTID.orderPrice) }).locator('.tk-unit')).toHaveText('EXKCC / KAS');
    await expect(page.getByTestId(TESTID.orderReview)).toContainText('Review Sell');
    await expect(page.getByTestId('disc-summary')).toHaveText('Sell 0.24 KAS for 10 EXKCC');
    const flipped = await openReview(page);
    // the confirmation states the shown order in KAS and EXKCC, above the decoded transaction, which is the same order
    await expect(page.getByTestId('confirm-shown-as')).toContainText('KAS/EXKCC Sell: you give 0.24 KAS and receive 10 EXKCC');
    await expect(page.getByTestId('confirm-shown-as')).toContainText('BUY order of EXKCC');
    expect(flipped.created.map((c) => c.title)).toEqual(['Buy limit order']);
    expect(flipped.created.map((c) => c.rows)).toEqual(native.created.map((c) => c.rows));
    await page.getByTestId(TESTID.confirmCancel).click();
    await expect(page.getByTestId(TESTID.confirmScreen)).toHaveCount(0);

    // flip back: the very same native form
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByTestId(TESTID.orderSideBuy)).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('10');
    await expect(amountUnit(page)).toHaveText('EXKCC');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('0.024');
  });

  test('typed in KAS/EXKCC: Buy 10 KAS at 40 EXKCC per KAS sells exactly 400 EXKCC at 0.025 KAS', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { tokens: 500n });
    await openMarket(page, mock);
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');
    await page.getByTestId(TESTID.orderSideBuy).click();
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-side', 'sell');
    await page.getByTestId(TESTID.orderAmount).fill('10');
    await page.getByTestId(TESTID.orderPrice).fill('40');
    await expect(amountUnit(page)).toHaveText('KAS');
    await expect(page.getByTestId('order-amount-tokens')).toContainText('10 KAS = 400 EXKCC at your price');
    await expect(page.getByTestId('order-price-native')).toContainText('0.025 KAS per EXKCC');
    await expect(page.getByTestId('disc-summary')).toHaveText('Buy 10 KAS for 400 EXKCC');
    const c = await openReview(page);
    await expect(page.getByTestId('confirm-shown-as')).toContainText('KAS/EXKCC Buy: you give 400 EXKCC and receive 10 KAS (at 40 EXKCC per KAS');
    expect(c.created).toHaveLength(1);
    expect(c.created[0]!.title).toBe('Sell limit order');
    expect(c.created[0]!.rows.amount).toBe('400 EXKCC');
    expect(c.created[0]!.rows.price).toBe('0.025 KAS / EXKCC');
    const txid = await acknowledgeAndSign(page);
    const placed = await placedBy(mock, txid);
    expect(placed.views).toHaveLength(1);
    const state = JSON.stringify(placed.views[0]);
    // 400 EXKCC = 40,000,000,000 base units, at 2,500,000 sompi per whole EXKCC
    expect(state).toContain('40000000000');
    expect(state).toContain('2500000');
  });

  test('a market order in KAS converts at the best price in the book and says it is approximate', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    await page.getByTestId('market-flip').click();
    await page.getByTestId('order-type').selectOption('market');
    // shown Sell KAS = native buy of EXKCC: sized at the best ask, 0.0251 KAS per EXKCC
    await page.getByTestId(TESTID.orderSideSell).click();
    await page.getByTestId(TESTID.orderAmount).fill('0.251');
    await expect(page.getByTestId('order-amount-tokens')).toContainText('0.251 KAS = 10 EXKCC at the best price in the book');
    await expect(page.getByTestId('disc-summary')).toHaveText('Sell about 0.251 KAS for 10 EXKCC');
  });
});

test.describe('a token pair: EXKCC/EXUSD and EXUSD/EXKCC', () => {
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

  test('the USD reference token opens as EXUSD/KAS (amount in EXUSD); the flip gives KAS/EXUSD (amount in KAS), same order', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await fund(mock, 'alice');
    await listBoth(page);
    await page.goto(`/#/market/${QUOTE}`);
    await connectWallet(page);
    const ticket = page.getByTestId('order-ticket');
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXUSD/KAS');
    await expect(ticket).toHaveAttribute('data-inverted', '0');
    // Buy 3 EXUSD at 0.3 KAS per EXUSD
    await page.getByTestId(TESTID.orderSideBuy).click();
    await page.getByTestId(TESTID.orderAmount).fill('3');
    await page.getByTestId(TESTID.orderPrice).fill('0.3');
    await expect(amountUnit(page)).toHaveText('EXUSD');
    await expect(page.getByTestId('disc-summary')).toHaveText('Buy 3 EXUSD');
    const native = await openReview(page);
    await page.getByTestId(TESTID.confirmCancel).click();
    await expect(page.getByTestId(TESTID.confirmScreen)).toHaveCount(0);

    // flip: KAS/EXUSD. Buying EXUSD is selling KAS: 0.9 KAS at 3.333333 EXUSD per KAS
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXUSD');
    await expect(ticket).toHaveAttribute('data-shown-side', 'sell');
    await expect(page.getByTestId(TESTID.orderSideSell)).toHaveAttribute('aria-checked', 'true');
    await expect(amountUnit(page)).toHaveText('KAS');
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('0.9');
    await expect(page.getByTestId('order-amount-tokens')).toContainText('0.9 KAS = 3 EXUSD at your price');
    await expect(page.getByTestId('disc-summary')).toHaveText('Sell 0.9 KAS for 3 EXUSD');
    const flipped = await openReview(page);
    await expect(page.getByTestId('confirm-shown-as')).toContainText('KAS/EXUSD Sell: you give 0.9 KAS and receive 3 EXUSD');
    expect(flipped.created.map((c) => c.rows)).toEqual(native.created.map((c) => c.rows));
    await page.getByTestId(TESTID.confirmCancel).click();
    await expect(page.getByTestId(TESTID.confirmScreen)).toHaveCount(0);
  });

  test('a flip opens EXUSD/EXKCC with the same order: Buy EXKCC turns into Sell EXUSD, amount and price converted', async ({ appPage: page, mock }) => {
    await mock.seed({ pair: true });
    await fund(mock, 'alice');
    const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
    for (const t of reg.templates) t.review_status = 'reviewed';
    const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
    exkcc.status = 'listed';
    exkcc.verified = true;
    reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional USD reference of the e2e mock.' } });
    await page.route('**/registry/tokens.json', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
    );
    const base = (await mock.token()).covenant_id;
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await connectWallet(page);
    const ticket = page.getByTestId('order-ticket');

    // EXKCC/EXUSD: Buy 2 EXKCC at 0.0515 EXUSD per EXKCC
    await page.getByTestId(TESTID.orderSideBuy).click();
    await page.getByTestId(TESTID.orderAmount).fill('2');
    await page.getByTestId(TESTID.orderPrice).fill('0.0515');
    await expect(amountUnit(page)).toHaveText('EXKCC');

    // flip: EXUSD/EXKCC. Buying EXKCC with EXUSD is selling EXUSD for EXKCC: 0.103 EXUSD at 19.417476 EXKCC per EXUSD
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXUSD/EXKCC');
    await expect(ticket).toHaveAttribute('data-side', 'sell');
    await expect(page.getByTestId(TESTID.orderSideSell)).toHaveAttribute('aria-checked', 'true');
    await expect(amountUnit(page)).toHaveText('EXUSD');
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('0.103');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('19.417476');
    await expect(page.locator('.field', { has: page.getByTestId(TESTID.orderPrice) }).locator('.tk-unit')).toHaveText('EXKCC / EXUSD');
    await expect(page.getByTestId('ticket-pair-carried')).toContainText('Carried over from EXKCC/EXUSD: the side is now Sell');

    // and back: Buy 2 EXKCC (0.103 EXUSD at 19.417476 = 2.0000000 EXKCC, rounded down to EXKCC's 8 decimals)
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    await expect(page.getByTestId(TESTID.orderSideBuy)).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('0.0515');
    const amount = Number(await page.getByTestId(TESTID.orderAmount).inputValue());
    expect(Math.abs(amount - 2)).toBeLessThan(1e-6);
  });
});
