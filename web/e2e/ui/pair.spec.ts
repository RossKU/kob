// Token/token pairs against the mock stack: the pair seed (`POST /mock/seed {pair: true}`: EXKCC/EXUSD with resting KobPair asks and a bid, a
// buy-first KobIfdPair entry, a KobCondPair sell stop and an EXUSD KAS book), reached from the token page's quote picker. The pair page shows the pair
// book (direct pair orders, if-done entries at their limit, the route through the two KAS books) and the UNIFIED order ticket with EXUSD as its quote:
// every order type of the KAS ticket, prices typed in EXUSD per EXKCC, tips in KAS. Pair orders go through the decoded confirmation screen, appear in
// My orders with their pair, what they hold of each token and (stops) the pair trigger rule, and are cancelled (custodies back to the wallet).
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { TOKEN, connectWallet, fund } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { acknowledgeAndSign, fillFields, openReview, pickType, placedBy, readConfirm, readDisclosure, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

const QUOTE = 'e7'.repeat(32);
/** EXUSD has 6 decimals: base units per whole EXUSD */
const USD = 1_000_000n;

/** The example registry with EXKCC listed and a second listed KCC-20, EXUSD (6 decimals), matching the mock pair seed. */
async function listPair(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  exkcc.status = 'listed';
  exkcc.verified = true;
  reg.tokens.push({ ...exkcc, ticker: 'EXUSD', name: 'Example USD (fictional)', covenant_id: QUOTE, decimals: 6, display: { description: 'Fictional pair quote token of the e2e mock.' } });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

async function openPair(page: Page, mock: MockClient, o: { usd?: bigint } = {}): Promise<string> {
  await mock.seed({ pair: true });
  await fund(mock, 'alice');
  if (o.usd) await mock.giveTokens('alice', o.usd * USD, { token: 'EXUSD' });
  await listPair(page);
  const base = (await mock.token()).covenant_id;
  await page.goto(`/#/market/${base}`);
  await connectWallet(page);
  // the token page's quote picker leads to the pair view
  await page.getByTestId('market-quote-select').selectOption(QUOTE);
  await expect(page).toHaveURL(new RegExp(`#/market/${base}/${QUOTE}$`));
  await expect(page.getByTestId('market-pair')).toHaveAttribute('data-pair', 'EXKCC/EXUSD');
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  return base;
}

const balanceOf = async (mock: MockClient, ticker: string) => BigInt((await mock.balance('alice')).tokens[ticker] ?? '0');

test('pair book (direct, entry, route) and a pair limit sell through the unified ticket, the confirmation screen, My orders and cancel', async ({ appPage: page, mock }) => {
  await openPair(page, mock);
  const book = page.getByTestId('pair-book');
  await expect(book).toBeVisible();
  // resting pair orders (direct), the buy-first if-done entry resting at its limit (entry) and the route through both KAS books ("via KAS")
  await expect(book.locator('[data-testid="pair-level"][data-source="direct"][data-side="ask"]').last()).toHaveAttribute('data-price', '0.0505');
  await expect(book.locator('[data-testid="pair-level"][data-source="direct"][data-side="bid"]').first()).toHaveAttribute('data-price', '0.0495');
  await expect(book.locator('[data-testid="pair-level"][data-source="entry"][data-side="bid"]')).toHaveAttribute('data-price', '0.049');
  await expect(book.getByTestId('pair-entry')).toHaveText('entry (1)');
  await expect(book.locator('[data-testid="pair-level"][data-source="route"]').first()).toBeVisible();
  await expect(book.getByTestId('pair-via-kas').first()).toHaveText('via KAS');
  // the spread line is the KAS page's: mid, signed spread, signed percent (negative when the book is crossed)
  await expect(book.getByTestId('pair-spread')).toHaveText(/^Mid [\d.,]+ · Spread -?[\d.,]+ \(-?[\d.]+%\)$/);
  // fixed decimals per column (decimal points line up)
  const decimals = (texts: string[]) => new Set(texts.map((x) => (x.includes('.') ? x.split('.')[1]!.length : 0)));
  const priceTexts = await book.locator('[data-testid="pair-level"] .book-price').allTextContents();
  expect(priceTexts.length).toBeGreaterThan(3);
  expect(decimals(priceTexts).size).toBe(1);

  // the unified ticket: a limit sell of 2 EXKCC at 0.0515 EXUSD each (at least), prices typed in EXUSD per EXKCC
  const ticket = page.getByTestId('order-ticket');
  await expect(ticket).toHaveAttribute('data-type', 'limit');
  await pickType(page, 'limit', 'sell');
  await expect(ticket.getByTestId('order-balances')).toContainText('EXUSD');
  await fillFields(page, { price: '0.0515', amount: '2' });
  await waitReviewable(page);
  const d = await readDisclosure(page);
  expect(d.rows.limit).toContain('0.0515 EXUSD / EXKCC');
  expect(d.rows.pairEscrowA).toContain('2 EXKCC');
  expect(d.rows.pairReceiveMinB).toContain('0.103 EXUSD');
  expect(d.notes).toEqual(expect.arrayContaining(['pairPricesFromKasBooks', 'pairTipKas', 'pairRoute', 'pairNetting', 'pairInventory']));
  expect(d.text).toContain('pair fills count only as volume');

  const confirm = await openReview(page);
  expect(confirm.kind).toBe('create');
  const card = confirm.created[0]!;
  expect(card.title).toBe('Sell limit order');
  expect(card.rows.pairPair).toContain('sells EXKCC for EXUSD');
  expect(card.rows.pairEscrowA).toContain('2 EXKCC');
  expect(card.rows.pairTotal).toContain('0.103 EXUSD');
  expect(card.rows.price).toContain('0.0515 EXUSD / EXKCC');
  const txid = await acknowledgeAndSign(page);
  const placed = await placedBy(mock, txid);
  expect(placed.views).toHaveLength(1);
  const view = placed.views[0];
  expect(view.contract).toBe('KobPair');
  // the price is EXUSD base units per whole EXKCC: 0.0515 EXUSD = 51_500
  expect(view.state.state).toMatchObject({ side: '1', price: '51500', amountLeft: String(2n * TOKEN), custody: String(2n * TOKEN), tCovId: QUOTE });
  expect(await balanceOf(mock, 'EXKCC')).toBe(98n * TOKEN);
  await page.getByTestId('confirm-close').click();
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  // the new direct ask shows up in the pair book (book:<EXKCC> / book:<EXUSD> trigger the refetch)
  await expect(book.locator('[data-testid="pair-level"][data-source="direct"][data-price="0.0515"]')).toHaveCount(1, { timeout: 15_000 });

  // My orders: the pair, its price in EXUSD per EXKCC, the EXKCC it holds; cancel returns the custody
  await page.getByTestId('pair-my-orders').click();
  await expect(page.getByTestId('orders-list')).toBeVisible();
  const id = view.covenant_id as string;
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row).toHaveAttribute('data-type', 'limit');
  await expect(row).toHaveAttribute('data-side', 'sell');
  await expect(row.getByTestId('order-pair')).toHaveText('EXKCC/EXUSD');
  await expect(row.getByTestId('order-price')).toContainText('0.0515 EXUSD/EXKCC');
  await expect(row.getByTestId('order-pair-escrow-a')).toHaveText('2 EXKCC');
  await expect(row.getByTestId('order-allin')).toHaveCount(0);
  await page.getByTestId(`order-cancel-${id}`).click();
  const [step] = await runFlow(page, mock, null, { steps: 1 });
  expect(step!.confirm.kind).toBe('cancel');
  expect(step!.confirm.closed[0]!.title).toBe('Cancel: Sell limit order');
  expect(step!.placed.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
  expect((await mock.order(id)).status).toBe('cancelled');
  expect(await balanceOf(mock, 'EXKCC')).toBe(100n * TOKEN);
});

test('a click on a pair book level prefills the ticket; a pair limit buy escrows EXUSD and is cancelled back', async ({ appPage: page, mock }) => {
  await openPair(page, mock, { usd: 10n });
  const book = page.getByTestId('pair-book');
  // the best ask (0.0505 EXUSD per EXKCC): a buy at that price
  await book.locator('[data-testid="pair-level"][data-source="direct"][data-side="ask"]').last().click();
  const ticket = page.getByTestId('order-ticket');
  await expect(ticket).toHaveAttribute('data-side', 'buy');
  await expect(page.getByTestId('order-price')).toHaveValue('0.0505');
  // below the book: a resting bid (no crossing auction), 1 EXKCC at 0.048 EXUSD
  await fillFields(page, { price: '0.048', amount: '1' });
  await waitReviewable(page);
  const d = await readDisclosure(page);
  expect(d.rows.pairEscrowB).toContain('EXUSD');
  expect(d.rows.pairPayMaxB).toContain('0.048 EXUSD');
  const confirm = await openReview(page);
  expect(confirm.created[0]!.title).toBe('Buy limit order');
  expect(confirm.created[0]!.rows.pairPair).toContain('buys EXKCC with EXUSD');
  expect(confirm.created[0]!.rows.pairTotal).toContain('0.048 EXUSD');
  const placed = await placedBy(mock, await acknowledgeAndSign(page));
  await page.getByTestId('confirm-close').click();
  const view = placed.views[0];
  expect(view.contract).toBe('KobPair');
  expect(view.state.state).toMatchObject({ side: '2', price: '48000', amountLeft: String(TOKEN), sCovId: QUOTE });
  const escrow = BigInt(view.state.state.custody);
  expect(escrow).toBeGreaterThanOrEqual(48_000n);
  expect(await balanceOf(mock, 'EXUSD')).toBe(10n * USD - escrow);

  await page.getByTestId('pair-my-orders').click();
  const id = view.covenant_id as string;
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row).toHaveAttribute('data-side', 'buy');
  await expect(row.getByTestId('order-pair-escrow-b')).toContainText('EXUSD');
  await page.getByTestId(`order-cancel-${id}`).click();
  const [step] = await runFlow(page, mock, null, { steps: 1 });
  expect(step!.placed.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
  expect(await balanceOf(mock, 'EXUSD')).toBe(10n * USD);
});

test('a pair stop discloses the trigger rule (two KAS books or a resting pair order) in the ticket, the confirmation screen and My orders', async ({ appPage: page, mock }) => {
  await openPair(page, mock);
  await pickType(page, 'stopMarket', 'sell');
  await fillFields(page, { amount: '1', stop: '0.046' });
  await waitReviewable(page);
  const d = await readDisclosure(page);
  const rule = 'arms on fills implying a rate at or below 0.046 EXUSD / EXKCC: a resting sell of EXKCC and buy of EXUSD filled together (each rested 5 s), or a resting EXKCC pair sell at or below the stop';
  expect(d.rows.pairTrigger).toContain(rule);
  expect(d.notes).toContain('pairTrigger');
  const confirm = await openReview(page);
  expect(confirm.created[0]!.title).toBe('Sell stop order');
  expect(confirm.created[0]!.rows.pairTrigger).toContain(rule);
  const placed = await placedBy(mock, await acknowledgeAndSign(page));
  await page.getByTestId('confirm-close').click();
  const view = placed.views[0];
  expect(view.contract).toBe('KobCondPair');
  expect(view.state.state).toMatchObject({ side: '1', stopPrice: '46000', armed: '0' });

  await page.getByTestId('pair-my-orders').click();
  const id = view.covenant_id as string;
  const row = page.getByTestId(`order-row-${id}`);
  await expect(row).toHaveAttribute('data-type', 'stop');
  await expect(row.getByTestId('order-stop')).toContainText('0.046 EXUSD/EXKCC');
  await expect(row.getByTestId('order-pair-trigger')).toContainText('arms on fills implying a rate at or below 0.046 EXUSD/EXKCC');
  // pair orders are replaced from the full pair ticket (no in-place amend in the protocol)
  await expect(row.getByTestId(`order-replace-${id}`)).toBeVisible();
  await page.getByTestId(`order-cancel-${id}`).click();
  const [step] = await runFlow(page, mock, null, { steps: 1 });
  expect(step!.placed.submission.closed).toEqual([{ covenantId: id, entry: 'cancel', status: 'cancelled' }]);
});

test('an IFD pair (buy first): the exit custody in the ticket, the entry in My orders, a fill books its exit as one position, the position cancel', async ({ appPage: page, mock }) => {
  await openPair(page, mock, { usd: 10n });
  await pickType(page, 'ifd', 'buy');
  await fillFields(page, { amount: '2', price: '0.048', 'exit.takeProfit': '0.055' });
  await waitReviewable(page);
  const d = await readDisclosure(page);
  expect(d.rows.pairExitCustody).toContain('the EXKCC its entry fill bought');
  expect(d.rows.pairEscrowB).toContain('EXUSD');
  expect(d.notes).toContain('pairExitCustody');
  const confirm = await openReview(page);
  expect(confirm.created[0]!.title).toBe('Buy IFD entry');
  expect(confirm.created[0]!.children[0]!.rows.pairPair).toContain('sells EXKCC for EXUSD');
  const placed = await placedBy(mock, await acknowledgeAndSign(page));
  await page.getByTestId('confirm-close').click();
  const entry = placed.views[0];
  expect(entry.contract).toBe('KobIfdPair');
  const id = entry.covenant_id as string;

  // a fill of 1 EXKCC books the entry's exit (a KobCondPair take-profit holding the bought EXKCC)
  await mock.fill(id, TOKEN);
  await page.getByTestId('pair-my-orders').click();
  const position = page.locator(`[data-testid^="position-"][data-kind]`).first();
  await expect(position).toBeVisible({ timeout: 15_000 });
  await expect(position.getByTestId('position-token')).toHaveText('EXKCC/EXUSD');
  await expect(position.getByTestId('position-entry-price')).toContainText('0.048 EXUSD/EXKCC');
  await expect(position.getByTestId('position-exit-prices')).toContainText('0.055 EXUSD/EXKCC');
  await expect(position.getByTestId(`order-row-${id}`)).toHaveAttribute('data-type', 'ifd');
  // the exit holds the EXKCC the fill bought
  await expect(position.locator('[data-testid^="order-row-"][data-type="take-profit"] [data-testid="order-pair-escrow-a"]')).toHaveText('1 EXKCC');
  await position.locator('[data-testid^="position-cancel-"]').click();
  const [step] = await runFlow(page, mock, null, { steps: 1 });
  expect(step!.confirm.kind).toBe('cancel-position');
  expect(step!.placed.submission.closed.map((c: { status: string }) => c.status)).toEqual(['cancelled', 'cancelled']);
  // the escrow left and the exit's EXKCC are back
  expect(await balanceOf(mock, 'EXKCC')).toBe(101n * TOKEN);
});

test('the pair ticket offers every order type of the KAS ticket in the same layout (prices in EXUSD per EXKCC, tips in KAS)', async ({ appPage: page, mock }) => {
  const base = await openPair(page, mock, { usd: 10n });
  const types = async () => page.getByTestId('order-type').locator('option').evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value));
  const pairTypes = await types();
  expect(pairTypes.length).toBeGreaterThan(15);
  const labels = async () => (await page.getByTestId('order-ticket').locator('.tk-form > .field > label').allTextContents());
  const pairLabels = await labels();
  // the price is typed in EXUSD per EXKCC, the tip in KAS per EXKCC
  await expect(page.getByTestId('order-ticket').locator('.tk-form')).toContainText('EXUSD / EXKCC');
  await expect(page.getByTestId('order-ticket').locator('.tk-form')).toContainText('KAS / EXKCC');
  // a pair market buy / sell, an OCO and a trailing stop plan on the pair too
  for (const [type, side, values] of [
    ['market', 'buy', { amount: '1' }],
    ['oco', 'sell', { amount: '1', takeProfit: '0.06', stop: '0.045' }],
    ['trailingStop', 'sell', { amount: '1', stop: '0.045', 'trail.step': '0.001', 'trail.gap': '0.002' }],
    ['dutch', 'sell', { amount: '1', price: '0.07', priceEnd: '0.06', duration: '10' }],
  ] as const) {
    await pickType(page, type, side);
    await fillFields(page, values);
    await waitReviewable(page);
    expect((await readDisclosure(page)).notes).toContain('pairPricesFromKasBooks');
  }
  // the KAS ticket of the same token lists the same order types and the same labels (units in brackets apart)
  await page.goto(`/#/market/${base}`);
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  expect(await types()).toEqual(pairTypes);
  const strip = (xs: string[]) => xs.map((x) => x.replace(/ \(.*\)$/, ''));
  expect(strip(await labels())).toEqual(strip(pairLabels));
});

test('without the pair routes the view says "pair book unavailable" and still plans pair orders', async ({ appPage: page, mock }) => {
  await page.route('**/v1/pairs/**', (route) => route.fulfill({ status: 404, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: '{"error":{"code":"not_found","message":"no such route"}}' }));
  await openPair(page, mock);
  await expect(page.getByTestId('pair-book-unavailable')).toBeVisible();
  await pickType(page, 'limit', 'buy');
  await fillFields(page, { price: '0.049', amount: '1' });
  // buying EXKCC pays EXUSD: alice holds none, so the planner reports the shortfall in EXUSD (the ticket still works without the pair book)
  await expect(page.getByTestId('order-issue-PAIR_INSUFFICIENT_TOKENS')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId('order-issue-PAIR_INSUFFICIENT_TOKENS')).toContainText('Not enough EXUSD');
  await expect(page.getByTestId('order-review')).toBeDisabled();
});

test('picking a quote token keeps the layout: the chart stays in the same place and size, the book and the ticket keep their areas', async ({ appPage: page, mock }) => {
  await mock.seed({ pair: true });
  await fund(mock, 'alice');
  await listPair(page);
  const base = (await mock.token()).covenant_id;
  await page.goto(`/#/market/${base}`);
  await connectWallet(page);
  await expect(page.getByTestId('chart-section')).toBeVisible();
  await expect(page.getByTestId('price-chart')).toBeVisible();
  const before = (await page.getByTestId('chart-section').boundingBox())!;
  const beforeBox = (await page.getByTestId('price-chart').locator('.chart-box').boundingBox())!;
  await page.getByTestId('market-quote-select').selectOption(QUOTE);
  await expect(page).toHaveURL(new RegExp(`#/market/${base}/${QUOTE}$`));
  await expect(page.getByTestId('chart-section')).toBeVisible();
  await expect(page.getByTestId('price-chart')).toBeVisible();
  const after = (await page.getByTestId('chart-section').boundingBox())!;
  const afterBox = (await page.getByTestId('price-chart').locator('.chart-box').boundingBox())!;
  expect(afterBox.height).toBe(beforeBox.height);
  expect(Math.abs(after.width - before.width)).toBeLessThan(2);
  expect(Math.abs(after.x - before.x)).toBeLessThan(2);
  // the book and the ticket keep their areas: book left of the chart, ticket right of it (three columns on a wide screen)
  const book = (await page.getByTestId('pair-book-section').boundingBox())!;
  const ticket = (await page.getByTestId('order-ticket').boundingBox())!;
  expect(book.x + book.width).toBeLessThanOrEqual(after.x + 1);
  expect(ticket.x).toBeGreaterThanOrEqual(after.x + after.width - 1);
  // the pair chart is derived from the two KAS markets, in the pair's unit
  await expect(page.getByTestId('chart-unit')).toHaveText('EXUSD per EXKCC');
  await expect(page.getByTestId('pair-chart-note')).toContainText('pair fills count only as volume');
  await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', /^(ready|empty)$/);
});
