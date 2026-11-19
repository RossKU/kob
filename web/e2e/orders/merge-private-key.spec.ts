// "Merge all" and "Merge, then sell" with the private-key signer: ONE approval in the signer frame
// covers the whole planned merge (its own screen: the token, the UTXOs, at most N transactions, the
// most network fee, how long it lasts), then the frame signs each step without showing itself, as
// long as its own decoding finds the step to be exactly the next link of the plan it was shown
// (src/signer/session.ts). A step off the plan (here: a fee above the session's per-transaction cap
// after the fee estimate jumps mid-run) is shown for confirmation as usual and ends the session. A
// sale after the merges always gets its own confirmation. Extension wallets keep one approval per
// transaction (merge.spec.ts, unchanged).
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS, type MockClient } from '../fixtures';
import { KAS, TOKEN } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { cancelReceipt, confirmReceipt, fillFields, pickType, readReceipt, settlePlan, waitSubmitted } from '../helpers/ticket';

test.use({ walletId: null, autoOpen: false });

const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);
const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
const shot = (page: Page, name: string) => page.screenshot({ path: test.info().outputPath(`${name}.png`) });

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
const ownedOf = async (mock: MockClient, pubkey: string) =>
  ((await mock.get(`/v1/token-utxos?owner=${pubkey}&token=${STD}&spent=false&limit=200`)).items as { amount: string; role: string }[]).filter((u) => u.role === 'owned');

async function connectKey(page: Page): Promise<void> {
  await page.getByTestId('wallet-connect-local-key').click();
  await expect(page.getByTestId('signer-overlay')).toBeVisible();
  await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
  await signer(page).getByTestId('signer-connect').click();
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
}

/** Counts how often the signer frame is shown from now on (each prompt shows it). */
async function countFrameShows(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as { __shows: number };
    w.__shows = 0;
    const el = document.querySelector('[data-testid="signer-overlay"]')!;
    let shown = !el.hasAttribute('hidden');
    new MutationObserver(() => {
      const now = !el.hasAttribute('hidden');
      if (now && !shown) w.__shows++;
      shown = now;
    }).observe(el, { attributes: true, attributeFilter: ['hidden'] });
  });
}
const frameShows = (page: Page): Promise<number> => page.evaluate(() => (window as unknown as { __shows: number }).__shows);

async function openBalances(page: Page, count: number): Promise<void> {
  await registryWithStd(page);
  await page.goto('/#/portfolio');
  await connectKey(page);
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  // the button says Merge all; the count and the KAS it frees are its tooltip
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveAttribute('title', new RegExp(`^Merge your ${count} UTXOs of this token into one: about \\+[0-9.,]+ KAS back\\. `), { timeout: 15_000 });
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveText('Merge all');
}

/** The session screen of the frame: what it says, then Allow. */
async function allowSession(page: Page, txs: number, screenshot?: string): Promise<void> {
  await expect(page.getByTestId('signer-overlay')).toBeVisible({ timeout: 30_000 });
  const read = await readReceipt(page);
  expect(read.from).toBe('signer');
  // the mock registry is not the table the bundle pins: the frame names the token by its covenant
  // id, not by the app's ticker
  expect(read.title).toBe('Merge token UTXOs without asking each time');
  expect(read.lines['Transactions']).toBe(`at most ${txs}`);
  expect(read.lines['What']).toMatch(/^\d+ of your UTXOs into \d+, to yourself\. .+ in total, unchanged\.$/);
  expect(read.lines['Valid for']).toMatch(/^\d+ min( \d+ s)?, in this tab only$/);
  expect(read.fee).toMatch(/^at most [0-9.]+ KAS in total \(planned [0-9.]+ KAS\)$/);
  await expect(signer(page).getByTestId('signer-session-note')).toHaveText('May use your KAS to pay the fees, within the cap. Anything else is shown for confirmation as usual. Rejecting any request ends this.');
  await expect(signer(page).getByTestId('signer-approve')).toHaveText('Allow');
  if (screenshot) {
    await expect(signer(page).getByTestId('signer-approve')).toBeEnabled();
    await shot(page, screenshot);
  }
  await confirmReceipt(page);
}

test('Merge all of 30 with the private key: one Allow in the signer, then 15 transactions signed without asking again', async ({ appPage: page, mock }) => {
  test.setTimeout(240_000);
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS, count: 30 });
  await mock.giveKas('alice', 20n * KAS);
  // an open sell order of the same token: its custody is the order's, never merged
  await mock.seed({ orders: [{ token: STD, side: 'ask', maker: 'alice', price: 2_700_000, amount: (3n * TOKEN).toString() }] });
  const [order] = await mock.ordersOf('alice');
  const custodyBefore = (await mock.order(order.covenant_id)).custody.utxo;

  await openBalances(page, 30);
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-dialog')).toBeVisible({ timeout: 30_000 });
  // the app's confirmation says the run takes one approval
  await expect(page.getByTestId('merge-all-summary')).toHaveText(/^Merge 30 EXSTD UTXOs into 1\. 15 transactions · 1 approval · about \+58\.\d+ KAS after fees\.$/);
  await countFrameShows(page);
  await page.getByTestId('merge-all-confirm').click();
  await expect(page.getByTestId('flow-session-ask')).toBeVisible();
  // the one approval is the stepper's first step
  await expect(page.getByTestId('flow-session-step')).toHaveAttribute('aria-current', 'step');
  await expect(page.getByTestId('flow-step-0')).toHaveAttribute('data-state', 'sending');
  await allowSession(page, 15, 'pk-session-approval');
  await expect(page.getByTestId('flow-session-step')).toHaveAttribute('data-state', 'open');
  await expect(page.getByTestId('flow-session')).toContainText('Approved once in the private key signer: at most 15 transactions');

  // mid-run: the frame stays hidden while the steps go through
  await expect(page.getByTestId('flow-step-6')).toHaveAttribute('data-state', /^(sending|preparing|submitted)$/, { timeout: 120_000 });
  await expect(page.getByTestId('signer-overlay')).toBeHidden();
  await shot(page, 'pk-session-running');
  const placed = await runFlow(page, mock, null, { steps: 15, confirm: false });
  await expect(page.getByTestId('flow-done')).toContainText('15 transactions submitted');
  await shot(page, 'pk-session-done');
  // the frame was shown once: for the session screen
  expect(await frameShows(page)).toBe(1);
  for (const p of placed) {
    expect(p.submission.token_outputs).toHaveLength(1);
    expect(p.submission.token_outputs[0]!.owner).toBe(alice);
  }
  const owned = await ownedOf(mock, alice);
  expect(owned).toHaveLength(1);
  expect(owned[0]!.amount).toBe((30n * TOKEN).toString());
  const after = await mock.order(order.covenant_id);
  expect(after.status).toBe('open');
  expect(after.custody.utxo.txid).toBe(custodyBefore.txid);
});

test('a step off the allowed plan (a fee above its cap after the estimate jumps) is shown for confirmation; Reject stops the run', async ({ appPage: page, mock }) => {
  test.setTimeout(240_000);
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS, count: 30 });
  await mock.giveKas('alice', 20n * KAS);
  await openBalances(page, 30);
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-dialog')).toBeVisible({ timeout: 30_000 });
  // the node's fee rate jumps tenfold after the plan was made (the app reads the estimate again
  // after its refresh time): the first step is the planned one, the next is rebuilt at the new
  // rate, above the per-transaction cap the frame set from the plan
  await mock.feeEstimate({ priority: 1000, normal: 1000, low: 1000 });
  await page.waitForTimeout(11_000);
  await page.getByTestId('merge-all-confirm').click();
  await allowSession(page, 15);
  await expect(page.getByTestId('flow-step-0')).toHaveAttribute('data-state', 'submitted', { timeout: 60_000 });
  await expect(page.getByTestId('signer-overlay')).toBeVisible({ timeout: 60_000 });
  const read = await readReceipt(page);
  expect(read.title).toBe('Merge token UTXOs');
  await expect(signer(page).getByTestId('signer-approve')).toHaveText('Approve');
  const before = (await mock.submitted()).length;
  expect(before).toBeGreaterThanOrEqual(1);
  expect(before).toBeLessThan(15);
  await cancelReceipt(page);
  await expect(page.getByTestId('flow-result')).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('[data-testid^="flow-step-"][data-state="cancelled"]')).toHaveCount(1);
  // the card no longer says the run is approved once: the session ended at the step off the plan
  await expect(page.getByTestId('flow-session')).toHaveText('The private key signer session ended: later steps were confirmed one by one.');
  expect((await mock.submitted()).length).toBe(before);
  // what was sent stays merged
  expect((await ownedOf(mock, alice)).length).toBeLessThan(30);
});

test('Merge, then sell with the private key: one Allow for the merges, then the sale\'s own confirmation in the frame', async ({ appPage: page, mock }) => {
  test.setTimeout(180_000);
  await seedStd(mock);
  await mock.giveTokens('alice', TOKEN, { token: STD, carrier: KAS, count: 9 });
  await mock.giveKas('alice', 50n * KAS);
  await registryWithStd(page);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`/#/market/${STD}`);
  await connectKey(page);
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '7', price: '1' });
  await settlePlan(page);
  await expect(page.getByTestId('order-note-merge')).toHaveText('Your EXSTD is split into many UTXOs: 2 merge transactions first, one approval in the private key signer.', { timeout: 30_000 });
  await countFrameShows(page);
  await page.getByTestId('order-place').click();
  // the stepper: Approve, the two merges, then Sell
  await expect(page.getByTestId('flow-progress').getByRole('listitem')).toHaveCount(4);
  await expect(page.getByTestId('flow-then-step')).toHaveAttribute('data-state', 'waiting');
  await allowSession(page, 2);
  // the sale: the frame's ordinary confirmation, Approve
  await expect(page.getByTestId('signer-overlay')).toBeVisible({ timeout: 90_000 });
  const sale = await readReceipt(page);
  expect(sale.title).toBe('Place order');
  await expect(signer(page).getByTestId('signer-approve')).toHaveText('Approve');
  await confirmReceipt(page);
  const txid = await waitSubmitted(page);
  // two prompts in all: the session screen and the sale
  expect(await frameShows(page)).toBe(2);
  const subs = await mock.submitted();
  expect(subs).toHaveLength(3);
  expect(subs[2]!.txid).toBe(txid);
});
