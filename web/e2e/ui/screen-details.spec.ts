// Screen details a user sees and clicks, each a regression of a defect found in a hands-on pass over the app (mock stack): the wallet
// address after a network switch, a confirmation screen that opens at its title, copy buttons, the pair ticket's price unit, panels when the
// indexer is down, the settings that pin servers and their reset, the skip link, a token of 18 decimals, the depth legend, the issue summary.
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { test, expect, TEST_KEYS } from '../fixtures';
import { connectWallet, fund, listExkcc, openMarket } from '../helpers/env';
import { runFlow } from '../helpers/orders';
import { fillFields, pickType, reviewAndSign, waitReviewable } from '../helpers/ticket';

test.use({ autoOpen: false });


test.describe('KasWare that starts on mainnet and switches', () => {
  test.use({ walletOptions: { network: 'mainnet' } });
  test('the address shown is the testnet one after the switch', async ({ appPage: page, mock }) => {
    await openMarket(page, mock);
    await expect(page.getByTestId('wallet-network')).toHaveText('testnet-10');
    await expect(page.getByTestId('wallet-address')).toHaveAttribute('data-value', TEST_KEYS.alice.testnetAddress);
  });
});

test('the confirmation screen opens at its title, not scrolled past it', async ({ appPage: page, mock }) => {
  await fund(mock);
  await openMarket(page, mock);
  await pickType(page, 'ifd', 'buy');
  await fillFields(page, { amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' });
  await waitReviewable(page);
  await page.getByTestId('order-review').click();
  const screen = page.getByTestId('confirm-screen');
  await expect(screen).toBeVisible();
  await expect(screen.locator('.modal-head h2')).toBeInViewport();
  expect(await page.locator('.modal-backdrop').evaluate((el) => el.scrollTop)).toBe(0);
});

test('copy buttons keep their label on one line (token details)', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  const btn = page.getByTestId('token-covenant-id').locator('xpath=..').locator('.copy-btn');
  await expect(btn).toHaveText('Copy');
  const box = await btn.boundingBox();
  expect(box!.height).toBeLessThan(30);
});

test.describe('market panels when the indexer cannot be reached', () => {
  test('chart and depth show no endless skeleton, the trades panel no "No trades yet"', async ({ appPage: page, mock }) => {
    await listExkcc(page);
    const tok = await mock.token();
    // the app learns the token from the registry; every indexer read fails
    await page.route('**/v1/**', (r) => r.abort());
    await page.goto(`/#/market/${tok.covenant_id}`);
    await expect(page.getByTestId('book-error')).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId('price-chart')).toHaveAttribute('data-state', 'error');
    await expect(page.getByTestId('chart-loading')).toHaveCount(0);
    await expect(page.getByTestId('trades-error')).toBeVisible();
    await expect(page.getByTestId('trades-section').getByText('No trades yet.')).toHaveCount(0);
    await expect(page.getByTestId('depth-chart')).toHaveAttribute('data-state', 'empty');
    await expect(page.getByTestId('depth-empty')).toHaveText('The book could not be loaded.');
  });
});

test('the depth legend keeps the estimate mark with its amounts', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  const summary = page.getByTestId('depth-summary');
  await expect(summary).toContainText(/^~ In view: /);
  // the price labels at the edges stay inside the chart
  const out = await page.getByTestId('depth-chart').locator('svg').evaluate((svg) => {
    const box = svg.getBoundingClientRect();
    return [...svg.querySelectorAll('text')].filter((t) => {
      const r = t.getBoundingClientRect();
      return r.left < box.left - 0.5 || r.right > box.right + 0.5;
    }).length;
  });
  expect(out).toBe(0);
});

test('the order book column titles keep their units (they wrap, no ellipsis)', async ({ appPage: page, mock }) => {
  await openMarket(page, mock, { connect: false });
  const head = page.getByTestId('order-book').locator('.ob-head > span');
  await expect(head.first()).toHaveText('Price (EXKCC / KAS)');
  const cut = await head.evaluateAll((els) => els.filter((e) => e.scrollWidth > e.clientWidth + 1).length);
  expect(cut).toBe(0);
});
