// Managing placed orders through the UI (mock KasWare wallet, default mock seed): cancel (single, partly filled), cancel-replace, cancel-all of a
// position / a token / everything, refund of an expired own order, stray sweep, the escrowed-balances panel, and the recovery paths (export, clear
// local data, import; My orders with the indexer down from placement records + the node).
import { readFileSync } from 'node:fs';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet, fund, openMarket } from '../helpers/env';
import { openOrders, runFlow, seedOrders, statusesOf } from '../helpers/orders';
import { fillFields, pickType, placeOrder, type Placed } from '../helpers/ticket';

test.use({ autoOpen: false });

const tokens = async (mock: MockClient) => BigInt((await mock.balance('alice')).tokens.EXKCC ?? '0');
/** Token amount a transaction pays out to `owner` (wallet-owned outputs only, never covenant custody). */
const tokensTo = (placed: Placed, owner: string): bigint => placed.submission.token_outputs.filter((o) => o.role === 'owned' && o.owner === owner).reduce((a, o) => a + BigInt(o.amount), 0n);

test.describe('cancel', () => {
  test('cancel: the wallet signs the covenant input, the accepted transaction releases the tokens to the maker, the tokens come back', async ({ appPage: page, mock, wallet }) => {
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
    await openOrders(page, mock);
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toHaveAttribute('data-status', 'open');
    expect(await tokens(mock)).toBe(0n); // the 3 tokens are inside the order: wallets do not show them
    await page.getByTestId(`order-cancel-${id}`).click();

    // no confirmation screen: the click asked the wallet at once
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(placed!.submission.created).toEqual([]);
    expect(placed!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
    const alice = (await mock.balance('alice')).pubkey;
    expect(tokensTo(placed!, alice)).toBe(3n * TOKEN);
    expect(placed!.submission.token_outputs.every((o) => o.owner === alice), 'nothing goes to other keys').toBe(true);

    expect((await mock.order(id)).status).toBe('cancelled');
    expect(await tokens(mock)).toBe(3n * TOKEN);
    // the order moves from Active to History
    await expect(page.getByTestId(`order-row-${id}`)).toHaveCount(0, { timeout: 15_000 });
    await page.getByTestId('orders-tab-history').click();
    await expect(page.getByTestId(`order-row-${id}`)).toHaveAttribute('data-status', 'cancelled');
  });

  test('cancel of a partly filled order returns only the amount that is left', async ({ appPage: page, mock, wallet }) => {
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
    await mock.fill(id);
    await mock.until(() => mock.order(id), (o) => o.status === 'partial' && o.amount_left === String(2n * TOKEN));
    await openOrders(page, mock);
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toHaveAttribute('data-status', 'partial');
    await expect(row.getByTestId('order-amount')).toContainText('2 / 3');
    await page.getByTestId(`order-cancel-${id}`).click();
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(tokensTo(placed!, (await mock.balance('alice')).pubkey)).toBe(2n * TOKEN);
    expect((await mock.order(id)).status).toBe('cancelled');
    expect(await tokens(mock)).toBe(2n * TOKEN);
  });

  test('amend the price of an ask: amended IN PLACE, the order keeps its id and its custody, one transaction', async ({ appPage: page, mock, wallet }) => {
    await mock.giveKas('alice', 50n * KAS); // the replacement needs a fresh carrier
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
    await openOrders(page, mock);
    await page.getByTestId(`order-replace-${id}`).click();
    await page.getByTestId('amend-price').fill('0.0272');
    await expect(page.getByTestId('amend-preview')).toContainText('0.0272');
    await page.getByTestId('amend-review').click();
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    // in place (AMEND record): nothing is closed or created, the same covenant id continues with the new price
    expect(placed!.submission.records.map((r) => r.type)).toContain('amend');
    expect(placed!.submission.closed).toEqual([]);
    expect(placed!.submission.created).toEqual([]);
    // the amended order keeps the old order's scale (1e8 base units, one whole token): 0.0272 KAS per token = 2_720_000 sompi
    const now = await mock.until(() => mock.order(id), (o) => o.state?.state?.price === '2720000');
    expect(now.state.state).toMatchObject({ price: '2720000', scale: '100000000', amountLeft: String(3n * TOKEN) });
    expect(now.status).toBe('open');
    const active = await mock.ordersOf('alice');
    expect(active.map((o) => o.covenant_id)).toEqual([id]);
    // My orders shows the amended order with its new price
    await expect(page.getByTestId(`order-row-${id}`).getByTestId('order-price')).toContainText('0.0272');
  });

  // B1: a plain bid is amended in place too (it owns no custody: its quantity is its escrow, which pays the fee)
  test('amend the price of a bid: amended IN PLACE, the order keeps its id, the escrow pays the fee', async ({ appPage: page, mock, wallet }) => {
    const [id] = await seedOrders(mock, [{ side: 'bid', price: 2_400_000, tokens: 2 }]);
    await openOrders(page, mock);
    await page.getByTestId(`order-replace-${id}`).click();
    await page.getByTestId('amend-price').fill('0.0235');
    await page.getByTestId('amend-review').click();
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(placed!.submission.closed).toEqual([]);
    expect(placed!.submission.created).toEqual([]);
    const now = await mock.until(() => mock.order(id), (o) => o.state?.state?.price === '2350000');
    expect(now.status).toBe('open');
    expect(now.contract).toBe('KobBid');
    expect((await mock.ordersOf('alice')).map((o) => o.covenant_id)).toEqual([id]);
    await expect(page.getByTestId(`order-row-${id}`).getByTestId('order-price')).toContainText('0.0235');
  });

  test('amend a TWAP: the replace form opens on its schedule; the edited TWAP replaces it in one transaction', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'twap', 'sell');
    await fillFields(page, { amount: '6', sliceAmount: '2', interval: '1', price: '0.027' });
    const placed = await placeOrder(page, mock);
    const id = placed.views[0].covenant_id as string;
    const before = (await mock.order(id)).state.state;
    await page.getByTestId('nav-orders').click();
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toHaveAttribute('data-type', 'twap');
    await row.getByTestId(`order-replace-${id}`).click();
    const dialog = page.getByTestId('replace-dialog');
    await expect(dialog.getByTestId('replace-form')).toHaveAttribute('data-type', 'twap');
    // the schedule as placed: slice, interval, price
    await expect(dialog.getByTestId('field-sliceAmount')).toHaveValue('2');
    await expect(dialog.getByTestId('field-interval')).toHaveValue('1');
    await expect(dialog.getByTestId('order-price')).toHaveValue('0.027');
    await dialog.getByTestId('order-price').fill('0.0272');
    await expect(dialog.getByTestId('replace-review')).toBeEnabled({ timeout: 30_000 });
    await dialog.getByTestId('replace-review').click();
    const [amended] = await runFlow(page, mock, wallet, { steps: 1 });
    // amended in place: the same covenant id continues as a TWAP with the same slice and interval, at the new price
    expect(amended!.submission.records.map((r) => r.type)).toContain('amend');
    expect(amended!.submission.closed).toEqual([]);
    const after = (await mock.until(() => mock.order(id), (o) => o.state?.state?.price === '2720000')).state.state;
    expect(after).toMatchObject({ interval: before.interval, maxFill: before.maxFill, amountLeft: before.amountLeft, price: '2720000' });
  });

  test('renew a GTC DCA near the end of its 90 days: the renewal replaces it with the same schedule and a fresh expiry', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'dca', 'buy');
    await fillFields(page, { amount: '4', sliceAmount: '2', interval: '5', price: '0.0245' });
    const placed = await placeOrder(page, mock);
    const id = placed.views[0].covenant_id as string;
    const before = (await mock.order(id)).state.state;
    // day 86 of 90: the renewal is due
    await mock.advanceSeconds(86 * 86_400);
    await page.getByTestId('nav-orders').click();
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toHaveAttribute('data-type', 'dca');
    await expect(row.getByTestId('order-renewal-due')).toBeVisible({ timeout: 30_000 });
    await row.getByTestId(`order-renew-${id}`).click();
    const dialog = page.getByTestId('replace-dialog');
    await expect(dialog.getByTestId('replace-form')).toHaveAttribute('data-type', 'dca');
    await expect(dialog.getByTestId('field-interval')).toHaveValue('5');
    await expect(dialog.getByTestId('replace-review')).toBeEnabled({ timeout: 30_000 });
    await dialog.getByTestId('replace-review').click();
    const [renewed] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(renewed!.submission.records.map((r) => r.type)).toContain('amend');
    // amended in place: the same covenant id, the same schedule, a fresh 90 days
    const after = (await mock.until(() => mock.order(id), (o) => o.state?.state?.expiryDaa !== before.expiryDaa)).state.state;
    expect(after).toMatchObject({ interval: before.interval, maxFill: before.maxFill, price: before.price });
    expect(BigInt(after.expiryDaa)).toBeGreaterThan(BigInt(before.expiryDaa) + 80n * 86_400n * 10n);
  });

  test('cancel-all of a position: the entry and its exit are cancelled in ONE transaction', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'ifd', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' });
    const entry = await placeOrder(page, mock);
    const entryId = entry.views[0].covenant_id;
    // a fill of the entry books its exit order (one whole token: the position now has an entry with 1 EXKCC left and an exit for 1 EXKCC)
    await mock.fill(entryId);
    const kids = await mock.until(async () => (await mock.order(entryId)).children as string[], (c) => c.length === 1);
    const exitId = kids[0]!;
    expect((await mock.order(exitId)).contract).toBe('KobCondAsk');

    await page.getByTestId('nav-orders').click();
    const card = page.locator('[data-testid^="position-"][data-kind]').first();
    await expect(card).toBeVisible();
    await expect(card.getByTestId(`order-row-${entryId}`)).toBeVisible();
    await expect(card.getByTestId('position-exits').getByTestId(`order-row-${exitId}`)).toHaveAttribute('data-type', 'take-profit');
    const before = (await mock.submitted(false)).length;
    await card.locator('[data-testid^="position-cancel-"]').click();

    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(placed!.submission.closed.map((c) => c.covenantId).sort()).toEqual([entryId, exitId].sort());
    expect(placed!.submission.closed.every((c) => c.entry === 'cancel' && c.status === 'cancelled')).toBe(true);
    expect((await mock.submitted(false)).length).toBe(before + 1);
    const st = await statusesOf(mock, 'alice');
    expect(st[entryId]).toBe('cancelled');
    expect(st[exitId]).toBe('cancelled');
    // everything the position held returns to alice (the 1 token the fill bought is back in her wallet)
    expect(await tokens(mock)).toBe(101n * TOKEN);
  });

  test('cancel-all of a token (balances panel) and cancel-all of everything', async ({ appPage: page, mock, wallet }) => {
    const ids = await seedOrders(mock, [
      { side: 'ask', price: 2_700_000, tokens: 2 },
      { side: 'ask', price: 2_800_000, tokens: 1 },
      { side: 'bid', price: 2_400_000, tokens: 2 },
    ]);
    await openOrders(page, mock);
    await expect(page.getByTestId('orders-tab-active')).toContainText('(3)');
    const tok = await mock.token();

    // per token
    await page.getByTestId(`orders-cancel-token-${tok.covenant_id}`).click();
    await expect(page.getByTestId('cancel-all-dialog')).toContainText('3 order');
    await page.getByTestId('cancel-all-confirm').click();
    const steps = await runFlow(page, mock, wallet);
    expect(steps.length).toBeGreaterThanOrEqual(1);
    // a several-step flow lists every step, all submitted
    if (steps.length > 1) for (let k = 0; k < steps.length; k++) await expect(page.getByTestId(`flow-step-${k}`)).toHaveAttribute('data-state', 'submitted');
    const closed = steps.flatMap((s) => s.submission.closed.map((c) => c.covenantId));
    expect(closed.sort()).toEqual([...ids].sort());
    const st = await statusesOf(mock, 'alice');
    for (const id of ids) expect(st[id]).toBe('cancelled');
    expect(await tokens(mock)).toBe(3n * TOKEN);
    await expect(page.getByTestId('orders-empty')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('orders-cancel-all')).toBeDisabled();
  });

  test('cancel-all of everything asks first, names the count, and cancels every live order', async ({ appPage: page, mock, wallet }) => {
    const ids = await seedOrders(mock, [
      { side: 'ask', price: 2_700_000, tokens: 2 },
      { side: 'bid', price: 2_400_000, tokens: 2 },
    ]);
    await openOrders(page, mock);
    await page.getByTestId('orders-cancel-all').click();
    await expect(page.getByTestId('cancel-all-dialog')).toContainText('2 order');
    // backing out cancels nothing
    await page.getByTestId('cancel-all-dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(page.getByTestId('cancel-all-dialog')).toHaveCount(0);
    expect(await mock.submitted(false)).toHaveLength(0);
    await page.getByTestId('orders-cancel-all').click();
    await page.getByTestId('cancel-all-confirm').click();
    const steps = await runFlow(page, mock, wallet);
    expect(steps.flatMap((s) => s.submission.closed.map((c) => c.covenantId)).sort()).toEqual([...ids].sort());
    expect(await mock.ordersOf('alice')).toEqual([]);
  });
});

test.describe('refund, strays, balances', () => {
  test('an expired own order is refunded: the tokens come back and the order ends as refunded', async ({ appPage: page, mock, wallet }) => {
    await mock.giveKas('alice', 50n * KAS); // reclaiming the refund tip is signed with a wallet coin
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 2 }]);
    await mock.advanceDaa(101_000_000); // past the seeded expiry
    await openOrders(page, mock);
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row.getByTestId('order-expired')).toBeVisible();
    await expect(row.getByTestId('order-refund-from')).toBeVisible();
    await page.getByTestId(`order-refund-${id}`).click();
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(placed!.submission.closed).toEqual([{ covenantId: id, entry: 'refund', status: 'refunded' }]);
    expect(tokensTo(placed!, (await mock.balance('alice')).pubkey)).toBe(2n * TOKEN);
    expect((await mock.order(id)).status).toBe('refunded');
    expect(await tokens(mock)).toBe(2n * TOKEN);
    await page.getByTestId('orders-tab-history').click();
    await expect(page.getByTestId(`order-row-${id}`)).toHaveAttribute('data-status', 'refunded');
  });

  test('stray tokens sent to an order are shown as inert and are swept by the maker cancel', async ({ appPage: page, mock, wallet }) => {
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
    await mock.stray(id, 2n * TOKEN);
    await openOrders(page, mock);
    const strays = page.getByTestId('orders-strays');
    await expect(strays).toBeVisible();
    await expect(strays.getByTestId('stray-row')).toHaveCount(1);
    await expect(strays.getByTestId('stray-row')).toHaveAttribute('data-lost', '0');
    await expect(strays.getByTestId('stray-row')).toContainText('recover: sweep or cancel');
    const bal = page.getByTestId('balances-panel');
    await expect(bal.getByTestId('balance-escrowed')).toHaveText(/^3/);
    await expect(bal.getByTestId('balance-strays')).toHaveText(/^2/);
    await expect(page.getByTestId('strays-note')).toBeVisible();

    await page.getByTestId(`order-cancel-${id}`).click();
    const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
    // the order's own 3 tokens plus the 2 strays come back to the maker, each counted once
    expect(placed!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
    expect(tokensTo(placed!, (await mock.balance('alice')).pubkey)).toBe(5n * TOKEN);
    expect(await tokens(mock)).toBe(5n * TOKEN);
    await expect(page.getByTestId('orders-strays')).toHaveCount(0, { timeout: 15_000 });
  });

  test('escrowed balances: tokens and KAS inside orders are listed next to the free balance', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { tokens: 10n });
    await seedOrders(mock, [
      { side: 'ask', price: 2_700_000, tokens: 3 },
      { side: 'bid', price: 2_400_000, tokens: 2 },
    ]);
    await openOrders(page, mock);
    const tok = await mock.token();
    const bal = page.getByTestId('balances-panel');
    const row = bal.getByTestId(`balance-row-${tok.covenant_id}`);
    await expect(row.getByTestId('balance-escrowed')).toHaveText(/^3/);
    await expect(row).toContainText('10'); // free tokens
    await expect(bal.getByTestId('kas-locked')).not.toHaveText(/^0(\.0+)? KAS/);
    await expect(bal.getByTestId('kas-free')).toContainText('KAS');
  });
});

test.describe('recovery', () => {
  test('export a backup, clear the local data, import it: the order is found again without the indexer', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.027' });
    const placed = await placeOrder(page, mock);
    const id = placed.views[0].covenant_id;

    await page.getByTestId('nav-orders').click();
    await expect(page.getByTestId('recover-count')).toContainText('1 placement');
    const [download] = await Promise.all([page.waitForEvent('download'), page.getByTestId('orders-export').click()]);
    const file = await download.path();
    const backup = JSON.parse(readFileSync(file, 'utf8'));
    expect(backup).toMatchObject({ format: 'kob-backup', version: 1, network: 'testnet-10' });
    expect(backup.records.map((r: any) => r.covenantId)).toEqual([id]);
    expect(backup.records[0].label).toBe('Sell · Limit');

    // clear the local data through Settings
    await page.getByTestId('nav-settings').click();
    await expect(page.getByTestId('local-data-list')).toContainText('Placement record');
    await page.getByTestId('settings-clear').click();
    await page.getByTestId('clear-confirm-yes').click(); // reloads the page
    await page.getByTestId('nav-orders').waitFor();
    // now the indexer disappears: nothing knows about the order any more
    await page.route('**/v1/**', (r) => r.abort());
    await page.goto('/#/orders');
    await page.reload();
    await connectWallet(page);
    await expect(page.getByTestId('orders-indexer-down')).toBeVisible();
    await expect(page.getByTestId('recover-count')).toContainText('0 placement');
    await expect(page.getByTestId(`order-row-${id}`)).toHaveCount(0);

    // import the backup: the order is resolved on the node from its placement record
    await page.getByTestId('orders-import').setInputFiles(file);
    await expect(page.getByTestId('import-preview')).toContainText('1 record(s) can be imported');
    await page.getByTestId('orders-import-apply').click();
    await expect(page.getByTestId('recover-message')).toContainText('Imported 1');
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-status', 'open');
    await expect(row.getByTestId('order-source')).toBeVisible(); // "from your records": read from the node, not the indexer
    await expect(row.getByTestId('order-price')).toContainText('0.027');
    await expect(row.getByTestId('order-amount')).toContainText('2');
  });

  test('with the indexer down, My orders is rebuilt from the placement records and the node, and the order can still be cancelled', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245' });
    const placed = await placeOrder(page, mock);
    const id = placed.views[0].covenant_id;
    // the indexer becomes unreachable (REST); the node keeps answering
    await page.route('**/v1/**', (r) => r.abort());
    await page.reload();
    await connectWallet(page);
    await page.getByTestId('nav-orders').click();
    await expect(page.getByTestId('orders-indexer-down')).toBeVisible();
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-status', 'open');
    await expect(row).toHaveAttribute('data-side', 'buy');
    await expect(row.getByTestId('order-source')).toBeVisible();
    await expect(row.getByTestId('order-price')).toContainText('0.0245');

    // the stray tokens owned by the order cannot be read without the indexer: the cancel goes ahead on the records and the node alone
    await page.getByTestId(`order-cancel-${id}`).click();
    const [cancel] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(cancel!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
    expect((await mock.order(id)).status).toBe('cancelled');
  });
});
