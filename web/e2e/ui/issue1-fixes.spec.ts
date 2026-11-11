// GitHub issue 1 and the founder's follow-ups (2026-11-03): the KAS-on-token-coins explanation, the image note of the token creation form, an order
// ticket that grows alone (its own column, Advanced options never pushes the panels below or leaves a gap), the folded template verification, and
// Settings with its operator part under "Advanced / self-hosting". (The final confirmation's notes are in min-order-value.spec.ts and order-flow.spec.ts.)
import { readFileSync } from 'node:fs';
import { expect, test } from '../fixtures';
import { KAS, TOKEN, connectWallet, fund, openMarket } from '../helpers/env';
import { horizontalOverflow } from '../helpers/i18n';
import { pickType, settlePlan } from '../helpers/ticket';

test.use({ autoOpen: false });

test('My orders: the KAS on token coins has a one-line muted explanation', async ({ appPage: page, mock }) => {
  const STD = '5a'.repeat(32);
  const EXT = 'ee'.repeat(32);
  await mock.seed({ tokens: [{ ticker: 'EXSTD', covenant_id: STD, program: 'KCC20Ref', extension_commitment: EXT, decimals: 8, template_id: 'kcc20-ref-3x3' }] });
  for (let k = 1; k <= 2; k++) await mock.giveTokens('alice', BigInt(k) * TOKEN, { token: STD, carrier: 2n * KAS });
  await mock.giveKas('alice', 20n * KAS);
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  const exkcc = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  reg.tokens.push({ ...exkcc, ticker: 'EXSTD', name: 'Example standard token', covenant_id: STD, template_id: 'kcc20-ref-3x3', extension_commitment: EXT });
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
  await page.goto('/#/portfolio');
  await connectWallet(page, 'kasware');
  await expect(page.getByTestId('kas-on-tokens')).toBeVisible({ timeout: 20_000 });
  const note = page.getByTestId('kas-on-tokens-note');
  await expect(note).toHaveText('KAS held by your token coins; it comes back when they move.');
  await expect(note).toHaveClass(/muted/);
  // one line at a desktop width, right under the figures
  const box = (await note.boundingBox())!;
  expect(box.height).toBeLessThan(28);
  expect(box.y).toBeGreaterThan((await page.getByTestId('kas-on-tokens').boundingBox())!.y);
});

test('token creation: the image address says each viewer loads the logo from it, and that ipfs:// is tamper-proof', async ({ appPage: page }) => {
  await page.goto('/#/create');
  await expect(page.getByTestId('issue-form')).toBeVisible();
  const field = page.getByTestId('issue-icon').locator('xpath=ancestor::*[contains(@class,"field")][1]');
  await expect(field).toContainText('loads the logo from this address');
  await expect(field).toContainText('ipfs:// address is tamper-proof');
});

for (const width of [1280, 880, 390]) {
  test(`${width} px: unfolding Advanced options changes nothing around the ticket (its form scrolls inside the panel; the panels beside it and the full-width facts stay put)`, async ({ appPage: page, mock }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width, height: 900 });
    await fund(mock);
    await openMarket(page, mock);
    await pickType(page, 'limit', 'buy');
    await settlePlan(page);
    // what loads under the grid (the activity log's rows, the wallet's panel) changes the page's height by itself: measured once it is in
    await page.getByTestId('activity-list').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('activity-list').getByTestId('activity-row').first()).toBeVisible();
    await expect(page.getByTestId('market-orders').getByTestId('orders-section')).toBeVisible();
    await page.waitForTimeout(500);
    await page.evaluate(() => scrollTo(0, 0));
    const adv = page.getByTestId('order-advanced');
    // document coordinates (the click on the summary may scroll the page)
    const box = (id: string) => page.getByTestId(id).evaluate((el) => { const r = el.getBoundingClientRect(); return { x: r.x + scrollX, y: r.y + scrollY, width: r.width, height: r.height }; });
    // the depth chart is a panel of its own on three columns only (below 1200 px it is a view of the book's panel, which stays put instead)
    const ids = [width >= 1200 ? 'depth-section' : 'book-section', 'token-details', 'ticket-container', 'order-place'];
    const closed: Record<string, { x: number; y: number; width: number; height: number }> = {};
    for (const id of ids) closed[id] = await box(id);
    await adv.locator('summary').click();
    await expect(adv.locator('.tk-advanced-body')).toBeVisible();
    await page.waitForTimeout(300);
    for (const id of ids) {
      const now = await box(id);
      for (const k of ['x', 'y', 'width', 'height'] as const) expect(Math.abs(now[k] - closed[id]![k]), id + '.' + k).toBeLessThanOrEqual(1);
    }
    // the form is what grew: it scrolls inside the panel
    expect(await page.getByTestId('order-scroll').evaluate((el) => el.scrollHeight > el.clientHeight), 'the form scrolls inside the panel').toBe(true);
    expect(await horizontalOverflow(page), 'page').toBe(0);
  });
}

test('the template checks sit in "Status and checks": one line while folded, beside the facts, and the facts keep their own height', async ({ appPage: page, mock }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await openMarket(page, mock, { connect: false });
  const verification = page.getByTestId('token-status-details');
  const facts = page.getByTestId('token-details').locator('section').first();
  await expect(verification).toBeVisible();
  const folded = (await verification.boundingBox())!;
  expect(folded.height).toBeLessThan(70);
  expect((await facts.boundingBox())!.height).toBeGreaterThan(folded.height);
  await expect(page.getByTestId('template-verification')).toBeHidden();
  // a phone too: still one line
  await page.setViewportSize({ width: 390, height: 900 });
  await verification.scrollIntoViewIfNeeded();
  expect((await verification.boundingBox())!.height).toBeLessThan(70);
});

test('Settings: network, wallet and fee options in view; the servers folded in the same card as Save; registry, tracker and indexer check folded under "Advanced / self-hosting"', async ({ appPage: page }) => {
  await page.goto('/#/settings');
  await expect(page.getByTestId('settings-view')).toBeVisible();
  for (const id of ['settings-network', 'settings-kastle', 'settings-priority-fee', 'settings-dynamic-fee', 'settings-save', 'settings-icons', 'settings-private-key-section', 'settings-local']) {
    await expect(page.getByTestId(id), id).toBeVisible();
  }
  const adv = page.getByTestId('settings-advanced');
  await expect(adv).toBeVisible();
  await expect(adv).toContainText('Advanced / self-hosting');
  // folded: the operator part is in the page but not shown
  const servers = ['settings-indexer-url', 'settings-node-url', 'settings-registry-url', 'settings-server-warning'];
  const operator = ['settings-registry', 'settings-tracker', 'settings-trust'];
  for (const id of [...servers, ...operator]) await expect(page.getByTestId(id), `${id} folded`).toBeHidden();
  // the servers are settings of the Save card: their fold is inside it, with the Save button below
  const fold = page.getByTestId('settings-connection').getByTestId('settings-servers-fold');
  await fold.getByRole('button').first().click();
  for (const id of servers) await expect(page.getByTestId(id), `${id} open`).toBeVisible();
  await adv.getByRole('button').first().click();
  for (const id of operator) await expect(page.getByTestId(id), `${id} open`).toBeVisible();
  // nothing was changed by looking: Save stays off
  await expect(page.getByTestId('settings-save')).toBeDisabled();
  expect(await horizontalOverflow(page), 'settings').toBe(0);
});
