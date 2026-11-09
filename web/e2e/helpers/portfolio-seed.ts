// A wallet with something on every tab of the Portfolio page: long KAS figures, an unregistered token named by its hex id, a part-filled sell, a KAS-budget buy,
// finished orders and a dozen fills. Shared by the Portfolio layout specs. Mock stack.
import type { MockClient } from '../fixtures';
import { KAS, TOKEN } from './env';
import { seedOrders } from './orders';

/** the founder's unregistered token: no ticker, named by 8 + 8 hex of its covenant id */
export const PF_ID = '83478d88' + '11'.repeat(24) + '6daacd64';

export async function seedPortfolio(mock: MockClient): Promise<{ filled: string; budget: string; open: string; unreg: string; done: string[] }> {
  // the wallet: long figures (593.01968763 KAS in one coin, 1,000 more in another) and tokens
  await mock.giveKas('alice', 59_301_968_763n, 1);
  await mock.giveKas('alice', 1_000n * KAS, 1);
  await mock.giveTokens('alice', 1_234n * TOKEN);
  await mock.seed({
    tokens: [{ ticker: '', covenant_id: PF_ID, program: 'KCC20Ref', template_id: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null, order_scale: 100_000_000 }],
    orders: [{ token: PF_ID, side: 'ask', maker: 'alice', amount: '4500000000000', price: 1_000_000_000 }],
  });
  const unreg = (await mock.ordersOf('alice')).find((o: { token: string }) => o.token === PF_ID)!.covenant_id as string;
  // a sell order filled a dozen times (part-filled at the end), a KAS-budget buy and a plain sell
  const [filled] = await seedOrders(mock, [{ side: 'ask', price: 2_567_222, tokens: 40 }]);
  for (let i = 0; i < 12; i++) await mock.fill(filled!, TOKEN, { price: 2_567_222 + i * 3_511 });
  const [budget] = await seedOrders(mock, [{ side: 'bid', price: 3_567_222, tokens: 20 }]);
  const [open] = await seedOrders(mock, [{ side: 'ask', price: 2_800_000, tokens: 3 }]);
  // finished orders for the history: two sells filled whole, a KAS-budget buy filled
  const done: string[] = [];
  for (const [side, price] of [['ask', 2_610_000], ['ask', 2_620_000], ['bid', 2_380_000]] as const) {
    const [id] = await seedOrders(mock, [{ side, price, tokens: 2 }]);
    await mock.fill(id!, 2n * TOKEN);
    done.push(id!);
  }
  await mock.fill(unreg, 1_000n * TOKEN);
  return { filled: filled!, budget: budget!, open: open!, unreg, done };
}

/** `n` more finished sell orders of the wallet (one token each, filled whole at their own prices): history rows and fills for paging. Returns their ids. */
export async function seedManyDone(mock: MockClient, n: number): Promise<string[]> {
  const tok = await mock.token();
  const before = new Set((await mock.ordersOf('alice')).map((o: { covenant_id: string }) => o.covenant_id));
  await mock.seed({ orders: Array.from({ length: n }, (_, i) => ({ token: tok.covenant_id, side: 'ask', maker: 'alice', price: 2_900_000 + i * 1_000, amount: TOKEN.toString() })) });
  const ids = (await mock.ordersOf('alice')).map((o: { covenant_id: string }) => o.covenant_id).filter((id: string) => !before.has(id));
  for (const id of ids) await mock.fill(id, TOKEN);
  return ids;
}
