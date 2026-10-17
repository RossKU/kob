// Helpers of the "My orders" flows: seeding orders for the wallet key, opening the orders page, and waiting for a (possibly multi-step) transaction flow
// that starts at once: each step shows the final confirmation popup (confirmed here) and then asks the wallet.
import type { Page } from '@playwright/test';
import { expect, type MockClient, type KeyName, type MockWalletHandle } from '../fixtures';
import { openMarket, type WalletName } from './env';
import { confirmUntil, expectWalletSigned, placedBy, type Placed } from './ticket';

export interface SeedAsk {
  side: 'ask' | 'bid';
  /** sompi per whole EXKCC: 2_700_000 = 0.027 EXKCC per KAS */
  price: number;
  /** whole EXKCC (the order amount is `tokens` x 1e8 base units) */
  tokens: number;
  maker?: KeyName;
}

/** Seeds real (cancellable) orders of `maker` (default alice) for the default token; returns their covenant ids in order. */
export async function seedOrders(mock: MockClient, specs: SeedAsk[]): Promise<string[]> {
  const tok = await mock.token();
  const before = new Set((await mock.ordersOf('alice')).map((o) => o.covenant_id));
  await mock.seed({ orders: specs.map((s) => ({ token: tok.covenant_id, side: s.side, maker: s.maker ?? 'alice', price: s.price, amount: (BigInt(s.tokens) * 100_000_000n).toString() })) });
  const after = await mock.ordersOf('alice');
  return after.filter((o) => !before.has(o.covenant_id)).map((o) => o.covenant_id);
}

/** Opens "My orders" with the mock wallet connected and waits for the list. */
export async function openOrders(page: Page, mock: MockClient, wallet: WalletName = 'kasware'): Promise<void> {
  await openMarket(page, mock, { route: '/orders', wallet });
  await expect(page.getByTestId('orders-list')).toBeVisible();
}

/**
 * Waits for the end of a transaction flow of My orders (`flow-done`, or `flow-stopped` with `stopped: true`): the action's click starts the first
 * step, and each step shows its final confirmation popup before its wallet request (confirmed here; `confirm: false` leaves the popups to the spec; the
 * private-key signer's frame is approved by the spec itself and shows no app popup). Returns the transactions the flow submitted, in order (decoded by the mock
 * node), and checks that the wallet signed exactly those and nothing else.
 */
export async function runFlow(page: Page, mock: MockClient, wallet: MockWalletHandle | null, opts: { steps?: number; stopped?: boolean; confirm?: boolean } = {}): Promise<Placed[]> {
  const end = page.getByTestId(opts.stopped ? 'flow-stopped' : 'flow-done');
  if (opts.confirm !== false) await confirmUntil(page, end);
  await expect(end).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
  // the transactions: a multi-step flow lists the first 10 characters of each submitted step, the status line shows the last one in full
  const prefixes = (await page.locator('[data-testid^="flow-step-"] code').allTextContents()).map((c) => c.trim().replace('…', ''));
  const status = page.getByTestId('flow-status');
  await expect(status).toHaveAttribute('data-phase', /^(submitted|blocked|rejected|failed)$/);
  // a stopped last step shows an error instead of the id
  const last = (await status.getAttribute('data-phase')) === 'submitted' ? await status.getByTestId('tx-id').getAttribute('data-value') : null;
  const all = await mock.submitted();
  const mine = prefixes.length ? prefixes.map((p) => all.find((s) => s.txid.startsWith(p))).filter((s) => !!s) : last ? all.filter((s) => s.txid === last) : [];
  if (last) await expect(page.getByTestId('flow-status').getByTestId('tx-status')).toHaveAttribute('data-status', 'confirmed', { timeout: 30_000 });
  const out = await Promise.all(mine.map((s) => placedBy(mock, s!.txid)));
  if (wallet && !opts.stopped) {
    const calls = await wallet.calls();
    expect(calls.length, 'sign requests of the whole test').toBeGreaterThanOrEqual(out.length);
    for (let i = 0; i < out.length; i++) await expectWalletSigned(wallet, out[i]!, { call: calls.length - out.length + i });
  }
  if (opts.steps !== undefined) expect(out).toHaveLength(opts.steps);
  return out;
}

/** All indexer orders of `key` by covenant id and status. */
export async function statusesOf(mock: MockClient, key: KeyName): Promise<Record<string, string>> {
  const all = await mock.orders(`maker=${(await mock.balance(key)).pubkey}&limit=200`);
  return Object.fromEntries(all.map((o) => [o.covenant_id, o.status]));
}
