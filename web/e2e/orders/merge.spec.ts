// "Merge all" (My orders > Balances) through the UI and the mock stack, on a token of KOB's standard 3 / 3 program. One click plans the whole
// tree of transfers to the wallet itself (level 1 merges the original UTXOs in threes, later levels merge the outputs), and its confirmation
// states the transactions, the wallet approvals (one per transaction, one after the other) and the KAS freed. A rejected or failed step stops
// the run; what was sent stays merged and Merge all plans again from it. The mock wallets refuse an input their own node does not know
// (KasWare 0.11.5), and that node sees a new output only when it is pushed to it here (`walletNodeLagMs`), so a later level is signed only
// through the visibility gate (src/wallet/visibility.ts). The mock node runs every transaction through the script engine.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet } from '../helpers/env';
import { runFlow } from '../helpers/orders';

test.use({ autoOpen: false, walletOptions: { walletNodeLagMs: 600_000 } });

const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);

/** The example registry plus EXSTD, a listed token of the standard 3 / 3 program (EXKCC listed too). */
async function registryWithStd(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  Object.assign(exkcc, { verified: true });
  reg.tokens.push({ ...exkcc, ticker: 'EXSTD', name: 'Example standard token', covenant_id: STD, template_id: 'kcc20-ref-3x3', extension_commitment: EXT });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

const seedStd = (mock: MockClient) => mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3' }] });

/** Counts every appearance of the app's own final confirmation (a merge run's steps are confirmed up front, so it never shows for them). */
async function recordConfirmPopups(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as { __confirms: number };
    w.__confirms = 0;
    let shown = false;
    new MutationObserver(() => {
      const now = !!document.querySelector('[data-testid="confirm-order"]');
      if (now && !shown) w.__confirms++;
      shown = now;
    }).observe(document.body, { subtree: true, childList: true });
  });
}
const confirmPopups = (page: Page): Promise<number> => page.evaluate(() => (window as unknown as { __confirms: number }).__confirms);

const ownedOf = async (mock: MockClient, pubkey: string) =>
  ((await mock.get(`/v1/token-utxos?owner=${pubkey}&token=${STD}&spent=false&limit=200`)).items as { txid: string; index: number; amount: string; role: string }[]).filter((u) => u.role === 'owned');

/** The screenshots of this run (saved with the test's results). */
const shot = (page: Page, name: string) => page.screenshot({ path: test.info().outputPath(`${name}.png`) });

async function openBalances(page: Page, count: number): Promise<void> {
  await registryWithStd(page);
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveText(new RegExp(`^Merge all \\(${count}\\): about \\+[0-9.,]+ KAS$`), { timeout: 15_000 });
}

test('Merge all of 30 UTXOs: one confirmation states 15 transactions and 15 approvals, then a tree of 3 -> 1 merges into one UTXO', async ({ appPage: page, mock, wallet }) => {
  test.setTimeout(240_000);
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS, count: 30 });
  await mock.giveKas('alice', 20n * KAS);
  // an open sell order of the same token: its custody is the order's, never merged
  await mock.seed({ orders: [{ token: STD, side: 'ask', maker: 'alice', price: 2_700_000, amount: (3n * TOKEN).toString() }] });
  const [order] = await mock.ordersOf('alice');
  const custodyBefore = (await mock.order(order.covenant_id)).custody.utxo;
  expect(await ownedOf(mock, alice)).toHaveLength(30);

  await openBalances(page, 30);
  await recordConfirmPopups(page);
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  const dialog = page.getByTestId('merge-all-dialog');
  await expect(dialog).toBeVisible({ timeout: 30_000 });
  // thirty 2 KAS carriers, the final UTXO keeps 1 KAS: about 59 KAS less the fees come back
  await expect(page.getByTestId('merge-all-summary')).toHaveText(
    /^Merge your 30 EXSTD UTXOs into one: 15 transactions, one after the other, each approved in your wallet \(15 approvals\)\. About 58\.\d+ KAS of their carriers returns to your wallet \(after network fees of about 0\.\d+ KAS\)\. One UTXO remains\.$/,
  );
  expect(await wallet!.calls()).toEqual([]);
  await shot(page, 'merge-all-confirmation');
  // each approval takes the wallet a second, as a person does: the run can be seen mid-way
  await wallet!.configure({ delayMs: 1000 });
  await page.getByTestId('merge-all-confirm').click();
  await expect(dialog).toHaveCount(0);

  const flow = page.getByTestId('tx-flow');
  await expect(flow).toContainText('Merge EXSTD UTXOs');
  await expect(page.locator('[data-testid^="flow-step-"]')).toHaveCount(15);
  // mid-run: the first level is through, a later level is being built from its outputs
  await expect(page.getByTestId('flow-step-11')).toHaveAttribute('data-state', 'sending', { timeout: 120_000 });
  await shot(page, 'merge-all-progress');
  const placed = await runFlow(page, mock, wallet, { steps: 15, confirm: false });
  await expect(page.getByTestId('flow-done')).toContainText('15 transactions submitted');

  // one wallet approval per transaction and never the app's own confirmation in between
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(Array(15).fill('signed'));
  expect(await confirmPopups(page)).toBe(0);
  // each transaction was handed to the wallet's own node (KasWare pushTx): only that made a later level's inputs visible to it
  expect((await wallet!.pushes()).map((p) => p.txid)).toEqual(placed.map((p) => p.txid));
  const tokenIns = (p: (typeof placed)[number]) => (p.submission.spent ?? []).filter((x) => x.kind === 'token').map((x) => x.outpoint);
  // level 1 (the first ten) spends only the original UTXOs; a later one spends outputs of earlier merges
  const made = new Set(placed.map((p) => p.txid));
  placed.slice(0, 10).forEach((p) => expect(tokenIns(p).some((o) => made.has(o.split(':')[0]!))).toBe(false));
  expect(tokenIns(placed[10]!).filter((o) => made.has(o.split(':')[0]!))).toHaveLength(3);
  for (const p of placed) {
    expect(tokenIns(p).length).toBeLessThanOrEqual(3);
    expect(p.submission.token_outputs).toHaveLength(1);
    expect(p.submission.token_outputs[0]!.owner).toBe(alice);
    expect(p.submission.closed).toEqual([]);
  }

  const owned = await ownedOf(mock, alice);
  expect(owned).toHaveLength(1);
  expect(owned[0]!.amount).toBe((30n * TOKEN).toString());
  const after = await mock.order(order.covenant_id);
  expect(after.status).toBe('open');
  expect(after.custody.utxo.txid).toBe(custodyBefore.txid);
  expect(after.custody.utxo.spent).toBe(false);
  // one UTXO left: nothing to merge
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveCount(0, { timeout: 15_000 });
});

test('a step rejected mid-run stops the merge cleanly: what was sent stays merged, and Merge all plans the rest from there', async ({ appPage: page, mock, wallet }) => {
  test.setTimeout(180_000);
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 7; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
  await openBalances(page, 7);

  // every request answers 2 s late: the second one is rejected while it waits
  await wallet!.configure({ delayMs: 2000 });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('Merge your 7 EXSTD UTXOs into one: 3 transactions, one after the other, each approved in your wallet (3 approvals).', { timeout: 30_000 });
  await page.getByTestId('merge-all-confirm').click();
  await wallet!.waitForCalls(2, 60_000);
  await wallet!.configure({ approve: false, rejectMessage: 'User rejected the request' });
  await expect(page.getByTestId('flow-done')).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId('flow-done')).toHaveText('Done: 1 transaction submitted.');
  await expect(page.getByTestId('flow-step-0')).toHaveAttribute('data-state', 'submitted');
  await expect(page.getByTestId('flow-step-1')).toHaveAttribute('data-state', 'cancelled');
  await expect(page.getByTestId('flow-step-2')).toHaveAttribute('data-state', 'pending');
  await expect(page.getByTestId('tx-flow')).toContainText('1 transaction not started');
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed', 'rejected']);
  // the first merge went through (1 + 2 + 3 = 6), the rest is untouched: a valid state
  await expect.poll(async () => (await ownedOf(mock, alice)).map((u) => BigInt(u.amount)).sort((a, b) => (a < b ? -1 : 1)), { timeout: 15_000 }).toEqual(
    [4n, 5n, 6n, 6n, 7n].map((k) => k * TOKEN),
  );

  // run again: planned from the five left, two transactions into one
  await page.getByTestId('tx-flow-dismiss').click();
  await wallet!.configure({ approve: true, delayMs: 0 });
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveText(/^Merge all \(5\): /, { timeout: 15_000 });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('Merge your 5 EXSTD UTXOs into one: 2 transactions', { timeout: 30_000 });
  await page.getByTestId('merge-all-confirm').click();
  await runFlow(page, mock, wallet, { steps: 2, confirm: false });
  const owned = await ownedOf(mock, alice);
  expect(owned.map((u) => u.amount)).toEqual([(28n * TOKEN).toString()]);
});

test('closing the confirmation sends nothing', async ({ appPage: page, mock, wallet }) => {
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS, count: 5 });
  await openBalances(page, 5);
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-dialog')).toBeVisible({ timeout: 30_000 });
  await page.getByTestId('merge-all-cancel').click();
  await expect(page.getByTestId('merge-all-dialog')).toHaveCount(0);
  await expect(page.getByTestId('tx-flow')).toHaveCount(0);
  expect(await wallet!.calls()).toEqual([]);
  expect(await mock.submitted(false)).toHaveLength(0);
  expect(await ownedOf(mock, alice)).toHaveLength(5);
});

test('a token the registry does not list is mergeable too, under its node-verified program', async ({ appPage: page, mock, wallet }) => {
  // EXSTD is in the mock chain only (no registry route here): the wallet holds it as an open-list token
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 4; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
  await mock.giveKas('alice', 20n * KAS);
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  const merge = page.getByTestId(`orders-merge-token-${STD}`);
  await expect(merge).toHaveText(/^Merge all \(4\): about \+[0-9.,]+ KAS$/, { timeout: 15_000 });
  await merge.click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('UTXOs into one: 2 transactions', { timeout: 30_000 });
  await page.getByTestId('merge-all-confirm').click();
  const placed = await runFlow(page, mock, wallet, { steps: 2, confirm: false });
  // level 1 merges the three smallest (1 + 2 + 3), level 2 that output with the fourth
  expect(placed[0]!.submission.token_outputs[0]!.amount).toBe((6n * TOKEN).toString());
  expect(placed[1]!.submission.token_outputs[0]!.amount).toBe((10n * TOKEN).toString());
  expect((await ownedOf(mock, alice)).map((u) => BigInt(u.amount))).toEqual([10n * TOKEN]);
});
