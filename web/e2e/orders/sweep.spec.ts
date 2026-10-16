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
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    // the transaction: the 3 strays are spent, the order continues (nothing closed or created), and what the strays held goes to the maker only
    const alice = (await mock.balance('alice')).pubkey;
    expect(placed!.submission.records.map((r) => r.type)).toContain('sweep');
    expect(placed!.submission.closed).toEqual([]);
    expect(placed!.submission.created).toEqual([]);
    const own = placed!.submission.token_outputs.filter((o) => o.role === 'owned');
    expect(own.every((o) => o.owner === alice), 'nothing goes to other keys').toBe(true);

    // the mock node: a continuation of the same order (same state, custody untouched), the strays spent, the tokens at the maker
    const after = await mock.order(id!);
    expect(after.status).toBe('open');
    expect(after.amount_left).toBe(String(3n * TOKEN));
    expect(after.current.txid).toBe(placed!.txid);
    expect(after.current.txid).not.toBe(before.current.txid);
    expect(after.state).toEqual(before.state);
    expect(after.custody.ok).toBe(true);
    expect(after.strays).toEqual([]);
    expect(await balanceOf(mock, 'EXKCC')).toBe(3n * TOKEN);
    expect(await balanceOf(mock, 'FRN')).toBe(777n);

    // My orders: the order is still active, the strays and the sweep action are gone
    await expect(page.getByTestId('orders-strays')).toHaveCount(0, { timeout: 15_000 });
    await expect(row).toHaveAttribute('data-status', 'open');
    await expect(row.getByTestId('order-amount')).toContainText('3 / 3');
    await expect(page.getByTestId(`order-sweep-${id}`)).toHaveCount(0);
    // and it is still the maker's to cancel (from its new outpoint); the sweep's result is still on screen until the cancel's flow replaces it:
    // its wallet request says it has
    const asked = (await wallet!.calls()).length;
    await page.getByTestId(`order-cancel-${id}`).click();
    await wallet!.waitForCalls(asked + 1);
    const [cancel] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(cancel!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
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
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(placed!.submission.records.map((r) => r.type)).toContain('sweep');
    expect(placed!.submission.closed).toEqual([]);
    expect(placed!.submission.token_outputs.filter((o) => o.role === 'owned').reduce((a, o) => a + BigInt(o.amount), 0n)).toBe(4n * TOKEN);
    // the order input and the wallet coin are signed
    const tx = placed!.submission.tx!;
    expect(tx.inputs).toHaveLength(3);
    const after = await mock.order(id!);
    expect(after.status).toBe('open');
    expect(after.current.value).toBe(before.current.value);
    expect(after.current.txid).toBe(placed!.txid);
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
    await expect(dialog).toContainText('now abandons 1 stray token UTXO(s) for good');
    await expect(page.getByTestId('sweep-first-abandon')).toHaveText('Cancel anyway (abandon 1)');
    await page.getByTestId('sweep-first-confirm').click();

    const [sweep] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(sweep!.submission.records.map((r) => r.type)).toContain('sweep');
    expect((await mock.order(id!)).status).toBe('open');
    expect(await balanceOf(mock, 'EXKCC')).toBe(36n * TOKEN);

    const follow = page.getByTestId('orders-sweep-followup');
    await expect(follow).toBeVisible();
    await expect(page.getByTestId('orders-strays')).toHaveCount(0, { timeout: 15_000 });
    const asked = (await wallet!.calls()).length;
    await page.getByTestId('orders-sweep-followup-cancel').click();
    await wallet!.waitForCalls(asked + 1); // the cancel's own flow has started: the sweep's result is gone
    const [cancel] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(cancel!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
    expect((await mock.order(id!)).status).toBe('cancelled');
    expect(await balanceOf(mock, 'EXKCC')).toBe(37n * TOKEN);
  });
});
