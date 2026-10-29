// The cost preview under the amount box (founder, 2026-10-29): what the amount costs (a buy) or yields (a sell) in KAS, from the node-confirmed book the
// planner sees; "—" when there is nothing to say; "book covers only N" when the book is too thin; the line has a fixed height, so it never moves anything.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType } from '../helpers/ticket';

test.use({ autoOpen: false });

for (const [name, width] of [['desktop', 1280], ['phone', 390]] as const) {
  test(`${name} ${width} px: the line under the amount shows the cost or proceeds, "—" when empty, the thin-book wording, in a box that never changes`, async ({ appPage: page, mock }) => {
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    const line = page.getByTestId('order-cost-preview');
    const box = async () => (await line.boundingBox())!;
    const ticketBox = async () => (await page.getByTestId('order-ticket').boundingBox())!;

    // empty: a dash
    await pickType(page, 'limit', 'buy');
    await expect(line).toHaveText('—');
    const first = await box();
    const ticket0 = await ticketBox();
    // the line keeps its size always, and its place while the form keeps its fields (another order type has other fields above the amount)
    const sameSize = async () => {
      const b = await box();
      expect([b.width, b.height]).toEqual([first.width, first.height]);
    };

    // a limit order: amount x price, one value
    await fillFields(page, { price: '0.0245', amount: '3' });
    await expect(line).toHaveText(/^0\.0735 KAS$/);
    expect(await box()).toEqual(first);

    // a market buy: an estimate from the asks, and the bound the slippage allows beside it when they differ
    await pickType(page, 'market', 'buy');
    let here = await box();
    await sameSize();
    await fillFields(page, { amount: '2' });
    // (the plan lands a moment after the estimate: its slippage bound joins the line)
    await expect(line).toHaveText(/^≈ [\d,.]+ KAS · max [\d,.]+ KAS$/);
    expect(await box()).toEqual(here);

    // a market sell: the proceeds, with the least the order accepts
    await pickType(page, 'market', 'sell');
    here = await box();
    await sameSize();
    await fillFields(page, { amount: '2' });
    await expect(line).toHaveText(/^≈ [\d,.]+ KAS · min [\d,.]+ KAS$/);
    expect(await box()).toEqual(here);

    // a book too thin for the amount
    await fillFields(page, { amount: '1000000' });
    await expect(line).toHaveText(/^book covers only [\d,.]+ EXKCC$/);
    expect(await box()).toEqual(here);

    // cleared: a dash again, in the same box; the ticket is as wide and as far left as it was (the line has a reserved height)
    await fillFields(page, { amount: '' });
    await expect(line).toHaveText('—');
    expect(await box()).toEqual(here);
    await sameSize();
    const ticket1 = await ticketBox();
    expect(ticket1.x).toBe(ticket0.x);
    expect(ticket1.width).toBe(ticket0.width);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
  });
}
