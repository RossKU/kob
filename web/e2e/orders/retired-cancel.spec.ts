// An order under a RETIRED template of the cancel-only table (an older contract version, docs/spec/template-retirement.md): the wallet's record of
// a bid placed by an older build under `KobBid` b995661f. My orders resolves it on the node under that template's own script and shows its terms
// with the "older contract version — cancel only" badge; Cancel all counts it; Cancel builds its retired cancel, which the private-key signer
// decodes as "Cancel an order under an older contract version (cancel only)" with what returns to the wallet; the node accepts it (engine-validated).
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { fund, gotoNav, openMarket } from '../helpers/env';
import { confirmReceipt, readReceipt } from '../helpers/ticket';

test.use({ walletId: null, autoOpen: false });

const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');

async function connectKey(page: Page): Promise<void> {
  await page.getByTestId('wallet-connect-local-key').click();
  await expect(page.getByTestId('signer-overlay')).toBeVisible();
  await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
  await signer(page).getByTestId('signer-connect').click();
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
}

test('an order under an older contract version: shown cancel-only, counted by Cancel all, cancelled through its own template', async ({ appPage: page, mock }) => {
  await fund(mock);
  const seeded = await mock.post<{ covenant_id: string; txid: string; value: string; record: Record<string, unknown> }>('/mock/retired-order', { maker: 'alice' });
  const id = seeded.covenant_id;
  // the record the older build's wallet kept (localStorage of this origin)
  await page.addInitScript(([k, v]) => {
    try {
      if (!localStorage.getItem(k)) localStorage.setItem(k, v);
    } catch {
      /* no storage */
    }
  }, [`kob.records.v1:testnet-10:${TEST_KEYS.alice.pubkey}`, JSON.stringify({ [id]: seeded.record })] as const);
  await openMarket(page, mock, { connect: false });
  await connectKey(page);
  await gotoNav(page, 'nav-orders');

  const row = page.getByTestId(`order-row-${id}`);
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(row.getByTestId('order-cancel-only')).toHaveText('older contract version — cancel only');
  await expect(row.getByTestId('order-old-template')).toHaveCount(0);
  // its terms decode from the retired template's state (today's layout): a buy limit, live on the node
  await expect(row).toHaveAttribute('data-live', '1', { timeout: 30_000 });
  await expect(row).toHaveAttribute('data-side', 'buy');
  await expect(row.getByTestId(`order-type-${id}`)).not.toHaveText(/unknown/i);
  await expect(row.getByTestId('order-price')).toBeVisible();
  // a working Cancel, and Cancel all counts it; nothing else is offered
  await expect(page.getByTestId(`order-cancel-${id}`)).toBeEnabled();
  await expect(page.getByTestId('orders-cancel-all')).toContainText('1');
  await expect(page.getByTestId(`order-replace-${id}`)).toHaveCount(0);
  await expect(page.getByTestId(`order-sweep-${id}`)).toHaveCount(0);

  await page.getByTestId(`order-cancel-${id}`).click();
  const shown = await readReceipt(page);
  expect(shown.from).toBe('signer');
  expect(shown.title).toBe('Cancel an order under an older contract version (cancel only)');
  expect(shown.labels).toContain('Closes order (older contract version)');
  expect(shown.orders).toEqual([]);
  expect(shown.problems).toBe(false);
  await confirmReceipt(page);

  // the node accepted it (the mock validates every submission in the engine): it spends the retired order's UTXO, back to alice
  await expect.poll(async () => (await mock.submitted()).some((s) => (s.spent ?? []).some((x) => x.outpoint === `${seeded.txid}:0`)), { timeout: 30_000 }).toBe(true);
  const sub = (await mock.submitted()).find((s) => (s.spent ?? []).some((x) => x.outpoint === `${seeded.txid}:0`))!;
  expect(sub.created).toEqual([]);
  expect(sub.tx.inputs[0].utxo.covenantId).toBe(id);
  expect(sub.tx.outputs.some((o: { scriptPublicKey: string }) => o.scriptPublicKey === `000020${TEST_KEYS.alice.pubkey}ac`)).toBe(true);
});
