// Smoke specs of "My orders": an order seeded for the wallet key shows up with its balances, cancel and amend reach the flow (the confirmation
// screen itself belongs to the ticket / confirm work: until it lands only the flow panel is asserted), recovery export / import round trip.
import { readFileSync } from 'node:fs';
import { expect, test, TEST_KEYS, type MockClient } from '../fixtures';
import { TESTID, orderCancel, orderReplace, orderRow } from '../testids';
import { loadKobNode } from '../../src/kob/wasm.node';
import { listExkcc } from '../helpers/env';

/** an ask of `tokens` whole EXKCC at `price` sompi per whole token (2_505_000 = 0.02505 KAS / EXKCC) */
async function seedAsk(mock: MockClient, price = 2_505_000, tokens = 3) {
  const tok = await mock.token();
  await mock.seed({ orders: [{ token: tok.covenant_id, side: 'ask', maker: 'alice', price, amount: (BigInt(tokens) * 100_000_000n).toString() }] });
  const [order] = await mock.ordersOf('alice');
  return { tok, order };
}

test.describe('my orders', () => {
  test('asks to connect a wallet first', async ({ appPage }) => {
    await appPage.goto('/#/orders');
    await expect(appPage.getByTestId('orders-connect')).toBeVisible();
  });

  test('an order seeded for the wallet key appears with type, price, amount, status and escrowed balances', async ({ appPage, mock }) => {
    const { tok, order } = await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    const row = appPage.getByTestId(orderRow(order.covenant_id));
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-side', 'sell');
    await expect(row).toHaveAttribute('data-status', 'open');
    await expect(row.getByTestId('order-price')).toContainText('0.02505');
    await expect(row.getByTestId('order-allin')).toContainText('0.02505');
    await expect(row.getByTestId('order-amount')).toContainText('3 / 3');
    await expect(appPage.getByTestId(TESTID.ordersList)).toBeVisible();
    // tokens inside the sell order are shown (wallets do not show them)
    const bal = appPage.getByTestId(TESTID.balancesPanel).getByTestId(`balance-row-${tok.covenant_id}`);
    await expect(bal.getByTestId('balance-escrowed')).toHaveText(/^3/);
    await expect(appPage.getByTestId('orders-tab-active')).toContainText('(1)');
    await appPage.getByTestId('orders-tab-history').click();
    await expect(appPage.getByTestId('orders-empty')).toBeVisible();
  });

  test('the same token reads the same on the market list, the token page and My orders (registry says verified, the mock indexer says unverified)', async ({ appPage, mock }) => {
    // EXKCC listed and verified in a registry served by the spec (so it is a custom registry for this build: `[listed in custom registry <hash>]`, never
    // `[unverified]`); the mock executor's standing for it is `unverified` (it says that of every token that is not official)
    await listExkcc(appPage);
    const { tok, order } = await seedAsk(mock);
    const short = `${tok.covenant_id.slice(0, 4)}…${tok.covenant_id.slice(-4)}`;
    await appPage.goto('/#/market');
    const listText = (await appPage.getByTestId(`token-link-${tok.covenant_id}`).textContent()) ?? '';
    expect(listText.startsWith(`EXKCC (${short}) [listed in custom registry `)).toBe(true);
    expect(listText).not.toContain('unverified');
    await expect(appPage.getByTestId(`token-row-${tok.covenant_id}`).getByTestId('badge-unverified')).toHaveCount(0);
    await appPage.goto(`/#/market/${tok.covenant_id}`);
    await expect(appPage.getByTestId('token-header')).toContainText(listText);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await expect(appPage.getByTestId(orderRow(order.covenant_id)).getByTestId('order-token')).toHaveText(listText);
  });

  test('cancel: the confirmation screen decodes the cancellation, nothing is signed before the user acts, then the order is closed', async ({ appPage, mock, wallet }) => {
    const { order } = await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(orderCancel(order.covenant_id)).click();
    await expect(appPage.getByTestId('tx-flow')).toBeVisible();
    await expect(appPage.getByTestId(TESTID.confirmScreen)).toBeVisible();
    await expect(appPage.getByTestId('flow-plan-errors')).toHaveCount(0);
    await expect(appPage.getByTestId('orders-action-error')).toHaveCount(0);
    expect(await wallet!.calls()).toHaveLength(0);
    expect(await mock.submitted(false)).toHaveLength(0);
    await appPage.getByTestId('confirm-ack').check();
    await appPage.getByTestId(TESTID.confirmSign).click();
    await expect(appPage.getByTestId(TESTID.txId)).toBeVisible({ timeout: 30_000 });
    await appPage.getByTestId('confirm-close').click();
    await expect(appPage.getByTestId('flow-done')).toBeVisible();
    const done = await mock.order(order.covenant_id);
    expect(done.status).toBe('cancelled');
    // the order moves from Active to History
    await expect(appPage.getByTestId(orderRow(order.covenant_id))).toHaveCount(0, { timeout: 15_000 });
    await appPage.getByTestId('orders-tab-history').click();
    await expect(appPage.getByTestId(orderRow(order.covenant_id))).toHaveAttribute('data-status', 'cancelled');
  });

  test('cancel-all asks for confirmation first', async ({ appPage, mock }) => {
    await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(TESTID.ordersCancelAll).click();
    await expect(appPage.getByTestId('cancel-all-dialog')).toContainText('1 order');
    await appPage.getByTestId('cancel-all-confirm').click();
    await expect(appPage.getByTestId('tx-flow')).toBeVisible();
  });

  test('amend: validates the form, previews the new price and builds the atomic replacement', async ({ appPage, mock }) => {
    const { order } = await seedAsk(mock);
    await mock.giveKas('alice', 50n * 10n ** 8n); // the replacement needs a fresh carrier: a wallet without KAS gets a clear refusal instead
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(orderReplace(order.covenant_id)).click();
    const dialog = appPage.getByTestId('amend-dialog');
    await expect(dialog).toBeVisible();
    await expect(appPage.getByTestId('amend-review')).toBeDisabled(); // nothing changed yet
    await appPage.getByTestId('amend-price').fill('abc');
    await expect(dialog).toContainText('valid amount');
    await appPage.getByTestId('amend-price').fill('0.0251');
    await expect(appPage.getByTestId('amend-preview')).toContainText('0.0251');
    await expect(appPage.getByTestId('amend-review')).toBeEnabled();
    await appPage.getByTestId('amend-review').click();
    await expect(appPage.getByTestId('tx-flow')).toBeVisible();
    await expect(dialog).toHaveCount(0);
  });

  test('amend end to end: the order is amended in place and rests at the new price under the same id', async ({ appPage, mock }) => {
    const { order } = await seedAsk(mock);
    await mock.giveKas('alice', 50n * 10n ** 8n);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(orderReplace(order.covenant_id)).click();
    await appPage.getByTestId('amend-price').fill('0.0251');
    await appPage.getByTestId('amend-review').click();
    await expect(appPage.getByTestId(TESTID.confirmScreen)).toBeVisible();
    await appPage.getByTestId('confirm-ack').check();
    await appPage.getByTestId(TESTID.confirmSign).click();
    await expect(appPage.getByTestId(TESTID.txId)).toBeVisible({ timeout: 30_000 });
    await appPage.getByTestId('confirm-close').click();
    // an ask keeping its amount is amended IN PLACE (AMEND record): the same order id continues with the new price, its custody never moved
    const active = await mock.until(() => mock.ordersOf('alice'), (o) => o.length === 1 && o[0].price === '2510000');
    expect(active[0].covenant_id).toBe(order.covenant_id); // 0.0251 KAS per token = 2_510_000 sompi per whole token
  });

  test('an in-place amend needs no KAS in the wallet: the order\'s own carrier pays the fee', async ({ appPage, mock }) => {
    const { order } = await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(orderReplace(order.covenant_id)).click();
    await appPage.getByTestId('amend-price').fill('0.0251');
    await appPage.getByTestId('amend-review').click();
    await expect(appPage.getByTestId('amend-error')).toHaveCount(0);
    await expect(appPage.getByTestId('tx-flow')).toBeVisible();
  });

  test('the page has no horizontal scroll on a phone', async ({ appPage, mock }) => {
    await seedAsk(mock);
    await appPage.setViewportSize({ width: 390, height: 800 });
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await expect(appPage.getByTestId(TESTID.ordersList)).toBeVisible();
    const over = await appPage.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(over).toBeLessThanOrEqual(0);
  });
});

test.describe('recover', () => {
  test('export a backup and import it again after the records were wiped', async ({ appPage, mock }) => {
    const { order } = await seedAsk(mock);
    const view = await mock.order(order.covenant_id);
    const kob = await loadKobNode();
    const key = TEST_KEYS.alice;
    // the record the app would have stored when the order was placed
    const record = {
      version: 1, network: 'testnet-10', maker: key.pubkey, txid: null, output: null, covenantId: order.covenant_id, kind: 'KobAsk', templateHash: view.template_hash,
      state: kob.encodeState(view.state), amount: '300000000', custody: null, value: '0', placedAtUnix: '1790000000', placedAtDaa: '1000', label: 'e2e',
    };
    const storeKey = `kob.records.v1:testnet-10:${key.pubkey}`;
    await appPage.evaluate(([k, v]) => localStorage.setItem(k, v), [storeKey, JSON.stringify({ [order.covenant_id]: record })]);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.reload();
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await expect(appPage.getByTestId('recover-count')).toContainText('1 placement');

    const [download] = await Promise.all([appPage.waitForEvent('download'), appPage.getByTestId(TESTID.ordersExport).click()]);
    expect(download.suggestedFilename()).toMatch(/^kob-backup-testnet-10-\d{8}\.json$/);
    const path = await download.path();
    const backup = JSON.parse(readFileSync(path, 'utf8'));
    expect(backup).toMatchObject({ format: 'kob-backup', version: 1, network: 'testnet-10' });
    expect(backup.records).toHaveLength(1);
    expect(backup.records[0].covenantId).toBe(order.covenant_id);

    // wipe the records, then import the backup. The indexer goes away first: while it answers, My orders writes a record for every own live
    // order it proves (C5-03), which would restore the wiped record by itself; a backup is what brings the record back when nothing else knows it
    await appPage.route('**/v1/**', (r) => r.abort());
    await appPage.reload(); // no page that still reads the indexer is left to write the record back
    await appPage.evaluate((k) => localStorage.removeItem(k), storeKey);
    await appPage.reload();
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await expect(appPage.getByTestId('recover-count')).toContainText('0 placement');
    await appPage.getByTestId(TESTID.ordersImport).setInputFiles(path);
    await expect(appPage.getByTestId('import-preview')).toContainText('1 record(s) can be imported');
    await appPage.getByTestId('orders-import-apply').click();
    await expect(appPage.getByTestId('recover-message')).toContainText('Imported 1');
    await expect(appPage.getByTestId('recover-count')).toContainText('1 placement');

    // a file that is not a backup is refused with a reason
    await appPage.getByTestId(TESTID.ordersImport).setInputFiles({ name: 'x.json', mimeType: 'application/json', buffer: Buffer.from('{"hello":1}') });
    await expect(appPage.getByTestId('recover-message')).toContainText('not a KOB backup');
  });
});
