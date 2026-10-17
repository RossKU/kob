// Limit ladder and the exact Max of the ticket (mock KasWare wallet, default mock seed, EXKCC listed as tradable), through the real UI to the mock node:
//   * several limit orders of one size at stepped prices (a percent step, or up to an end price), each its own wallet signature and transaction;
//   * Max of a buy: the largest amount the builder accepts after carriers, fee and a reserve; Max of a sell: the whole token balance.
// Prices are KAS per EXKCC, amounts EXKCC (1e8 base units); state prices are sompi per whole token (0.0245 KAS = 2_450_000).
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { expectWalletSigned, fillFields, pickType, placeLadder, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

test.describe('limit ladder', () => {
  test('a buy ladder by a percent step: the levels listed before placing, one wallet request and one transaction per order', async ({ appPage: page, mock, wallet }) => {
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

    // one click places every level in turn, each its own wallet request; the status line names the level being sent
    await wallet!.configure({ delayMs: 400 });
    const placing = placeLadder(page, mock, 3);
    await expect(page.getByTestId('order-status')).toContainText('Level 1 of 3');
    const placed = await placing;
    await expect(page.getByTestId('order-result-ladder')).toContainText('All 3 levels placed');
    await expect(page.getByTestId('order-ladder-stopped')).toHaveCount(0);
    expect(await wallet!.calls()).toHaveLength(3);
    for (let i = 0; i < 3; i++) {
      expect(placed[i]!.submission.created).toHaveLength(1);
      await expectWalletSigned(wallet!, placed[i]!, { call: i });
    }
    const orders = placed.map((p) => p.views[0]);
    // three independent resting buys of the same size, one step apart
    expect(orders.map((v) => v.state.state.price)).toEqual(['2450000', '2401000', '2352980']);
    expect(orders.map((v) => v.min_fill)).toEqual(['100000000', '100000000', '100000000']);
    expect(orders.map((v) => v.side)).toEqual([2, 2, 2]);
    expect(new Set(orders.map((v) => v.covenant_id)).size).toBe(3);
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
    await expect(page.getByTestId('order-place')).toBeDisabled();
    await fillFields(page, { 'ladder.levels': '20' });
    await waitReviewable(page);
    await expect(page.getByTestId('order-ladder-levels').locator('li')).toHaveCount(20);
    // a step so large that a level would reach zero
    await fillFields(page, { 'ladder.pct': '60' });
    await expect(page.getByTestId('order-ladder-errors')).toBeVisible();
    await expect(page.getByTestId('order-place')).toBeDisabled();
  });
});

test.describe('Max', () => {
  test('Available at the amount label shows only the asset the order spends, and clicking it is Max', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 30n, utxos: 1, tokens: 100n });
    await openMarket(page, mock);
    const available = page.getByTestId('order-available');
    // a sell spends the token: its balance only (no KAS line)
    await pickType(page, 'limit', 'sell');
    await expect(available).toHaveText('Available 100 EXKCC');
    await expect(page.getByTestId('order-balances')).toHaveCount(0);
    await available.click();
    await expect(page.getByTestId('order-amount')).toHaveValue('100');
    // a buy spends KAS: the KAS balance (the whole of it; Max keeps carriers, fees and a reserve back from the amount)
    await page.getByTestId('order-side-buy').click();
    await expect(available).toHaveText('Available 30 KAS');
    await fillFields(page, { price: '0.0245' });
    await available.click();
    const amount = Number(await page.getByTestId('order-amount').inputValue());
    expect(amount).toBeGreaterThan(100);
    expect(amount * 0.0245, 'Max spends less than the balance').toBeLessThan(30);
    // the market page has no balance card any more
    await expect(page.getByTestId('wallet-token-balances')).toHaveCount(0);
  });

  test('a buy: the largest amount the builder accepts, placeable as it stands', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 30n, utxos: 1 });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { price: '0.0245' });
    await page.getByTestId('order-available').click();
    const amount = await page.getByTestId('order-amount').inputValue();
    expect(Number(amount)).toBeGreaterThan(100);
    await expect(page.getByTestId('order-max-note')).toContainText('after carriers, network fees and a small reserve');
    await waitReviewable(page);
    // twenty more tokens (0.49 KAS) do not fit
    await page.getByTestId('order-amount').fill(String(Number(amount) + 20));
    await expect(page.getByTestId('order-issue-INSUFFICIENT_KAS')).toBeVisible();
    await expect(page.getByTestId('order-place')).toBeDisabled();
  });

  test('a buy ladder: the amount of one order such that the whole ladder fits', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 60n, utxos: 1 });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { price: '0.0245', 'ladder.on': true, 'ladder.levels': '4', 'ladder.pct': '1' });
    await page.getByTestId('order-available').click();
    await waitReviewable(page);
    await expect(page.getByTestId('order-ladder-funds')).toHaveCount(0);
    // placed as it stands: every level is accepted, the wallet pays for the whole ladder
    expect(await placeLadder(page, mock, 4)).toHaveLength(4);
  });

  test('a buy ladder the wallet cannot pay for as a whole is an error line and the button stays disabled', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 30n, utxos: 1 });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    // one level of 600 EXKCC costs about 14.7 KAS: one fits, three do not
    await fillFields(page, { amount: '600', price: '0.0245' });
    await waitReviewable(page);
    await expect(page.getByTestId('order-ladder-funds')).toHaveCount(0);
    await fillFields(page, { 'ladder.on': true, 'ladder.levels': '3', 'ladder.pct': '1' });
    const funds = page.getByTestId('order-ladder-funds');
    await expect(funds).toBeVisible();
    await expect(funds).toHaveAttribute('data-code', 'kas');
    await expect(funds).toContainText('The ladder needs about');
    await expect(page.getByTestId('order-place')).toBeDisabled();
    // a smaller order per level fits again
    await fillFields(page, { amount: '300' });
    await waitReviewable(page);
    await expect(funds).toHaveCount(0);
  });

  test('a sell: the whole token balance; a price-less buy says when nothing is affordable', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { kas: 1n, utxos: 1, tokens: 100n });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await page.getByTestId('order-available').click();
    await expect(page.getByTestId('order-amount')).toHaveValue('100');
    await page.getByTestId('order-side-buy').click();
    await fillFields(page, { price: '0.0245' });
    await page.getByTestId('order-available').click();
    await expect(page.getByTestId('order-max-note')).toContainText('cannot cover any amount');
    await expect(page.getByTestId('order-amount')).toHaveValue('');
  });
});
