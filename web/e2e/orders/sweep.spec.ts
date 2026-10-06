// The maker's SWEEP of an order's stray tokens IN PLACE (KOB1 SWEEP record, matcher.md 1.2) through the UI and the mock stack: an ask with two
// strays of its own token and one FOREIGN stray (another token, outside the registry), a bid (its sweep is paid by a wallet coin), and the C5-01
// case where a cancel would abandon strays: "Sweep first", then the cancel. The mock node runs every transaction through the script engine and
// registers a verified sweep as a continuation (the order stays live at the new outpoint, its strays are spent, the tokens go to the maker).
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN } from '../helpers/env';
import { openOrders, runFlow, seedOrders } from '../helpers/orders';

test.use({ autoOpen: false });

const FOREIGN = '81'.repeat(32);
const balanceOf = async (mock: MockClient, ticker: string) => BigInt((await mock.balance('alice')).tokens[ticker] ?? '0');

test.describe('sweep strays in place', () => {
  test('an ask: 2 own strays + 1 foreign stray return to the maker, the order stays live with the same amount', async ({ appPage: page, mock, wallet }) => {
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
    await mock.stray(id!, 2n * TOKEN);
    await mock.stray(id!, 1n * TOKEN);
    await mock.stray(id!, 777, { token: FOREIGN, program: 'KCC20Ref_8x8', ticker: 'FRN' });
    const before = await mock.order(id!);
    await openOrders(page, mock);

    const row = page.getByTestId(`order-row-${id}`);
    await expect(row.getByTestId('order-strays')).toContainText('3 UTXO(s) sent to this order: sweep them back');
    const panel = page.getByTestId('orders-strays');
    await expect(panel.getByTestId('stray-row')).toHaveCount(3);
    const foreignRow = panel.locator('[data-testid="stray-row"][data-foreign="1"]');
    await expect(foreignRow).toHaveCount(1);
    await expect(foreignRow.getByTestId('stray-unknown-token')).toHaveText('unknown token');
    await expect(foreignRow.getByTestId('stray-foreign')).toHaveText('other token');
    await expect(foreignRow).toHaveAttribute('data-lost', '0');
    await expect(panel.getByTestId(`stray-sweep-${id}`)).toBeVisible();

    await page.getByTestId(`order-sweep-${id}`).click();
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    const confirm = step!.confirm;
    expect(confirm.kind).toBe('sweep');
    expect(confirm.heading).toBe('Sweep strays');
    expect(confirm.text).toContain(
      '3 stray token UTXO(s) (3 EXKCC, 777 base units of unknown token (8181...8181)) and their 30 KAS return to you. The order continues unchanged (same price, amount and custody); as a new UTXO, its 90-day idle window restarts.',
    );
    expect(confirm.closed).toEqual([]);
    expect(confirm.created).toEqual([]);
    const card = confirm.sections.sweep!.cards[0]!;
    expect(card.title).toBe('Sweep strays: Sell limit order');
    expect(card.rows.strays).toContain('3 EXKCC');
    expect(card.rows[`strays-${FOREIGN}`]).toContain('777');
    expect(card.rows.continues).toContain('continues unchanged at output 0');
    expect(confirm.warnings.join(' ')).toContain('8181'); // the foreign token is not in the registry
    expect(confirm.sections.others, 'nothing goes to other keys').toBeUndefined();

    // the mock node: a continuation of the same order (same state, custody untouched), the strays spent, the tokens at the maker
    const after = await mock.order(id!);
    expect(after.status).toBe('open');
    expect(after.amount_left).toBe(String(3n * TOKEN));
    expect(after.current.txid).toBe(step!.placed.txid);
    expect(after.current.txid).not.toBe(before.current.txid);
    expect(after.state).toEqual(before.state);
    expect(after.custody.ok).toBe(true);
    expect(after.strays).toEqual([]);
    expect(step!.placed.submission.closed).toEqual([]);
    expect(await balanceOf(mock, 'EXKCC')).toBe(3n * TOKEN);
    expect(await balanceOf(mock, 'FRN')).toBe(777n);

    // My orders: the order is still active, the strays and the sweep action are gone
    await expect(page.getByTestId('orders-strays')).toHaveCount(0, { timeout: 15_000 });
    await expect(row).toHaveAttribute('data-status', 'open');
    await expect(row.getByTestId('order-amount')).toContainText('3 / 3');
    await expect(page.getByTestId(`order-sweep-${id}`)).toHaveCount(0);
    // and it is still the maker's to cancel (from its new outpoint)
    await page.getByTestId(`order-cancel-${id}`).click();
    // the sweep's finished flow is still shown: wait for the cancel's own confirmation screen
    await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
    const [cancel] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(cancel!.confirm.kind).toBe('cancel');
    expect((await mock.order(id!)).status).toBe('cancelled');
    expect(await balanceOf(mock, 'EXKCC')).toBe(6n * TOKEN);
  });

  test('a bid: its sweep is paid by a wallet coin, its escrow stays whole', async ({ appPage: page, mock, wallet }) => {
    await mock.giveKas('alice', 50n * KAS);
    const [id] = await seedOrders(mock, [{ side: 'bid', price: 2_400_000, tokens: 2 }]);
    await mock.stray(id!, 4n * TOKEN);
    const before = await mock.order(id!);
    await openOrders(page, mock);
    await page.getByTestId(`order-sweep-${id}`).click();
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(step!.confirm.kind).toBe('sweep');
    expect(step!.confirm.text).toContain('1 stray token UTXO(s) (4 EXKCC) and their 10 KAS return to you. The order continues unchanged');
    expect(step!.confirm.sections.sweep!.cards[0]!.title).toBe('Sweep strays: Buy limit order');
    // the order input and the wallet coin are signed
    const tx = step!.placed.submission.tx!;
    expect(tx.inputs).toHaveLength(3);
    const after = await mock.order(id!);
    expect(after.status).toBe('open');
    expect(after.current.value).toBe(before.current.value);
    expect(after.current.txid).toBe(step!.placed.txid);
    expect(after.strays).toEqual([]);
    expect(await balanceOf(mock, 'EXKCC')).toBe(4n * TOKEN);
  });

  test('a cancel that would abandon strays offers "Sweep first": the sweep takes them, then the cancel', async ({ appPage: page, mock, wallet }) => {
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 1 }]);
    // the 8/8 program: the custody and 7 strays fit one cancel, the eighth stray would be abandoned
    for (let k = 1; k <= 8; k++) await mock.stray(id!, BigInt(k) * TOKEN);
    await openOrders(page, mock);
    await page.getByTestId(`order-cancel-${id}`).click();
    const dialog = page.getByTestId('sweep-first-dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText('cancelling now abandons 1 stray token UTXO(s) for good');
    await expect(page.getByTestId('sweep-first-abandon')).toHaveText('Cancel anyway (abandon 1)');
    await page.getByTestId('sweep-first-confirm').click();

    const [sweep] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(sweep!.confirm.kind).toBe('sweep');
    expect(sweep!.confirm.text).toContain('8 stray token UTXO(s) (36 EXKCC) and their 80 KAS return to you');
    expect((await mock.order(id!)).status).toBe('open');
    expect(await balanceOf(mock, 'EXKCC')).toBe(36n * TOKEN);

    const follow = page.getByTestId('orders-sweep-followup');
    await expect(follow).toBeVisible();
    await expect(page.getByTestId('orders-strays')).toHaveCount(0, { timeout: 15_000 });
    await page.getByTestId('orders-sweep-followup-cancel').click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
    const [cancel] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(cancel!.confirm.kind).toBe('cancel');
    expect(cancel!.confirm.warnings.join(' ')).not.toContain('abandoned');
    expect((await mock.order(id!)).status).toBe('cancelled');
    expect(await balanceOf(mock, 'EXKCC')).toBe(37n * TOKEN);
  });
});
