// Shared environment helpers of the order-matrix specs: the tradable-registry patch, opening the token page with a connected mock wallet,
// funding the test keys, and reading the state the mock indexer holds for a placed order.
//
// Money model of the default mock seed (fictional token EXKCC, 8 decimals, scale 1e8 base units per token, tick = 0.0001 KAS / token):
// the book rests around a mid of 0.025 KAS / token (asks 0.0251 .. 0.026, bids 0.0249 .. 0.024; the best level of each side holds 2 orders).
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import { expect, type MockClient, type KeyName } from '../fixtures';

export const KAS = 100_000_000n;
/** one EXKCC = 1e8 base units (the order scale of the token) */
export const TOKEN = 100_000_000n;

/** Mock wallets a spec can connect through: the header button carries the wallet id. */
export type WalletName = 'kasware' | 'kaspire' | 'kastle';

/** The example registry with its templates marked reviewed (programs this build supports for trading) and EXKCC `verified` (its covenant id and template hash checked against chain, a fact; the UI shows no verification state either way). */
export async function listExkcc(page: Page): Promise<void> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  for (const tk of reg.tokens) {
    if (tk.ticker === 'EXKCC') tk.verified = true;
  }
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
}

export interface OpenOptions {
  /** connect the wallet (default true) */
  connect?: boolean;
  wallet?: WalletName;
  /** the hash route after `#` (default: the token page) */
  route?: string;
}

/** Opens the token page of EXKCC (or `route`) with the tradable registry, connects the mock wallet and waits for the ticket. Returns the token covenant id. */
export async function openMarket(page: Page, mock: MockClient, opts: OpenOptions = {}): Promise<string> {
  await listExkcc(page);
  const token = await mock.token();
  await page.goto(`/#${opts.route ?? `/market/${token.covenant_id}`}`);
  if (opts.connect !== false) await connectWallet(page, opts.wallet ?? 'kasware');
  if (opts.route === undefined) await expect(page.getByTestId('order-ticket')).toBeVisible();
  return token.covenant_id;
}

/**
 * Opens the way to a connect option: a desktop with one extension wallet shows its button at once (`wallet-connect-<id>`); a phone (one "Connect" button
 * that opens a sheet), a desktop with several or no extension (a dialog) first press `wallet-connect`.
 */
export async function openConnect(page: Page, optionTestId: string): Promise<void> {
  const option = page.getByTestId(optionTestId);
  // extensions inject late: a button of one detected wallet may be about to appear
  if (await option.waitFor({ state: 'visible', timeout: 1_500 }).then(() => true, () => false)) return;
  await page.getByTestId('wallet-connect').click();
  await expect(option).toBeVisible();
}

/** Follows a tab of the header (`nav-orders` ...): on a phone the tabs are in the menu, which is opened first. */
export async function gotoNav(page: Page, testid: string): Promise<void> {
  const link = page.getByTestId(testid);
  await expect(async () => {
    if (!(await link.isVisible())) await page.getByTestId('menu-button').click();
    await link.click({ timeout: 2_000 });
  }).toPass({ timeout: 20_000 });
}

/**
 * Clicks a connect option wherever the header has it right now, retried as a whole: a header row that does not fit (long translations, wide fonts) takes
 * the phone layout, possibly just after the page was first measured, and the option then moves behind `wallet-connect`.
 */
export async function clickConnectOption(page: Page, optionTestId: string): Promise<void> {
  const option = page.getByTestId(optionTestId);
  await expect(async () => {
    if (!(await option.isVisible())) {
      const opener = page.getByTestId('wallet-connect');
      if (await opener.isVisible()) await opener.click();
      await expect(option).toBeVisible({ timeout: 1_500 });
    }
    await option.click({ timeout: 2_000 });
  }).toPass({ timeout: 20_000 });
}

/** Opens the status lines (`status-node`, `status-indexer`): the status dot of the desktop row, or the menu of the phone layout. */
export async function openStatus(page: Page): Promise<void> {
  await expect(async () => {
    if (await page.getByTestId('status-node').isVisible()) return;
    const dot = page.getByTestId('status-dot');
    if (await dot.isVisible()) await dot.click();
    else await page.getByTestId('menu-button').click();
    await expect(page.getByTestId('status-node')).toBeVisible({ timeout: 1_500 });
  }).toPass({ timeout: 20_000 });
}

export async function connectWallet(page: Page, wallet: WalletName = 'kasware'): Promise<void> {
  await clickConnectOption(page, `wallet-connect-${wallet}`);
  await expect(page.getByTestId('wallet-address')).toBeVisible();
}

/** Gives a key plenty of KAS (several UTXOs, so parallel carriers never run short) and EXKCC. */
export async function fund(mock: MockClient, key: KeyName = 'alice', o: { kas?: bigint; tokens?: bigint; utxos?: number } = {}): Promise<void> {
  await mock.giveKas(key, (o.kas ?? 2000n) * KAS, o.utxos ?? 3);
  if ((o.tokens ?? 100n) > 0n) await mock.giveTokens(key, (o.tokens ?? 100n) * TOKEN);
}

/** The orders of `key` the mock indexer holds (any status), newest first. */
export async function ordersOf(mock: MockClient, key: KeyName, status = 'active'): Promise<any[]> {
  return mock.ordersOf(key, status);
}

/** Polls the mock indexer until `key` has `n` orders in `status`. */
export async function waitOrders(mock: MockClient, key: KeyName, n: number, status = 'active'): Promise<any[]> {
  return mock.until(() => mock.ordersOf(key, status), (o) => o.length === n);
}

/** The full indexer view (decoded state included) of every order a submission created. */
export async function viewsOf(mock: MockClient, created: { covenantId: string }[]): Promise<any[]> {
  return Promise.all(created.map((c) => mock.order(c.covenantId)));
}

/** The 00:00 UTC that ends the UTC day `nowUnix` falls in, in unix seconds. */
export const endOfUtcDay = (nowUnix: number): number => (Math.floor(nowUnix / 86_400) + 1) * 86_400;

/** `datetime-local` text of a unix time in the browser's wall clock (tests run with the machine time zone: the app converts back the same way). */
export function localDateTimeText(unixSeconds: number, tzOffsetMin: number): string {
  const d = new Date((unixSeconds - tzOffsetMin * 60) * 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}T${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}

/** The browser's `Date.getTimezoneOffset()`. */
export async function browserTzOffset(page: Page): Promise<number> {
  return page.evaluate(() => new Date().getTimezoneOffset());
}

/** The node clock of the mock (DAA score, unix seconds) as the app reads it. */
export async function mockClock(mock: MockClient): Promise<{ daa: number; unix: number }> {
  const s = await mock.state();
  return { daa: Number(s.daa), unix: Number(s.unix) };
}
