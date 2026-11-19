// Chain signing with a wallet that looks every input up on its own node (KasWare 0.11.5).
// The mock wallets do so by default (src/testing/mock-wallets.ts).
//
// - The old creation flow (the genesis signed before the preparation was sent) is refused by such a
//   wallet, because its input is on no node.
// - The new flow signs and sends the preparation, hands it to the wallet's node (pushTx), then asks
//   for the genesis. It passes.
// - A declined second approval leaves the preparation on chain and a record. The Create token page
//   finishes the creation with one more approval, also after a reload, on the confirmation screen.
//   The creation is planned again from the record and checked as a fresh one.
// - A tampered record (anything can write localStorage) is refused before the wallet is asked.
//
// Screenshots go to `KOB_CHAINSIGN_SHOTS`, else a temporary folder.
import { mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { clickConnectOption } from '../helpers/env';

const KAS = 100_000_000n;
const SHOTS = process.env.KOB_CHAINSIGN_SHOTS ?? join(tmpdir(), 'kob-chainsign-shots');
mkdirSync(SHOTS, { recursive: true });
/** A screenshot with `focus` (a test id) in view. */
const shot = async (page: Page, name: string, focus?: string) => {
  if (focus) await page.getByTestId(focus).first().scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(SHOTS, `${name}.png`), fullPage: false });
};

test.use({ autoOpen: false });

/**
 * "Finish the creation" opens the confirmation screen of the creation planned again: checked, then
 * signed with one approval.
 */
async function confirmResume(page: Page): Promise<void> {
  await expect(page.getByTestId('confirm-screen')).toBeVisible();
  await expect(page.getByTestId('confirm-blocking')).toHaveCount(0);
  await expect(page.getByTestId('confirm-wallet-line')).toHaveText(/Your wallet asks once, for the token itself/);
  await page.getByTestId('confirm-wallet-help').click();
  await expect(page.getByTestId('confirm-info').locator('[data-code="resumed"]')).toContainText('checked it like a new one');
  await expect(page.getByTestId('confirm-info').locator('[data-code="walletTwice"]')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await page.getByTestId('confirm-ack').check();
  await page.getByTestId('confirm-sign').click();
}

async function openCreate(page: Page, ticker: string): Promise<void> {
  await page.goto('/#/create');
  await clickConnectOption(page, 'wallet-connect-kasware');
  await expect(page.getByTestId('wallet-address')).toBeVisible();
  await page.getByTestId('issue-name').fill(`${ticker} Coin`);
  await page.getByTestId('issue-ticker').fill(ticker);
  await page.getByTestId('issue-supply').fill('1000');
  await expect(page.getByTestId('issue-summary')).toBeVisible();
}

test.describe('chain signing: a wallet that checks its inputs on its own node', () => {
  test.use({ walletOptions: { delayMs: 1200 } });

  test('the old flow is refused (the genesis input is on no node); the new flow sends step 1 first and passes', async ({ appPage: page, mock, wallet, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await mock.giveKas('alice', 500n * KAS);
    await openCreate(page, 'CHAIN');
    await page.getByTestId('issue-intro-help').click();
    await expect(page.getByTestId('issue-intro-long')).toContainText('your wallet asks twice');
    await page.keyboard.press('Escape');
    await page.getByTestId('issue-review').click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    await expect(page.getByTestId('confirm-wallet-line')).toHaveText(/Your wallet asks twice/);
    await page.getByTestId('confirm-wallet-help').click();
    const twice = page.getByTestId('confirm-info').locator('[data-code="walletTwice"]');
    await expect(twice).toContainText('The first approval records the token');
    await expect(twice).toContainText('you can finish the creation later from this page');
    await expect(twice).not.toContainText('nothing is sent until both are signed');
    await shot(page, '1-confirm', 'confirm-info');
    await page.keyboard.press('Escape');

    // The old flow: the genesis (the transaction this screen copies) asked for before the
    // preparation is on any node.
    await page.getByTestId('confirm-advanced').locator('summary').click();
    await page.getByTestId('confirm-copy-tx').click();
    const genesisJson = await page.evaluate(() => navigator.clipboard.readText());
    const old = await page.evaluate(async (txJsonString) => {
      try {
        await (window as any).kasware.signPskt({ txJsonString, options: { signInputs: [{ index: 0, sighashType: 1 }] } });
        return 'signed';
      } catch (e) {
        return String((e as Error).message);
      }
    }, genesisJson);
    expect(old).toMatch(/^signPskt: Input #0 \([0-9a-f]{64}:0\) is not an unspent output on this network\.$/);
    expect(await mock.submitted()).toHaveLength(0);
    await wallet!.clearCalls();
    await page.getByTestId('confirm-advanced').locator('summary').click();

    // The new flow.
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-chain-step')).toHaveAttribute('data-step', 'prep');
    // The stepper: Prepare is current, Create token waits.
    await expect(page.getByTestId('create-step-prep')).toHaveAttribute('aria-current', 'step');
    await expect(page.getByTestId('create-step-genesis')).toHaveAttribute('data-state', 'waiting');
    await shot(page, '2-popup-step1', 'confirm-chain-step');
    await expect(page.getByTestId('confirm-chain-step')).toHaveAttribute('data-step', 'genesis', { timeout: 15_000 });
    await expect(page.getByTestId('create-step-prep')).toHaveAttribute('data-state', 'done');
    await expect(page.getByTestId('create-step-genesis')).toHaveAttribute('aria-current', 'step');
    // The second popup is open only once step 1 is on the node and was handed to the wallet's node.
    const atSecond = await mock.submitted();
    expect(atSecond).toHaveLength(1);
    expect((await wallet!.pushes()).map((p) => p.txid)).toContain(atSecond[0]!.txid);
    await shot(page, '3-popup-step2', 'confirm-chain-step');
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await shot(page, '4-sent', 'tx-result');
    await expect(page.getByTestId('create-stepper').locator('[data-state="done"]')).toHaveCount(2);
    const calls = await wallet!.calls();
    expect(calls.map((c) => c.status)).toEqual(['signed', 'signed']);
    const submitted = await mock.submitted();
    expect(submitted.map((s) => s.txid)).toEqual([calls[0]!.tx.id, calls[1]!.tx.id]);
    expect(calls[1]!.tx.inputs[0].transactionId).toBe(submitted[0]!.txid);
    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('issue-result')).toBeVisible();
  });

  test('a declined second approval: step 1 stays on chain, the page offers to finish it, also after a reload; one more approval finishes it', async ({ appPage: page, mock, wallet }) => {
    await mock.giveKas('alice', 500n * KAS);
    await openCreate(page, 'DECL');
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    // The user declines the second popup.
    await wallet!.waitForCalls(2, 20_000);
    await wallet!.configure({ approve: false });
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0, { timeout: 20_000 });
    await expect(page.getByTestId('issue-notice')).toContainText('your wallet did not sign the token itself');
    await expect(page.getByTestId('issue-notice-finish')).toHaveText('Finish the creation');
    // No new creation is prepared beside the unfinished one.
    await expect(page.getByTestId('issue-plan-pending')).toHaveCount(0);
    await expect(page.getByTestId('issue-review')).toBeDisabled();
    await expect(page.getByTestId('issue-review-hint')).toHaveText('Finish or forget the unfinished creation first.');
    const pending = page.getByTestId('issue-pending');
    await expect(pending).toContainText('DECL');
    await expect(page.getByTestId('issue-pending-body')).toContainText('Finish the creation (your wallet asks once more)');
    await expect(page.getByTestId('issue-pending-window')).toContainText('within 72 hours');
    await expect(page.getByTestId('issue-pending-send')).toHaveText('Finish the creation');
    await shot(page, '5-resume-after-decline', 'issue-pending');
    expect(await mock.submitted()).toHaveLength(1);
    const record = await page.evaluate(() => JSON.parse(localStorage.getItem('kob.creations.v2') ?? '[]'));
    expect(record).toHaveLength(1);
    expect(record[0]).toMatchObject({ prepSent: true });
    expect(record[0].genesis).toBeUndefined();

    // A reload: the record decides.
    await page.reload();
    await clickConnectOption(page, 'wallet-connect-kasware');
    await expect(page.getByTestId('issue-pending')).toBeVisible();
    // The record says where it stopped: Prepare done, Create token left.
    const steps = page.getByTestId('issue-pending-stepper');
    await expect(steps.getByTestId('create-step-prep')).toHaveAttribute('data-state', 'done');
    await expect(steps.getByTestId('create-step-genesis')).toHaveAttribute('aria-current', 'step');
    await expect(steps.getByTestId('create-step-genesis')).toContainText('Not signed yet');
    await shot(page, '6-resume-after-reload', 'issue-pending');
    await wallet!.clearCalls();
    await page.getByTestId('issue-pending-send').click();
    // The same confirmation as a fresh creation, with the stepper once signing: Prepare done,
    // Create token current.
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    expect(await wallet!.calls()).toHaveLength(0);
    await confirmResume(page);
    await expect(page.getByTestId('create-step-prep')).toHaveAttribute('data-state', 'done');
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId('create-stepper').locator('[data-state="done"]')).toHaveCount(2);
    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
    await shot(page, '7-finished', 'issue-result');
    const calls = await wallet!.calls();
    expect(calls).toHaveLength(1);
    const submitted = await mock.submitted();
    expect(submitted).toHaveLength(2);
    expect(calls[0]!.tx.id).toBe(submitted[1]!.txid);
    expect(submitted[1]!.tx.inputs[0].transactionId).toBe(submitted[0]!.txid);
    expect(await page.evaluate(() => localStorage.getItem('kob.creations.v2'))).toBeNull();
  });

  test('a tampered record (its stored genesis pays another key) is refused before the wallet is asked; the real one still finishes', async ({ appPage: page, mock, wallet }) => {
    await mock.giveKas('alice', 500n * KAS);
    await openCreate(page, 'TAMP');
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await wallet!.waitForCalls(2, 20_000);
    await wallet!.configure({ approve: false });
    await expect(page.getByTestId('issue-pending')).toBeVisible({ timeout: 20_000 });
    const real = await page.evaluate(() => localStorage.getItem('kob.creations.v2'));
    // Someone who can write this origin's storage swaps the genesis: the coin of the first step to
    // their key, the ids left as they were.
    await page.evaluate(() => {
      const recs = JSON.parse(localStorage.getItem('kob.creations.v2')!);
      recs[0].genesisBuilt.tx.outputs = [{ value: recs[0].prep.outputs[0].value, scriptPublicKey: '000020' + '77'.repeat(32) + 'ac', covenant: null }];
      localStorage.setItem('kob.creations.v2', JSON.stringify(recs));
    });
    await page.reload();
    await clickConnectOption(page, 'wallet-connect-kasware');
    await expect(page.getByTestId('issue-pending')).toBeVisible();
    await wallet!.clearCalls();
    await page.getByTestId('issue-pending-send').click();
    await expect(page.getByTestId('issue-pending-invalid')).toContainText('does not match what its first step recorded on chain');
    await expect(page.getByTestId('issue-pending-error')).toContainText('This saved creation is not valid');
    // Only forgetting is left.
    await expect(page.getByTestId('issue-pending-send')).toHaveCount(0);
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    expect(await wallet!.calls()).toHaveLength(0);
    expect(await mock.submitted()).toHaveLength(1);
    await shot(page, '9-tampered', 'issue-pending');

    // The record as the page wrote it: planned again, confirmed, signed.
    await page.evaluate((r) => localStorage.setItem('kob.creations.v2', r!), real);
    await page.reload();
    await clickConnectOption(page, 'wallet-connect-kasware');
    await page.getByTestId('issue-pending-send').click();
    await confirmResume(page);
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed']);
    expect(await mock.submitted()).toHaveLength(2);
  });
});

// A wallet's node may still be moving the preparation from its mempool into its UTXO index when the
// genesis is asked for.
// The wallet then shows only Reject ("is not an unspent output on this network").
// The gate makes this rare: KOB's node lists the output, then 2 s pass.
// When it happens anyway, nothing more is sent, and finishing a few seconds later succeeds.
test.describe('chain signing: the wallet node lags the move from mempool to UTXO index', () => {
  test.use({ walletOptions: { pushTxOutcome: 'lost', walletNodeLagMs: 6000, refusal: 'reject' } });

  test('the first ask is refused (Reject only), the page says why and offers to finish; finishing a moment later succeeds', async ({ appPage: page, mock, wallet }) => {
    await mock.giveKas('alice', 500n * KAS);
    await openCreate(page, 'LAGS');
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    // The genesis is asked for once KOB's node lists the preparation and the margin passed,
    // announced or not.
    await wallet!.waitForCalls(2, 20_000);
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0, { timeout: 20_000 });
    const calls = await wallet!.calls();
    expect(calls.map((c) => c.status)).toEqual(['signed', 'rejected']);
    expect(calls[1]!.at - calls[0]!.at).toBeGreaterThanOrEqual(2000);
    const notice = page.getByTestId('issue-notice');
    await expect(notice).toContainText('nothing more was sent');
    await expect(notice).toContainText('is not an unspent output on this network');
    await expect(notice).toContainText('wait a few seconds, then finish the creation.');
    await expect(notice).not.toContainText('at the top of this page');
    await expect(page.getByTestId('issue-notice-finish')).toHaveText('Finish the creation');
    await expect(page.getByTestId('issue-pending-send')).toHaveText('Finish the creation');
    await expect(page.getByTestId('issue-plan-pending')).toHaveCount(0);
    await expect(page.getByTestId('issue-review')).toBeDisabled();
    await shot(page, '8-declined-fresh', 'issue-notice');
    expect(await mock.submitted()).toHaveLength(1);

    // A few seconds later the wallet's node has it: the same step, asked again, goes through.
    await page.waitForTimeout(Math.max(0, 6500 - (Date.now() - calls[0]!.at)));
    await wallet!.clearCalls();
    // The notice's own button finishes it, as the banner's does.
    await page.getByTestId('issue-notice-finish').click();
    await confirmResume(page);
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
    expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed']);
    const submitted = await mock.submitted();
    expect(submitted).toHaveLength(2);
    expect(submitted[1]!.tx.inputs[0].transactionId).toBe(submitted[0]!.txid);
  });
});
