// Screenshots of the round-9 screens (review only, skipped without KOB_R9_SHOTS=<dir>): the
// confirmation with the DAA next to its dates, the same with the indexer down (the chain time
// checked against the last verified time), and the private-key frame's lines for a placement and a
// cancel (the order's bound terms next to the id the app names).
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { expect, test, TEST_KEYS } from '../fixtures';
import { KAS, TOKEN, fund, gotoOrders, openMarket } from '../helpers/env';
import { openOrderRow, openOrders, seedOrders } from '../helpers/orders';
import { loadKobNode } from '../../src/kob/wasm.node';
import { goldenRequest } from '../../src/testing/chain-fixtures';
import { bundle } from '../../src/testing/hostile-tx';
import { SIGNER_PROTOCOL } from '../../src/signer/protocol';
import { clickPlaceOnly, fillFields, pickType, readSignerFrame, waitReviewable } from '../helpers/ticket';

const SHOTS = process.env.KOB_R9_SHOTS;
const TAG = process.env.KOB_R9_TAG ?? 'after';
test.skip(!SHOTS, 'screenshots only (KOB_R9_SHOTS)');
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

const shot = (name: string) => join(SHOTS!, `${name}-${TAG}.png`);

test.describe('the app confirmation', () => {
  test.use({ autoOpen: false });
  test('a GTC limit sell: its expiry with the DAA, healthy and with the indexer down', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1000 });
    // the init script runs in the sandboxed signer frame too, where localStorage throws
    await page.addInitScript(() => {
      try {
        localStorage.setItem('kob.theme', 'light');
      } catch {
        /* the signer frame */
      }
    });
    await fund(mock, 'alice', { tokens: 50n });
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.05' });
    await clickPlaceOnly(page);
    const confirm = page.getByRole('dialog').first();
    await expect(confirm).toBeVisible({ timeout: 30_000 });
    await page.screenshot({ path: shot('r9-confirm-dates'), fullPage: false });
    await page.keyboard.press('Escape');
    // the indexer goes down: the dates are checked against the last verified time this browser saw
    await mock.setHealth('stopped');
    await page.waitForTimeout(20_000);
    await clickPlaceOnly(page).catch(() => undefined);
    await page.waitForTimeout(3_000);
    await page.screenshot({ path: shot('r9-confirm-dates-indexer-down'), fullPage: false });
  });
});

test.describe('the private-key frame', () => {
  test.use({ walletId: null, autoOpen: false });
  const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
  async function connectKey(page: Page): Promise<void> {
    await page.getByTestId('wallet-connect-local-key').click();
    await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await signer(page).getByTestId('signer-connect').click();
  }
  test('a placement and a cancel in the frame', async ({ appPage: page, mock }) => {
    test.setTimeout(300_000);
    await page.setViewportSize({ width: 1440, height: 1200 });
    // the init script runs in the sandboxed signer frame too, where localStorage throws
    await page.addInitScript(() => {
      try {
        localStorage.setItem('kob.theme', 'light');
      } catch {
        /* the signer frame */
      }
    });
    await fund(mock, 'alice', { tokens: 50n });
    await openMarket(page, mock, { connect: false });
    await connectKey(page);
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '2', price: '0.05' });
    await waitReviewable(page);
    await page.getByTestId('order-place').click();
    const approve = signer(page).getByTestId('signer-approve');
    await expect(approve).toBeVisible({ timeout: 30_000 });
    await readSignerFrame(page);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-place') });
    await expect(approve).toBeEnabled({ timeout: 30_000 });
    await approve.click();
    await expect.poll(async () => (await mock.submitted(false)).length, { timeout: 60_000 }).toBeGreaterThan(0);
    await gotoOrders(page);
    await page.locator('[data-testid^="order-cancel-"]:not([data-testid="order-cancel-only"]):not([data-testid="order-cancel-blocked"])').first().click({ timeout: 60_000 });
    await expect(approve).toBeVisible({ timeout: 60_000 });
    await readSignerFrame(page);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-cancel') });
  });
});

test.describe('My orders', () => {
  test.use({ autoOpen: false });
  const light = async (page: Page) => page.addInitScript(() => {
    try {
      localStorage.setItem('kob.theme', 'light');
    } catch {
      /* a sandboxed frame */
    }
  });

  test('the stray choices of a cancel, and its confirmation', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1000 });
    await light(page);
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 1 }]);
    for (let k = 1; k <= 8; k++) await mock.stray(id!, BigInt(k + 1) * TOKEN);
    await openOrders(page, mock);
    await page.getByTestId(`order-cancel-${id}`).click();
    await expect(page.getByTestId('sweep-first-dialog')).toBeVisible({ timeout: 30_000 });
    await page.screenshot({ path: shot('r9-stray-choice') });
    await page.getByTestId('sweep-first-abandon').click();
    await page.waitForTimeout(4_000);
    await page.screenshot({ path: shot('r9-stray-cancel-confirm') });
  });

  test('the refund confirmation of an order with strays', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1000 });
    await light(page);
    await mock.giveKas('alice', 50n * KAS);
    const [id] = await seedOrders(mock, [{ side: 'ask', price: 2_700_000, tokens: 2 }]);
    await mock.stray(id!, 3n * TOKEN);
    await mock.advanceDaa(101_000_000);
    await openOrders(page, mock);
    await page.getByTestId(`order-refund-${id}`).click({ timeout: 60_000 });
    await page.waitForTimeout(5_000);
    await page.screenshot({ path: shot('r9-refund-confirm-strays') });
  });

  test('the amend form of an auction-shaped sell (a crossing limit)', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1000 });
    await light(page);
    await fund(mock, 'alice', { tokens: 50n });
    await mock.giveKas('alice', 50n * KAS);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'sell');
    // below the best bid: it crosses the book and is placed as an auction from the touch to this limit
    await fillFields(page, { amount: '2', price: '0.0240' });
    await clickPlaceOnly(page);
    await page.getByTestId('confirm-order-ok').click({ timeout: 30_000 }).catch(() => undefined);
    await expect.poll(async () => (await mock.ordersOf('alice')).length, { timeout: 60_000 }).toBeGreaterThan(0);
    const id = (await mock.ordersOf('alice'))[0]!.covenant_id;
    // past its 30 DAA lead: an active limit, which the amend form takes
    await mock.advanceDaa(1_000);
    await gotoOrders(page);
    void id;
    const toggle = page.locator('[data-testid^="order-toggle-"]').first();
    if ((await toggle.getAttribute('aria-expanded')) !== 'true') await toggle.click();
    await page.locator('[data-testid^="order-replace-"]').first().click({ timeout: 30_000 });
    await page.getByTestId('amend-price').fill('0.0262');
    await page.waitForTimeout(2_000);
    await page.screenshot({ path: shot('r9-amend-auction') });
  });
});

test.describe('the private-key frame: a take and a refusal', () => {
  test.use({ walletId: null, autoOpen: false });
  const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
  async function connectKey(page: Page): Promise<void> {
    await page.getByTestId('wallet-connect-local-key').click();
    await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await signer(page).getByTestId('signer-connect').click();
  }

  test('the frame lines of a fill from the book', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1200 });
    await fund(mock);
    await openMarket(page, mock, { connect: false });
    await connectKey(page);
    await pickType(page, 'limit', 'buy');
    await fillFields(page, { amount: '2', price: '0.0252' });
    await waitReviewable(page);
    await page.getByTestId('order-place').click();
    await expect(signer(page).getByTestId('signer-approve')).toBeVisible({ timeout: 30_000 });
    await readSignerFrame(page);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-take') });
  });

  test('a hostile transaction the frame refuses (a fill bundled with a sweep of the user\'s order)', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1200 });
    await fund(mock);
    await openMarket(page, mock, { connect: false });
    await connectKey(page);
    const kob = loadKobNode();
    const pk = TEST_KEYS.alice.pubkey;
    const fill = kob.build(goldenRequest('take.ask.market', pk) as never);
    const own = kob.build(goldenRequest('amend.bid', pk) as never);
    const built = bundle(kob, fill, own);
    const msg = JSON.parse(JSON.stringify({ p: SIGNER_PROTOCOL, type: 'sign', id: 'hostile-1', built, tokens: [] }));
    await page.evaluate((m) => {
      // the app shows the frame only for its own requests: shown here for the injected one
      document.querySelector<HTMLElement>('[data-testid="signer-overlay"]')!.hidden = false;
      (document.querySelector('[data-testid="signer-frame"]') as HTMLIFrameElement).contentWindow!.postMessage(m, '*');
    }, msg);
    await expect(signer(page).getByTestId('signer-approve')).toBeVisible({ timeout: 30_000 });
    await page.waitForTimeout(3_000);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-refusal') });
    // the frame before this round signed it; now it is refused with one sentence
    if (TAG === 'after') await expect(signer(page).getByTestId('signer-problems')).toBeVisible();
  });
});

test.describe('the private-key frame: a send', () => {
  test.use({ walletId: null, autoOpen: false });
  const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
  test('a token send names the whole key it pays', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1200 });
    await fund(mock);
    await openMarket(page, mock, { connect: false });
    await page.getByTestId('wallet-connect-local-key').click();
    await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await signer(page).getByTestId('signer-connect').click();
    const kob = loadKobNode();
    const built = kob.build(goldenRequest('send.tokens', TEST_KEYS.alice.pubkey) as never);
    const msg = JSON.parse(JSON.stringify({ p: SIGNER_PROTOCOL, type: 'sign', id: 'send-1', built, tokens: [] }));
    await page.evaluate((m) => {
      document.querySelector<HTMLElement>('[data-testid="signer-overlay"]')!.hidden = false;
      (document.querySelector('[data-testid="signer-frame"]') as HTMLIFrameElement).contentWindow!.postMessage(m, '*');
    }, msg);
    await expect(signer(page).getByTestId('signer-approve')).toBeVisible({ timeout: 30_000 });
    await page.waitForTimeout(2_000);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-send') });
  });
});

test.describe('the private-key frame: a market buy of a token the frame cannot name', () => {
  test.use({ walletId: null, autoOpen: false });
  const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
  test('the unit is "token" and the token is named once, in full', async ({ appPage: page, mock }) => {
    test.setTimeout(240_000);
    await page.setViewportSize({ width: 1440, height: 1400 });
    await fund(mock);
    await openMarket(page, mock, { connect: false });
    await page.getByTestId('wallet-connect-local-key').click();
    await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await signer(page).getByTestId('signer-connect').click();
    const kob = loadKobNode();
    const built = kob.build(goldenRequest('create.bid.market', TEST_KEYS.alice.pubkey) as never);
    // the app calls it TUSD, with no source the frame can vouch for (the founder's TN10 case)
    const msg = JSON.parse(JSON.stringify({ p: SIGNER_PROTOCOL, type: 'sign', id: 'mkt-1', built, tokens: [{ covenantId: built.plans.length ? (kob.recoverOrders(built.tx)[0]!.order.state as { tokenCovId: string }).tokenCovId : '', ticker: '', decimals: 3, source: 'scale', ...(TAG.startsWith('before') ? {} : { appName: 'TUSD' }) }] }));
    await page.evaluate((m) => {
      document.querySelector<HTMLElement>('[data-testid="signer-overlay"]')!.hidden = false;
      (document.querySelector('[data-testid="signer-frame"]') as HTMLIFrameElement).contentWindow!.postMessage(m, '*');
    }, msg);
    await expect(signer(page).getByTestId('signer-approve')).toBeVisible({ timeout: 30_000 });
    await page.waitForTimeout(2_000);
    await page.getByTestId('signer-frame').screenshot({ path: shot('r9-frame-market-buy-unnamed') });
  });
});

test.describe('the private-key frame: amend, sweep and claim posted from builds', () => {
  test.use({ walletId: null, autoOpen: false });
  const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
  const cases: [string, () => Promise<unknown>][] = [
    ['amend', async () => loadKobNode().build(goldenRequest('amend.ask', TEST_KEYS.alice.pubkey) as never)],
    ['sweep', async () => {
      const kob = loadKobNode();
      const { placeGolden, strayFor, keyUtxo } = await import('../../src/testing/chain-fixtures');
      const { planSweep } = await import('../../src/kob/cancel');
      const who = { sk: TEST_KEYS.alice.secretKey, pk: TEST_KEYS.alice.pubkey };
      const snap = placeGolden(kob, 'create.ask', who as never).snapshots[0]!;
      return planSweep({ kob, maker: who.pk, funding: [keyUtxo(who.pk, 100n * 100_000_000n, 201)], tokenUtxos: [], clock: { daa: 78_000_000n } }, { ...snap, strays: [strayFor(snap, 4n, 92)] }).built;
    }],
  ];
  for (const [name, make] of cases) {
    test(`the frame of a ${name}`, async ({ appPage: page, mock }) => {
      test.setTimeout(240_000);
      await page.setViewportSize({ width: 1440, height: 1400 });
      await fund(mock);
      await openMarket(page, mock, { connect: false });
      await page.getByTestId('wallet-connect-local-key').click();
      await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
      await signer(page).getByTestId('signer-connect').click();
      const built = await make();
      const msg = JSON.parse(JSON.stringify({ p: SIGNER_PROTOCOL, type: 'sign', id: `${name}-1`, built, tokens: [] }));
      await page.evaluate((m) => {
        document.querySelector<HTMLElement>('[data-testid="signer-overlay"]')!.hidden = false;
        (document.querySelector('[data-testid="signer-frame"]') as HTMLIFrameElement).contentWindow!.postMessage(m, '*');
      }, msg);
      await expect(signer(page).getByTestId('signer-approve')).toBeVisible({ timeout: 30_000 });
      await page.waitForTimeout(2_000);
      await page.getByTestId('signer-frame').screenshot({ path: shot(`r9-frame-${name}`) });
    });
  }
});

test.describe('a flipped market (KAS/TOKEN): the confirmation of each order type', () => {
  const kinds: [string, 'limit' | 'market', 'buy' | 'sell', Record<string, string>][] = [
    ['limit-buy', 'limit', 'buy', { amount: '2', price: '0.02' }],
    ['limit-sell', 'limit', 'sell', { amount: '2', price: '0.03' }],
    ['market-buy', 'market', 'buy', { amount: '2' }],
    ['market-sell', 'market', 'sell', { amount: '2' }],
  ];
  test.describe('a mock extension wallet (the app confirmation)', () => {
    test.use({ autoOpen: false });
    for (const [name, type, side, fields] of kinds) {
      test(`flipped ${name}`, async ({ appPage: page, mock }) => {
        test.setTimeout(240_000);
        await page.setViewportSize({ width: 1440, height: 1100 });
        await fund(mock, 'alice', { tokens: 50n });
        await openMarket(page, mock);
        await pickType(page, type, side);
        await fillFields(page, fields);
        await page.getByTestId('market-flip').click();
        await expect(page.getByTestId('order-ticket')).toHaveAttribute('data-inverted', '1');
        await clickPlaceOnly(page);
        await expect(page.getByTestId('confirm-order')).toBeVisible({ timeout: 30_000 });
        await page.waitForTimeout(1_500);
        await page.getByTestId('confirm-order').screenshot({ path: shot(`r9-flip-app-${name}`) });
      });
    }
  });
  test.describe('the private-key frame', () => {
    test.use({ walletId: null, autoOpen: false });
    const signer = (page: Page) => page.frameLocator('[data-testid="signer-frame"]');
    for (const [name, type, side, fields] of kinds) {
      test(`flipped ${name}`, async ({ appPage: page, mock }) => {
        test.setTimeout(240_000);
        await page.setViewportSize({ width: 1440, height: 1400 });
        await fund(mock, 'alice', { tokens: 50n });
        await openMarket(page, mock, { connect: false });
        await page.getByTestId('wallet-connect-local-key').click();
        await signer(page).getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
        await signer(page).getByTestId('signer-connect').click();
        await pickType(page, type, side);
        await fillFields(page, fields);
        await page.getByTestId('market-flip').click();
        await waitReviewable(page);
        await page.getByTestId('order-place').click();
        await expect(signer(page).getByTestId('signer-approve')).toBeVisible({ timeout: 30_000 });
        await readSignerFrame(page);
        await page.getByTestId('signer-frame').screenshot({ path: shot(`r9-flip-frame-${name}`) });
      });
    }
  });
});
