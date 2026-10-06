// Managing placed orders through the UI (mock KasWare wallet, default mock seed): cancel (single, partly filled), cancel-replace, cancel-all of a
// position / a token / everything, refund of an expired own order, stray sweep, the escrowed-balances panel, and the recovery paths (export, clear
// local data, import; My orders with the indexer down from placement records + the node).
import { readFileSync } from 'node:fs';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet, fund, openMarket } from '../helpers/env';
import { openOrders, runFlow, seedOrders, statusesOf } from '../helpers/orders';
import { fillFields, pickType, reviewAndSign, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

const tokens = async (mock: MockClient) => BigInt((await mock.balance('alice')).tokens.EXKCC ?? '0');

test.describe('cancel', () => {
  test('cancel: the screen decodes the cancellation, the wallet signs the covenant input, the tokens come back', async ({ appPage: page, mock, wallet }) => {
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
    await openOrders(page, mock);
    const row = page.getByTestId(`order-row-${id}`);
    await expect(row).toHaveAttribute('data-status', 'open');
    expect(await tokens(mock)).toBe(0n); // the 3 tokens are inside the order: wallets do not show them
    await page.getByTestId(`order-cancel-${id}`).click();

    const steps = await runFlow(page, mock, wallet, { steps: 1 });
    const { confirm, placed } = steps[0]!;
    expect(confirm.kind).toBe('cancel');
    expect(confirm.created).toEqual([]);
    expect(confirm.closed).toHaveLength(1);
    expect(confirm.closed[0]!.title).toBe('Cancel: Sell limit order');
    expect(confirm.closed[0]!.badge).toMatch(/your order/i);
    expect(confirm.closed[0]!.rows.price).toContain('0.027 KAS / EXKCC');
    const netTok = Object.entries(confirm.sections.net!.rows).find(([k]) => k.startsWith('tok-net-'));
    expect(netTok?.[1]).toContain('+3 EXKCC');
    expect(confirm.sections.others, 'nothing goes to other keys').toBeUndefined();
    expect(placed.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);

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
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    const netTok = Object.entries(step!.confirm.sections.net!.rows).find(([k]) => k.startsWith('tok-net-'));
    expect(netTok?.[1]).toContain('+2 EXKCC');
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
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    const { confirm, placed } = step!;
    expect(confirm.kind).toBe('cancel-replace');
    expect(confirm.closed).toHaveLength(1);
    expect(confirm.closed[0]!.title).toBe('Cancel: Sell limit order');
    expect(confirm.created).toHaveLength(1);
    expect(confirm.created[0]!.title).toBe('Sell limit order');
    expect(confirm.created[0]!.rows.price).toContain('0.0272 KAS / EXKCC');
    expect(confirm.created[0]!.rows.amount).toBe('3 EXKCC');
    // in place (AMEND record): nothing is closed, the same covenant id continues with the new price
    expect(placed.submission.closed).toEqual([]);
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
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(step!.placed.submission.closed).toEqual([]);
    expect(step!.placed.submission.created).toEqual([]);
    const now = await mock.until(() => mock.order(id), (o) => o.state?.state?.price === '2350000');
    expect(now.status).toBe('open');
    expect(now.contract).toBe('KobBid');
    expect((await mock.ordersOf('alice')).map((o) => o.covenant_id)).toEqual([id]);
    await expect(page.getByTestId(`order-row-${id}`).getByTestId('order-price')).toContainText('0.0235');
  });

  test('cancel-all of a position: the entry and its exit are cancelled in ONE transaction', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'ifd', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' });
    const { placed: entry } = await reviewAndSign(page, mock);
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

    const steps = await runFlow(page, mock, wallet, { steps: 1 });
    const { confirm, placed } = steps[0]!;
    expect(confirm.kind).toBe('cancel-position');
    expect(confirm.closed.map((c) => c.title).sort()).toEqual(['Cancel: Buy IFD entry', 'Cancel: Sell take-profit order'].sort());
    expect(placed.submission.closed.map((c) => c.covenantId).sort()).toEqual([entryId, exitId].sort());
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
    const closed = steps.flatMap((s) => s.placed.submission.closed.map((c) => c.covenantId));
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
    expect(steps.flatMap((s) => s.placed.submission.closed.map((c) => c.covenantId)).sort()).toEqual([...ids].sort());
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
    const steps = await runFlow(page, mock, wallet, { steps: 1 });
    const { confirm, placed } = steps[0]!;
    expect(confirm.kind).toBe('refund');
    expect(confirm.closed).toHaveLength(1);
    expect(confirm.closed[0]!.title).toMatch(/^Refund: Sell limit order/);
    expect(placed.submission.closed).toEqual([{ covenantId: id, entry: 'refund', status: 'refunded' }]);
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
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(step!.confirm.closed[0]!.rows.strays).toContain('2 EXKCC');
    const netTok = Object.entries(step!.confirm.sections.net!.rows).find(([k]) => k.startsWith('tok-net-'));
    expect(netTok?.[1]).toContain('+5 EXKCC'); // 3 escrowed + 2 stray
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
    await waitReviewable(page);
    const { placed } = await reviewAndSign(page, mock);
    const id = placed.views[0].covenant_id;

    await page.getByTestId('nav-orders').click();
    await expect(page.getByTestId('recover-count')).toContainText('1 placement');
    const [download] = await Promise.all([page.waitForEvent('download'), page.getByTestId('orders-export').click()]);
    const file = await download.path();
    const backup = JSON.parse(readFileSync(file, 'utf8'));
    expect(backup).toMatchObject({ format: 'kob-backup', version: 1, network: 'testnet-10' });
    expect(backup.records.map((r: any) => r.covenantId)).toEqual([id]);
    expect(backup.records[0].label).toBe('limit sell');

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

  test('with the indexer down, My orders is rebuilt from the placement records and the node, and the order can still be cancelled (unread strays acknowledged)', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245' });
    const { placed } = await reviewAndSign(page, mock);
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

    await page.getByTestId(`order-cancel-${id}`).click();
    // the stray tokens owned by the order cannot be read without the indexer: the cancel says any that exist are left behind and asks the user to
    // accept that (C5-09: never abandoned silently); signing stays disabled until both boxes are ticked
    await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('confirm-plan-warnings')).toContainText('The stray tokens of this order could not be read');
    await page.getByTestId('confirm-ack').check();
    await expect(page.getByTestId('confirm-sign')).toBeDisabled();
    await page.getByTestId('confirm-ack').uncheck();
    await page.getByTestId('confirm-plan-ack').check();
    const steps = await runFlow(page, mock, wallet, { steps: 1 });
    expect(steps[0]!.confirm.kind).toBe('cancel');
    expect(steps[0]!.placed.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
    expect((await mock.order(id)).status).toBe('cancelled');
  });
});
