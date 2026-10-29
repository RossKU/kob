// Nothing in the token page moves on a tick (founder, 2026-10-29): the order ticket, its submit button and the order book keep their boxes while the book
// changes (levels come and go, fills), the book is pulled again (the refresh glyph spins), the plan is recalculated ("Calculating..." shows) and the
// ticket's facts change. An in-page sampler records the boxes every ~20 ms; each of them must have exactly one position and one size over the whole run.
import { expect, test } from '../fixtures';
import { fund, openMarket } from '../helpers/env';
import { fillFields, pickType } from '../helpers/ticket';

test.use({ autoOpen: false });

/** What is watched: the whole ticket, its title row and submit button, and the book's header, its two sides (their slot blocks), the spread row and the tools. */
/** (the trades tape and the stats strip too) */
const WATCHED = [
  'order-ticket',
  'order-place',
  'order-cost-preview',
  'order-amount-slider',
  'book-asks',
  'book-bids',
  'book-spread',
  'book-group',
  'book-refresh',
  'trades-list',
  'market-stats',
] as const;

test('the ticket, its button and the order book keep their boxes through ticks, level changes, refreshes and a "Calculating..." state', async ({ appPage: page, mock }) => {
  test.setTimeout(120_000);
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'limit', 'buy');
  await expect(page.getByTestId('book-asks').getByTestId('book-ask-row').first()).toBeVisible();

  // the sampler: the box of every watched element, and the counts that must not change the layout (levels, whether "Calculating..." is up)
  await page.evaluate((ids) => {
    const w = window as unknown as { __jitter: { boxes: Record<string, Set<string>>; levels: Set<string>; pending: number; samples: number; head: Set<string> } };
    w.__jitter = { boxes: Object.fromEntries(ids.map((i) => [i, new Set<string>()])), levels: new Set(), pending: 0, samples: 0, head: new Set() };
    setInterval(() => {
      const j = w.__jitter;
      j.samples++;
      for (const id of ids) {
        const el = document.querySelector(`[data-testid="${id}"]`);
        if (!el) continue;
        const r = el.getBoundingClientRect();
        j.boxes[id]!.add([r.x, r.y + window.scrollY, r.width, r.height].map((n) => Math.round(n * 2) / 2).join(','));
      }
      const asks = document.querySelectorAll('[data-testid="book-ask-row"]').length;
      const bids = document.querySelectorAll('[data-testid="book-bid-row"]').length;
      j.levels.add(`${asks}/${bids}`);
      if (document.querySelector('[data-testid="order-pending"]')) j.pending++;
      const head = document.querySelector('.ob-head');
      if (head) j.head.add(String(Math.round(head.getBoundingClientRect().height)));
    }, 20);
  }, [...WATCHED]);

  // slow answers, so a refresh and a re-plan last long enough to be seen (and the "Calculating..." indicator, which waits 500 ms, shows)
  await mock.latency(900, { jitterMs: 400, scope: 'indexer' });
  const token = await mock.token();
  const asks = await mock.orders(`token=${token.covenant_id}&status=active&limit=200`);
  const sells = asks.filter((o: { side: number }) => o.side === 1);

  // ticks: fills of the best asks, new levels on both sides (the number of levels changes), a user edit of the ticket in between
  await fillFields(page, { amount: '3', price: '0.0201' });
  for (let i = 0; i < 4; i++) {
    if (sells[i]) await mock.fill(sells[i].covenant_id, 1000n);
    await page.waitForTimeout(500);
    await mock.seed({ book: { levels: 1 + i, mid: 2_500_000 + 200_000 * (i + 1), tick: 7_000 + i * 1_000 } });
    await page.waitForTimeout(700);
    await fillFields(page, { amount: String(3 + i), price: '0.0201' });
    await page.getByTestId('book-refresh').click();
    await page.waitForTimeout(900);
  }
  await mock.seed({ book: { levels: 4, mid: 2_300_000, tick: 6_000 } });
  await page.waitForTimeout(2_500);
  await mock.latency(0);

  const seen = await page.evaluate(() => {
    const j = (window as unknown as { __jitter: { boxes: Record<string, Set<string>>; levels: Set<string>; pending: number; samples: number; head: Set<string> } }).__jitter;
    return { boxes: Object.fromEntries(Object.entries(j.boxes).map(([k, v]) => [k, [...v]])), levels: [...j.levels], pending: j.pending, samples: j.samples, head: [...j.head] };
  });
  expect(seen.samples).toBeGreaterThan(100);
  // the run really changed things: the number of levels moved, and the "Calculating..." state was up for a while
  expect(seen.levels.length, `levels per side seen: ${seen.levels.join(' ')}`).toBeGreaterThan(1);
  expect(seen.pending, 'the "Calculating..." indicator was never shown').toBeGreaterThan(0);
  // and nothing moved or changed size
  for (const id of WATCHED) expect(seen.boxes[id], `${id} boxes seen: ${seen.boxes[id]!.join(' | ')}`).toHaveLength(1);
  expect(seen.head, 'the book header changed height').toHaveLength(1);
});
