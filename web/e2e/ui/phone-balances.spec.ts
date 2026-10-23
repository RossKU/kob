// My orders > balances on a phone: one card per token (the name on one line with its full name as the tooltip, every figure labelled, the buttons under them); a table on a desktop. Same test ids. Mock stack.
import { expect, test, type MockClient } from '../fixtures';
import { TESTID } from '../testids';
import { fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';

test.use({ autoOpen: false });

async function seedAsk(mock: MockClient) {
  const tok = await mock.token();
  await mock.seed({ orders: [{ token: tok.covenant_id, side: 'ask', maker: 'alice', price: 2_505_000, amount: (3n * 100_000_000n).toString() }] });
  return tok;
}

test.describe('phone, 390 px wide', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('My orders > balances: one card per token, every figure labelled, the name on one line with the full name as its tooltip', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const tok = await seedAsk(mock);
    await page.goto('/#/orders');
    const row = page.getByTestId(TESTID.balancesPanel).getByTestId(`balance-row-${tok.covenant_id}`);
    await expect(row).toBeVisible();
    expect(await row.evaluate((el) => getComputedStyle(el).display)).toBe('flex'); // a card, not a table row
    const name = row.getByTestId('balance-market');
    const label = (await name.textContent())!;
    await expect(name).toHaveAttribute('title', label); // the full name
    expect(await name.evaluate((el) => el.getBoundingClientRect().height < 2.2 * parseFloat(getComputedStyle(el).lineHeight))).toBe(true); // one line
    // Free / In your orders / Strays / KAS locked: title and figure
    const labels = await row.locator('td[data-label]').evaluateAll((tds) => tds.map((td) => getComputedStyle(td, '::before').content));
    expect(labels).toEqual(['"Free tokens"', '"In your orders"', '"Strays"', '"KAS locked in orders"']);
    await expect(row.getByTestId('balance-escrowed')).toHaveText(/^3/); // test ids and text unchanged
    const rb = (await row.boundingBox())!;
    expect(rb.x).toBeGreaterThanOrEqual(0);
    expect(rb.x + rb.width).toBeLessThanOrEqual(390 + 1);
    expect(await horizontalOverflow(page), 'My orders').toBe(0);
    // the cancel button sits under the figures, inside the card
    const cancel = row.getByTestId(`orders-cancel-token-${tok.covenant_id}`);
    await expect(cancel).toBeVisible();
    const cb = (await cancel.boundingBox())!;
    expect(cb.y).toBeGreaterThan((await row.getByTestId('balance-strays').boundingBox())!.y);
    expect(cb.x + cb.width).toBeLessThanOrEqual(rb.x + rb.width);
  });
});

test.describe('desktop, 1280 px wide', () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test('the balances stay a table', async ({ appPage: page, mock }) => {
    await fund(mock);
    const tokenId = await openMarket(page, mock);
    await seedAsk(mock);
    await page.goto('/#/orders');
    const row = page.getByTestId(TESTID.balancesPanel).getByTestId(`balance-row-${tokenId}`);
    await expect(row).toBeVisible();
    expect(await row.evaluate((el) => getComputedStyle(el).display)).toBe('table-row');
    await expect(page.getByTestId('token-balances-table').locator('thead')).toBeVisible();
  });
});
