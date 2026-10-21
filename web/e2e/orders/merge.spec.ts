// "Merge tokens" (My orders > Balances) through the UI and the mock stack, on a token of KOB's standard 3 / 3 program. One click = ONE transfer
// to the wallet itself of its smallest up-to-3 plain UTXOs (a merge is manual and costs a fee: the user clicks again to merge more), its
// confirmation stating the KAS the merge frees and how many mergeable UTXOs remain: seven UTXOs become one in three clicks (3 -> 1, then the
// smallest three of the five left, then the last three). The wallet's open sell order of the same token keeps its custody. The mock node runs
// every transaction through the script engine.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { confirmUntil, expectStopped } from '../helpers/ticket';

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

/** Clicks Merge, runs its one transaction through the wallet and returns it with the summary its confirmation showed. */
async function mergeOnce(page: Page, mock: MockClient, wallet: Parameters<typeof runFlow>[2]) {
  const before = (await shownAs(page)).length;
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('tx-flow')).toContainText('Merge EXSTD UTXOs');
  // one transaction per click: no multi-step progress list
  await expect(page.getByTestId('flow-progress')).toHaveCount(0);
  const [placed] = await runFlow(page, mock, wallet, { steps: 1 });
  await expect(page.getByTestId('flow-done')).toContainText('1 transaction(s) submitted');
  const shown = (await shownAs(page)).slice(before);
  expect(shown).toHaveLength(1);
  return { placed: placed!, shown: shown[0]! };
}

test('merge seven UTXOs of a 3 / 3 token: one 3 -> 1 transaction per click, the KAS freed stated, the open order untouched', async ({ appPage: page, mock, wallet }) => {
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

  // click 1: the three smallest (1 + 2 + 3) into one; two of their three 2 KAS carriers come back, less the fee
  const first = await mergeOnce(page, mock, wallet);
  expect(first.shown).toMatch(
    /^Merge your 3 smallest EXSTD UTXOs into one in one transaction: 7 UTXOs become 5\. About 3\.9\d* KAS of their carriers returns to your wallet \(after the fee of about 0\.\d+ KAS\)\. 5 mergeable UTXOs remain: click Merge again to merge more\.$/,
  );
  const tokenIns = (p: typeof first.placed) => (p.submission.spent ?? []).filter((x) => x.kind === 'token');
  expect(tokenIns(first.placed)).toHaveLength(3);
  // ONE token output: the wallet's, holding the three merged amounts (nothing goes to other keys)
  expect(first.placed.submission.token_outputs).toHaveLength(1);
  expect(first.placed.submission.token_outputs[0]!.owner).toBe(alice);
  expect(first.placed.submission.token_outputs[0]!.amount).toBe((6n * TOKEN).toString());
  expect(first.placed.submission.closed).toEqual([]);
  expect(await ownedOf(mock, alice)).toHaveLength(5);
  // nothing merged by itself: the button now offers the five left
  await expect(merge).toHaveText('Merge (5)', { timeout: 15_000 });

  // click 2: the smallest three of 4, 5, 6, 6 (the first merge's output), 7: 4 + 5 + 6
  const second = await mergeOnce(page, mock, wallet);
  expect(second.shown).toContain('Merge your 3 smallest EXSTD UTXOs into one in one transaction: 5 UTXOs become 3.');
  expect(second.shown).toContain('3 mergeable UTXOs remain');
  expect(tokenIns(second.placed)).toHaveLength(3);
  expect(second.placed.submission.token_outputs[0]!.amount).toBe((15n * TOKEN).toString());
  await expect(merge).toHaveText('Merge (3)', { timeout: 15_000 });

  // click 3: the last three (6, 7 and the second merge's output, 15)
  const third = await mergeOnce(page, mock, wallet);
  expect(third.shown).toMatch(/^Merge your 3 smallest EXSTD UTXOs into one in one transaction: 3 UTXOs become 1\. .* One UTXO remains: nothing more to merge\.$/);
  expect(tokenIns(third.placed)).toHaveLength(3);
  const out2 = second.placed.submission.token_outputs[0]!.output;
  expect((third.placed.submission.spent ?? []).map((x) => x.outpoint)).toContain(`${second.placed.txid}:${out2}`);
  expect(third.placed.submission.token_outputs[0]!.amount).toBe((28n * TOKEN).toString());

  const owned = await ownedOf(mock, alice);
  expect(owned).toHaveLength(1);
  expect(owned[0]!.amount).toBe((28n * TOKEN).toString());
  const after = await mock.order(order.covenant_id);
  expect(after.status).toBe('open');
  expect(after.custody.utxo.txid).toBe(custodyBefore.txid);
  expect(after.custody.utxo.spent).toBe(false);
  // one UTXO left: nothing to merge
  await expect(merge).toHaveCount(0, { timeout: 15_000 });
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed', 'signed', 'signed']);
});

test('a merge the wallet rejects sends nothing: the UTXOs stay as they were', async ({ appPage: page, mock, wallet }) => {
  await mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3', standing: 'official' }] });
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 5; k++) await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS });
  await registryWithStd(page);
  await page.goto('/#/orders');
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveText('Merge (5)', { timeout: 15_000 });
  await recordShownAs(page);
  await wallet!.configure({ approve: false, rejectMessage: 'User rejected the request' });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await confirmUntil(page, page.getByTestId('flow-stopped'));
  await expectStopped(page, 'rejected', { testid: 'flow-status' });
  const shown = await shownAs(page);
  expect(shown[0]).toContain('Merge your 3 smallest EXSTD UTXOs into one in one transaction: 5 UTXOs become 3.');
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(['rejected']);
  expect(await mock.submitted(false)).toHaveLength(0);
  const owned = await ownedOf(mock, alice);
  expect(owned.map((u) => BigInt(u.amount))).toEqual([TOKEN, TOKEN, TOKEN, TOKEN, TOKEN]);
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveText('Merge (5)');
});
