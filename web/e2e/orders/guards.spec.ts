// Wallet-side guards and input validation of the ticket: everything that must stop or explain an order BEFORE the review button works
// (docs/spec/matcher.md section 10): self-trade prevention, the FOK pre-check against the visible book, tick rounding, amount and time-range errors.
import { test, expect } from '../fixtures';
import { browserTzOffset, fund, localDateTimeText, mockClock, openMarket } from '../helpers/env';
import { seedOrders } from '../helpers/orders';
import { fillFields, pickType, settlePlan, waitReviewable, openReview } from '../helpers/ticket';

test.use({ autoOpen: false });

const errorsOf = (page: import('@playwright/test').Page) => page.getByTestId('order-issues').locator('[data-severity="error"]');

test.describe('self-trade prevention', () => {
  test('a sell that would trade against your own resting bid is refused; a price that stays clear of it is fine', async ({ appPage: page, mock }) => {
    await fund(mock);
    const [own] = await seedOrders(mock, [{ side: 'bid', price: 2_460_000, tokens: 2 }]); // your bid at 0.0246
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '1', price: '0.0245' }); // at or below your own bid
    const issue = page.getByTestId('order-issue-SELF_TRADE');
    await expect(issue).toBeVisible({ timeout: 30_000 });
    await expect(issue).toContainText(`${own.slice(0, 4)}…${own.slice(-4)}`); // the covenant id of the order in the way
    await expect(issue).toHaveAttribute('data-severity', 'error');
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // the same order above your bid: nothing crosses your own order
    await fillFields(page, { price: '0.0247' });
    await expect(page.getByTestId('order-issue-SELF_TRADE')).toHaveCount(0);
    await waitReviewable(page);
  });

  test('a market sell is judged at its worst price: its auction would reach your own bid', async ({ appPage: page, mock }) => {
    await fund(mock);
    await seedOrders(mock, [{ side: 'bid', price: 2_460_000, tokens: 2 }]);
    await openMarket(page, mock);
    await pickType(page, 'market', 'sell');
    await fillFields(page, { amount: '1' }); // 3% below the best bid 0.0249 is 0.024153: below 0.0246
    await expect(page.getByTestId('order-issue-SELF_TRADE')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // a tight bound stays above your bid (0.0249 -0.1% = 0.024875)
    await fillFields(page, { slippageBps: '0.1' });
    await expect(page.getByTestId('order-issue-SELF_TRADE')).toHaveCount(0);
    await waitReviewable(page);
  });

  test('a buy that would trade against your own resting sell is refused', async ({ appPage: page, mock }) => {
    await fund(mock);
    await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 2 }]); // your ask at 0.027
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '1', price: '0.0275' });
    await expect(page.getByTestId('order-issue-SELF_TRADE')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });
});

test.describe('fill-or-kill pre-check', () => {
  test('FOK the visible book cannot fill is refused with the depth it lacks', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { tokens: 500n });
    await openMarket(page, mock);
    await pickType(page, 'fok', 'sell');
    await fillFields(page, { amount: '400', price: '0.0245' }); // far more than all bids at or above 0.0245 hold
    const issue = page.getByTestId('order-issue-FOK_INSUFFICIENT_DEPTH');
    await expect(issue).toBeVisible({ timeout: 30_000 });
    await expect(issue).toContainText('400 EXKCC');
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // a size the book holds passes
    await fillFields(page, { amount: '2' });
    await expect(page.getByTestId('order-issue-FOK_INSUFFICIENT_DEPTH')).toHaveCount(0);
    await waitReviewable(page);
  });

  test('FOK that would need more counterparties than one transaction can carry is refused', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { tokens: 100n });
    // twelve one-token bids at the best price: 21 crossing tokens, but a fill of 17 needs at least 10 orders (the token allows 8)
    await seedOrders(mock, Array.from({ length: 12 }, () => ({ side: 'bid' as const, price: 2_490_000, tokens: 1, maker: 'maker' as const })));
    await openMarket(page, mock);
    await pickType(page, 'fok', 'sell');
    await fillFields(page, { amount: '17', price: '0.0249' });
    const issue = page.getByTestId('order-issue-FOK_TOO_MANY_COUNTERPARTIES');
    await expect(issue).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });
});

test.describe('input validation', () => {
  test('a KAS price has sompi precision (protocol v3 has no price tick): an 8-decimal price is placed exactly, a ninth decimal is refused inline', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '1', price: '0.02705011' });
    await waitReviewable(page);
    await expect(page.getByTestId('order-price-rounded')).toHaveCount(0);
    const sell = await openReview(page);
    expect(sell.created[0]!.rows.price).toContain('0.02705011 KAS / EXKCC');
    await page.getByTestId('confirm-cancel').click();
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { price: '0.024450011' });
    await expect(page.locator('.field:has([data-testid="order-price"]) .field-error')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
    expect(await mock.submitted(false)).toHaveLength(0);
  });

  test('amount errors are shown inline and keep review disabled', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    const amountError = page.locator('.field:has([data-testid="order-amount"]) .field-error');
    await fillFields(page, { price: '0.027', amount: '0' });
    await expect(amountError).toHaveText('Must be greater than zero.');
    await expect(page.getByTestId('order-amount')).toHaveAttribute('aria-invalid', 'true');
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // EXKCC has 8 decimals: a ninth is finer than one base unit
    await fillFields(page, { amount: '1.123456789' });
    await expect(amountError).toHaveText('At most 8 decimal places.');
    await expect(page.getByTestId('order-review')).toBeDisabled();
    await fillFields(page, { amount: 'abc' });
    await expect(amountError).toHaveText('Enter a plain number.');
    await fillFields(page, { amount: '-2' });
    await expect(amountError).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // any amount down to one base unit is fine
    await fillFields(page, { amount: '1.5' });
    await expect(amountError).toHaveCount(0);
    await waitReviewable(page);
    // an empty required field is not an error yet: the ticket says what is missing
    await fillFields(page, { amount: '' });
    await expect(page.getByTestId('order-need-incomplete')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });

  test('price precision and range errors are shown inline', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '1', price: '0.123456789' });
    await expect(page.locator('.field:has([data-testid="order-price"]) .field-error')).toHaveText('At most 8 decimal places.');
    await expect(page.getByTestId('order-review')).toBeDisabled();
    await fillFields(page, { price: 'abc' });
    await expect(page.locator('.field:has([data-testid="order-price"]) .field-error')).toHaveText('Enter a plain number.');
    await fillFields(page, { price: '0' });
    await expect(page.locator('.field:has([data-testid="order-price"]) .field-error')).toHaveText('Must be greater than zero.');
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });

  test('an expiry or activation beyond 90 days, or before the activation, is an error that blocks review', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const tz = await browserTzOffset(page);
    const { unix } = await mockClock(mock);
    const at = (days: number) => localDateTimeText(unix + days * 86_400, tz);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '1', price: '0.027', lifetime: 'gtd', lifetimeAt: at(100) });
    await settlePlan(page);
    await expect(page.getByTestId('order-issue-EXPIRY_TOO_FAR')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-issue-EXPIRY_TOO_FAR')).toContainText('at most 90 days');
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // exactly inside the limit is fine
    await fillFields(page, { lifetimeAt: at(89) });
    await expect(page.getByTestId('order-issue-EXPIRY_TOO_FAR')).toHaveCount(0);
    await waitReviewable(page);
    // a date in the past
    await fillFields(page, { lifetimeAt: at(-1) });
    await expect(page.getByTestId('order-issue-EXPIRY_TOO_SOON')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // activation beyond 90 days
    await fillFields(page, { lifetime: 'gtc', activeFrom: at(100) });
    await expect(page.getByTestId('order-issue-ACTIVE_TOO_FAR')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-review')).toBeDisabled();
    // expiry before the activation
    await fillFields(page, { lifetime: 'gtd', lifetimeAt: at(5), activeFrom: at(10) });
    await expect(errorsOf(page).first()).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('order-issue-ACTIVE_AFTER_EXPIRY')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });

  test('funding errors name the exact shortfall: not enough KAS for a buy', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 100_000_000n); // 1 KAS: less than the 2 KAS delivery carrier a resting buy locks
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245' });
    const issue = page.getByTestId('order-issue-INSUFFICIENT_KAS');
    await expect(issue).toBeVisible({ timeout: 30_000 });
    await expect(issue).toContainText('short by');
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });
});
