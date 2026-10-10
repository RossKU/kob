// "Merge tokens" (My orders > Balances) through the UI and the mock stack, on a token of KOB's standard 3 / 3 program: seven plain UTXOs of the
// wallet become one in a chain of three transfers to itself (3 -> 1, then 1 + 2 -> 1 twice), each signed on its own confirmation screen that
// states the whole merge. The wallet's open sell order of the same token keeps its custody. The mock node runs every transaction through the
// script engine and accepts a link only once the previous one is on its UTXO set.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet } from '../helpers/env';
import { runFlow } from '../helpers/orders';

test.use({ autoOpen: false });

const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);

/** The example registry plus EXSTD, a listed token of the standard 3 / 3 program (EXKCC listed too). */
async function registryWithStd(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  Object.assign(exkcc, { status: 'listed', verified: true });
  reg.tokens.push({ ...exkcc, ticker: 'EXSTD', name: 'Example standard token', covenant_id: STD, template_id: 'kcc20-ref-3x3', extension_commitment: EXT });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

const ownedOf = async (mock: MockClient, pubkey: string) =>
  ((await mock.get(`/v1/token-utxos?owner=${pubkey}&token=${STD}&spent=false&limit=200`)).items as { txid: string; index: number; amount: string; role: string }[]).filter((u) => u.role === 'owned');

test('merge seven UTXOs of a 3 / 3 token into one: three chained transfers, the open order untouched', async ({ appPage: page, mock, wallet }) => {
  await mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3', standing: 'official' }] });
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 7; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
  await mock.giveKas('alice', 20n * KAS);
  // an open sell order of the same token: its custody is the order's, never merged
  await mock.seed({ orders: [{ token: STD, side: 'ask', maker: 'alice', price: 2_700_000, amount: (3n * TOKEN).toString() }] });
  const [order] = await mock.ordersOf('alice');
  const custodyBefore = (await mock.order(order.covenant_id)).custody.utxo;
  expect(await ownedOf(mock, alice)).toHaveLength(7);

  await registryWithStd(page);
  await page.goto('/#/orders');
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('orders-list')).toBeVisible();

  const merge = page.getByTestId(`orders-merge-token-${STD}`);
  await expect(merge).toHaveText('Merge (7)', { timeout: 15_000 });
  await merge.click();

  const flow = page.getByTestId('tx-flow');
  await expect(flow).toContainText('Merge EXSTD UTXOs');
  await expect(page.getByTestId('flow-progress').locator('li')).toHaveCount(3);
  const steps = await runFlow(page, mock, wallet, { steps: 3 });
  steps.forEach((s, k) => {
    expect(s.confirm.kind).toBe('send');
    expect(s.confirm.text).toMatch(/Merge 7 EXSTD UTXOs into 1: 3 transaction\(s\), total fee about 0\.\d+ KAS\./);
    expect(s.confirm.text).toContain(`This is transaction ${k + 1} of 3.`);
    expect(s.confirm.sections.others, 'nothing goes to other keys').toBeUndefined();
    // at most 3 token inputs, ONE token output: the wallet's, holding everything merged so far
    const tokenIns = (s.placed.submission.spent ?? []).filter((x) => x.kind === 'token');
    expect(tokenIns).toHaveLength(3);
    expect(s.placed.submission.token_outputs).toHaveLength(1);
    expect(s.placed.submission.token_outputs[0]!.owner).toBe(alice);
    expect(s.placed.submission.closed).toEqual([]);
  });
  // links 2 and 3 spend the previous link's output
  for (const k of [1, 2]) {
    const prev = steps[k - 1]!.placed;
    const out = prev.submission.token_outputs[0]!.output;
    expect((steps[k]!.placed.submission.spent ?? []).map((x) => x.outpoint)).toContain(`${prev.txid}:${out}`);
  }
  expect(steps[2]!.placed.submission.token_outputs[0]!.amount).toBe((28n * TOKEN).toString());
  await expect(page.getByTestId('flow-done')).toContainText('3 transaction(s) submitted');

  const owned = await ownedOf(mock, alice);
  expect(owned).toHaveLength(1);
  expect(owned[0]!.amount).toBe((28n * TOKEN).toString());
  const after = await mock.order(order.covenant_id);
  expect(after.status).toBe('open');
  expect(after.custody.utxo.txid).toBe(custodyBefore.txid);
  expect(after.custody.utxo.spent).toBe(false);
  // one UTXO left: nothing to merge
  await expect(merge).toHaveCount(0, { timeout: 15_000 });
});

test('a merge stopped after the first transaction leaves fewer UTXOs and nothing lost', async ({ appPage: page, mock }) => {
  await mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3', standing: 'official' }] });
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 5; k++) await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS });
  await registryWithStd(page);
  await page.goto('/#/orders');
  await connectWallet(page, 'kasware');
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId('confirm-shown-as')).toContainText('Merge 5 EXSTD UTXOs into 1: 2 transaction(s)');
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
  await expect(page.getByTestId('tx-result')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('confirm-close').click();
  // the second link is built from the node and shown; the user stops there
  await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId('confirm-shown-as')).toContainText('This is transaction 2 of 2.');
  await page.getByTestId('confirm-cancel').click();
  await expect(page.getByTestId('flow-done')).toContainText('1 transaction(s) submitted');
  const owned = await ownedOf(mock, alice);
  expect(owned.map((u) => BigInt(u.amount)).sort()).toEqual([TOKEN, TOKEN, 3n * TOKEN]);
});
