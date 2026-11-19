// "Merge all" (My orders > Balances) and the sell ticket's "Merge, then sell", through the UI and
// the mock stack, on a token of KOB's standard 3 / 3 program. One click plans the whole tree of
// transfers to the wallet itself (level 1 merges the original UTXOs in threes, later levels merge
// the outputs), and its confirmation states the transactions, the wallet approvals (one per
// transaction, one after the other) and the KAS freed. A rejected or failed step stops the run;
// what was sent stays merged and Merge all plans again from it. The mock wallets refuse an input
// their own node does not know (KasWare 0.11.5), and that node sees a new output only when it is
// pushed to it here (`walletNodeLagMs`), so a later level is signed only through the visibility
// gate (src/wallet/visibility.ts). The mock node runs every transaction through the script engine.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { confirmReceipt, fillFields, pickType, settlePlan, waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false, walletOptions: { walletNodeLagMs: 600_000 } });

const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);

/**
 * The example registry plus EXSTD, a listed token of the standard 3 / 3 program (EXKCC listed too).
 */
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

/**
 * Counts every appearance of the app's own final confirmation (a merge run's steps are confirmed up
 * front, so it never shows for them).
 */
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

/** The wallet's own UTXOs of EXSTD as [amount, borrow scheme, borrow guard], smallest first. */
type BorrowState = { borrow_scheme: number; borrow_guard: string };
const ownedStates = async (mock: MockClient, pubkey: string) =>
  ((await mock.get(`/v1/token-utxos?owner=${pubkey}&token=${STD}&spent=false&limit=50`)).items as { amount: string; role: string; state: BorrowState }[])
    .filter((u) => u.role === 'owned')
    .map((u) => [BigInt(u.amount), u.state.borrow_scheme, u.state.borrow_guard] as const)
    .sort((a, b) => Number(a[0] - b[0]));
/** The one guard the UTXOs with a borrow setting share. */
const guardOf = (owned: Awaited<ReturnType<typeof ownedStates>>): string => {
  const guards = new Set(owned.filter((u) => u[1] !== 0).map((u) => u[2]));
  expect(guards.size).toBe(1);
  return [...guards][0]!;
};

/** The screenshots of this run (saved with the test's results). */
const shot = (page: Page, name: string) => page.screenshot({ path: test.info().outputPath(`${name}.png`) });

async function openBalances(page: Page, count: number): Promise<void> {
  await registryWithStd(page);
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  // the button says Merge all; the count and the KAS it frees are its tooltip
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveAttribute('title', new RegExp(`^Merge your ${count} UTXOs of this token into one: about \\+[0-9.,]+ KAS back\\. `), { timeout: 15_000 });
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveText('Merge all');
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
    /^Merge 30 EXSTD UTXOs into 1\. 15 transactions · 15 approvals · about \+58\.\d+ KAS after fees\.$/,
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
  // fifteen steps: a column, the running one current
  await expect(page.getByTestId('flow-progress')).toHaveClass(/stepper-column/);
  await expect(page.getByTestId('flow-step-11')).toHaveAttribute('aria-current', 'step');
  await shot(page, 'merge-all-progress');
  // the flow's card: at the top right just under the header, on top of any
  // toast, and never past the bottom of the window
  const [fb, hb, vp] = [await flow.boundingBox(), await page.locator('header.header').boundingBox(), page.viewportSize()!];
  expect(fb!.y - (hb!.y + hb!.height), 'just under the header').toBeLessThanOrEqual(12);
  expect(vp.width - (fb!.x + fb!.width), 'at the right edge').toBeLessThanOrEqual(16);
  expect(fb!.y + fb!.height, 'within the window').toBeLessThanOrEqual(vp.height);
  const placed = await runFlow(page, mock, wallet, { steps: 15, confirm: false });
  await expect(page.getByTestId('flow-done')).toContainText('15 transactions submitted');

  // one wallet approval per transaction and never the app's own confirmation in between
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(Array(15).fill('signed'));
  expect(await confirmPopups(page)).toBe(0);
  // each transaction was handed to the wallet's own node (KasWare pushTx): only that made a later
  // level's inputs visible to it
  expect((await wallet!.pushes()).map((p) => p.txid)).toEqual(placed.map((p) => p.txid));
  const tokenIns = (p: (typeof placed)[number]) => (p.submission.spent ?? []).filter((x) => x.kind === 'token').map((x) => x.outpoint);
  // level 1 (the first ten) spends only the original UTXOs; a later one spends outputs of earlier
  // merges
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

  // the notifications on: a payment while the flow runs goes to the bell only,
  // never on screen under the card
  await page.getByTestId('notify-bell').click();
  await page.getByTestId('notify-enable').click();
  await page.getByTestId('notify-panel').getByRole('button', { name: 'Close' }).click();
  // the first poll stores the baseline: a payment after it is a new one
  const pk = alice;
  await expect.poll(() => page.evaluate((k) => { try { return JSON.parse(localStorage.getItem(`kob.notify.testnet-10.${k}`) ?? 'null')?.snaps != null; } catch { return false; } }, pk), { timeout: 30_000 }).toBe(true);
  // every request answers 2 s late: the second one is rejected while it waits
  await wallet!.configure({ delayMs: 2000 });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('Merge 7 EXSTD UTXOs into 1. 3 transactions · 3 approvals', { timeout: 30_000 });
  await page.getByTestId('merge-all-confirm').click();
  await wallet!.waitForCalls(2, 60_000);
  await wallet!.configure({ approve: false, rejectMessage: 'User rejected the request' });
  await expect(page.getByTestId('flow-done')).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId('flow-done')).toHaveText('Done: 1 transaction submitted.');
  await expect(page.getByTestId('flow-step-0')).toHaveAttribute('data-state', 'submitted');
  await expect(page.getByTestId('flow-step-1')).toHaveAttribute('data-state', 'cancelled');
  await expect(page.getByTestId('flow-step-1')).toContainText('Declined');
  await expect(page.getByTestId('flow-step-2')).toHaveAttribute('data-state', 'pending');
  await expect(page.getByTestId('tx-flow')).toContainText('1 transaction not started');
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed', 'rejected']);
  // the stopped card stays on screen: a payment now goes to the bell only, never on screen under
  // the card
  await mock.giveKas('alice', 3n * KAS);
  await expect(page.getByTestId('notify-unread')).toHaveText('1', { timeout: 30_000 });
  await expect(page.locator('[data-testid="toast-host"] > .toast:not(.send-toast)')).toHaveCount(0);
  // the first merge went through (1 + 2 + 3 = 6), the rest is untouched: a valid state
  await expect.poll(async () => (await ownedOf(mock, alice)).map((u) => BigInt(u.amount)).sort((a, b) => (a < b ? -1 : 1)), { timeout: 15_000 }).toEqual(
    [4n, 5n, 6n, 6n, 7n].map((k) => k * TOKEN),
  );

  // nothing was queued behind the card: closing it replays no toast (the bell has the payment)
  await page.getByTestId('tx-flow-dismiss').click();
  await page.waitForTimeout(500);
  await expect(page.locator('[data-testid="toast-host"] > .toast:not(.send-toast)')).toHaveCount(0);
  await expect(page.getByTestId('notify-unread')).toHaveText('1');
  // run again: planned from the five left, two transactions into one
  await wallet!.configure({ approve: true, delayMs: 0 });
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveAttribute('title', /^Merge your 5 UTXOs /, { timeout: 15_000 });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('Merge 5 EXSTD UTXOs into 1. 2 transactions', { timeout: 30_000 });
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
  // EXSTD is in the mock chain only (no registry route here): the wallet holds it as an open-list
  // token
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 4; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
  await mock.giveKas('alice', 20n * KAS);
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  const merge = page.getByTestId(`orders-merge-token-${STD}`);
  await expect(merge).toHaveAttribute('title', /^Merge your 4 UTXOs of this token into one: about \+[0-9.,]+ KAS back\. /, { timeout: 15_000 });
  await expect(merge).toHaveText('Merge all');
  await merge.click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('UTXOs into 1. 2 transactions', { timeout: 30_000 });
  await page.getByTestId('merge-all-confirm').click();
  const placed = await runFlow(page, mock, wallet, { steps: 2, confirm: false });
  // level 1 merges the three smallest (1 + 2 + 3), level 2 that output with the fourth
  expect(placed[0]!.submission.token_outputs[0]!.amount).toBe((6n * TOKEN).toString());
  expect(placed[1]!.submission.token_outputs[0]!.amount).toBe((10n * TOKEN).toString());
  expect((await ownedOf(mock, alice)).map((u) => BigInt(u.amount))).toEqual([10n * TOKEN]);
});

test('Merge all merges each borrow setting apart: one UTXO per setting, each keeping its scheme and guard', async ({ appPage: page, mock, wallet }) => {
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  // three plain UTXOs and two of borrow scheme 2 (option A: spent by the owner key on the owner
  // path)
  for (let k = 1; k <= 3; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
  for (let k = 4; k <= 5; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS, borrow_scheme: 2 });
  await mock.giveKas('alice', 20n * KAS);
  const before = await ownedStates(mock, alice);
  await openBalances(page, 5);
  await expect(page.getByTestId('balances-panel')).toContainText('15');
  await page.screenshot({ path: test.info().outputPath('balances-borrow-merge.png'), animations: 'disabled' });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  const summary = page.getByTestId('merge-all-summary');
  await expect(summary).toContainText('Merge 5 EXSTD UTXOs into 2, one per borrow setting.', { timeout: 30_000 });
  await shot(page, 'merge-borrow-confirm');
  await page.getByTestId('merge-all-confirm').click();
  const placed = await runFlow(page, mock, wallet, { steps: 2, confirm: false });
  expect(placed.map((p) => p.submission.token_outputs[0]!.amount)).toEqual([(6n * TOKEN).toString(), (9n * TOKEN).toString()]);
  // the mock gives the scheme-2 UTXOs a real guard key (not zeros): the merged one carries that
  // same guard
  const guard = guardOf(before);
  expect(guard).not.toBe('00'.repeat(32));
  expect(await ownedStates(mock, alice)).toEqual([[6n * TOKEN, 0, '00'.repeat(32)], [9n * TOKEN, 2, guard]]);
});

test('the sell ticket merges just enough first, then places the sale with its own confirmation', async ({ appPage: page, mock, wallet }) => {
  test.setTimeout(180_000);
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  // nine UTXOs of 1 EXSTD: a sale of 7 needs more than the 3 a transaction takes
  await mock.giveTokens('alice', TOKEN, { token: STD, carrier: KAS, count: 9 });
  await mock.giveKas('alice', 50n * KAS);
  await registryWithStd(page);
  // tall enough for the whole ticket, its order button included, on the screenshot
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`/#/market/${STD}`);
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await recordConfirmPopups(page);
  await pickType(page, 'limit', 'sell');
  // a ladder's levels are planned apart: no merge path, the dead end and the disabled button stay
  // as they are
  await fillFields(page, { amount: '4', price: '1', 'ladder.on': true, 'ladder.levels': '2', 'ladder.pct': '2' });
  await settlePlan(page);
  await expect(page.getByTestId('order-issue-TOKEN_UTXOS_FRAGMENTED')).toHaveAttribute('data-severity', 'error', { timeout: 30_000 });
  await expect(page.getByTestId('order-note-merge')).toHaveCount(0);
  await expect(page.getByTestId('order-place')).toBeDisabled();
  await fillFields(page, { 'ladder.on': false, amount: '7' });
  await settlePlan(page);

  // a single sale: the order button itself merges first, and a neutral note above it states the
  // merges (the 7 largest into 3 UTXOs)
  await expect(page.getByTestId('order-note-merge')).toHaveText('Your EXSTD is split into many UTXOs: 2 merge transactions first, one wallet approval each.', { timeout: 30_000 });
  await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
  const button = page.getByTestId('order-place');
  await expect(button).toBeEnabled();
  await expect(button).toHaveText('Merge, then sell');
  await expect(button).toHaveAttribute('title', /^A sale takes at most 3 token UTXOs\. /);
  expect(await wallet!.calls()).toEqual([]);
  await shot(page, 'sell-ticket-merge-offer');
  await button.click();

  // the stepper: the two merges, then the sale, current once they went through
  await expect(page.getByTestId('flow-then-step')).toContainText('Sell');
  await expect(page.getByTestId('flow-then-step')).toHaveAttribute('aria-current', 'step', { timeout: 90_000 });
  await expect(page.getByTestId('flow-then-step')).toContainText('Confirm the sale');
  // the merges, each approved in the wallet, then the sale's own final confirmation
  await expect(page.getByTestId('confirm-order')).toBeVisible({ timeout: 90_000 });
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed', 'signed']);
  expect(await confirmPopups(page)).toBe(1);
  await confirmReceipt(page);
  const txid = await waitSubmitted(page);
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed', 'signed', 'signed']);

  const subs = await mock.submitted();
  expect(subs).toHaveLength(3);
  const [m1, m2, sale] = subs;
  expect(sale!.txid).toBe(txid);
  // two level-1 merges of three, the sale takes their outputs and the one UTXO left over
  for (const m of [m1!, m2!]) expect(m.token_outputs[0]!.amount).toBe((3n * TOKEN).toString());
  const saleIns = (sale!.spent ?? []).filter((x) => x.kind === 'token').map((x) => x.outpoint.split(':')[0]);
  expect(saleIns).toHaveLength(3);
  expect(saleIns).toEqual(expect.arrayContaining([m1!.txid, m2!.txid]));
  expect(sale!.created).toHaveLength(1);
  expect((await ownedOf(mock, alice)).length).toBeLessThanOrEqual(2);
});


test('Convert to plain: the UTXOs with a borrow setting become one plain UTXO; the plain ones stay as they are', async ({ appPage: page, mock, wallet }) => {
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  for (let k = 1; k <= 2; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
  // borrow scheme 2 with a guard key (the mock gives carol's): spent by the owner on the owner path
  for (let k = 4; k <= 5; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS, borrow_scheme: 2 });
  await mock.giveKas('alice', 20n * KAS);
  const before = await ownedStates(mock, alice);
  expect(before.filter((u) => u[1] === 2).every((u) => u[2] !== '00'.repeat(32))).toBe(true);
  await openBalances(page, 4);
  // the row keeps its one Merge all button (the Portfolio layout is unchanged): its dialog offers
  // the conversion
  await expect(page.getByTestId(`orders-convert-token-${STD}`)).toHaveCount(0);
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('Merge 4 EXSTD UTXOs into 2, one per borrow setting.', { timeout: 30_000 });
  await page.getByTestId('merge-all-convert').click();
  await expect(page.getByTestId('merge-all-dialog')).toContainText('Convert EXSTD to plain', { timeout: 30_000 });
  await expect(page.getByTestId('merge-all-summary')).toHaveText(/^Convert 2 EXSTD UTXOs with a borrow setting into 1 plain UTXO\. 1 transaction · 1 approval · about \+[0-9.]+ KAS after fees\.$/, { timeout: 30_000 });
  await shot(page, 'convert-confirm');
  await page.getByTestId('merge-all-confirm').click();
  const placed = await runFlow(page, mock, wallet, { steps: 1, confirm: false });
  expect(placed[0]!.submission.token_outputs).toHaveLength(1);
  expect(await ownedStates(mock, alice)).toEqual([[1n * TOKEN, 0, '00'.repeat(32)], [2n * TOKEN, 0, '00'.repeat(32)], [9n * TOKEN, 0, '00'.repeat(32)]]);
  // three plain UTXOs now: Merge all merges them in one tree, and offers no conversion
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveAttribute('title', /^Merge your 3 UTXOs /, { timeout: 30_000 });
  await page.getByTestId(`orders-merge-token-${STD}`).click();
  await expect(page.getByTestId('merge-all-summary')).toContainText('Merge 3 EXSTD UTXOs into 1.', { timeout: 30_000 });
  await expect(page.getByTestId('merge-all-convert')).toHaveCount(0);
});

test('Convert to plain with nothing to merge: a small button in the same cell converts the one UTXO with a borrow setting', async ({ appPage: page, mock, wallet }) => {
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  await mock.giveTokens('alice', 2n * TOKEN, { token: STD, carrier: 2n * KAS });
  await mock.giveTokens('alice', 3n * TOKEN, { token: STD, carrier: 2n * KAS, borrow_scheme: 1 });
  await mock.giveKas('alice', 20n * KAS);
  await registryWithStd(page);
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  const convert = page.getByTestId(`orders-convert-token-${STD}`);
  await expect(convert).toHaveText('Convert to plain', { timeout: 15_000 });
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveCount(0);
  await convert.click();
  await expect(page.getByTestId('merge-all-summary')).toHaveText(/^Convert 1 EXSTD UTXO with a borrow setting into a plain one\. 1 transaction · 1 approval · about \+[0-9.]+ KAS after fees\.$/, { timeout: 30_000 });
  await page.getByTestId('merge-all-confirm').click();
  await runFlow(page, mock, wallet, { steps: 1, confirm: false });
  expect(await ownedStates(mock, alice)).toEqual([[2n * TOKEN, 0, '00'.repeat(32)], [3n * TOKEN, 0, '00'.repeat(32)]]);
  // two plain UTXOs: Merge all takes the cell's place
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toBeVisible({ timeout: 30_000 });
  await expect(convert).toHaveCount(0);
});

test('the sell ticket: no one borrow setting holds the amount, so the order button converts first, then places the sale', async ({ appPage: page, mock, wallet }) => {
  test.setTimeout(180_000);
  await seedStd(mock);
  const alice = (await mock.balance('alice')).pubkey;
  // plain 5 and scheme 2 holding 7: a sale of 10 fits neither
  await mock.giveTokens('alice', 5n * TOKEN, { token: STD, carrier: KAS });
  await mock.giveTokens('alice', 7n * TOKEN, { token: STD, carrier: KAS, borrow_scheme: 2 });
  await mock.giveKas('alice', 50n * KAS);
  await registryWithStd(page);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`/#/market/${STD}`);
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await recordConfirmPopups(page);
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '10', price: '1' });
  await settlePlan(page);
  await expect(page.getByTestId('order-note-merge')).toHaveText('No one borrow setting of your EXSTD holds this amount: 1 transaction converts to plain first, one wallet approval.', { timeout: 30_000 });
  await expect(page.getByTestId('order-issues').locator('[data-severity="error"]')).toHaveCount(0);
  const button = page.getByTestId('order-place');
  await expect(button).toHaveText('Convert, then sell');
  await expect(button).toHaveAttribute('title', /^A sale spends UTXOs of one borrow setting only\. /);
  await shot(page, 'sell-ticket-convert-offer');
  await button.click();
  await expect(page.getByTestId('confirm-order')).toBeVisible({ timeout: 90_000 });
  expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed']);
  await confirmReceipt(page);
  const txid = await waitSubmitted(page);
  const [conv, sale] = await mock.submitted();
  expect(sale!.txid).toBe(txid);
  expect(conv!.token_outputs).toHaveLength(1);
  // the sale spends the plain 5 and the converted 7, both plain now
  const saleIns = (sale!.spent ?? []).filter((x) => x.kind === 'token').map((x) => x.outpoint.split(':')[0]);
  expect(saleIns).toHaveLength(2);
  expect(saleIns).toContain(conv!.txid);
  expect(await ownedStates(mock, alice)).toEqual([[2n * TOKEN, 0, '00'.repeat(32)]]);
});
