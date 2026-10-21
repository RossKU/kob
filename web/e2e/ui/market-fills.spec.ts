// A market order's "fills" setting against a book of many small asks (mock stack). One transaction takes at most 3 asks (the token's input slots),
// so a market buy of fills 1 (the default, one IOC transaction) fills only the first 3 of them: the ticket says so under the amount. With Fills = 3
// the same auction is placed as a short-lived RESTING order (tif 0): the estimate next to the box says the visible book needs about 3 fills, the
// final confirmation says it fills in up to 3 transactions, when the rest returns, and the 3 delivery carriers it deposits; the escrow budgets 3
// fills, and three fills of 3 tokens each (the mock's simulated fills: it has no matcher) take the whole amount in three transactions.
//
// Not covered here (the mock has no matcher): a real batch filling the order from those asks in several transactions, the covenant's own
// checks of those fills (the unit tests of orders/simple.ts plan and run the placement through the script engine, not its fills), and the
// keeper refund of an unfilled rest after the expiry.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { TOKEN, connectWallet, fund, openMarket } from '../helpers/env';
import { clickPlaceOnly, confirmReceipt, fillFields, pickType, placedBy, readReceipt, settlePlan, waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false });

/** nine asks of one EXSTD (KOB's standard 3 / 3 program: 3 token inputs per transaction) at 0.025 KAS: its whole book */
const SMALL_ASKS = 9;
const SMALL_PRICE = 2_500_000;
const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);

/** The example registry plus EXSTD, a listed token of the standard 3 / 3 program (as e2e/orders/merge.spec.ts). */
async function registryWithStd(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  Object.assign(exkcc, { status: 'listed', verified: true });
  reg.tokens.push({ ...exkcc, ticker: 'EXSTD', name: 'Example standard token', covenant_id: STD, template_id: 'kcc20-ref-3x3', extension_commitment: EXT });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

test('a market buy against many small asks: one transaction takes 3, with Fills 3 it rests briefly and fills in three transactions', async ({ appPage: page, mock, wallet }) => {
  await fund(mock);
  await mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3', standing: 'official' }] });
  await mock.seed({ orders: Array.from({ length: SMALL_ASKS }, () => ({ token: STD, side: 'ask', maker: 'bob', price: SMALL_PRICE, amount: TOKEN.toString() })) });
  await registryWithStd(page);
  await page.goto(`/#/market/${STD}`);
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await pickType(page, 'market', 'buy');
  await fillFields(page, { amount: '9' });
  await settlePlan(page);

  // fills 1: one transaction takes 3 of the 9 asks, the rest returns
  await expect(page.getByTestId('order-fills-short')).toHaveText(
    'One transaction takes at most 3 asks: only about 3 EXSTD of 9 EXSTD fills now, the rest returns. Raise Fills (Advanced) to trade more.',
  );
  await expect(page.getByTestId('field-maxFills-note')).toHaveText(/^The visible book needs about 3 fill\(s\) for this amount \(3 asks per transaction\)\. 1 fill\(s\) deposit 2 KAS in carriers; unused ones come back\.$/);

  // Fills 3: enough for the 9 asks, three carriers deposited
  await fillFields(page, { maxFills: '3' });
  await settlePlan(page);
  await expect(page.getByTestId('field-maxFills-note')).toHaveText('The visible book needs about 3 fill(s) for this amount (3 asks per transaction). 3 fill(s) deposit 6 KAS in carriers; unused ones come back.');
  await expect(page.getByTestId('order-fills-short')).toHaveCount(0);

  await clickPlaceOnly(page);
  const receipt = await readReceipt(page);
  const card = receipt.orders[0]!;
  expect(card.title).toBe('Buy market order (auction, several fills)');
  expect(card.rows['Time in force']).toMatch(/^fills in up to 3 transactions within the worst price; what is not filled returns after .+$/);
  expect(card.rows['Delivery carriers deposited']).toBe('6 KAS');
  expect(card.rows['Expected price (auction start)']).toBe('0.025 KAS per EXSTD');
  // the escrow budgets three fills: three delivery carriers of 2 KAS on top of the budget of 9 tokens at the bound (about 0.23 KAS)
  expect(Number(card.rows['KAS on the order output']!.replace(' KAS', ''))).toBeGreaterThan(6.2);
  await confirmReceipt(page);
  const txid = await waitSubmitted(page);
  const placed = await placedBy(mock, txid);
  const order = placed.views[0]!;
  const s = order.state.state as Record<string, string>;
  // the market auction (start at the touch, the 3% bound) as a resting order of a short life
  expect(s.tif).toBe('0');
  expect(s.price).toBe(String(SMALL_PRICE));
  expect(BigInt(s.priceEnd)).toBeGreaterThan(BigInt(SMALL_PRICE));
  expect(BigInt(s.expiryDaa) - BigInt(s.activeFrom)).toBe(300n);
  // no 3 asks of one token fall below its minimum fill
  expect(s.minFill).toBe(TOKEN.toString());
  expect(await wallet!.calls()).toHaveLength(1);

  // three transactions of 3 tokens each (simulated fills) take the whole amount: the order continues after each partial fill
  await mock.advanceDaa(40);
  const first = await mock.fill(order.covenant_id, (3n * TOKEN).toString());
  expect(first.status).toBe('partial');
  const second = await mock.fill(order.covenant_id, (3n * TOKEN).toString());
  expect(second.status).toBe('partial');
  const third = await mock.fill(order.covenant_id, (3n * TOKEN).toString());
  expect(third.status).toBe('filled');
  expect(third.filled_amount).toBe((9n * TOKEN).toString());
});

test('a market sell with Fills: no carrier per fill; FOK hides the box', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'market', 'sell');
  await fillFields(page, { amount: '2', maxFills: '2' });
  await settlePlan(page);
  await expect(page.getByTestId('field-maxFills-note')).toHaveText(/A sell deposits no carrier per fill\.$/);
  await clickPlaceOnly(page);
  const receipt = await readReceipt(page);
  expect(receipt.orders[0]!.title).toBe('Sell market order (auction, several fills)');
  expect(receipt.orders[0]!.rows['Time in force']).toMatch(/^fills in several transactions within the worst price; the tokens not sold return after /);
  expect(receipt.orders[0]!.rows['Delivery carriers deposited']).toBeUndefined();
  await confirmReceipt(page);
  const placed = await placedBy(mock, await waitSubmitted(page));
  expect((placed.views[0]!.state.state as Record<string, string>).tif).toBe('0');
  // all or nothing is one transaction: no Fills box
  await fillFields(page, { allOrNothing: true });
  await expect(page.getByTestId('field-maxFills')).toHaveCount(0);
});
