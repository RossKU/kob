// Limit ladder and the exact Max of the ticket (mock KasWare wallet, default mock seed, EXKCC listed as tradable), through the real UI to the mock node:
//   * several limit orders of one size at stepped prices (a percent step, or up to an end price), each its own confirmation, signature and transaction;
//   * Max of a buy: the largest amount the builder accepts after carriers, fee and a reserve; Max of a sell: the whole token balance.
// Prices are KAS per EXKCC, amounts EXKCC (1e8 base units); state prices are sompi per whole token (0.0245 KAS = 2_450_000).
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { acknowledgeAndSign, fillFields, pickType, placedBy, readConfirm, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

test.describe('limit ladder', () => {
  test('a buy ladder by a percent step: the levels listed before review, every order on the confirmation, one transaction per order', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '1', price: '0.0245' });
    // off by default: no ladder fields, no level list
    await expect(page.getByTestId('field-ladder.levels')).toHaveCount(0);
    await expect(page.getByTestId('order-ladder')).toHaveCount(0);

    await fillFields(page, { 'ladder.on': true, 'ladder.levels': '3', 'ladder.pct': '2' });
    await waitReviewable(page);
    const ladder = page.getByTestId('order-ladder');
    await expect(ladder).toContainText('Ladder: 3 levels');
    await expect(page.getByTestId('order-ladder-level-1')).toHaveText(/Order 1: Buy 1 EXKCC at 0\.0245\b.* KAS \(about 0\.0245 KAS\)/);
    await expect(page.getByTestId('order-ladder-level-2')).toHaveText(/Order 2: Buy 1 EXKCC at 0\.02401\b/);
    await expect(page.getByTestId('order-ladder-level-3')).toHaveText(/Order 3: Buy 1 EXKCC at 0\.0235298\b/);
    await expect(page.getByTestId('order-ladder-total')).toContainText('Total: 3 EXKCC in 3 orders');
    await expect(ladder).toContainText('you sign 3 transactions in turn');

    const before = (await mock.submitted(false)).length;
    await page.getByTestId('order-review').click();
    const txids: string[] = [];
    for (let level = 1; level <= 3; level++) {
      await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
      const confirm = await readConfirm(page);
      expect(confirm.blocking).toEqual([]);
      expect(confirm.heading).toContain(`Level ${level} of 3`);
      // every order of the ladder is on every confirmation, the one being signed is marked
      const list = page.getByTestId('confirm-ladder');
      await expect(list).toContainText('All 3 orders of this ladder');
      await expect(list.locator('li')).toHaveCount(3);
      await expect(page.getByTestId(`confirm-ladder-level-${level}`)).toHaveAttribute('aria-current', 'true');
      await expect(list.locator('[aria-current="true"]')).toHaveCount(1);
      expect(confirm.created).toHaveLength(1);
      txids.push(await acknowledgeAndSign(page));
      await page.getByTestId('confirm-close').click();
    }
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    await expect(page.getByTestId('order-result-ladder')).toContainText('All 3 levels placed');

    const subs = (await mock.submitted()).slice(before);
    expect(subs.map((s) => s.txid)).toEqual(txids);
    const orders = [];
    for (const s of subs) {
      expect(s.created).toHaveLength(1);
      orders.push((await placedBy(mock, s.txid)).views[0]);
    }
    // three independent resting buys of the same size, one step apart
    expect(orders.map((v) => v.state.state.price)).toEqual(['2450000', '2401000', '2352980']);
    expect(orders.map((v) => v.min_fill)).toEqual(['100000000', '100000000', '100000000']);
    expect(orders.map((v) => v.side)).toEqual([2, 2, 2]);
    expect(new Set(orders.map((v) => v.covenant_id)).size).toBe(3);
    expect((await wallet!.calls()).filter((c) => c.status === 'signed')).toHaveLength(3);
  });

  test('a sell ladder up to an end price: evenly spaced, same side', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.03', 'ladder.on': true, 'ladder.levels': '4', 'ladder.end': '0.036' });
    await waitReviewable(page);
    const prices = ['0.03', '0.032', '0.034', '0.036'];
    for (let i = 0; i < 4; i++) await expect(page.getByTestId(`order-ladder-level-${i + 1}`)).toContainText(`Sell 2 EXKCC at ${prices[i]}`);
    await expect(page.getByTestId('order-ladder-total')).toContainText('Total: 8 EXKCC in 4 orders');
  });

  test('ladder limits: 2 to 20 orders, a step or an end price, no collapsing levels', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '1', price: '0.0245', 'ladder.on': true, 'ladder.levels': '21', 'ladder.pct': '1' });
    await expect(page.getByTestId('order-review')).toBeDisabled();
    await fillFields(page, { 'ladder.levels': '20' });
    await waitReviewable(page);
    await expect(page.getByTestId('order-ladder-levels').locator('li')).toHaveCount(20);
    // a step so large that a level would reach zero
    await fillFields(page, { 'ladder.pct': '60' });
    await expect(page.getByTestId('order-ladder-errors')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });
});

test.describe('Max', () => {
  test('a buy: the largest amount the builder accepts, placeable as it stands', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 30n, utxos: 1 });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { price: '0.0245' });
    await page.getByTestId('order-amount-max').click();
    const amount = await page.getByTestId('order-amount').inputValue();
    expect(Number(amount)).toBeGreaterThan(100);
    await expect(page.getByTestId('order-max-note')).toContainText('after carriers, network fees and a small reserve');
    await waitReviewable(page);
    // twenty more tokens (0.49 KAS) do not fit
    await page.getByTestId('order-amount').fill(String(Number(amount) + 20));
    await expect(page.getByTestId('order-issue-INSUFFICIENT_KAS')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });

  test('a buy ladder: the amount of one order such that the whole ladder fits', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 60n, utxos: 1 });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { price: '0.0245', 'ladder.on': true, 'ladder.levels': '4', 'ladder.pct': '1' });
    await page.getByTestId('order-amount-max').click();
    await waitReviewable(page);
    await expect(page.getByTestId('order-ladder-funds')).toHaveCount(0);
  });

  test('a sell: the whole token balance; a price-less buy says when nothing is affordable', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 1n, utxos: 1, tokens: 100n });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await page.getByTestId('order-amount-max').click();
    await expect(page.getByTestId('order-amount')).toHaveValue('100');
    await page.getByTestId('order-side-buy').click();
    await fillFields(page, { price: '0.0245' });
    await page.getByTestId('order-amount-max').click();
    await expect(page.getByTestId('order-max-note')).toContainText('cannot cover any amount');
    await expect(page.getByTestId('order-amount')).toHaveValue('');
  });
});
