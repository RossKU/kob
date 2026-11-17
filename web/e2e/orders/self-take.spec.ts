// A crossing order fills directly from the wallet (kob/take.ts): the user is the taker of the resting orders, at their prices, with no executor,
// and pays only the network fee. The default book (mock/seed.mjs) is the market maker's: asks of 3 and 4 EXKCC at 0.0251, then 10 at 0.0252;
// bids of 4 and 5 at 0.0249. Every take is a real transaction the mock node validates in the script engine.
import { mkdirSync } from 'node:fs';
import { expect, test, type MockClient } from '../fixtures';
import { TOKEN, fund, openMarket } from '../helpers/env';
import { clickPlace, clickPlaceOnly, confirmUntil, fillFields, pickType, placedBy, readReceipt, cancelReceipt, settlePlan, waitReviewable, waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false });

const SHOTS = process.env.KOB_TAKE_SHOTS ?? '';

/** alice's funded balance of the default token, base units (the funding first: it may land a moment after `fund`) */
async function tokensOf(mock: MockClient): Promise<bigint> {
  await expect.poll(() => tokensNow(mock)).toBeGreaterThan(0n);
  return tokensNow(mock);
}
async function tokensNow(mock: MockClient): Promise<bigint> {
  const tok = await mock.token();
  const b = await mock.balance('alice');
  return BigInt(b.tokens[tok.ticker] ?? '0');
}
const activeOf = async (mock: MockClient) => (await mock.ordersOf('alice')).filter((o) => o.status === 'open' || o.status === 'partial');

/** The ticket says it fills from the book, with the amount and the rest. */
async function expectTake(page: import('@playwright/test').Page, fills: RegExp, rest?: RegExp): Promise<void> {
  await settlePlan(page);
  await expect(page.getByTestId('order-take')).toBeVisible();
  await expect(page.getByTestId('order-take-fills')).toHaveText(fills);
  if (rest) await expect(page.getByTestId('order-take-rest')).toHaveText(rest);
  else await expect(page.getByTestId('order-take-rest')).toHaveCount(0);
}

test.describe('direct fill from the book', () => {
  test('a crossing limit buy fills now from the asks at their price, no order is placed', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    await pickType(page, 'limit', 'buy');
    // 0.0001 KAS above the best ask
    await fillFields(page, { amount: '2', price: '0.0252' });
    await expectTake(page, /^Fills now: 2 EXKCC at 0\.0251 avg · fee ~0\.0\d+ KAS$/);
    await clickPlaceOnly(page);
    // the final confirmation: what it fills and at what price, the fee
    const r = await readReceipt(page);
    expect(r.title).toBe('Fill from the book');
    expect(r.lines['Fills from the book']).toBe('2 EXKCC from 1 order(s)');
    expect(r.lines['Fill price']).toBe('0.0251 KAS per EXKCC (average)');
    expect(r.problems).toBe(false);
    if (SHOTS) {
      mkdirSync(SHOTS, { recursive: true });
      await page.screenshot({ path: `${SHOTS}/desktop-confirm.png` });
    }
    await page.getByTestId('confirm-order-ok').click();
    const txid = await waitSubmitted(page);
    const placed = await placedBy(mock, txid);
    expect(placed.submission.created).toEqual([]);
    expect(await tokensOf(mock)).toBe(before + 2n * TOKEN);
    expect(await activeOf(mock)).toEqual([]);
    expect((await wallet!.calls()).length).toBe(1);
  });

  test('a partial cross: the crossing asks are taken, the rest rests at the limit in the same flow', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '10', price: '0.0251' });
    await expectTake(page, /^Fills now: 7 EXKCC at 0\.0251 avg · fee ~0\.0\d+ KAS$/, /^Rest: 3 EXKCC at 0\.0251$/);
    await clickPlaceOnly(page);
    await confirmUntil(page, page.getByTestId('order-take-step-rest').and(page.locator('[data-state="done"]')));
    await expect(page.getByTestId('order-take-stepper')).toBeVisible();
    await expect(page.getByTestId('order-take-step-1')).toHaveAttribute('data-state', 'done');
    await waitSubmitted(page);
    expect(await tokensOf(mock)).toBe(before + 7n * TOKEN);
    const rest = await activeOf(mock);
    expect(rest).toHaveLength(1);
    expect([2, 'bid', 'buy']).toContain(rest[0].side); // the indexer's side code of a bid
    expect(String(rest[0].price)).toBe('2510000');
    // one signature per transaction: the take, then the rest
    expect((await wallet!.calls()).length).toBe(2);
  });

  test('a lost race: the taken order is gone at the node, the book is read again and the take is tried once more', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0252' });
    await expectTake(page, /^Fills now: 2 EXKCC at 0\.0251 avg/);
    // the node refuses the first take as if its ask had been spent by someone else
    await mock.failNextSubmit('transaction has an orphan input: already spent by another transaction', 1);
    await clickPlaceOnly(page);
    await confirmUntil(page, page.getByTestId('order-take-step-1').and(page.locator('[data-state="done"]')));
    await waitSubmitted(page);
    expect(await tokensOf(mock)).toBe(before + 2n * TOKEN);
    expect(await activeOf(mock)).toEqual([]);
    expect((await wallet!.calls()).length).toBe(2);
  });

  test('a crossing limit sell fills now into the bids', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    const kas0 = BigInt((await mock.balance('alice')).kas);
    await pickType(page, 'limit', 'sell');
    // the whole first bid (a part of it would leave its rest as dust: KIP-9)
    await fillFields(page, { amount: '4', price: '0.0248' });
    await expectTake(page, /^Fills now: 4 EXKCC at 0\.0249 avg · fee ~0\.0\d+ KAS$/);
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await tokensOf(mock)).toBe(before - 4n * TOKEN);
    // 4 x 0.0249 KAS less the fee (and the KAS the token change no longer carries)
    const kas1 = BigInt((await mock.balance('alice')).kas);
    expect(kas1 - kas0).toBeGreaterThan(9_000_000n);
    expect(await activeOf(mock)).toEqual([]);
  });

  test('the auction stays one choice away', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0252', crossing: 'auction' });
    await settlePlan(page);
    await expect(page.getByTestId('order-take')).toHaveCount(0);
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await activeOf(mock)).toHaveLength(1);
  });
});

test.describe('direct fill by time in force', () => {
  test('market buy: takes the asks within its worst price, nothing else is placed', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '5' });
    await expectTake(page, /^Fills now: 5 EXKCC at 0\.0251 avg · fee ~0\.0\d+ KAS$/);
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await tokensOf(mock)).toBe(before + 5n * TOKEN);
    expect(await activeOf(mock)).toEqual([]);
  });

  test('market sell: takes the bids within its worst price', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    await pickType(page, 'market', 'sell');
    await fillFields(page, { amount: '9' });
    await expectTake(page, /^Fills now: 9 EXKCC at 0\.0249 avg · fee ~0\.0\d+ KAS$/);
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await tokensOf(mock)).toBe(before - 9n * TOKEN);
    expect(await activeOf(mock)).toEqual([]);
  });

  test('IOC: takes what crosses, the rest is not placed', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    await pickType(page, 'ioc', 'buy');
    await fillFields(page, { amount: '10', price: '0.0251' });
    await expectTake(page, /^Fills now: 7 EXKCC at 0\.0251 avg/, /^Rest: not placed$/);
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await tokensOf(mock)).toBe(before + 7n * TOKEN);
    expect(await activeOf(mock)).toEqual([]);
  });

  test('FOK: all in one transaction, or refused before signing', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const before = await tokensOf(mock);
    await pickType(page, 'fok', 'buy');
    await fillFields(page, { amount: '10', price: '0.0251' });
    await settlePlan(page);
    await expect(page.getByTestId('order-issue-TAKE_FOK_UNFILLED')).toBeVisible();
    await expect(page.getByTestId('order-place')).toBeDisabled();
    await fillFields(page, { amount: '7' });
    await expectTake(page, /^Fills now: 7 EXKCC at 0\.0251 avg · fee ~0\.0\d+ KAS$/);
    await clickPlace(page);
    await waitSubmitted(page);
    expect(await tokensOf(mock)).toBe(before + 7n * TOKEN);
  });
});

test.describe('executor tip', () => {
  test('the toggle sets the tip from the measured match fee; a tip typed by hand turns it off', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '20', price: '0.024', tipAuto: true });
    const tip = page.getByTestId('order-tip');
    await expect(tip).not.toHaveValue('');
    await expect(page.getByTestId('field-tipAuto-total')).toHaveText(/^Tip in all: 0\.0\d+ KAS$/);
    const v = Number(await tip.inputValue());
    expect(v).toBeGreaterThan(0);
    await waitReviewable(page);
    // a hand-typed tip is the user's own
    await fillFields(page, { tip: '0.0001' });
    await expect(page.getByTestId('field-tipAuto')).not.toBeChecked();
    await expect(tip).toHaveValue('0.0001');
    await expect(page.getByTestId('field-tipAuto-total')).toHaveCount(0);
  });
});

test.describe('screenshots of the ticket before signing', () => {
  test.skip(!SHOTS, 'KOB_TAKE_SHOTS names the folder');
  test('desktop light', async ({ appPage: page, mock }) => {
    await page.emulateMedia({ colorScheme: 'light' });
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '10', price: '0.0251' });
    await expectTake(page, /^Fills now: 7 EXKCC/, /^Rest: 3 EXKCC at 0\.0251$/);
    // the block is at the end of the ticket's scrolling form
    await page.getByTestId('order-take').scrollIntoViewIfNeeded();
    mkdirSync(SHOTS, { recursive: true });
    await page.getByTestId('order-ticket').screenshot({ path: `${SHOTS}/desktop-ticket.png` });
  });
  test.describe('phone', () => {
    test.use({ viewport: { width: 390, height: 844 } });
    test('phone', async ({ appPage: page, mock }) => {
      await page.emulateMedia({ colorScheme: 'light' });
      await fund(mock);
      await openMarket(page, mock);
      await pickType(page, 'limit', 'buy');
      await fillFields(page, { amount: '10', price: '0.0251' });
      await expectTake(page, /^Fills now: 7 EXKCC/, /^Rest: 3 EXKCC at 0\.0251$/);
      await page.getByTestId('order-take').scrollIntoViewIfNeeded();
      mkdirSync(SHOTS, { recursive: true });
      await page.screenshot({ path: `${SHOTS}/phone-ticket.png` });
      await clickPlaceOnly(page);
      await readReceipt(page);
      await page.screenshot({ path: `${SHOTS}/phone-confirm.png` });
      await cancelReceipt(page);
    });
  });
});
