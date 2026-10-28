// Smoke specs of "My orders": an order seeded for the wallet key shows up with its balances, cancel and amend start their flow at once (the wallet's own
// approval is the only confirmation), recovery export / import round trip.
import { readFileSync } from 'node:fs';
import { expect, test, TEST_KEYS, type MockClient } from '../fixtures';
import { TESTID, orderCancel, orderReplace, orderRow } from '../testids';
import { loadKobNode } from '../../src/kob/wasm.node';
import { listExkcc, openConnect } from '../helpers/env';
import { runFlow } from '../helpers/orders';

/** an ask of `tokens` whole EXKCC at `price` sompi per whole token (2_505_000 = 0.02505 EXKCC per KAS) */
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
    await expect(row.getByTestId('order-allin')).toHaveCount(0); // no tip: the all-in price would repeat the price
    await expect(row.getByTestId('order-amount')).toContainText('3 / 3');
    await expect(appPage.getByTestId(TESTID.ordersList)).toBeVisible();
    // tokens inside the sell order are shown (wallets do not show them)
    const bal = appPage.getByTestId(TESTID.balancesPanel).getByTestId(`balance-row-${tok.covenant_id}`);
    await expect(bal.getByTestId('balance-escrowed')).toHaveText(/^3/);
    // the token's name opens its market (the way to a first sell of a token received elsewhere)
    await expect(bal.getByTestId('balance-market')).toHaveAttribute('href', new RegExp(tok.covenant_id.toLowerCase()));
    await expect(appPage.getByTestId('orders-tab-active')).toContainText('(1)');
    await appPage.getByTestId('orders-tab-history').click();
    await expect(appPage.getByTestId('orders-empty')).toBeVisible();
  });

  test('the same token reads the same on the market list, the token page and My orders, with no verification wording (registry says verified, the mock indexer says unverified)', async ({ appPage, mock }) => {
    // EXKCC `verified` in a registry served by the spec; the mock executor serves no verification state at all. Token issuance is permissionless and
    // KOB makes no per-token judgement: the label is `TICKER (abcd…1234)` whatever either side says (the list row leads with the ticker and prints the
    // id under it, with its fingerprint beside it)
    await listExkcc(appPage);
    const { tok, order } = await seedAsk(mock);
    const short = `${tok.covenant_id.slice(0, 4)}…${tok.covenant_id.slice(-4)}`;
    await appPage.goto('/#/market');
    const listText = `EXKCC (${short})`;
    await expect(appPage.getByTestId(`token-link-${tok.covenant_id}`)).toHaveText('EXKCC');
    await expect(appPage.getByTestId(`token-hash-${tok.covenant_id}`)).toHaveText(`${tok.covenant_id.slice(0, 8)}…${tok.covenant_id.slice(-8)}`);
    await expect(appPage.getByTestId(`token-row-${tok.covenant_id}`)).not.toContainText(/unverified|(?<!Genesis not )verified|official|registry/i);
    await expect(appPage.getByTestId(`token-row-${tok.covenant_id}`).getByTestId('badge-unverified')).toHaveCount(0);
    await appPage.goto(`/#/market/${tok.covenant_id}`);
    await expect(appPage.getByTestId('token-header')).toContainText(listText);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await expect(appPage.getByTestId(orderRow(order.covenant_id)).getByTestId('order-token')).toHaveText(listText);
  });

  test('cancel: the click starts the flow and asks the wallet at once, the cancellation is accepted and the order is closed', async ({ appPage, mock, wallet }) => {
    const { order } = await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(orderCancel(order.covenant_id)).click();
    await expect(appPage.getByTestId('tx-flow')).toBeVisible();
    await expect(appPage.getByTestId('flow-plan-errors')).toHaveCount(0);
    await expect(appPage.getByTestId('orders-action-error')).toHaveCount(0);
    // no confirmation screen: the first wallet request is made by the click itself
    const [placed] = await runFlow(appPage, mock, wallet!, { steps: 1 });
    expect(placed!.submission.closed).toEqual([expect.objectContaining({ covenantId: order.covenant_id, status: 'cancelled' })]);
    expect(await wallet!.calls()).toHaveLength(1);
    const done = await mock.order(order.covenant_id);
    expect(done.status).toBe('cancelled');
    // the order moves from Active to History
    await expect(appPage.getByTestId(orderRow(order.covenant_id))).toHaveCount(0, { timeout: 15_000 });
    await appPage.getByTestId('orders-tab-history').click();
    await expect(appPage.getByTestId(orderRow(order.covenant_id))).toHaveAttribute('data-status', 'cancelled');
  });

  test('cancel-all asks for confirmation first; the confirmed flow asks the wallet at once', async ({ appPage, mock, wallet }) => {
    await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(TESTID.ordersCancelAll).click();
    await expect(appPage.getByTestId('cancel-all-dialog')).toContainText('1 order');
    expect(await wallet!.calls()).toHaveLength(0);
    await appPage.getByTestId('cancel-all-confirm').click();
    await expect(appPage.getByTestId('tx-flow')).toBeVisible();
    const placed = await runFlow(appPage, mock, wallet!);
    expect(placed.length).toBeGreaterThan(0);
  });

  test('amend: validates the form, previews the new price and starts the replacement', async ({ appPage, mock, wallet }) => {
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
    await runFlow(appPage, mock, wallet!);
  });

  test('amend end to end: the order is amended in place and rests at the new price under the same id', async ({ appPage, mock, wallet }) => {
    const { order } = await seedAsk(mock);
    await mock.giveKas('alice', 50n * 10n ** 8n);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(orderReplace(order.covenant_id)).click();
    await appPage.getByTestId('amend-price').fill('0.0251');
    await appPage.getByTestId('amend-review').click();
    await runFlow(appPage, mock, wallet!, { steps: 1 });
    // an ask keeping its amount is amended IN PLACE (AMEND record): the same order id continues with the new price, its custody never moved
    const active = await mock.until(() => mock.ordersOf('alice'), (o) => o.length === 1 && o[0].price === '2510000');
    expect(active[0].covenant_id).toBe(order.covenant_id); // 0.0251 KAS per token = 2_510_000 sompi per whole token
  });

  test('an in-place amend needs no KAS in the wallet: the order\'s own carrier pays the fee', async ({ appPage, mock, wallet }) => {
    const { order } = await seedAsk(mock);
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await appPage.goto('/#/orders');
    await appPage.getByTestId(orderReplace(order.covenant_id)).click();
    await appPage.getByTestId('amend-price').fill('0.0251');
    await appPage.getByTestId('amend-review').click();
    await expect(appPage.getByTestId('amend-error')).toHaveCount(0);
    await expect(appPage.getByTestId('tx-flow')).toBeVisible();
    await runFlow(appPage, mock, wallet!, { steps: 1 });
  });

  test('the page has no horizontal scroll on a phone', async ({ appPage, mock }) => {
    await seedAsk(mock);
    await appPage.setViewportSize({ width: 390, height: 800 });
    await openConnect(appPage, TESTID.walletConnectKasware);
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
