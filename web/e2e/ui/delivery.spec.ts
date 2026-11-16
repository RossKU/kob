// Delivery of buy orders (bid-delivery-spec §10, mock stack): the ticket's Advanced options gain a Delivery row (Collected by default, Each fill,
// Top-up with its tip per fill), the main form is unchanged, and Settings holds the wallet-wide default and, read-only, the Top-up threshold per
// token. The placed orders carry the delivery block the planner chose (decoded by the mock indexer from the real transaction).
//
// With KOB_DELIVERY_SHOTS=<dir> the spec also saves the three screenshots of the change (Advanced default, Advanced with Top-up, Settings).
//
// Not covered here (the fill builders of the delivery modes are later work packages): a fill of a Collected or a Top-up order.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { STD, openStd } from '../helpers/delivery';
import { expect, test } from '../fixtures';
import { listExkcc } from '../helpers/env';
import { fillFields, pickType, placeOrder, settlePlan, waitReviewable } from '../helpers/ticket';
import { chooseOption } from '../helpers/dropdown';

test.use({ autoOpen: false });

const shots = process.env.KOB_DELIVERY_SHOTS;

const openAdvanced = async (page: Page): Promise<void> => {
  const box = page.getByTestId('order-advanced');
  if ((await box.getAttribute('open')) === null) await box.locator('summary').click();
};
const stateOf = (placed: { views: any[] }): Record<string, string> => placed.views[0]!.state.state as Record<string, string>;

test('a limit buy is Collected by default: Delivery under Advanced, no Fills box, one carrier; the main form is unchanged', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await openStd(page, mock);
  await pickType(page, 'limit', 'buy');
  await fillFields(page, { price: '0.025', amount: '40', minFill: '10' });
  await openAdvanced(page);
  // the main form is the one of before (amount, price, ladder, lifetime): Delivery is an Advanced row only
  const main = await page.getByTestId('order-ticket').locator('[data-testid^="field-"], [data-testid="order-amount"], [data-testid="order-price"]').evaluateAll((els) =>
    els.filter((e) => !e.closest('.tk-advanced') && (e as HTMLElement).offsetParent !== null).map((e) => e.getAttribute('data-testid')),
  );
  expect(main).toEqual(['order-amount', 'order-price', 'field-ladder.on', 'field-lifetime']);
  await expect(page.getByTestId('field-delivery')).toHaveAttribute('data-value', 'collected');
  await expect(page.getByTestId('field-delivery')).toHaveText(/Collected/);
  await expect(page.getByTestId('order-delivery-note')).toHaveText('Bought tokens arrive as one UTXO when the order ends. You can claim them earlier.');
  await expect(page.getByTestId('field-maxFills')).toHaveCount(0);
  await expect(page.getByTestId('field-topupTip')).toHaveCount(0);
  // the row right after the priority tip
  const ids = await page.getByTestId('order-advanced').locator('[data-testid^="field-"], [data-testid="order-tip"]').evaluateAll((els) => els.map((e) => e.getAttribute('data-testid')));
  expect(ids.slice(0, 2)).toEqual(['order-tip', 'field-delivery']);
  await waitReviewable(page);
  if (shots) await page.getByTestId('order-ticket').screenshot({ path: `${shots}/delivery-advanced-default.png` });
  const placed = await placeOrder(page, mock);
  expect(stateOf(placed)).toMatchObject({ mode: '1', recvThreshold: '0', topupTip: '0', bought: '0' });

});

test('Top-up: the tip per fill (default from the program), the tip total and Fills; the order carries the threshold, Settings shows it', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await openStd(page, mock);
  await pickType(page, 'limit', 'buy');
  await fillFields(page, { price: '0.025', amount: '40', minFill: '10' });
  await openAdvanced(page);
  await chooseOption(page.getByTestId('field-delivery'), 'topup');
  await expect(page.getByTestId('field-topupTip')).toBeVisible();
  // ceil(1.25 x 595 400 sompi), the reference top-up extra of KCC20Ref
  await expect(page.getByTestId('field-topupTip')).toHaveAttribute('placeholder', '0.0074425');
  await expect(page.getByTestId('field-maxFills')).toBeVisible();
  await settlePlan(page);
  await expect(page.getByTestId('order-topup-total')).toHaveText('Tip total: up to 0.0223275 KAS for 3 fills. A fill that cannot top up arrives as a new UTXO.');
  await fillFields(page, { topupTip: '0.009' });
  await settlePlan(page);
  await expect(page.getByTestId('order-topup-total')).toHaveText('Tip total: up to 0.027 KAS for 3 fills. A fill that cannot top up arrives as a new UTXO.');
  await waitReviewable(page);
  if (shots) await page.getByTestId('order-ticket').screenshot({ path: `${shots}/delivery-advanced-topup.png` });
  const placed = await placeOrder(page, mock);
  // the wallet's T = ceil(5 KAS x 10^8 / 0.025 KAS) = 200 EXSTD; the order's recvThreshold is min(T, minFill - 1) = 10 EXSTD less one base unit
  expect(stateOf(placed)).toMatchObject({ mode: '2', recvThreshold: '999999999', topupTip: '900000', bought: '0' });

  await page.goto('/#/settings');
  const card = page.getByTestId('settings-delivery');
  await expect(card).toBeVisible();
  await expect(card.getByTestId(`settings-delivery-threshold-${STD}`)).toContainText('200 EXSTD');
});

test('Settings: the wallet default applies to the ticket at once; IOC, sells and KRON have no Delivery row', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await openStd(page, mock);
  await page.goto('/#/settings');
  const card = page.getByTestId('settings-delivery');
  await expect(card.getByTestId('settings-delivery-collected')).toHaveAttribute('aria-checked', 'true');
  await card.getByTestId('settings-delivery-each').click();
  await expect(card.getByTestId('settings-delivery-each')).toHaveAttribute('aria-checked', 'true');
  await expect(card.getByTestId('settings-delivery-thresholds-none')).toHaveText('None yet.');
  if (shots) {
    await card.scrollIntoViewIfNeeded();
    await card.screenshot({ path: `${shots}/delivery-settings.png` });
  }

  await page.goto(`/#/market/${STD}`);
  await pickType(page, 'limit', 'buy');
  await openAdvanced(page);
  await expect(page.getByTestId('field-delivery')).toHaveAttribute('data-value', 'each');
  await expect(page.getByTestId('field-delivery')).toHaveText(/Each fill/);
  await expect(page.getByTestId('order-delivery-note')).toHaveCount(0);
  await expect(page.getByTestId('field-maxFills')).toBeVisible();
  await fillFields(page, { price: '0.025', amount: '40', minFill: '10' });
  const placed = await placeOrder(page, mock);
  expect(stateOf(placed).mode).toBe('0');

  await pickType(page, 'ioc', 'buy');
  await expect(page.getByTestId('field-delivery')).toHaveCount(0);
  await pickType(page, 'limit', 'sell');
  await expect(page.getByTestId('field-delivery')).toHaveCount(0);
  // a KRON token delivers at each fill: no choice
  await listExkcc(page);
  const kron = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8')).tokens.find((t: { ticker: string }) => t.ticker === 'EXKRON');
  await page.goto(`/#/market/${kron.covenant_id}`);
  await expect(page.getByTestId('order-ticket')).toBeVisible();
  await pickType(page, 'limit', 'buy');
  await openAdvanced(page);
  await expect(page.getByTestId('field-delivery')).toHaveCount(0);
  await expect(page.getByTestId('field-maxFills')).toBeVisible();
});

test("What's this?: the step-by-step help opens from the Delivery label, plays, pauses, steps, and closes back to the link", async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await openStd(page, mock);
  await pickType(page, 'limit', 'buy');
  await openAdvanced(page);
  const link = page.getByTestId('order-delivery-help');
  // on the label row of Delivery, at its right end
  const [lb, label] = await Promise.all([link.boundingBox(), page.locator('label', { hasText: /^Delivery$/ }).boundingBox()]);
  expect(Math.abs(lb!.y + lb!.height - (label!.y + label!.height))).toBeLessThan(6);
  expect(lb!.x).toBeGreaterThan(label!.x + label!.width);
  await link.click();
  const help = page.getByTestId('delivery-help');
  await expect(help).toBeVisible();
  // the choice in force (Collected) is highlighted
  await expect(help.getByTestId('delivery-help-lane-collected')).toHaveAttribute('data-on', 'true');
  await expect(help.getByTestId('delivery-help-lane-each')).toHaveAttribute('data-on', 'false');
  const caption = help.getByTestId('delivery-help-caption');
  const play = help.getByTestId('delivery-help-play');
  // it plays: the caption moves on by itself
  await expect(play).toHaveAttribute('aria-label', 'Pause');
  await expect(caption).toHaveText('Fill 2 of 3', { timeout: 5_000 });
  await play.click();
  await expect(play).toHaveAttribute('aria-label', 'Play');
  const held = await caption.textContent();
  await page.waitForTimeout(1_600);
  await expect(caption).toHaveText(held!);
  // steps by hand
  // steps by hand, to the end (the forward button stops there)
  const forward = help.getByTestId('delivery-help-forward');
  while (await forward.isEnabled()) await forward.click();
  await expect(caption).toHaveText('Order ends');
  await expect(help.getByTestId('delivery-help-result-collected')).toHaveText('1 UTXO · when the order ends');
  await help.getByTestId('delivery-help-back').click();
  await expect(caption).toHaveText('Fill 3 of 3');
  // Escape closes; focus is back on the link
  await page.keyboard.press('Escape');
  await expect(help).toHaveCount(0);
  await expect(link).toBeFocused();
  // the close button too
  await link.click();
  await help.getByRole('button', { name: 'Close' }).click();
  await expect(help).toHaveCount(0);
});

test('reduced motion: the help opens paused on the final state', async ({ appPage: page, mock }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await openStd(page, mock);
  await pickType(page, 'limit', 'buy');
  await openAdvanced(page);
  await page.getByTestId('order-delivery-help').click();
  const help = page.getByTestId('delivery-help');
  await expect(help.getByTestId('delivery-help-caption')).toHaveText('Order ends');
  await expect(help.getByTestId('delivery-help-play')).toHaveAttribute('aria-label', 'Play');
  await page.waitForTimeout(1_500);
  await expect(help.getByTestId('delivery-help-caption')).toHaveText('Order ends');
});

