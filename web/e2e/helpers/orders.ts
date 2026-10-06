// Helpers of the "My orders" flows: seeding orders for the wallet key, opening the orders page, and running the confirmation screens of a
// (possibly multi-step) transaction flow.
import type { Page } from '@playwright/test';
import { expect, type MockClient, type KeyName, type MockWalletHandle } from '../fixtures';
import { openMarket, type WalletName } from './env';
import { acknowledgeAndSign, placedBy, readConfirm, expectWalletSigned, type ConfirmRead, type Placed } from './ticket';

export interface SeedAsk {
  side: 'ask' | 'bid';
  /** sompi per whole EXKCC: 2_700_000 = 0.027 KAS / EXKCC */
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

export interface FlowStepResult {
  confirm: ConfirmRead;
  placed: Placed;
}

/**
 * Runs every confirmation screen of a transaction flow to its end: read the decoded screen, acknowledge, sign, wait for the accepted tx, close.
 * Stops when the flow shows its result (`flow-done`). Returns the decoded screens and the accepted transactions in order.
 */
export async function runFlow(page: Page, mock: MockClient, wallet: MockWalletHandle | null, opts: { steps?: number } = {}): Promise<FlowStepResult[]> {
  const out: FlowStepResult[] = [];
  const base = wallet ? (await wallet.calls()).length : 0; // sign requests of earlier steps of the test
  for (;;) {
    const screen = page.getByTestId('confirm-screen');
    const done = page.getByTestId('flow-done');
    await expect(screen.or(done)).toBeVisible({ timeout: 30_000 });
    if (await done.isVisible()) break;
    const confirm = await readConfirm(page);
    expect(confirm.blocking, 'no blocking finding').toEqual([]);
    const txid = await acknowledgeAndSign(page);
    const placed = await placedBy(mock, txid);
    if (wallet) await expectWalletSigned(wallet, placed, { requests: base + out.length + 1 });
    out.push({ confirm, placed });
    await page.getByTestId('confirm-close').click();
    await expect(screen).toHaveCount(0);
  }
  if (opts.steps !== undefined) expect(out).toHaveLength(opts.steps);
  return out;
}

/** All indexer orders of `key` by covenant id and status. */
export async function statusesOf(mock: MockClient, key: KeyName): Promise<Record<string, string>> {
  const all = await mock.orders(`maker=${(await mock.balance(key)).pubkey}&limit=200`);
  return Object.fromEntries(all.map((o) => [o.covenant_id, o.status]));
}
