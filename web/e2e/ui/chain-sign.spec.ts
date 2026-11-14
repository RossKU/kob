// Chain signing with a wallet that looks every input up on its OWN node (KasWare 0.11.5; the mock wallets do so by default, src/testing/mock-wallets.ts):
//   * the old creation flow (the genesis signed before the preparation was sent) is refused by such a wallet: its input is on no node;
//   * the new flow signs and SENDS the preparation, hands it to the wallet's node (pushTx), then asks for the genesis: it passes;
//   * a declined second approval leaves the preparation on chain and a record: the Create token page finishes the creation with one more
//     approval, also after a reload.
// Screenshots (the founder's review of the flow) go to `KOB_CHAINSIGN_SHOTS`, else a temporary folder.
import { mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { clickConnectOption } from '../helpers/env';

const KAS = 100_000_000n;
const SHOTS = process.env.KOB_CHAINSIGN_SHOTS ?? join(tmpdir(), 'kob-chainsign-shots');
mkdirSync(SHOTS, { recursive: true });
/** a screenshot with `focus` (a test id) in view */
const shot = async (page: Page, name: string, focus?: string) => {
  if (focus) await page.getByTestId(focus).first().scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(SHOTS, `${name}.png`), fullPage: false });
};

test.use({ autoOpen: false });

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
    await expect(page.getByTestId('issue-intro')).toContainText('your wallet asks twice');
    await page.getByTestId('issue-review').click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    const twice = page.getByTestId('confirm-info').locator('[data-code="walletTwice"]');
    await expect(twice).toContainText('The first approval records the token');
    await expect(twice).toContainText('you can finish the creation later from this page');
    await expect(twice).not.toContainText('nothing is sent until both are signed');
    await shot(page, '1-confirm', 'confirm-info');

    // the old flow: the genesis (the transaction this screen copies) asked for before the preparation is on any node
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

    // the new flow
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-chain-step')).toHaveAttribute('data-step', 'prep');
    await shot(page, '2-popup-step1', 'confirm-chain-step');
    await expect(page.getByTestId('confirm-chain-step')).toHaveAttribute('data-step', 'genesis', { timeout: 15_000 });
    // the second popup is open only once step 1 is on the node and was handed to the wallet's node
    const atSecond = await mock.submitted();
    expect(atSecond).toHaveLength(1);
    expect((await wallet!.pushes()).map((p) => p.txid)).toContain(atSecond[0]!.txid);
    await shot(page, '3-popup-step2', 'confirm-chain-step');
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await shot(page, '4-sent', 'tx-result');
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
    // the user declines the second popup
    await wallet!.waitForCalls(2, 20_000);
    await wallet!.configure({ approve: false });
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0, { timeout: 20_000 });
    await expect(page.getByTestId('issue-notice')).toContainText('Finish the creation at the top of this page');
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

    // a reload: the record decides
    await page.reload();
    await clickConnectOption(page, 'wallet-connect-kasware');
    await expect(page.getByTestId('issue-pending')).toBeVisible();
    await shot(page, '6-resume-after-reload', 'issue-pending');
    await wallet!.clearCalls();
    await page.getByTestId('issue-pending-send').click();
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
});

// TN10, KasWare 0.11.5: once a genesis window showed only Reject ("is not an unspent output on
// this network") although its preparation had been announced: the wallet's node was moving it from
// its mempool into its UTXO index. The gate makes that rare (KOB's node lists the output, then 2 s);
// when it happens anyway, nothing more is sent, and finishing a few seconds later succeeds.
test.describe('chain signing: the wallet node lags the move from mempool to UTXO index', () => {
  test.use({ walletOptions: { pushTxOutcome: 'lost', walletNodeLagMs: 6000, refusal: 'reject' } });

  test('the first ask is refused (Reject only), the page says why and offers to finish; finishing a moment later succeeds', async ({ appPage: page, mock, wallet }) => {
    await mock.giveKas('alice', 500n * KAS);
    await openCreate(page, 'LAGS');
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    // the genesis is asked for once KOB's node lists the preparation and the margin passed, announced or not
    await wallet!.waitForCalls(2, 20_000);
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0, { timeout: 20_000 });
    const calls = await wallet!.calls();
    expect(calls.map((c) => c.status)).toEqual(['signed', 'rejected']);
    expect(calls[1]!.at - calls[0]!.at).toBeGreaterThanOrEqual(2000);
    const notice = page.getByTestId('issue-notice');
    await expect(notice).toContainText('nothing more was sent');
    await expect(notice).toContainText('is not an unspent output on this network');
    await expect(notice).toContainText('wait a few seconds, then Finish the creation');
    await expect(page.getByTestId('issue-pending-send')).toHaveText('Finish the creation');
    await shot(page, '8-declined-fresh', 'issue-notice');
    expect(await mock.submitted()).toHaveLength(1);

    // a few seconds later the wallet's node has it: the same step, asked again, goes through
    await page.waitForTimeout(Math.max(0, 6500 - (Date.now() - calls[0]!.at)));
    await wallet!.clearCalls();
    await page.getByTestId('issue-pending-send').click();
    await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
    expect((await wallet!.calls()).map((c) => c.status)).toEqual(['signed']);
    const submitted = await mock.submitted();
    expect(submitted).toHaveLength(2);
    expect(submitted[1]!.tx.inputs[0].transactionId).toBe(submitted[0]!.txid);
  });
});
