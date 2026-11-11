// My orders > balances on a phone: one compact card per token (line 1 the name with its full name as the tooltip and its value at the right, line 2 the figures with their words, the buttons under them); a table on a desktop. Same test ids. Mock stack.
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

  test('My orders > balances: one compact card per token, the figures with their words, the name on one line with the full name as its tooltip', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const tok = await seedAsk(mock);
    await page.goto('/#/portfolio');
    const row = page.getByTestId(TESTID.balancesPanel).getByTestId(`balance-row-${tok.covenant_id}`);
    await expect(row).toBeVisible();
    expect(await row.locator('.dt-row').evaluate((el) => getComputedStyle(el).display)).toBe('flex'); // a card of lines, not a row of the table's columns
    const name = row.getByTestId('balance-market');
    const label = (await name.textContent())!;
    await expect(name).toHaveAttribute('title', label); // the full name
    expect(await name.evaluate((el) => el.getBoundingClientRect().height < 2.2 * parseFloat(getComputedStyle(el).lineHeight))).toBe(true); // one line
    // Free tokens / In your orders on the second line, the value at the right of the first; the strays and the KAS locked in orders are not on the card unless there are strays (here none)
    const shown = await row.locator('.dt-c[data-label]').evaluateAll((cells) => cells.filter((c) => getComputedStyle(c).display !== 'none').map((c) => getComputedStyle(c, '::before').content));
    expect(shown).toEqual(['"Free tokens"', '"In your orders"', '"Value (KAS): "']);
    const tops = await row.locator('.dt-c-available > span, .dt-c-inOrders').evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().top)));
    expect(new Set(tops).size, `the two figures stand on one line: ${tops}`).toBe(1);
    expect((await row.boundingBox())!.height, 'a compact card').toBeLessThan(120);
    await expect(row.getByTestId('balance-escrowed')).toHaveText(/^3/); // test ids and text unchanged
    const rb = (await row.boundingBox())!;
    expect(rb.x).toBeGreaterThanOrEqual(0);
    expect(rb.x + rb.width).toBeLessThanOrEqual(390 + 1);
    expect(await horizontalOverflow(page), 'My orders').toBe(0);
    // the cancel button sits under the figures, inside the card
    const cancel = row.getByTestId(`orders-cancel-token-${tok.covenant_id}`);
    await expect(cancel).toBeVisible();
    const cb = (await cancel.boundingBox())!;
    expect(cb.y).toBeGreaterThan((await row.getByTestId('balance-escrowed').boundingBox())!.y);
    expect(cb.x + cb.width).toBeLessThanOrEqual(rb.x + rb.width);
  });
});

test.describe('desktop, 1280 px wide', () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test('the balances stay a table', async ({ appPage: page, mock }) => {
    await fund(mock);
    const tokenId = await openMarket(page, mock);
    await seedAsk(mock);
    await page.goto('/#/portfolio');
    const row = page.getByTestId(TESTID.balancesPanel).getByTestId(`balance-row-${tokenId}`);
    await expect(row).toBeVisible();
    expect(await row.locator('.dt-row').evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length)).toBe(7); // a row of the table's seven columns, not a card
    await expect(page.getByTestId('token-balances-table').getByTestId('balances-head')).toBeVisible();
  });
});
