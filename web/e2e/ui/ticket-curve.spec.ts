// The sell ladder placed as one curve order (KobCurveAsk: the ladder's prices are its table), or as
// separate orders with the stepper. And the amend of a curve from its details. Mock KasWare,
// default mock seed. Screenshots of the ticket option: `KOB_CURVE_SHOTS`.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';
import { TOKEN, fund, openMarket } from '../helpers/env';
import { fillFields, pickType, placeLadder, placeOrder, waitReviewable } from '../helpers/ticket';
import { openOrderRow, runFlow } from '../helpers/orders';

const SHOTS = process.env.KOB_CURVE_SHOTS;
if (SHOTS) mkdirSync(SHOTS, { recursive: true });
const shot = (page: Page, name: string) => page.getByTestId('order-ticket').screenshot({ path: SHOTS ? join(SHOTS, `${name}.png`) : test.info().outputPath(`${name}.png`) });

test.use({ autoOpen: false });

async function sellLadder(page: Page, levels: string): Promise<void> {
  await pickType(page, 'limit', 'sell');
  await fillFields(page, { amount: '2', price: '0.03', 'ladder.on': true, 'ladder.levels': levels, 'ladder.pct': '10' });
  await waitReviewable(page);
}

const VARIANTS = [
  { name: 'light', theme: 'light', width: 1280, height: 900 },
  { name: 'dark', theme: 'dark', width: 1280, height: 900 },
  { name: 'phone', theme: 'light', width: 390, height: 844 },
] as const;

for (const v of VARIANTS) {
  test(`a sell ladder of 3 levels is one curve order by default (${v.name})`, async ({ appPage: page, mock, wallet }) => {
    await page.setViewportSize({ width: v.width, height: v.height });
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), v.theme);
    await fund(mock);
    await openMarket(page, mock);
    await sellLadder(page, '3');
    await expect(page.getByTestId('order-ladder-as-curve')).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId('order-ladder-how')).toContainText('One order and one UTXO');
    // A curve has no expiry: no "Lasts". The footer says what placing it costs (its carriers and
    // the fee).
    await expect(page.getByTestId('field-lifetime')).toHaveCount(0);
    await expect(page.getByTestId('order-total')).toContainText('To place');
    await expect(page.getByTestId('order-cost-preview')).toContainText(/^\s*2\.0\d+ KAS\s*$/);
    await page.getByTestId('order-ladder-as-separate').click();
    await expect(page.getByTestId('field-lifetime')).toBeVisible();
    await expect(page.getByTestId('order-total')).toContainText('Total');
    await page.getByTestId('order-ladder-as-curve').click();
    await waitReviewable(page);
    await page.getByTestId('order-ladder-place-as').scrollIntoViewIfNeeded();
    await shot(page, `ticket-curve-${v.name}`);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(v.width);
    if (v.name !== 'light') return;
    // One transaction: one KobCurveAsk of 6 EXKCC, 2 per level, the ladder's prices as its table.
    const placed = await placeOrder(page, mock);
    expect(await wallet!.calls()).toHaveLength(1);
    const o = placed.views[0];
    expect(o.contract).toBe('KobCurveAsk');
    expect(o.state.state).toMatchObject({ levelSize: (2n * TOKEN).toString(), total: (6n * TOKEN).toString(), table: ['3000000', '3300000', '3630000'] });
    await expect(page.getByTestId('order-result-ladder')).toContainText('Curve order placed');
  });
}

test('two levels default to separate orders; separate orders show the stepper', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await sellLadder(page, '2');
  await expect(page.getByTestId('order-ladder-as-separate')).toHaveAttribute('aria-checked', 'true');
  await expect(page.getByTestId('order-ladder-how')).toContainText('you sign 2 transactions');
  const placed = await placeLadder(page, mock, 2);
  expect(placed.map((p) => p.views[0].contract)).toEqual(['KobAsk', 'KobAsk']);
  const steps = page.getByTestId('order-ladder-stepper');
  await expect(steps.getByTestId('order-ladder-step-1')).toHaveAttribute('data-state', 'done');
  await expect(steps.getByTestId('order-ladder-step-2')).toHaveAttribute('data-state', 'done');
  // A buy ladder offers no curve.
  await pickType(page, 'limit', 'buy');
  await fillFields(page, { amount: '1', price: '0.0245', 'ladder.on': true, 'ladder.levels': '3', 'ladder.pct': '2' });
  await waitReviewable(page);
  await expect(page.getByTestId('order-ladder-place-as')).toHaveCount(0);
});

test('a curve is amended in place from its details: new last price and minimum fill', async ({ appPage: page, mock, wallet }) => {
  await fund(mock);
  await openMarket(page, mock);
  await sellLadder(page, '3');
  const placed = await placeOrder(page, mock);
  const id = placed.views[0].covenant_id as string;
  await page.goto('/#/portfolio/open');
  await expect(page.getByTestId('orders-list')).toBeVisible();
  await openOrderRow(page, id);
  await page.getByTestId(`order-replace-${id}`).click();
  const dlg = page.getByTestId('curve-amend-dialog');
  await expect(dlg).toBeVisible();
  await expect(dlg.getByTestId('curve-amend-now')).toHaveValue('0.03');
  await dlg.getByTestId('curve-amend-last').fill('0.05');
  await dlg.getByTestId('curve-amend-min-fill').fill('0.5');
  await expect(dlg.getByTestId('curve-amend-rest')).toContainText('3 levels');
  await dlg.getByTestId('amend-review').click();
  const [done] = await runFlow(page, mock, wallet, { steps: 1 });
  expect(done!.submission.closed ?? []).toEqual([]);
  const after = await mock.order(id);
  expect(after.state.state.table).toEqual(['3000000', expect.any(String), '5000000']);
  expect(after.state.state.minFill).toBe((TOKEN / 2n).toString());
  expect(after.state.state.amountLeft).toBe((6n * TOKEN).toString());
});
