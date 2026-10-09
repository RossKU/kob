// Order lifetime and ladder features of the ticket (mock KasWare wallet, default mock seed, EXKCC listed as tradable), each through the real UI to
// the mock node:
//   * exit lifetime of IFD / IFO exits (good till cancelled or until a date): the disclosure row, the decoded exit card, the exit template the
//     entry commits to and the exit order a fill creates;
//   * timed activation (`activeFrom`) of conditional orders and if-done entries: the disclosure, the confirmation screen and the on-chain state;
//   * the repeat ladder: several repeat IFD levels from one ticket, each its own confirmation, signature and transaction.
// Prices are KAS per EXKCC, amounts EXKCC (1e8 base units); state prices are sompi per whole token (0.026 KAS = 2_600_000).
import { test, expect } from '../fixtures';
import { browserTzOffset, fund, mockClock, openMarket } from '../helpers/env';
import { acknowledgeAndSign, fillFields, openReview, pickType, placedBy, readConfirm, readDisclosure, reviewAndSign, waitReviewable, type FieldValues } from '../helpers/ticket';

test.use({ autoOpen: false });

/** 90 days at 10 DAA/s */
const GTC_DAA = 77_760_000;
/** the "never" a good-till-cancelled exit commits to in its template (cond-common.ts GTC_EXIT_EXPIRY_DAA): its 90 days start when a fill creates it */
const GTC_EXIT_EXPIRY_DAA = 1n << 62n;
/** the 8-byte little-endian integer push of the state codec: 08 <u64 le> */
const pushInt = (v: bigint): string => '08' + Array.from({ length: 8 }, (_, i) => Number((v >> BigInt(8 * i)) & 0xffn).toString(16).padStart(2, '0')).join('');
const two = (n: number) => String(n).padStart(2, '0');
const utcText = (unix: number): string => `${new Date(unix * 1000).toISOString().slice(0, 16).replace('T', ' ')} UTC`;
const jstText = (unix: number): string => {
  const d = new Date((unix + 9 * 3600) * 1000);
  return `${d.getUTCFullYear()}-${two(d.getUTCMonth() + 1)}-${two(d.getUTCDate())} ${two(d.getUTCHours())}:${two(d.getUTCMinutes())} JST`;
};
/** `datetime-local` text of a unix time in the browser's wall clock (minute precision, like the input) */
const localText = (unix: number, tz: number): string => {
  const d = new Date((unix - tz * 60) * 1000);
  return `${d.getUTCFullYear()}-${two(d.getUTCMonth() + 1)}-${two(d.getUTCDate())}T${two(d.getUTCHours())}:${two(d.getUTCMinutes())}`;
};
/** the planner converts a wall-clock moment to DAA with the measured rate: 1% of the distance plus the planning lag */
const slack = (secs: number) => Math.round(secs * 10 * 0.01) + 600;
const daaAt = (placed: { daa: number; unix: number }, target: number) => placed.daa + (target - placed.unix) * 10;

test.describe('exit lifetime (R-10)', () => {
  const cases: { name: string; type: string; side: 'buy' | 'sell'; fields: FieldValues; exitKind: string; exitTitle: string }[] = [
    { name: 'IFD buy-first', type: 'ifd', side: 'buy' as const, fields: { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' }, exitKind: 'KobCondAsk', exitTitle: 'Exit created after each entry fill: Sell take-profit order' },
    { name: 'IFO sell-first', type: 'ifo', side: 'sell' as const, fields: { amount: '2', price: '0.026', 'exit.takeProfit': '0.0245', 'exit.stop': '0.027' }, exitKind: 'KobCondBid', exitTitle: 'Exit created after each entry fill: Buy OCO order (take-profit and stop)' },
  ];
  for (const c of cases) {
    test(`${c.name}: an exit with an end date commits that date and the exit a fill creates ends on it`, async ({ appPage: page, mock }) => {
      await fund(mock);
      await openMarket(page, mock);
      const clock = await mockClock(mock);
      const tz = await browserTzOffset(page);
      await pickType(page, c.type, c.side);
      await fillFields(page, c.fields);
      await waitReviewable(page);
      // default: good till cancelled, counted from each exit's creation
      await expect(page.getByTestId('field-exit.lifetime')).toHaveValue('gtc');
      let disc = await readDisclosure(page);
      expect(disc.rows.exitLifetime).toContain('Until cancelled (refundable after 90 days idle)');
      expect(disc.notes).toContain('exitGtc');

      // until a date 5 days ahead (the input has minute precision)
      const target = Math.floor((clock.unix + 5 * 86_400) / 60) * 60;
      await fillFields(page, { 'exit.lifetime': 'gtd', 'exit.lifetimeAt': localText(target, tz) });
      await waitReviewable(page);
      await expect(page.getByTestId('field-exit.lifetimeAt')).toBeVisible();
      disc = await readDisclosure(page);
      expect(disc.rows.exitLifetime).toContain(`Until ${utcText(target)} (${jstText(target)})`);
      expect(disc.notes).toContain('exitGtd');
      expect(disc.notes).not.toContain('exitGtc');

      // the decoded exit card reads the date out of the template the entry commits to
      const confirm = await openReview(page);
      const card = confirm.created[0]!;
      expect(card.children).toHaveLength(1);
      expect(card.children[0]!.title).toBe(c.exitTitle);
      expect(card.children[0]!.rows.expiry).toContain(utcText(target));
      expect(card.children[0]!.rows.expiry).not.toContain('when the exit is created');
      const txid = await acknowledgeAndSign(page);
      const placed = await placedBy(mock, txid);
      await page.getByTestId('confirm-close').click();
      const at = { daa: placed.submission.daa, unix: Math.floor(Date.parse(placed.submission.at) / 1000) };
      const entry = placed.views[0];
      // the entry itself stays good till cancelled (90 days from placement); only its exits end on the date
      expect(Math.abs(Number(entry.state.state.expiryDaa) - at.daa - GTC_DAA)).toBeLessThan(5_000);
      const tpl: string = entry.state.state.exitState;
      expect(tpl, 'the exit template no longer carries the GTC "never"').not.toContain(pushInt(GTC_EXIT_EXPIRY_DAA));

      // a fill of the entry creates the exit: it ends on the chosen date (the DAA the planner converted it to), not 90 days after its creation
      await mock.fill(entry.covenant_id);
      const children = await mock.until(async () => (await mock.order(entry.covenant_id)).children as string[], (x) => x.length >= 1);
      const exit = await mock.order(children[0]!);
      expect(exit.contract).toBe(c.exitKind);
      const exitExpiry = Number(exit.state.state.expiryDaa);
      expect(Math.abs(exitExpiry - daaAt(at, target)), 'exit expiry = the chosen date').toBeLessThan(slack(5 * 86_400));
      expect(tpl).toContain(pushInt(BigInt(exitExpiry)));
    });
  }

  test('IFD: the default good-till-cancelled exit commits the "never" its 90 days start from', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'ifd', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' });
    const { confirm, placed } = await reviewAndSign(page, mock);
    expect(confirm.created[0]!.children[0]!.rows.expiry).toContain('when the exit is created');
    expect(placed.views[0].state.state.exitState).toContain(pushInt(GTC_EXIT_EXPIRY_DAA));
  });
});

test.describe('timed start (R-11)', () => {
  const cases: { name: string; type: string; side: 'buy' | 'sell'; fields: FieldValues; kind: string; title: string }[] = [
    { name: 'stop-market sell', type: 'stopMarket', side: 'sell' as const, fields: { amount: '2', stop: '0.023' }, kind: 'KobCondAsk', title: 'Sell stop order' },
    { name: 'take-profit buy', type: 'takeProfit', side: 'buy' as const, fields: { amount: '2', price: '0.023' }, kind: 'KobCondBid', title: 'Buy take-profit order' },
    { name: 'IFD buy-first entry', type: 'ifd', side: 'buy' as const, fields: { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' }, kind: 'KobIfdBid', title: 'Buy IFD entry' },
    { name: 'repeat IFO sell-first entry', type: 'repeatIfo', side: 'sell' as const, fields: { amount: '2', minFill: '1', price: '0.04', 'exit.takeProfit': '0.0245', 'exit.stop': '0.05' }, kind: 'KobIfdAsk', title: 'Sell repeat IFO entry' },
  ];
  for (const c of cases) {
    test(`${c.name}: a start 2 hours ahead is shown, decoded and committed as activeFrom; the expiry still counts from placement`, async ({ appPage: page, mock }) => {
      await fund(mock);
      await openMarket(page, mock);
      const clock = await mockClock(mock);
      const tz = await browserTzOffset(page);
      await pickType(page, c.type, c.side);
      await fillFields(page, c.fields);
      await waitReviewable(page);
      expect((await readDisclosure(page)).rows.activates, 'no start row without a start').toBeUndefined();

      const target = Math.floor((clock.unix + 2 * 3600) / 60) * 60;
      await fillFields(page, { activeFrom: localText(target, tz) });
      await waitReviewable(page);
      const disc = await readDisclosure(page);
      expect(disc.rows.activates).toContain(utcText(target));

      const confirm = await openReview(page);
      const card = confirm.created[0]!;
      expect(card.title).toBe(c.title);
      expect(card.rows.activeFrom).toContain(utcText(target));
      const txid = await acknowledgeAndSign(page);
      const placed = await placedBy(mock, txid);
      expect(placed.submission.created).toHaveLength(1);
      expect(placed.submission.created[0]!.kind).toBe(c.kind);
      const state = placed.views[0].state.state;
      const at = { daa: placed.submission.daa, unix: Math.floor(Date.parse(placed.submission.at) / 1000) };
      const activeFrom = Number(state.activeFrom);
      expect(Math.abs(activeFrom - daaAt(at, target)), 'activeFrom = the chosen start').toBeLessThan(slack(2 * 3600));
      expect(activeFrom).toBeGreaterThan(at.daa);
      // good till cancelled: 90 days from the placement, not from the start (the start does not extend the order)
      expect(Math.abs(Number(state.expiryDaa) - at.daa - GTC_DAA)).toBeLessThan(5_000);
      expect(Number(state.expiryDaa)).toBeGreaterThan(activeFrom);
    });
  }

  test('a start beyond the 90-day horizon is refused on the ticket', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    const clock = await mockClock(mock);
    const tz = await browserTzOffset(page);
    await pickType(page, 'ifd', 'buy');
    await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026', activeFrom: localText(clock.unix + 120 * 86_400, tz) });
    await expect(page.getByTestId('order-ticket')).toHaveAttribute('aria-busy', 'false');
    await expect(page.getByTestId('order-issues').locator('[data-severity="error"]').first()).toBeVisible();
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });
});

test.describe('repeat ladder (R-14)', () => {
  test('three repeat IFD levels from one ticket: one confirmation, signature and transaction per level, prices one step apart', async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'repeatIfd', 'buy');
    // a minimum fill of the whole amount keeps the merge tip of the repeats at 0.01 KAS per token (cases.ts, repeat section)
    await fillFields(page, { amount: '1', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04' });
    await waitReviewable(page);
    // one level by default: no ladder, no step field
    await expect(page.getByTestId('field-ladder.levels')).toHaveValue('1');
    await expect(page.getByTestId('field-ladder.step')).toHaveCount(0);
    await expect(page.getByTestId('order-ladder')).toHaveCount(0);

    await fillFields(page, { 'ladder.levels': '3', 'ladder.step': '0.001' });
    await waitReviewable(page);
    const ladder = page.getByTestId('order-ladder');
    await expect(ladder).toContainText('Ladder: 3 levels');
    // buy-first levels wait one step lower each; the exit moves with its entry (same profit per token)
    await expect(page.getByTestId('order-ladder-level-1')).toHaveText(/Level 1: entry 0\.0245\b.*KAS, take-profit 0\.04\b.*KAS/);
    await expect(page.getByTestId('order-ladder-level-2')).toHaveText(/Level 2: entry 0\.0235\b.*KAS, take-profit 0\.039\b.*KAS/);
    await expect(page.getByTestId('order-ladder-level-3')).toHaveText(/Level 3: entry 0\.0225\b.*KAS, take-profit 0\.038\b.*KAS/);
    await expect(page.getByTestId('order-ladder-total')).toContainText('Total: 3 EXKCC in 3 orders');
    await expect(ladder).toContainText('you sign 3 transactions in turn');

    const before = (await mock.submitted(false)).length;
    await page.getByTestId('order-review').click();
    const txids: string[] = [];
    for (let level = 1; level <= 3; level++) {
      await expect(page.getByTestId('confirm-screen')).toBeVisible({ timeout: 30_000 });
      const confirm = await readConfirm(page);
      expect(confirm.blocking).toEqual([]);
      expect(confirm.heading).toContain(`Level ${level} of 3`);
      expect(confirm.created).toHaveLength(1);
      expect(confirm.created[0]!.title).toBe('Buy repeat IFD entry');
      txids.push(await acknowledgeAndSign(page));
      await page.getByTestId('confirm-close').click();
    }
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    await expect(page.getByTestId('order-result-ladder')).toContainText('All 3 levels placed');

    // the node got exactly three transactions, one repeat entry each, at the stepped prices
    const subs = (await mock.submitted()).slice(before);
    expect(subs.map((s) => s.txid)).toEqual(txids);
    const entries = [];
    for (const s of subs) {
      expect(s.created).toHaveLength(1);
      expect(s.created[0]!.kind).toBe('KobIfdBid');
      entries.push((await placedBy(mock, s.txid)).views[0]);
    }
    expect(entries.map((v) => v.state.state.price)).toEqual(['2450000', '2350000', '2250000']);
    expect(entries.map((v) => v.state.state.amountLeft)).toEqual(['100000000', '100000000', '100000000']);
    for (const v of entries) expect(BigInt(v.state.state.rptAmount)).toBeGreaterThan(0n); // every level repeats
    const tps = [4_000_000n, 3_900_000n, 3_800_000n];
    entries.forEach((v, i) => expect(v.state.state.exitState, `level ${i + 1} exit take-profit`).toContain(pushInt(tps[i]!)));
    // one signature request per level
    expect((await wallet!.calls()).filter((c) => c.status === 'signed')).toHaveLength(3);
  });

  test('a ladder that would step below zero is refused before anything is signed', async ({ appPage: page, mock }) => {
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'repeatIfd', 'buy');
    await fillFields(page, { amount: '1', price: '0.0245', 'exit.takeProfit': '0.04', 'ladder.levels': '4', 'ladder.step': '0.01' });
    await expect(page.getByTestId('order-ladder-errors')).toContainText('Level 4 would price at or below zero');
    await expect(page.getByTestId('order-review')).toBeDisabled();
  });
});
