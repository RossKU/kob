// The signing paths of every wallet the app supports (KasWare is the default of the other specs), and every way signing can end without an
// accepted order: a wallet that declines, a wallet that signs the wrong thing, a network mismatch, a node that refuses the transaction.
// Wallet popups are blind to token semantics, so the failure modes are what protects the user: none of them may broadcast anything.
import { test, expect } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { runFlow, seedOrders } from '../helpers/orders';
import { acknowledgeAndSign, expectWalletSigned, fillFields, openReview, pickType, placedBy, reviewAndSign, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

async function sellTicket(page: import('@playwright/test').Page, reviewable = true): Promise<void> {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  if (reviewable) await waitReviewable(page);
}

test.describe('Kaspire', () => {
  test.use({ walletId: 'kaspire' });

  test('a placement and its cancel are signed through Kaspire with per-input scripts and accepted', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock, { wallet: 'kaspire' });
    await expect(page.getByTestId('wallet-connected')).toContainText('Kaspire');
    await sellTicket(page);
    const { confirm, placed } = await reviewAndSign(page, mock);
    expect(confirm.wallet.join(' ')).toMatch(/Kaspire/);
    await expectWalletSigned(wallet!, placed);
    const [call] = await wallet!.calls();
    // the token input is a covenant (P2SH) input: Kaspire needs its redeem script and how to assemble the signature script
    expect(call!.scripts, 'per-input scripts').not.toBeNull();
    expect(call!.scripts!.length).toBeGreaterThan(0);
    for (const s of call!.scripts!) {
      expect(typeof s.scriptHex).toBe('string');
      expect(call!.inputs).toContain(s.inputIndex);
      expect(s.signatureScript.mode, 'a KCC-20 leader witness needs the encoder that lives in kob-wasm').toBe('wrap-signature');
    }
    const id = placed.views[0].covenant_id;
    expect(placed.views[0].contract).toBe('KobAsk');

    // cancel it: the only argument of `cancel` is the signature, so Kaspire can assemble the whole script (ordered-args)
    await page.getByTestId('nav-orders').click();
    await page.getByTestId(`order-cancel-${id}`).click();
    const [step] = await runFlow(page, mock, wallet, { steps: 1 });
    expect(step!.placed.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
    const calls = await wallet!.calls();
    expect(calls).toHaveLength(2);
    const cancelScripts = calls[1]!.scripts!;
    expect(cancelScripts.length).toBeGreaterThan(0);
    for (const sc of cancelScripts) {
      expect(['wrap-signature', 'ordered-args']).toContain(sc.signatureScript.mode);
      expect(call!.inputs.length).toBeGreaterThan(0);
    }
    expect((await mock.order(id)).status).toBe('cancelled');
  });

  test('a Kaspire that ignores the scripts argument answers with bare signatures: the app assembles the script itself and the tx is still accepted', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock, { wallet: 'kaspire' });
    await sellTicket(page);
    await wallet!.configure({ dropScripts: true });
    await openReview(page);
    const txid = await acknowledgeAndSign(page);
    const placed = await placedBy(mock, txid);
    await expectWalletSigned(wallet!, placed);
    // the wallet was asked with scripts but answered without assembling them: only its signatures were used
    const [call] = await wallet!.calls();
    expect(call!.scripts).not.toBeNull();
  });
});

test.describe('Kastle (behind the feature flag)', () => {
  test.use({ walletId: 'kastle' });

  test('a limit sell is signed with the scripts argument and the network id and accepted', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock, { wallet: 'kastle' });
    await expect(page.getByTestId('wallet-connected')).toContainText('Kastle');
    await sellTicket(page);
    const { confirm, placed } = await reviewAndSign(page, mock);
    expect(confirm.wallet.join(' ')).toMatch(/Kastle/);
    await expectWalletSigned(wallet!, placed);
    const [call] = await wallet!.calls();
    expect(call!.network).toMatch(/testnet/);
    expect(call!.scripts, 'Kastle leaves P2SH inputs unsigned without scripts').not.toBeNull();
    expect(call!.scripts!.length).toBeGreaterThan(0);
    expect(placed.views[0].contract).toBe('KobAsk');
    expect(placed.views[0].status).toBe('open');
  });

  test('a limit buy (no token input, only KAS coins) is signed as well', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock, { wallet: 'kastle' });
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245' });
    const { placed } = await reviewAndSign(page, mock);
    await expectWalletSigned(wallet!, placed);
    expect(placed.views[0].contract).toBe('KobBid');
  });

  test('a wallet that declines is a neutral outcome: nothing is signed or sent, and the same screen can try again', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock, { wallet: 'kastle' });
    await sellTicket(page);
    await wallet!.configure({ approve: false, rejectMessage: 'User rejected the request' });
    await openReview(page);
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-rejected')).toBeVisible();
    await expect(page.getByTestId('confirm-error')).toHaveCount(0);
    expect((await wallet!.calls()).at(-1)!.status).toBe('rejected');
    expect(await mock.submitted(false)).toHaveLength(0);
    await wallet!.configure({ approve: true });
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
    expect(await mock.submitted(false)).toHaveLength(1);
  });
});

test.describe('Kastle: the one-time covenant-signing check (C5-10)', () => {
  test.use({ walletId: 'kastle', covenantChecked: false });

  test('before the first order Kastle signs a test cancel (never sent); once it passes, ordering is enabled and remembered', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock, { wallet: 'kastle' });
    await sellTicket(page, false);
    await expect(page.getByTestId('order-covenant-check')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
    await page.getByTestId('order-covenant-check-run').click();
    await expect(page.getByTestId('order-covenant-check')).toHaveCount(0);
    const [probe] = await wallet!.calls();
    expect(probe!.scripts!.length).toBeGreaterThan(0);
    expect(await mock.submitted(false)).toHaveLength(0);
    await expect(page.getByTestId('order-review')).toBeEnabled();
    await page.reload();
    await openMarket(page, mock, { wallet: 'kastle' });
    await sellTicket(page);
    await expect(page.getByTestId('order-covenant-check')).toHaveCount(0);
  });

  test('a Kastle build that returns the covenant input unsigned cannot place orders', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock, { wallet: 'kastle' });
    await wallet!.configure({ dropScripts: true }); // issue #353: the covenant input comes back unsigned
    await sellTicket(page, false);
    await page.getByTestId('order-covenant-check-run').click();
    await expect(page.getByTestId('order-covenant-unsupported')).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });
});

test.describe('KasWare failure modes', () => {
  test('a wrong signature is caught by finalize: the error is shown and NOTHING is broadcast', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await sellTicket(page);
    await wallet!.configure({ wrongSignature: true });
    await openReview(page);
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    const err = page.getByTestId('confirm-error');
    await expect(err).toBeVisible();
    await expect(err).toContainText(/signature/i);
    expect((await wallet!.calls()).at(-1)!.status).toBe('signed'); // the wallet did answer: the answer was wrong
    await expect(page.getByTestId('tx-status')).toHaveCount(0);
    expect(await mock.submitted(false)).toHaveLength(0);
    // the acknowledgement belongs to one attempt
    await expect(page.getByTestId('confirm-ack')).not.toBeChecked();
    await expect(page.getByTestId('confirm-sign')).toBeDisabled();
    // a good wallet answer on the retry goes through
    await wallet!.configure({ wrongSignature: false });
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
    expect(await mock.submitted(false)).toHaveLength(1);
  });

  test('a node that refuses the transaction shows its reason; the screen stays open and the retry is accepted', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await sellTicket(page);
    await mock.failNextSubmit('mock: injected submit failure');
    await openReview(page);
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    const err = page.getByTestId('confirm-error');
    await expect(err).toBeVisible();
    await expect(err).toContainText('injected submit failure');
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    expect(await mock.submitted(false)).toHaveLength(0);
    await expect(page.getByTestId('confirm-sign')).toHaveText('Try again');
    await expect(page.getByTestId('confirm-ack')).not.toBeChecked(); // an acknowledgement belongs to one attempt
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
    expect(await mock.submitted(false)).toHaveLength(1);
  });

  test('a refusal that says the transaction itself is stale (fee, inputs) must be planned again: the old screen cannot be retried', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await sellTicket(page);
    await mock.failNextSubmit('mock: transaction is spent by another transaction (double spend)');
    await openReview(page);
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-error')).toBeVisible();
    await expect(page.getByTestId('confirm-replan')).toBeVisible();
    await expect(page.getByTestId('confirm-sign')).toBeDisabled();
    await page.getByTestId('confirm-replan').click();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    await expect(page.getByTestId('order-review-note')).toBeVisible(); // the ticket asks for a fresh review
    expect(await mock.submitted(false)).toHaveLength(0);
    // a fresh review builds a new transaction from fresh data and goes through
    const { placed } = await reviewAndSign(page, mock);
    expect(placed.views[0].contract).toBe('KobAsk');
  });
});

test.describe('network mismatch', () => {
  test.use({ walletOptions: { network: 'mainnet', allowNetworkSwitch: false } });

  test('the ticket says which networks differ and review stays disabled; cancel is disabled in My orders', async ({ appPage: page, mock }) => {
    await fund(mock);
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 2 }]);
    await openMarket(page, mock);
    await expect(page.getByTestId('wallet-network')).toHaveText('mainnet');
    const need = page.getByTestId('order-need-network');
    await expect(need).toBeVisible();
    await expect(need).toContainText('mainnet');
    await expect(need).toContainText('testnet-10');
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '1', price: '0.027' });
    await expect(page.getByTestId('order-review')).toBeDisabled();
    await page.getByTestId('nav-orders').click();
    await expect(page.getByTestId('orders-network-blocked')).toBeVisible();
    await expect(page.getByTestId(`order-cancel-${id}`)).toBeDisabled();
    await expect(page.getByTestId('orders-cancel-all')).toBeDisabled();
    expect(await mock.submitted(false)).toHaveLength(0);
  });
});

test.describe('blocked signing', () => {
  test('the wallet disappears while the confirmation screen is open: a blocking finding stops signing and nothing is asked of the wallet', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await sellTicket(page);
    await openReview(page);
    await page.getByTestId('confirm-ack').check();
    await expect(page.getByTestId('confirm-sign')).toBeEnabled();
    // the header sits behind the modal: trigger its disconnect the way a script or a second tab would
    await page.evaluate(() => (document.querySelector('[data-testid="wallet-disconnect"]') as HTMLElement).click());
    const blocking = page.getByTestId('confirm-blocking');
    await expect(blocking).toBeVisible();
    await expect(page.getByTestId('confirm-blocking-list').locator('li[data-code="no-wallet"]')).toBeVisible();
    await expect(page.getByTestId('confirm-sign')).toBeDisabled();
    await expect(page.getByTestId('confirm-ack')).toBeDisabled();
    expect(await wallet!.calls()).toHaveLength(0);
    expect(await mock.submitted(false)).toHaveLength(0);
    await page.getByTestId('confirm-cancel').click();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  });
});
