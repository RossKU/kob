// A token/token pair needs no registry entry: only the token program is reviewed. The pair seed's EXKCC / EXUSD served as an executor serves tokens outside the
// registry (no ticker, no decimals: the scale of their orders tells them), with an empty registry (the page of a host that ships none) or EXKCC alone listed.
// The page resolves each token as its own page does, an order of them is decoded against the registry plus BOTH tokens, the final confirmation shows the identity of
// each, a commitment known without the indexer that differs stops it, and My orders cancels it. A token the page cannot trade yet says why.
import type { Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { test, expect, TEST_KEYS, type MockClient } from '../fixtures';
import { connectWallet, fund, listExkcc, seedFavorites, TOKEN } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { clickPlace, clickPlaceOnly, confirmReceipt, expectStopped, fillFields, pickType, placedBy, readReceipt, waitReviewable, waitSubmitted } from '../helpers/ticket';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);
const NO_ORDER = 'c1'.repeat(32);
/** EXUSD has 6 decimals: base units per whole EXUSD */
const USD = 1_000_000n;
const long = (id: string): string => `${id.slice(0, 8)}…${id.slice(-8)}`;

/** The example registry's templates, reviewed, and no token: what a host without a token list ships. */
async function emptyRegistry(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  reg.tokens = [];
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

/** `/v1/tokens` as the executor serves tokens outside the registry; `over` per covenant id (null drops the token from the list); `delayMs` holds the answer back. */
async function openListed(page: Page, ids: readonly string[], over: Record<string, Record<string, unknown> | null> = {}, delayMs = 0): Promise<void> {
  await page.route(/\/v1\/tokens(\?.*)?$/, async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    for (const t of body.tokens) if (ids.includes(t.covenant_id)) Object.assign(t, { ticker: '', decimals: null, scale: null }, over[t.covenant_id] ?? {});
    body.tokens = body.tokens.filter((t: { covenant_id: string }) => over[t.covenant_id] !== null);
    if (delayMs) await new Promise((r) => setTimeout(r, delayMs));
    await route.fulfill({ response: res, json: body });
  });
}

/** Records in the page whether `pair-not-found` was ever in the document, however briefly. */
async function watchNotFound(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const w = window as unknown as { __notFound: boolean };
    w.__notFound = false;
    new MutationObserver(() => {
      if (document.querySelector('[data-testid="pair-not-found"]')) w.__notFound = true;
    }).observe(document, { childList: true, subtree: true });
  });
}
const sawNotFound = (page: Page): Promise<boolean> => page.evaluate(() => (window as unknown as { __notFound: boolean }).__notFound);

async function seedPair(mock: MockClient, o: { usd?: bigint } = {}): Promise<string> {
  await mock.seed({ pair: true });
  await fund(mock, 'alice');
  if (o.usd) await mock.giveTokens('alice', o.usd * USD, { token: 'EXUSD' });
  return (await mock.token()).covenant_id;
}

async function openPage(page: Page, base: string): Promise<void> {
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('pair-page')).toBeVisible();
  await connectWallet(page);
  await expect(page.getByTestId('order-ticket')).toBeVisible();
}

/** The identity rows of the open final confirmation by row id. */
async function identityRows(page: Page): Promise<Record<string, { tone: string; text: string }>> {
  const rows = page.getByTestId('confirm-order-identity-row');
  const out: Record<string, { tone: string; text: string }> = {};
  for (const r of await rows.all()) out[(await r.getAttribute('data-row'))!] = { tone: (await r.getAttribute('data-tone'))!, text: await r.innerText() };
  return out;
}

const balanceOf = async (mock: MockClient, ticker: string) => BigInt((await mock.balance('alice')).tokens[ticker] ?? '0');

test('both tokens unregistered, no token in the registry: the pair opens, an order is placed with the identity of both, My orders cancels it and refuses the amend', async ({ appPage: page, mock, wallet }) => {
  const base = await seedPair(mock);
  await mock.giveTokens('alice', TOKEN, { count: 2 }); // several UTXOs of it: a registry token would offer a merge
  await emptyRegistry(page);
  await openListed(page, [base, QUOTE], {}, 400);
  await watchNotFound(page);
  await page.goto(`/#/market/${base}/${QUOTE}`);
  // the token list is on its way: the page waits, it never says the pair does not exist
  await expect(page.getByTestId('pair-loading')).toBeVisible();
  await expect(page.getByTestId('pair-page')).toBeVisible();
  expect(await sawNotFound(page)).toBe(false);
  await connectWallet(page);
  await expect(page.getByTestId('pair-book')).toBeVisible();
  await expect(page.getByTestId('pair-book').locator('[data-testid="pair-level"][data-source="direct"][data-side="ask"]').last()).toHaveAttribute('data-price', '0.0505');
  // named by their covenant ids; the ids of both stay on the page
  await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', `${long(base)}/${long(QUOTE)}`);
  await expect(page.getByTestId('pair-token-meta')).toHaveCount(2);

  await pickType(page, 'limit', 'sell');
  await fillFields(page, { price: '0.0515', amount: '2' });
  await waitReviewable(page);
  await clickPlaceOnly(page);
  const rows = await identityRows(page);
  expect(Object.keys(rows).sort()).toEqual([`ext-${base}`, `ext-${QUOTE}`].sort());
  expect(rows[`ext-${QUOTE}`]!.text).toContain(long(QUOTE));
  const receipt = await readReceipt(page);
  // the amounts read as the ticket read them: 8 and 6 decimals, the scales of the two markets; a token without a ticker is named by its id, also as the quote
  expect(receipt.orders[0]!.rows).toMatchObject({
    Pair: `sells ${long(base)} for ${long(QUOTE)} (prices in ${long(QUOTE)} per ${long(base)})`,
    'Base tokens locked in the order': `2 ${long(base)}`,
    'Limit price': `0.0515 ${long(QUOTE)} per ${long(base)}`,
  });
  expect(Object.values(receipt.orders[0]!.rows), 'the least it receives, 2 x 0.0515').toContain(`0.103 ${long(QUOTE)}`);
  await confirmReceipt(page);
  const placed = await placedBy(mock, await waitSubmitted(page));
  const view = placed.views[0];
  expect(view.contract).toBe('KobPair');
  expect(view.state.state).toMatchObject({ side: '1', price: '51500', amountLeft: String(2n * TOKEN), sCovId: base, tCovId: QUOTE });
  expect(await balanceOf(mock, 'EXKCC')).toBe(100n * TOKEN);
  const id = view.covenant_id as string;

  await page.getByTestId('pair-my-orders').click();
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row.getByTestId('order-pair')).toContainText(long(QUOTE));
  await expect(page.getByTestId('balances-panel')).toBeVisible();
  // a token without a registry entry offers no merge (as in its KAS market)
  await expect(page.getByTestId(`balance-row-${base}`)).toBeVisible();
  await expect(page.getByTestId(`orders-merge-token-${base}`)).toHaveCount(0);

  // the amend needs both registry entries: refused in a sentence
  await page.getByTestId(`order-replace-${id}`).click();
  await expect(page.getByTestId('replace-no-token')).toContainText('cancel the order and place a new one');
  await page.getByTestId('replace-no-token').getByRole('button').first().click();
  await expect(page.getByTestId('replace-no-token')).toHaveCount(0);

  // a stray of the order is swept in place, the order lives on
  await mock.stray(id, TOKEN);
  await page.getByTestId('orders-refresh').click();
  await expect(page.getByTestId('orders-strays').getByTestId('stray-row')).toHaveCount(1);
  await page.getByTestId(`order-sweep-${id}`).click();
  const [swept] = await runFlow(page, mock, wallet, { steps: 1 });
  expect(swept!.submission.closed ?? []).toEqual([]);
  expect((await mock.order(id)).status).toBe('open');

  await page.getByTestId('orders-refresh').click();
  await expect(page.getByTestId(`order-cancel-${id}`)).toBeEnabled();
  await page.getByTestId(`order-cancel-${id}`).click();
  await expect(page.getByTestId('flow-done')).toHaveCount(0); // the sweep's flow gives way to the cancel's
  const [step] = await runFlow(page, mock, wallet, { steps: 1 });
  expect(step!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
  expect((await mock.order(id)).status).toBe('cancelled');
  expect(await balanceOf(mock, 'EXKCC')).toBe(103n * TOKEN);
});

test('only the quote unregistered: a buy escrows it, its identity alone is shown, the registry token keeps its name', async ({ appPage: page, mock, wallet }) => {
  const base = await seedPair(mock, { usd: 10n });
  await listExkcc(page);
  await openListed(page, [QUOTE]);
  await openPage(page, base);
  await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', `EXKCC/${long(QUOTE)}`);
  await pickType(page, 'limit', 'buy');
  await fillFields(page, { price: '0.048', amount: '1' });
  await waitReviewable(page);
  await clickPlaceOnly(page);
  expect(Object.keys(await identityRows(page))).toEqual([`ext-${QUOTE}`]);
  const receipt = await readReceipt(page);
  expect(receipt.orders[0]!.rows).toMatchObject({ Pair: `buys EXKCC with ${long(QUOTE)} (prices in ${long(QUOTE)} per EXKCC)`, 'Limit price': `0.048 ${long(QUOTE)} per EXKCC` });
  await confirmReceipt(page);
  const placed = await placedBy(mock, await waitSubmitted(page));
  const view = placed.views[0];
  expect(view.state.state).toMatchObject({ side: '2', price: '48000', sCovId: QUOTE, tCovId: base });
  const id = view.covenant_id as string;
  await page.getByTestId('pair-my-orders').click();
  await page.getByTestId(`order-cancel-${id}`).click();
  const [step] = await runFlow(page, mock, wallet, { steps: 1 });
  expect(step!.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
  expect(await balanceOf(mock, 'EXUSD')).toBe(10n * USD);
});

test('a quote whose extension commitment differs from the node-verified holdings stops the order before the wallet is asked', async ({ appPage: page, mock, wallet }) => {
  const base = await seedPair(mock, { usd: 10n });
  await listExkcc(page);
  // the indexer names a look-alike's commitment; the wallet's EXUSD, confirmed on the node, carries the real one
  await openListed(page, [QUOTE], { [QUOTE]: { extension_commitment: 'ab'.repeat(32) } });
  await openPage(page, base);
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { price: '0.0515', amount: '2' });
  await waitReviewable(page);
  await page.getByTestId('order-place').click();
  const stopped = await expectStopped(page, 'blocked', { code: 'open-token-extension-mismatch' });
  expect(stopped.text).toContain(long(QUOTE));
  expect(await wallet!.calls()).toHaveLength(0);
  expect(await mock.submitted()).toHaveLength(0);
});

test('a token with no order yet: the pair says so and leads to its page, where the first order sets the decimals; none are assumed', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock);
  await mock.seed({ tokens: [{ ticker: '', covenant_id: NO_ORDER, program: 'KCC20Ref_8x8', template_id: 'KCC20Ref_8x8', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null, order_scale: 1_000_000 }] });
  await listExkcc(page);
  await watchNotFound(page);
  await page.goto(`/#/market/${base}/${NO_ORDER}`);
  const box = page.getByTestId('pair-no-scale');
  await expect(box).toContainText('never assumed');
  expect(await sawNotFound(page)).toBe(false);
  await expect(page.getByTestId('order-ticket')).toHaveCount(0);
  await expect(box.getByTestId('pair-no-scale-link')).toHaveCount(1);
  await box.getByTestId('pair-no-scale-link').click();
  await expect(page).toHaveURL(new RegExp(`#/market/${NO_ORDER}$`));
  await expect(page.getByTestId('first-order')).toBeVisible();
});

test('a token program this build cannot trade, or one the orders disagree on: the not-tradable page of the token page, with its reason', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock);
  await listExkcc(page);
  await openListed(page, [QUOTE], { [QUOTE]: { template_hash: 'ab'.repeat(32) } });
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('not-tradable-page')).toContainText('does not contain the token program');
  await expect(page.getByTestId('not-tradable-page')).toContainText(`${QUOTE.slice(0, 4)}…${QUOTE.slice(-4)}`);

  await page.unroute(/\/v1\/tokens(\?.*)?$/);
  // orders claim several programs: a token output the node confirms decides it
  await openListed(page, [QUOTE], { [QUOTE]: { template_hash: null, template_id: undefined, program_ambiguous: true } });
  await page.reload();
  await expect(page.getByTestId('pair-page')).toBeVisible();
  // and with none the page says so, after waiting for that answer
  await page.route(/\/v1\/token-utxos(\?.*)?$/, (route) => route.fulfill({ status: 200, contentType: 'application/json', body: '{"items":[],"next_cursor":null}' }));
  await page.reload();
  await expect(page.getByTestId('not-tradable-page')).toContainText('more than one token program');
});

test('a token that cannot be read now is a clear message, and the pair opens when it can', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock);
  await listExkcc(page);
  await openListed(page, [QUOTE]);
  let down = true;
  await page.route(new RegExp(`/v1/books/${QUOTE}(\\?.*)?$`), (route) => (down ? route.fulfill({ status: 500, contentType: 'application/json', body: '{"error":{"code":"internal","message":"down"}}' }) : route.continue()));
  await watchNotFound(page);
  await page.goto(`/#/market/${base}/${QUOTE}`);
  const box = page.getByTestId('pair-token-unavailable');
  await expect(box).toBeVisible({ timeout: 20_000 });
  await expect(box.getByTestId('pair-token-unavailable-reason')).toHaveAttribute('data-token', QUOTE);
  await expect(box.getByTestId('pair-token-unavailable-reason')).toContainText("orders could not be read from the indexer");
  expect(await sawNotFound(page)).toBe(false);
  down = false;
  await page.reload();
  await expect(page.getByTestId('pair-page')).toBeVisible();
});

test('a market scale this build cannot read says so', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock);
  await listExkcc(page);
  await openListed(page, [QUOTE], { [QUOTE]: { market_scale: 3 } });
  await page.goto(`/#/market/${base}/${QUOTE}`);
  await expect(page.getByTestId('pair-token-unavailable-reason')).toContainText('price scale its orders use');
});

test('an id nothing identifies: not available, with the fact that is missing, and no word of a registry', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock);
  await listExkcc(page);
  await page.goto(`/#/market/${base}/${'ab'.repeat(32)}`);
  const box = page.getByTestId('pair-not-found');
  await expect(box).toBeVisible();
  await expect(box.getByTestId('pair-public-reason')).toHaveCount(1);
  await expect(box).not.toContainText('registry');
});

test('the selectors: an unregistered token is offered next to a token, and a pair of one no list names keeps it among the choices', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock, { usd: 1n });
  await listExkcc(page);
  await openListed(page, [QUOTE]);
  await page.goto(`/#/market/${base}`);
  await page.getByTestId('market-quote-select').selectOption(QUOTE);
  await expect(page).toHaveURL(new RegExp(`#/market/${base}/${QUOTE}$`));
  await expect(page.getByTestId('pair-page')).toBeVisible();

  // no token list names it: the node confirms the outputs of it the indexer names, and the page keeps it selected
  await page.unroute(/\/v1\/tokens(\?.*)?$/);
  await openListed(page, [QUOTE], { [QUOTE]: null });
  await page.reload();
  await expect(page.getByTestId('pair-page')).toBeVisible();
  await expect(page.getByTestId('market-quote-select')).toHaveValue(QUOTE);
  await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', `EXKCC/${long(QUOTE)}`);
});

test('a flip of an open-list pair carries the order across, in the decimals of each token', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock, { usd: 10n });
  await listExkcc(page);
  await openListed(page, [QUOTE]);
  await openPage(page, base);
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { price: '0.05', amount: '2' });
  await page.getByTestId('market-flip').click();
  await expect(page).toHaveURL(new RegExp(`#/market/${QUOTE}/${base}$`));
  const ticket = page.getByTestId('order-ticket');
  await expect(ticket).toHaveAttribute('data-side', 'buy');
  await expect(page.getByTestId('order-price')).toHaveValue('20');
  await expect(page.getByTestId('order-amount')).toHaveValue('0.1');
});

test('standalone mode: an open-list pair has no ticket (its decimals cannot be checked without the indexer)', async ({ appPage: page, mock }) => {
  test.setTimeout(180_000);
  const base = await seedPair(mock, { usd: 1n });
  await emptyRegistry(page);
  await openListed(page, [base, QUOTE]);
  await openPage(page, base);
  await mock.post('/mock/indexer', { down: true });
  await expect(page.getByTestId('banner-standalone')).toBeVisible({ timeout: 90_000 });
  await expect(page.getByTestId('order-ticket')).toHaveCount(0, { timeout: 30_000 });
  await mock.post('/mock/indexer', { down: false });
});

test('a favourite pair of open-list tokens is on the list and leads to its page', async ({ appPage: page, mock }) => {
  const base = await seedPair(mock);
  await emptyRegistry(page);
  await openListed(page, [base, QUOTE], { [base]: { market_scale: 100_000_000 }, [QUOTE]: { market_scale: 1_000_000 } });
  await seedFavorites(page, { pairs: [{ base, quote: QUOTE }] });
  await page.goto('/#/market');
  await page.getByTestId('token-list-favorites').click();
  const link = page.getByTestId('pair-table').locator(`a[href="#/market/${base}/${QUOTE}"]`);
  await expect(link).toHaveText(`${long(base)} / ${long(QUOTE)}`, { timeout: 20_000 });
  await link.click();
  await expect(page.getByTestId('pair-page')).toBeVisible();
});

test.describe('the private-key signer', () => {
  test.use({ walletId: null });

  test('its frame reads the order on the scales the order names and names a quote without a ticker by its id', async ({ appPage: page, mock }) => {
    const base = await seedPair(mock);
    await emptyRegistry(page);
    await openListed(page, [base, QUOTE]);
    await page.goto(`/#/market/${base}/${QUOTE}`);
    await expect(page.getByTestId('pair-page')).toBeVisible();
    await page.getByTestId('wallet-connect-local-key').click();
    const frame = page.frameLocator('[data-testid="signer-frame"]');
    await frame.getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await frame.getByTestId('signer-connect').click();
    await expect(page.getByTestId('signer-overlay')).toBeHidden();
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { price: '0.0515', amount: '2' });
    await waitReviewable(page);
    await clickPlace(page);
    await expect(page.getByTestId('signer-overlay')).toBeVisible();
    const receipt = await readReceipt(page);
    expect(receipt.from).toBe('signer');
    expect(receipt.orders[0]!.rows).toMatchObject({
      Pair: `sells ${long(base)} for ${long(QUOTE)} (prices in ${long(QUOTE)} per ${long(base)})`,
      'Base tokens locked in the order': `2 ${long(base)}`,
      'Limit price': `0.0515 ${long(QUOTE)} per ${long(base)}`,
    });
    await expect(frame.getByTestId('signer-problems')).toHaveCount(0);
    await confirmReceipt(page);
    const placed = await placedBy(mock, await waitSubmitted(page));
    expect(placed.views[0].contract).toBe('KobPair');
  });
});
