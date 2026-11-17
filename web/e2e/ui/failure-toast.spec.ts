// Every failure is seen at the top right (founder 2026-11-17: "When something fails, the red toast doesn't appear at the top right"). A send of the
// order ticket that stops (the wallet declines, the node refuses: fee, mass, a missing input; the pre-sign check refuses; the node cannot be reached;
// a direct fill from the book that loses its race every time; a chained step the wallet cannot see) raises ONE red toast with the reason, also on a
// phone, and also while another flow's card is on screen (an error is never held back for the bell alone). A My orders flow (a cancel, a merge)
// shows its failure on its own card, in red. A lost race that the retry wins shows no error at all. KOB_TOAST_SHOTS=<dir> saves screenshots.
import { mkdirSync, readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, test, type MockClient } from '../fixtures';
import { KAS, TOKEN, connectWallet, fund, openMarket } from '../helpers/env';
import { openOrders, seedOrders } from '../helpers/orders';
import { clickPlace, clickPlaceOnly, confirmUntil, expectStopped, fillFields, pickType, waitReviewable, waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false });

const SHOTS = process.env.KOB_TOAST_SHOTS ?? '';
const VIEWPORTS = [['desktop', { width: 1280, height: 800 }], ['phone', { width: 390, height: 844 }]] as const;
type View = (typeof VIEWPORTS)[number][0];

async function shot(page: Page, name: string, view: View): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  await page.waitForTimeout(400); // the toast slides in
  await page.screenshot({ path: `${SHOTS}/${name}-${view}.png` });
}

/** The one red toast at the top right (a phone: across the top), just under the header, with `text` in it. */
async function expectRedToast(page: Page, view: View, text: RegExp): Promise<void> {
  const toast = page.getByTestId('toast-error');
  await expect(toast).toHaveCount(1, { timeout: 30_000 });
  await expect(toast).toBeVisible();
  await expect(toast).toContainText(text);
  await expect(toast).toHaveAttribute('role', 'alert');
  // red: the error tone's colour on its edge
  const edge = await toast.evaluate((e) => getComputedStyle(e).borderLeftColor);
  const bad = await page.evaluate(() => {
    const p = document.createElement('div');
    p.style.color = 'var(--bad)';
    document.body.appendChild(p);
    const c = getComputedStyle(p).color;
    p.remove();
    return c;
  });
  expect(edge, 'the red edge of an error').toBe(bad);
  await expect.poll(async () => (await toast.boundingBox())!.y, { message: 'under the header' }).toBeGreaterThanOrEqual(((await page.locator('header.header').boundingBox())!.y + (await page.locator('header.header').boundingBox())!.height) - 1);
  const box = (await toast.boundingBox())!;
  const vw = page.viewportSize()!.width;
  expect(box.y, 'at the top').toBeLessThan(200);
  expect(vw - (box.x + box.width), 'at the right edge').toBeLessThanOrEqual(17);
  if (view === 'desktop') expect(box.x, 'the right part of the screen').toBeGreaterThan(vw / 2);
}

async function sellTicket(page: Page): Promise<void> {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.027' });
  await waitReviewable(page);
}

for (const [view, viewport] of VIEWPORTS) {
  test.describe(`${view}: a stopped order shows one red toast`, () => {
    test.use({ viewport });

    test('the wallet declines', async ({ appPage: page, mock, wallet }) => {
      await fund(mock);
      await openMarket(page, mock);
      await sellTicket(page);
      await wallet!.configure({ approve: false });
      await clickPlace(page);
      await expectStopped(page, 'rejected');
      await expectRedToast(page, view, /^Order not sent: You declined the request in your wallet/);
      await shot(page, 'wallet-declined', view);
      // the same failure again: still one toast, counted
      await clickPlace(page);
      await expectStopped(page, 'rejected');
      await expect(page.getByTestId('toast-error')).toHaveCount(1);
      await expect(page.getByTestId('toast-error')).toContainText('×2');
      // a later order that goes through takes the stale error away
      await wallet!.configure({ approve: true });
      await clickPlace(page);
      await waitSubmitted(page);
      await expect(page.getByTestId('toast-error')).toHaveCount(0);
    });

    for (const [name, message, reason] of [
      ['fee-too-low', 'transaction fee 1000 is below the minimum relay fee 2000', /network fee/],
      ['mass', 'transaction storage mass 120000 exceeds the limit 100000', /network fee or the size/],
      ['missing-input', 'transaction has an orphan input', /does not know an input/],
    ] as const) {
      test(`the node refuses: ${name}`, async ({ appPage: page, mock }) => {
        await fund(mock);
        await openMarket(page, mock);
        await sellTicket(page);
        await mock.failNextSubmit(message);
        await clickPlace(page);
        await expectStopped(page, 'failed');
        await expectRedToast(page, view, new RegExp(`^Order not sent: .*${reason.source}`));
        await shot(page, `node-${name}`, view);
        expect(await mock.submitted(false)).toHaveLength(0);
      });
    }

    test('the node cannot be reached when the order is sent', async ({ appPage: page, mock }) => {
      await fund(mock);
      await openMarket(page, mock);
      await sellTicket(page);
      await page.route('**/node/submit', (route) => route.abort('connectionrefused'));
      await clickPlace(page);
      await expectStopped(page, 'failed');
      await expectRedToast(page, view, /^Order not sent: /);
      await shot(page, 'node-unreachable', view);
    });

    test('the pre-sign check refuses (a coin the node values differently)', async ({ appPage: page, mock, wallet }) => {
      await fund(mock);
      await openMarket(page, mock);
      await sellTicket(page);
      // from the click on, the node reports one more sompi in every coin: the decoder does not sign what it cannot confirm
      await page.route('**/node/utxos', async (route) => {
        const res = await route.fetch();
        const body = await res.json();
        const bump = (x: any): any => (Array.isArray(x) ? x.map(bump) : x && typeof x === 'object' ? Object.fromEntries(Object.entries(x).map(([k, v]) => [k, k === 'amount' && /^\d+$/.test(String(v)) ? (typeof v === 'number' ? v + 1 : String(BigInt(String(v)) + 1n)) : bump(v)])) : x);
        await route.fulfill({ response: res, json: bump(body) });
      });
      await clickPlaceOnly(page);
      await expectStopped(page, 'blocked');
      await expectRedToast(page, view, /^Order not sent: /);
      await shot(page, 'presign-refusal', view);
      expect(await wallet!.calls()).toHaveLength(0);
    });

    test('a direct fill: a lost race that the retry wins shows no error; one lost every time shows one red toast', async ({ appPage: page, mock }) => {
      await fund(mock);
      await openMarket(page, mock);
      await pickType(page, 'limit', 'buy');
      await fillFields(page, { amount: '2', price: '0.0252' });
      await expect(page.getByTestId('order-take')).toBeVisible({ timeout: 15_000 });
      await mock.failNextSubmit('transaction has an orphan input: already spent by another transaction', 1);
      await clickPlaceOnly(page);
      await confirmUntil(page, page.getByTestId('order-take-step-1').and(page.locator('[data-state="done"]')));
      await waitSubmitted(page);
      await expect(page.getByTestId('toast-error')).toHaveCount(0);
      // the take, its retry and the placement of the order instead all lose: one toast for the whole click
      await fillFields(page, { amount: '2', price: '0.0252' });
      await expect(page.getByTestId('order-take')).toBeVisible({ timeout: 15_000 });
      await mock.failNextSubmit('transaction has an orphan input: already spent by another transaction', 3);
      await clickPlaceOnly(page);
      // (the status line says "failed" for a moment at each lost race too: the end is the step the stepper marks failed)
      await confirmUntil(page, page.locator('[data-testid^="order-take-step-"][data-state="failed"]'));
      await expectStopped(page, 'failed');
      await expectRedToast(page, view, /^Fill from the book stopped: /);
      await shot(page, 'take-lost-race', view);
    });

    test('a direct fill whose orders are all gone at the click (IOC): said, not a silent end', async ({ appPage: page, mock, wallet }) => {
      await fund(mock);
      await openMarket(page, mock);
      await pickType(page, 'ioc', 'buy');
      await fillFields(page, { amount: '2', price: '0.0251', direct: 'take' });
      await expect(page.getByTestId('order-take')).toBeVisible({ timeout: 15_000 });
      await waitReviewable(page);
      // someone else takes both asks at 0.0251 just as the click reads the book again
      const asks = (await mock.ordersOf('maker')).filter((o) => [1, 'ask', 'sell'].includes(o.side) && String(o.price) === '2510000');
      expect(asks.length).toBeGreaterThan(0);
      let taken = false;
      await page.route('**/v1/books/**', async (route) => {
        if (!taken) {
          taken = true;
          for (const a of asks) await fetch(`${process.env.KOB_MOCK_URL}/mock/take`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ covenant_id: a.covenant_id }) });
        }
        await route.continue();
      });
      await page.getByTestId('order-place').click();
      await expectStopped(page, 'blocked', { code: 'take-empty' });
      await expectRedToast(page, view, /^Fill from the book stopped: The orders at your price are gone/);
      await shot(page, 'take-nothing-left', view);
      expect(await wallet!.calls()).toHaveLength(0);
    });

    test('a direct fill the wallet declines', async ({ appPage: page, mock, wallet }) => {
      await fund(mock);
      await openMarket(page, mock);
      await pickType(page, 'limit', 'buy');
      await fillFields(page, { amount: '2', price: '0.0252' });
      await expect(page.getByTestId('order-take')).toBeVisible({ timeout: 15_000 });
      await wallet!.configure({ approve: false });
      await clickPlace(page);
      await expectStopped(page, 'rejected');
      await expectRedToast(page, view, /^Fill from the book stopped: You declined/);
      await shot(page, 'take-declined', view);
    });

    test('the app\'s own Cancel of the confirmation is no failure: no toast', async ({ appPage: page, mock }) => {
      await fund(mock);
      await openMarket(page, mock);
      await sellTicket(page);
      await clickPlaceOnly(page);
      await page.getByTestId('confirm-order-cancel').click();
      await expectStopped(page, 'rejected', { code: 'cancelled' });
      await page.waitForTimeout(500);
      await expect(page.getByTestId('toast-error')).toHaveCount(0);
    });

    test('an amount above the balance: the button is off and the reason is next to it (nothing to fail)', async ({ appPage: page, mock }) => {
      await mock.giveKas('alice', 1000n * KAS);
      await mock.giveTokens('alice', 5n * TOKEN);
      await openMarket(page, mock);
      await pickType(page, 'limit', 'sell');
      await fillFields(page, { amount: '8', price: '0.027' });
      await expect(page.getByTestId('order-issue-INSUFFICIENT_TOKENS')).toBeVisible({ timeout: 15_000 });
      await expect(page.getByTestId('order-place')).toBeDisabled();
      await expect(page.getByTestId('toast-error')).toHaveCount(0);
      await shot(page, 'insufficient-balance', view);
    });
  });

  test.describe(`${view}: a chained step the wallet cannot see yet`, () => {
    test.use({ viewport, walletOptions: { pushTxOutcome: 'lost', walletNodeLagMs: 60_000, refusal: 'reject' } });

    test('the second level of a ladder is refused by the wallet: one red toast', async ({ appPage: page, mock }) => {
      await fund(mock, 'alice', { utxos: 1 });
      await openMarket(page, mock);
      await pickType(page, 'limit', 'buy');
      await fillFields(page, { amount: '1', price: '0.0245', 'ladder.on': true, 'ladder.levels': '2', 'ladder.pct': '2' });
      await waitReviewable(page);
      await page.getByTestId('order-place').click();
      await confirmUntil(page, page.locator('[data-testid="order-status"][data-phase="failed"], [data-testid="order-status"][data-phase="rejected"]'), { timeout: 90_000 });
      await expectRedToast(page, view, /^Order not sent: /);
      await shot(page, 'chain-sign-refused', view);
    });
  });

  test.describe(`${view}: a My orders flow shows its failure on its card`, () => {
    test.use({ viewport });

    test('a cancel the wallet declines: the card turns red and says so', async ({ appPage: page, mock, wallet }) => {
      const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
      await openOrders(page, mock);
      await wallet!.configure({ approve: false });
      await page.getByTestId(`order-cancel-${id}`).click();
      await confirmUntil(page, page.getByTestId('flow-stopped'));
      const card = page.getByTestId('tx-flow');
      await expect(card).toHaveClass(/toast-error/);
      await expect(card).toContainText(/declined/i);
      await shot(page, 'flow-declined', view);
    });

    test('a cancel the node refuses: the card turns red with the reason', async ({ appPage: page, mock }) => {
      const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
      await openOrders(page, mock);
      await mock.failNextSubmit('transaction fee 1000 is below the minimum relay fee 2000');
      await page.getByTestId(`order-cancel-${id}`).click();
      await confirmUntil(page, page.getByTestId('flow-stopped'));
      const card = page.getByTestId('tx-flow');
      await expect(card).toHaveClass(/toast-error/);
      await expect(card.getByTestId('send-error')).toContainText(/network fee/);
      await shot(page, 'flow-node-refused', view);
    });

    test('a merge step that fails: the card turns red with the reason', async ({ appPage: page, mock }) => {
      await seedStd(mock);
      await mock.giveTokens('alice', TOKEN, { token: STD, carrier: 2n * KAS, count: 3 });
      await openBalances(page, 3);
      await mock.failNextSubmit('transaction storage mass 120000 exceeds the limit 100000');
      await page.getByTestId(`orders-merge-token-${STD}`).click();
      await page.getByTestId('merge-all-confirm').click();
      await expect(page.getByTestId('flow-stopped')).toBeVisible({ timeout: 30_000 });
      const card = page.getByTestId('tx-flow');
      await expect(card).toHaveClass(/toast-error/);
      await expect(card.getByTestId('send-error')).toContainText(/network fee or the size/);
      await shot(page, 'merge-step-failed', view);
    });

    test('an order that fails while a stopped flow card is on screen still raises its red toast (never only the bell)', async ({ appPage: page, mock, wallet }) => {
      await fund(mock);
      const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 3 }]);
      await openMarket(page, mock);
      // the order first (the card at the top right would sit over the ticket's first rows)
      await sellTicket(page);
      // the market page's own orders panel: a cancel the wallet declines leaves its stopped card on screen
      await wallet!.configure({ approve: false });
      const cancel = page.getByTestId(`order-cancel-${id}`);
      await cancel.scrollIntoViewIfNeeded();
      await cancel.click();
      await confirmUntil(page, page.getByTestId('flow-stopped'));
      await expect(page.getByTestId('tx-flow')).toHaveClass(/toast-error/);
      // now the ticket's order fails too
      await page.getByTestId('order-place').scrollIntoViewIfNeeded();
      await clickPlace(page);
      await expectStopped(page, 'rejected');
      if (view === 'desktop') {
        await expectRedToast(page, view, /^Order not sent: You declined/);
        await shot(page, 'failure-under-flow-card', view);
      } else {
        // a phone shows one notification at a time: the card, and a red "+1" for the error waiting behind it
        const chip = page.getByTestId('toast-more');
        await expect(chip).toBeVisible();
        await expect(chip).toHaveAttribute('data-tone', 'error');
        await shot(page, 'failure-under-flow-card', view);
        await page.getByTestId('tx-flow-dismiss').click();
        await expectRedToast(page, view, /^Order not sent: You declined/);
        await shot(page, 'failure-after-flow-card', view);
      }
    });
  });
}

// ---- the merge set-up of e2e/orders/merge.spec.ts: EXSTD, a token of the standard 3 / 3 program
const STD = '5a'.repeat(32);
const EXT = 'ee'.repeat(32);
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
async function openBalances(page: Page, count: number): Promise<void> {
  await registryWithStd(page);
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  await expect(page.getByTestId(`orders-merge-token-${STD}`)).toHaveAttribute('title', new RegExp(`^Merge your ${count} UTXOs `), { timeout: 15_000 });
}

