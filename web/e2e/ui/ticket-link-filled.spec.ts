// A link that fills the order ticket (`?ticket=ask&amount=..&price=..`) says so above the ticket.
// The note goes once the book or the form takes over. Mock stack.
import { mkdirSync } from 'node:fs';
import { expect, test } from '../fixtures';
import { openMarket } from '../helpers/env';

test.use({ autoOpen: false });

test('a limit sell filled in by a link carries a note; a plain token link does not', async ({ appPage: page, mock }) => {
  const id = await openMarket(page, mock, { route: undefined });
  await expect(page.getByTestId('ticket-link-filled')).toHaveCount(0);
  await page.goto('/#/market'); // the link opens the page afresh (as from Create token)
  await page.goto(`/#/market/${id}?ticket=ask&amount=100000000&price=1`);
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  const note = page.getByTestId('ticket-link-filled');
  await expect(note).toBeVisible();
  await expect(note).toContainText('A link filled in this sell order');
  const dir = process.env.KOB_SHOTS;
  if (dir) {
    mkdirSync(dir, { recursive: true });
    await page.screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-ticket-link-filled.png` });
  }
});

test('a market sell, buy or close filled in by a link carries a note too', async ({ appPage: page, mock }) => {
  const id = await openMarket(page, mock, { route: undefined });
  const dir = process.env.KOB_SHOTS;
  if (dir) mkdirSync(dir, { recursive: true });
  for (const [query, text, tag] of [
    ['ticket=sell&amount=100000000', 'A link filled in this market order', 'sell'],
    ['ticket=cover&amount=100000000', 'A link filled in this market order', 'cover'],
    ['ticket=close', 'A link set this ticket to sell all you hold', 'close'],
  ] as const) {
    await page.goto('/#/market');
    await page.goto(`/#/market/${id}?${query}`);
    await expect(page.getByTestId('order-ticket')).toBeVisible();
    if (dir) {
      await page.waitForTimeout(500);
      await page.getByTestId('ticket-container').screenshot({ path: `${dir}/${process.env.KOB_SHOT_TAG ?? 'shot'}-ticket-link-${tag}.png` });
    }
    const note = page.getByTestId('ticket-link-filled');
    await expect(note).toBeVisible();
    await expect(note).toContainText(text);
  }
});
