// "Merge tokens" (My orders > Balances) through the UI and the mock stack, on a token of KOB's standard 3 / 3 program: seven plain UTXOs of the
// wallet become one in a chain of three transfers to itself (3 -> 1, then 1 + 2 -> 1 twice), each signed in the wallet as soon
// as the previous one is on the node, the whole merge stated on the flow. The wallet's open sell order of the same token keeps its custody. The mock node runs every transaction through the
// script engine and accepts a link only once the previous one is on its UTXO set.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { expectStopped, expectWalletSigned, placedBy } from '../helpers/ticket';

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

/** Records every text `flow-shown-as` (the chain summary of the running merge step) takes from now on: each step shows it only while it runs. */
async function recordShownAs(page: Page): Promise<void> {
  await page.evaluate(() => {
    const seen: string[] = [];
    (window as unknown as { __shownAs: string[] }).__shownAs = seen;
    const read = () => {
      const text = document.querySelector('[data-testid="flow-shown-as"]')?.textContent?.trim();
      if (text && !seen.includes(text)) seen.push(text);
    };
    new MutationObserver(read).observe(document.body, { subtree: true, childList: true, characterData: true });
    read();
  });
}
const shownAs = (page: Page): Promise<string[]> => page.evaluate(() => (window as unknown as { __shownAs: string[] }).__shownAs);

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
  await recordShownAs(page);
  await merge.click();

  const flow = page.getByTestId('tx-flow');
  await expect(flow).toContainText('Merge EXSTD UTXOs');
  await expect(page.getByTestId('flow-progress').locator('li')).toHaveCount(3);
  const steps = await runFlow(page, mock, wallet, { steps: 3 });
  // each step stated the whole merge while it ran
  const shown = await shownAs(page);
  expect(shown).toHaveLength(3);
  shown.forEach((text, k) => {
    expect(text).toMatch(/Merge 7 EXSTD UTXOs into 1: 3 transaction\(s\), total fee about 0\.\d+ KAS\./);
    expect(text).toContain(`This is transaction ${k + 1} of 3.`);
  });
  for (const k of [0, 1, 2]) await expect(page.getByTestId(`flow-step-${k}`)).toHaveAttribute('data-state', 'submitted');
  steps.forEach((s) => {
    // at most 3 token inputs, ONE token output: the wallet's, holding everything merged so far (nothing goes to other keys)
    const tokenIns = (s.submission.spent ?? []).filter((x) => x.kind === 'token');
    expect(tokenIns).toHaveLength(3);
    expect(s.submission.token_outputs).toHaveLength(1);
    expect(s.submission.token_outputs[0]!.owner).toBe(alice);
    expect(s.submission.closed).toEqual([]);
  });
  // links 2 and 3 spend the previous link's output
  for (const k of [1, 2]) {
    const prev = steps[k - 1]!;
    const out = prev.submission.token_outputs[0]!.output;
    expect((steps[k]!.submission.spent ?? []).map((x) => x.outpoint)).toContain(`${prev.txid}:${out}`);
  }
  expect(steps[2]!.submission.token_outputs[0]!.amount).toBe((28n * TOKEN).toString());
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

test('a merge stopped after the first transaction leaves fewer UTXOs and nothing lost', async ({ appPage: page, mock, wallet }) => {
  await mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3', standing: 'official' }] });
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 5; k++) await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS });
  await registryWithStd(page);
  await page.goto('/#/orders');
  await connectWallet(page, 'kasware');
  await recordShownAs(page);
  // both requests wait a moment in the wallet (a popup not answered yet): the user rejects the second one while it is open
  await wallet!.configure({ delayMs: 1500 });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('flow-step-0')).toHaveAttribute('data-state', 'submitted', { timeout: 30_000 });
  await wallet!.configure({ approve: false, rejectMessage: 'User rejected the request' });
  // the flow ends with its first transaction submitted and the second request rejected
  await expect(page.getByTestId('flow-done')).toContainText('1 transaction(s) submitted', { timeout: 30_000 });
  const [first] = await mock.submitted();
  const shown = await shownAs(page);
  expect(shown[0]).toContain('Merge 5 EXSTD UTXOs into 1: 2 transaction(s)');
  expect(shown[1]).toContain('This is transaction 2 of 2.');
  await expectStopped(page, 'rejected', { testid: 'flow-status' });
  await expect(page.getByTestId('flow-step-1')).toHaveAttribute('data-state', 'cancelled');
  const calls = await wallet!.calls();
  expect(calls.map((c) => c.status)).toEqual(['signed', 'rejected']);
  await expectWalletSigned(wallet!, await placedBy(mock, first!.txid), { call: 0 });
  expect(await mock.submitted(false)).toHaveLength(1);
  expect(first!.token_outputs).toHaveLength(1);
  const owned = await ownedOf(mock, alice);
  expect(owned.map((u) => BigInt(u.amount)).sort()).toEqual([TOKEN, TOKEN, 3n * TOKEN]);
});
