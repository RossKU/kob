// Field help of the ticket wraps (a phone has no hover tooltip) instead of ending in an ellipsis. Mock stack.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { pickType } from '../helpers/ticket';

test.use({ autoOpen: false });

test.describe('phone, 390 px wide', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('field help is wrapped, never cut with an ellipsis', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'stopMarket', 'sell'); // the longest help texts
    const cut = await page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>('.tk-help, .tk-sub')].map((el) => ({ text: (el.textContent ?? '').slice(0, 40), ws: getComputedStyle(el).whiteSpace, over: el.scrollWidth - el.clientWidth })).filter((r) => r.ws === 'nowrap' || r.over > 1),
    );
    expect(cut).toEqual([]);
    const help = page.getByTestId('order-type-help');
    const lines = await help.evaluate((el) => Math.round(el.getBoundingClientRect().height / parseFloat(getComputedStyle(el).lineHeight)));
    expect(lines, 'the type help takes more than one line and is shown whole').toBeGreaterThan(1);
  });
});
