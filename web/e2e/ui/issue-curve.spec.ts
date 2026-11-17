// Create token, "After creation": a sell curve created in the token's own genesis (KobCurveAsk, unbound), with a share kept in the wallet; and
// "Sell at one price", which hands the first sale to the order ticket. Mock KasWare (two approvals) and the private key (one approval).
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { clickConnectOption } from '../helpers/env';
import { confirmReceipt, readReceipt } from '../helpers/ticket';

const KAS = 100_000_000n;

test.use({ autoOpen: false });

async function fillToken(page: Page): Promise<void> {
  await page.getByTestId('issue-name').fill('Curve Coin');
  await page.getByTestId('issue-ticker').fill('CURV');
  await page.getByTestId('issue-supply').fill('1000000');
}

test.describe('Create token: after creation', () => {
  test('sells on a curve created with the token, keeping 10% in the wallet', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await clickConnectOption(page, 'wallet-connect-kasware');
    await fillToken(page);
    // the default keeps everything; the curve's defaults are all filled in
    await expect(page.getByTestId('issue-after-keep').locator('input')).toBeChecked();
    await page.getByTestId('issue-after-curve').click();
    await expect(page.getByTestId('sale-keep')).toHaveValue('0');
    await expect(page.getByTestId('sale-amount')).toHaveValue('1000000');
    await expect(page.getByTestId('sale-levels')).toHaveValue('100');
    await expect(page.getByTestId('sale-total')).toHaveValue('10000');
    await expect(page.getByTestId('sale-multiple')).toHaveValue('5');
    await expect(page.getByTestId('sale-split')).toHaveText('All 1,000,000 CURV go on sale.');
    await expect(page.getByTestId('sale-graph')).toHaveAttribute('data-steps', '100');
    // keep % and the amount for sale stay linked both ways
    await page.getByTestId('sale-keep').fill('10');
    await expect(page.getByTestId('sale-amount')).toHaveValue('900000');
    await expect(page.getByTestId('sale-split')).toHaveText('You keep 100,000 CURV; 900,000 CURV go on sale.');
    await page.getByTestId('sale-amount').fill('750000');
    await expect(page.getByTestId('sale-keep')).toHaveValue('25');
    await page.getByTestId('sale-keep').fill('10');
    // the numbers follow the amount: about 10,000 KAS if sold out, the last level 5x the first
    await expect(page.getByTestId('sale-realised')).toContainText(/^9,?99\d(\.\d+)? KAS$/);
    const first = Number((await page.getByTestId('sale-first').textContent())!.replace(/[^0-9.]/g, ''));
    const last = Number((await page.getByTestId('sale-last').textContent())!.replace(/[^0-9.]/g, ''));
    expect(last / first).toBeGreaterThan(4.9);
    expect(last / first).toBeLessThan(5.1);
    // a bad number blocks the review with its reason at the field
    await page.getByTestId('sale-levels').fill('0');
    await expect(page.getByTestId('issue-review')).toBeDisabled();
    await page.getByTestId('sale-levels').fill('100');

    await expect(page.getByTestId('issue-summary-sale')).toContainText('Sell 900,000 CURV on a curve: 100 levels');
    await page.getByTestId('issue-review').click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    await expect(page.getByTestId('confirm-section-curve')).toBeVisible();
    await expect(page.getByTestId('confirm-blocking')).toHaveCount(0);
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('issue-result-curve')).toContainText('Your curve order is on the book: 900,000 CURV');

    // the genesis: the custody (owned by the curve), the kept share, the curve; two covenant groups
    const submitted = await mock.submitted();
    const genesis = submitted[submitted.length - 1]!;
    const prep = submitted[submitted.length - 2]!;
    expect(genesis.tx.inputs.map((i: any) => [i.transactionId, i.index])).toEqual([[prep.txid, 0]]);
    const ids = genesis.tx.outputs.map((o: any) => o?.covenant?.covenantId ?? null);
    expect(ids).toHaveLength(3);
    const tokenId = await page.getByTestId('issue-result-tokenId').getAttribute('data-value');
    expect(ids.slice(0, 2)).toEqual([tokenId, tokenId]);
    expect(ids[2]).toMatch(/^[0-9a-f]{64}$/);
    expect(ids[2]).not.toBe(tokenId);
    // a KOB1 payload: the curve's ORDER record with the token states
    expect(genesis.tx.payload).toMatch(/^4b4f4231/);
    // the kept share is the wallet's (tracked), the custody is not
    const tracked = await page.evaluate((pk) => (window as any).__kob.services.tracker.list(pk), TEST_KEYS.alice.pubkey);
    expect(tracked.map((x: any) => [x.index, x.state.amount])).toEqual([[1, (100_000n * 100_000_000n).toString()]]);
    // the indexer lists the curve at once: unbound, level 0 of 100, the custody's token
    const listed = (await mock.ordersOf('alice')) as any;
    const curve = (listed.orders ?? listed).find((o: any) => o.covenant_id === ids[2]);
    expect(curve).toMatchObject({ contract: 'KobCurveAsk', token: tokenId, curve: { level: 0, levels: 100, unbound: true } });
  });

  test('sells at one price: the token keeps a share, and the result opens the ticket on the sale', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await clickConnectOption(page, 'wallet-connect-kasware');
    await fillToken(page);
    await page.getByTestId('issue-after-ask').click();
    await page.getByTestId('sale-keep').fill('25');
    // the price field shows the default (10,000 KAS over the amount); it can be typed
    await expect(page.getByTestId('sale-price')).toHaveAttribute('placeholder', /0\.01333333/);
    await page.getByTestId('sale-price').fill('0.02');
    await expect(page.getByTestId('sale-total-ask')).toContainText('15000 KAS');
    await expect(page.getByTestId('issue-summary-sale')).toContainText('Sell 750,000 CURV at 0.02 KAS each');
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();
    const place = page.getByTestId('issue-result-ask-place');
    await expect(place).toBeVisible();
    const tokenId = await page.getByTestId('issue-result-tokenId').getAttribute('data-value');
    await expect(place).toHaveAttribute('href', `#/market/${tokenId}?ticket=ask&amount=${750_000n * 100_000_000n}&price=2000000`);
    const genesis = (await mock.submitted()).at(-1)!;
    expect(genesis.tx.outputs.filter((o: any) => o?.covenant).length).toBe(2);
  });
});

test.describe('Create token with the private key: after creation', () => {
  test.use({ walletId: null });

  test('one approval in the signer frame: the frame names the curve and the share on sale', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await page.getByTestId('wallet-connect-local-key').click();
    const f = page.frameLocator('[data-testid="signer-frame"]');
    await f.getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await f.getByTestId('signer-connect').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
    await fillToken(page);
    await page.getByTestId('issue-after-curve').click();
    await page.getByTestId('sale-keep').fill('10');
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    const receipt = await readReceipt(page);
    expect(receipt.title).toBe('Create token');
    expect(receipt.lines['Sell on a curve']).toMatch(/^100 levels, /);
    expect(receipt.lines['On your curve sell order']).toMatch(/^900000 CURV$/);
    await confirmReceipt(page);
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('issue-result-curve')).toBeVisible();
  });
});
