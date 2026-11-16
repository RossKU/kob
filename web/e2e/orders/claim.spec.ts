// Claim of what a Collected buy order bought (bid-delivery-spec §7, §10) through the UI and the mock stack: the order row and the balances show
// the bought tokens as the wallet's, Claim shows its fee, one approval sends them to the wallet and the order stays open with nothing bought.
// With KOB_W2_SHOTS=<dir> the same run saves the review shots (Portfolio desktop and phone, the order row, the claim dialog).
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN } from '../helpers/env';
import { openOrders, runFlow, openOrderRow } from '../helpers/orders';

test.use({ autoOpen: false });

const shots = process.env.KOB_W2_SHOTS;
const balanceOf = async (mock: MockClient, ticker: string) => BigInt((await mock.balance('alice')).tokens[ticker] ?? '0');

/** A Collected limit buy of 3 tokens of alice's, filled twice by one token: 2 tokens held by the order. */
async function collectedBid(mock: MockClient): Promise<string> {
  await mock.giveKas('alice', 500n * KAS, 2);
  await mock.giveTokens('alice', 5n * TOKEN);
  const tok = await mock.token();
  const before = new Set((await mock.ordersOf('alice')).map((o) => o.covenant_id));
  await mock.seed({ orders: [{ token: tok.covenant_id, side: 'bid', maker: 'alice', price: 2_400_000, amount: (3n * TOKEN).toString(), min_fill: TOKEN.toString(), value: '3500000000', state: { mode: '1' } }] });
  const id = (await mock.ordersOf('alice')).find((o) => !before.has(o.covenant_id))!.covenant_id as string;
  await mock.fill(id, TOKEN);
  await mock.fill(id, TOKEN);
  return id;
}

test('a Collected buy order: bought tokens shown as yours, Claim with its fee, one approval, the order stays open', async ({ appPage: page, mock, wallet }) => {
  const id = await collectedBid(mock);
  const view = await mock.order(id);
  expect(view).toMatchObject({ delivery: 'order', bought: String(2n * TOKEN), bought_custody: { ok: true } });
  const free = await balanceOf(mock, 'EXKCC');

  await page.setViewportSize({ width: 1280, height: 900 });
  await openOrders(page, mock);
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row.getByTestId(`order-bought-${id}`)).toHaveText('2 in the order');
  await openOrderRow(page, id);
  await expect(row.getByTestId('order-delivery')).toHaveText('Collected');
  await expect(row.getByTestId('order-bought-detail')).toContainText('2 EXKCC');
  await expect(row.getByTestId(`order-claim-${id}`)).toBeVisible();
  if (shots) await page.waitForTimeout(900);
  if (shots) await row.screenshot({ path: `${shots}/after-order-row.png` });

  // Portfolio balances: in your orders = bought, as a sub-line; Claim next to the other buttons
  await page.goto('/#/portfolio');
  const tok = await mock.token();
  const bal = page.getByTestId(`balance-row-${tok.covenant_id}`);
  await expect(bal.getByTestId('balance-bought')).toHaveText('bought 2');
  await expect(bal.getByTestId(`balance-claim-${tok.covenant_id}`)).toHaveText('Claim');
  if (shots) {
    await page.waitForTimeout(900);
    await page.screenshot({ path: `${shots}/after-portfolio-desktop.png`, fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(bal.getByTestId('balance-bought')).toBeVisible();
    await page.waitForTimeout(900);
    await page.screenshot({ path: `${shots}/after-portfolio-phone.png`, fullPage: true });
    await page.setViewportSize({ width: 1280, height: 900 });
  }

  await bal.getByTestId(`balance-claim-${tok.covenant_id}`).click();
  const dialog = page.getByTestId('claim-dialog');
  await expect(dialog.getByTestId('claim-amount')).toContainText('2 EXKCC');
  await expect(dialog.getByTestId('claim-fee')).toContainText(/Network fee: [\d.]+ KAS/);
  await expect(dialog.getByTestId('claim-carrier')).toBeVisible();
  // merge: the wallet's UTXO can join; sell: a price below the order's own is refused
  await dialog.getByTestId('claim-kind-sell').click();
  await dialog.getByTestId('claim-sell-price').fill('0.01');
  await expect(dialog.getByText('This price would sell into your own buy order')).toBeVisible();
  await expect(dialog.getByTestId('claim-confirm')).toBeDisabled();
  await dialog.getByTestId('claim-kind-claim').click();
  if (shots) await page.waitForTimeout(500);
  if (shots) await dialog.screenshot({ path: `${shots}/after-claim-dialog.png` });
  await dialog.getByTestId('claim-confirm').click();
  const [tx] = await runFlow(page, mock, wallet, { steps: 1 });

  // one transaction: the order continues (nothing closed), its bought-custody is spent, the tokens go to alice as one UTXO
  expect(tx!.submission.closed).toEqual([]);
  expect(tx!.submission.spent.some((s) => s.covenantId === tok.covenant_id && s.outpoint === `${view.bought_custody.utxo.txid}:${view.bought_custody.utxo.index}`)).toBe(true);
  const after = await mock.order(id);
  expect(after).toMatchObject({ status: 'partial', bought: '0', state_known: true });
  expect(after.current.txid).toBe(tx!.txid);
  expect(await balanceOf(mock, 'EXKCC')).toBe(free + 2n * TOKEN);
  await expect(bal.getByTestId('balance-bought')).toHaveCount(0, { timeout: 15_000 });
});

test('cancel of a Collected buy order returns its KAS and the tokens it bought', async ({ appPage: page, mock, wallet }) => {
  const id = await collectedBid(mock);
  const free = await balanceOf(mock, 'EXKCC');
  await openOrders(page, mock);
  await page.getByTestId(`order-cancel-${id}`).click();
  const [tx] = await runFlow(page, mock, wallet, { steps: 1 });
  expect(tx!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
  expect(await balanceOf(mock, 'EXKCC')).toBe(free + 2n * TOKEN);
});
