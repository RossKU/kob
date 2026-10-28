// The book against the node (app/book-verify.ts): a market order is an auction that starts at the best price of the indexer's book, so before it is
// signed the orders at that touch must exist unspent on the NODE at the script their state derives. An indexer that lists a cheap ask nobody holds
// (a phantom) cannot make a buyer's auction start from it: the ticket plans from the real ask and says so; with an honest book nothing is shown.
import type { Page } from '@playwright/test';
import { test, expect, type MockClient } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, onChainTerms, pickType, placeOrder, settlePlan, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });

const PHANTOM = 'f0'.repeat(32);
const PHANTOM_TXID = 'f1'.repeat(32);

/**
 * The indexer lists, besides the real book, an ask at half the best ask that exists nowhere: its book row, and its order view (the real best ask's
 * state with that price, at an outpoint no node holds). Returns the price the phantom is listed at and the real best ask's.
 */
async function listPhantomAsk(page: Page, mock: MockClient): Promise<{ real: bigint; phantom: bigint }> {
  const tok = await mock.token();
  const book = await mock.get(`/v1/books/${tok.covenant_id}?aggregate=false&depth=100`);
  const best = book.asks[0];
  const real = BigInt(best.price);
  const phantom = real / 2n;
  const view = await mock.get(`/v1/orders/${best.covenant_id}`);
  const forged = JSON.parse(JSON.stringify(view));
  forged.covenant_id = PHANTOM;
  forged.price = phantom.toString();
  forged.state.state.price = phantom.toString();
  forged.current = { ...forged.current, txid: PHANTOM_TXID };
  const cors = { 'access-control-allow-origin': '*' };
  await page.route(`**/v1/books/${tok.covenant_id}*`, async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    if (body.aggregated === false) body.asks = [{ ...best, covenant_id: PHANTOM, price: phantom.toString(), quote: phantom.toString() }, ...body.asks];
    await route.fulfill({ response: res, json: body, headers: { ...res.headers(), ...cors } });
  });
  await page.route(`**/v1/orders/${PHANTOM}`, (route) => route.fulfill({ status: 200, contentType: 'application/json', headers: cors, body: JSON.stringify(forged) }));
  return { real, phantom };
}

test.describe('market orders start from a price the node confirms', () => {
  test('an honest book: nothing is shown, the order starts at the best ask', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '2' });
    await waitReviewable(page);
    await expect(page.getByTestId('order-note-BOOK_ORDERS_NOT_ON_NODE')).toHaveCount(0);
    const placed = await placeOrder(page, mock);
    expect(onChainTerms(placed)[0]!.state.price).toBe('2510000');
  });

  test('the indexer lists a phantom cheap ask: the ticket re-plans to the real ask, says so, and signs only that price', async ({ appPage: page, mock }) => {
    await fund(mock);
    const { real, phantom } = await listPhantomAsk(page, mock);
    expect(phantom).toBeLessThan(real);
    await openMarket(page, mock);
    await pickType(page, 'market', 'buy');
    await fillFields(page, { amount: '2' });
    await settlePlan(page);
    await waitReviewable(page);
    // one line, no checkbox: the order button stays a single click
    const note = page.getByTestId('order-note-BOOK_ORDERS_NOT_ON_NODE');
    await expect(note).toBeVisible();
    await expect(note).toContainText('not on the node');
    await expect(note).toContainText('0.0125'); // the phantom's price (0.0251 / 2 = 0.01255): named so the user sees what was left out
    // the auction starts at the real best ask, not at the phantom
    const placed = await placeOrder(page, mock);
    const terms = onChainTerms(placed)[0]!;
    expect(terms.state.price).toBe(real.toString());
    expect(BigInt(terms.state.price)).toBeGreaterThan(phantom);
  });

  test('a phantom sits at the touch of the bids: a market sell does not start from it', async ({ appPage: page, mock }) => {
    await fund(mock, 'alice', { tokens: 50n });
    const tok = await mock.token();
    const book = await mock.get(`/v1/books/${tok.covenant_id}?aggregate=false&depth=100`);
    const bestBid = book.bids[0];
    const view = await mock.get(`/v1/orders/${bestBid.covenant_id}`);
    const forged = JSON.parse(JSON.stringify(view));
    const higher = (BigInt(bestBid.price) * 2n).toString(); // a bid nobody holds, at twice the best bid: the best price a seller could "get"
    forged.covenant_id = PHANTOM;
    forged.price = higher;
    forged.state.state.price = higher;
    forged.current = { ...forged.current, txid: PHANTOM_TXID };
    const cors = { 'access-control-allow-origin': '*' };
    await page.route(`**/v1/books/${tok.covenant_id}*`, async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      if (body.aggregated === false) body.bids = [{ ...bestBid, covenant_id: PHANTOM, price: higher, quote: higher }, ...body.bids];
      await route.fulfill({ response: res, json: body, headers: { ...res.headers(), ...cors } });
    });
    await page.route(`**/v1/orders/${PHANTOM}`, (route) => route.fulfill({ status: 200, contentType: 'application/json', headers: cors, body: JSON.stringify(forged) }));
    await openMarket(page, mock);
    await pickType(page, 'market', 'sell');
    await fillFields(page, { amount: '2' });
    await settlePlan(page);
    await waitReviewable(page);
    await expect(page.getByTestId('order-note-BOOK_ORDERS_NOT_ON_NODE')).toBeVisible();
    const placed = await placeOrder(page, mock);
    expect(onChainTerms(placed)[0]!.state.price).toBe(bestBid.price);
  });
});
