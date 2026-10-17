// The ticket follows the pair AS SHOWN, BASE/QUOTE: the left asset is the base, Amount counts it, the price is QUOTE per BASE and Buy / Sell
// are the base's. A token's KAS book is TOKEN/KAS on chain, so its inverted view KAS/TOKEN counts KAS and builds the native order of the same
// economic meaning (buying KAS = selling the token); a pair page flips by opening B/A and carries the order across with the side turned over.
// Every check clicks the real controls and reads what the screen shows and what the accepted transaction commits to.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TESTID } from '../testids';
import { connectWallet, fund, openMarket } from '../helpers/env';
import { expectWalletSigned, onChainTerms, placeOrder, placeOrderAndRead } from '../helpers/ticket';

test.use({ autoOpen: false });

const amountUnit = (page: Page) => page.locator('.field', { has: page.getByTestId(TESTID.orderAmount) }).locator('.tk-unit').first();

test.describe('a KAS book: EXKCC/KAS and KAS/EXKCC', () => {
  test('a flip keeps the order and rewords it in the shown base: Buy EXKCC = Sell KAS, the amount in KAS', async ({ appPage: page, mock, wallet }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    const ticket = page.getByTestId('order-ticket');

    // native EXKCC/KAS: Buy 10 EXKCC at 0.024 KAS per EXKCC (below the book: it rests)
    await page.getByTestId(TESTID.orderSideBuy).click();
    await page.getByTestId(TESTID.orderAmount).fill('10');
    await page.getByTestId(TESTID.orderPrice).fill('0.024');
    await expect(amountUnit(page)).toHaveText('EXKCC');
    const native = await placeOrderAndRead(page, mock);
    expect(native.receipt.orders.map((o) => o.title)).toEqual(['Buy limit order']);
    expect(native.receipt.orders[0]!.rows).toMatchObject({ 'Limit price': '0.024 EXKCC / KAS', 'You pay at most per token (all-in)': '0.024 EXKCC / KAS' });
    // the sent order clears the amount: the same native form again
    await page.getByTestId(TESTID.orderAmount).fill('10');
    await page.getByTestId(TESTID.orderPrice).fill('0.024');

    // flip: KAS/EXKCC. Buying EXKCC is selling KAS: the Sell button is on, the amount is the 0.24 KAS it costs, the price 41.67 EXKCC per KAS
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');
    await expect(ticket).toHaveAttribute('data-shown-side', 'sell');
    await expect(page.getByTestId(TESTID.orderSideSell)).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId(TESTID.orderSideBuy)).toHaveAttribute('aria-checked', 'false');
    await expect(amountUnit(page)).toHaveText('KAS');
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('0.24');
    await expect(page.getByTestId('order-amount-tokens')).toHaveText('= 10 EXKCC at your price');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('41.666667');
    await expect(page.locator('.field', { has: page.getByTestId(TESTID.orderPrice) }).locator('.tk-unit')).toHaveText('KAS / EXKCC');
    await expect(page.getByTestId(TESTID.orderPlace)).toContainText('Sell');

    // flip back: the very same native form
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/KAS');
    await expect(page.getByTestId(TESTID.orderSideBuy)).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('10');
    await expect(amountUnit(page)).toHaveText('EXKCC');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('0.024');

    // sent from the flipped form, the order is the native Buy (a bid of 10 EXKCC at 0.024 KAS), identical to the one sent natively
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXKCC');
    const flipped = await placeOrderAndRead(page, mock);
    await expectWalletSigned(wallet!, flipped, { requests: 2, call: 1 });
    // the confirmation of the flipped form is the same table as the native one (only the DAA times differ)
    const terms = (r: { orders: { rows: Record<string, string> }[]; lines: Record<string, string> }) => ({ rows: { ...r.orders[0]!.rows, Expires: '' }, lines: r.lines });
    expect(terms(flipped.receipt)).toEqual(terms(native.receipt));
    expect(native.views[0].contract).toBe('KobBid');
    expect(native.views[0].state.state).toMatchObject({ price: '2400000' });
    expect(onChainTerms(flipped)).toEqual(onChainTerms(native));
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
    await expect(page.getByTestId('order-amount-tokens')).toHaveText('= 400 EXKCC at your price');
    await expect(page.getByTestId('order-price-native')).toContainText('0.025 KAS per EXKCC');
    const placed = await placeOrderAndRead(page, mock);
    // the confirmation is the native order's table: a sell of 400 EXKCC at 0.025 KAS
    expect(placed.receipt.orders.map((o) => o.title)).toEqual(['Sell limit order']);
    expect(placed.receipt.orders[0]!.rows).toMatchObject({ Amount: '400 EXKCC', 'Limit price': '0.025 EXKCC / KAS' });
    expect(placed.views).toHaveLength(1);
    // the signed order is a sell of 400 EXKCC = 40,000,000,000 base units, at 2,500,000 sompi per whole EXKCC
    expect(placed.views[0].contract).toBe('KobAsk');
    expect(placed.views[0].state.state).toMatchObject({ amountLeft: '40000000000', price: '2500000' });
    expect(placed.submission.token_outputs.filter((o) => o.role === 'custody').map((o) => o.amount)).toEqual(['40000000000']);
  });

  test('a market order in KAS spending KAS converts at its worst price: never more KAS than typed', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice');
    await openMarket(page, mock);
    await page.getByTestId('market-flip').click();
    await page.getByTestId('order-type').selectOption('market');
    // shown Sell KAS = native buy of EXKCC: sized at the best ask (0.0251 KAS per EXKCC) moved by the 3 % slippage, 0.025853: the most its
    // auction can pay (TN10 2026-10-17: sized at the ask, 20 KAS typed became 20.60 KAS at the worst price)
    await page.getByTestId(TESTID.orderSideSell).click();
    await page.getByTestId(TESTID.orderAmount).fill('0.251');
    await expect(page.getByTestId('order-amount-tokens')).toHaveText('≈ 9.70874 EXKCC (spends at most 0.26 KAS)');
    // the exact amount behind the rounded hint: 0.25099999 KAS at 0.025853 KAS per EXKCC
    await expect(page.getByTestId('order-amount-tokens')).toHaveAttribute('data-base', '970873786');
    // and the shown Buy KAS = native sell: what it receives, at the best bid (approximate)
    await page.getByTestId(TESTID.orderSideBuy).click();
    await page.getByTestId(TESTID.orderAmount).fill('0.245');
    await expect(page.getByTestId('order-amount-tokens')).toContainText('(receives about');
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

  test('the USD reference token opens as EXUSD/KAS (amount in EXUSD); the flip gives KAS/EXUSD (amount in KAS), same order', async ({ appPage: page, mock, wallet }) => {
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
    const native = await placeOrderAndRead(page, mock);
    expect(native.receipt.orders.map((o) => o.title)).toEqual(['Buy limit order']);
    // the sent order clears the amount: the same native form again
    await page.getByTestId(TESTID.orderAmount).fill('3');
    await page.getByTestId(TESTID.orderPrice).fill('0.3');

    // flip: KAS/EXUSD. Buying EXUSD is selling KAS: 0.9 KAS at 3.333333 EXUSD per KAS
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'KAS/EXUSD');
    await expect(ticket).toHaveAttribute('data-shown-side', 'sell');
    await expect(page.getByTestId(TESTID.orderSideSell)).toHaveAttribute('aria-checked', 'true');
    await expect(amountUnit(page)).toHaveText('KAS');
    await expect(page.getByTestId(TESTID.orderAmount)).toHaveValue('0.9');
    await expect(page.getByTestId('order-amount-tokens')).toHaveText('= 3 EXUSD at your price');
    const flipped = await placeOrder(page, mock);
    await expectWalletSigned(wallet!, flipped, { requests: 2, call: 1 });
    // the signed order is the native Buy of 3 EXUSD at 0.3 KAS, identical to the one sent natively
    expect(native.views[0].contract).toBe('KobBid');
    expect(native.views[0].state.state).toMatchObject({ price: '30000000' });
    expect(onChainTerms(flipped)).toEqual(onChainTerms(native));
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
    await expect(page.locator('.field', { has: page.getByTestId(TESTID.orderPrice) }).locator('.tk-unit')).toHaveText('EXUSD / EXKCC');
    await expect(page.getByTestId('ticket-pair-carried')).toHaveCount(0);

    // and back: Buy 2 EXKCC (0.103 EXUSD at 19.417476 = 2.0000000 EXKCC, rounded down to EXKCC's 8 decimals)
    await page.getByTestId('market-flip').click();
    await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
    await expect(page.getByTestId(TESTID.orderSideBuy)).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId(TESTID.orderPrice)).toHaveValue('0.0515');
    const amount = Number(await page.getByTestId(TESTID.orderAmount).inputValue());
    expect(Math.abs(amount - 2)).toBeLessThan(1e-6);
  });
});
