// My orders for pair orders (KobPair, KobCondPair, KobIfdPair) from REAL planned states: the row's type is the KAS kind the order mirrors, its prices
// are B per whole A (no KAS all-in price: the tip is KAS), it shows what it holds of each token and the trigger rule of a pair stop, and its only
// amend is the full pair ticket (cancel-replace: the protocol has no in-place pair amend). The replace form re-plans the very same order.
import { describe, expect, it } from 'vitest';
import type { OrderView } from '../../data/indexer-types';
import { planOrder, type Intent } from '../../kob/plan';
import { errors, type PairOrderPlan } from '../../kob/plan-types';
import { makePairEnv, tokenA } from '../../kob/orders/pair-fixtures';
import type { OrderState } from '../../kob/types';
import { amendKind, describeEntry, type OrderEntry, type OrderRowModel } from './orders-model';
import { intentFromOrder } from './replace-intent';
import { replaceEnv } from './ReplaceDialog';

const env = makePairEnv();
const A = tokenA();
const AMOUNT = 10n * A.scale;
const ctx = { kob: env.kob, clock: env.clock };

function stateOf(intent: unknown): OrderState {
  const p = planOrder(env, intent as Intent) as PairOrderPlan;
  expect(errors(p)).toEqual([]);
  return p.states[0]!;
}
const entryOf = (state: OrderState, id = 'ab'.repeat(32)): OrderEntry =>
  ({ id, view: null, record: null, resolved: { status: 'live', order: { transactionId: 'cd'.repeat(32), index: 0, amount: '2000000000', covenantId: id, state } } }) as unknown as OrderEntry;
const rowOf = (intent: unknown): { row: OrderRowModel; state: OrderState } => {
  const state = stateOf(intent);
  return { row: describeEntry(entryOf(state), ctx), state };
};

describe('pair orders in My orders', () => {
  it('a resting pair limit: type limit, prices B per whole A, no KAS all-in, A held (sell) or the B escrow held (buy), replace via the pair ticket', () => {
    const { row: sell, state } = rowOf({ type: 'limit', side: 'sell', amount: AMOUNT, price: 1_550n, tip: 2_000n });
    expect([sell.typeKey, sell.side, sell.price, sell.allInPrice]).toEqual(['limit', 'sell', 1_550n, null]);
    expect(sell.pair).toMatchObject({ kind: 'KobPair', base: A.covenantId, escrowA: AMOUNT, escrowB: 0n, tipKas: 2_000n, trigger: null });
    expect(amendKind(state, sell.typeKey)).toBe('replace');
    const { row: buy } = rowOf({ type: 'limit', side: 'buy', amount: AMOUNT, price: 1_400n });
    expect(buy.side).toBe('buy');
    expect(buy.pair!.escrowA).toBe(0n);
    expect(buy.pair!.escrowB).toBeGreaterThanOrEqual((AMOUNT * 1_400n) / A.scale);
  });

  it('a pair stop: type stop, the stop in B, the kob-wasm trigger rule (falls to the stop for a sell)', () => {
    const { row, state } = rowOf({ type: 'stopMarket', side: 'sell', amount: AMOUNT, stop: 1_400n });
    expect([row.typeKey, row.stopPrice, row.armed]).toEqual(['stop', 1_400n, false]);
    expect(row.pair!.trigger).toMatchObject({ direction: 'fallsTo', stop: '1400' });
    expect(amendKind(state, row.typeKey)).toBe('replace');
  });

  it('an if-done pair entry: type ifd / ifo; a sell-first one holds A and the B prefund', () => {
    const { row: ifd } = rowOf({ type: 'ifd', side: 'buy', amount: AMOUNT, entry: { price: 1_401n }, exit: { takeProfit: 1_601n } });
    expect([ifd.typeKey, ifd.price]).toEqual(['ifd', 1_401n]);
    expect(ifd.pair!.kind).toBe('KobIfdPair');
    expect(ifd.pair!.escrowB).toBeGreaterThan(0n);
    const { row: ifo } = rowOf({ type: 'ifo', side: 'sell', amount: AMOUNT, entry: { price: 1_599n }, exit: { takeProfit: 1_399n, stop: 1_700n } });
    expect(ifo.typeKey).toBe('ifo');
    expect(ifo.pair!.escrowA).toBe(AMOUNT);
    expect(ifo.pair!.escrowB).toBeGreaterThan(0n);
    const { row: rpt, state } = rowOf({ type: 'repeatIfd', side: 'buy', amount: AMOUNT, entry: { price: 1_400n }, exit: { takeProfit: 1_500n }, repeat: { count: 5n } });
    expect(rpt.typeKey).toBe('repeat');
    expect(amendKind(state, 'repeat')).toBeNull();
  });

  it('without a proven state the row reads the indexer view\'s pair object (custodies, the auction price now)', () => {
    const view = {
      covenant_id: 'ef'.repeat(32), contract: 'KobPair', status: 'open', side: 1, token: A.covenantId, state_known: false, state: null, current: null,
      genesis: { txid: '01'.repeat(32), daa: 1 }, initial_amount: '10000', amount_left: '10000', amount_estimated: false, tip: '0', expired: false,
      auction: { kind: 'pair_decay', origin_daa: 1, start_price: '1700', end_price: '1550', current_price: '1650', elapsed_daa: 5, complete: false },
      pair: {
        base: A.covenantId, quote: '71'.repeat(32), base_family: 'kcc20', quote_family: 'kcc20', base_template_hash: '00'.repeat(32), quote_template_hash: '00'.repeat(32),
        base_scale: 1000, quote_scale: 100, side: 'ask', price: '1700', quote_now: '1650', amount_left: '10000', delivery_carrier: '100000000',
        custodies: [{ token: A.covenantId, role: 'base', expected_amount: '10000' }],
      },
    } as unknown as OrderView;
    const row = describeEntry({ id: view.covenant_id, view, record: null, resolved: null }, ctx);
    expect(row.pair).toMatchObject({ kind: 'KobPair', quote: '71'.repeat(32), escrowA: 10_000n, escrowB: 0n, quoteNow: 1_650n });
    expect(row.allInPrice).toBeNull();
  });
});

describe('the pair replace form re-plans the same order (cancel-replace round trip)', () => {
  const roundTrip = (intent: unknown, typeKey: OrderRowModel['typeKey']) => {
    const { row, state } = rowOf(intent);
    expect(row.typeKey).toBe(typeKey);
    const back = intentFromOrder(state, { typeKey, expiryKind: row.expiry.kind, nowDaa: env.clock.daa, expiryUnix: row.expiry.approxUnix }, env.kob);
    expect(back.ok).toBe(true);
    if (!back.ok) return;
    const p = planOrder(replaceEnv(env, row, state), back.intent);
    expect(errors(p)).toEqual([]);
    expect(env.kob.encodeState(p.states[0]!)).toBe(env.kob.encodeState(state));
  };

  it('pair limit, stop, OCO, trailing stop and IFD / IFO entries', () => {
    roundTrip({ type: 'limit', side: 'sell', amount: AMOUNT, price: 1_550n }, 'limit');
    roundTrip({ type: 'limit', side: 'buy', amount: AMOUNT, price: 1_400n }, 'limit');
    roundTrip({ type: 'stopMarket', side: 'sell', amount: AMOUNT, stop: 1_400n }, 'stop');
    roundTrip({ type: 'oco', side: 'sell', amount: AMOUNT, stop: 1_400n, takeProfit: 1_700n }, 'oco');
    roundTrip({ type: 'trailingStop', side: 'sell', amount: AMOUNT, stop: 1_400n, trail: { step: 10n, gap: 30n } }, 'trailing');
    roundTrip({ type: 'ifd', side: 'buy', amount: AMOUNT, entry: { price: 1_401n }, exit: { takeProfit: 1_601n } }, 'ifd');
    roundTrip({ type: 'ifo', side: 'sell', amount: AMOUNT, entry: { price: 1_599n }, exit: { takeProfit: 1_399n, stop: 1_700n } }, 'ifo');
  });

  it('the replace environment returns the order\'s own A and B custodies to the planner', () => {
    const { row, state } = rowOf({ type: 'ifo', side: 'sell', amount: AMOUNT, entry: { price: 1_599n }, exit: { takeProfit: 1_399n, stop: 1_700n } });
    const bare = makePairEnv({ aAmounts: [], bAmounts: [] });
    const e = replaceEnv(bare, row, state) as ReturnType<typeof makePairEnv>;
    expect(e.tokenUtxos.reduce((a, u) => a + BigInt(u.state.amount), 0n)).toBe(AMOUNT);
    expect(e.pair.quoteTokenUtxos.reduce((a, u) => a + BigInt(u.state.amount), 0n)).toBe(row.pair!.escrowB);
  });
});

describe('the fill history of a pair order is volume only', () => {
  it('reads the B amount and the counterparty of `detail.pair`, never a price', async () => {
    const { fillHistory } = await import('./fills-model');
    const ev = (id: number, a: string, b: string, cp: string) => ({
      id, covenant_id: 'ab'.repeat(32), block_seq: 1, daa: 10 + id, ts: 1_000 + id, txid: 'cd'.repeat(32), tx_pos: 0, kind: 'fill', token: A.covenantId, side: 1,
      amount: a, price: '999', payout: null, closes: false, confirmations: 1, settled: true,
      detail: { pair: { side: 'ask', base: A.covenantId, quote: '71'.repeat(32), a_scale: 1000, amount_a: a, amount_b: b, price: '1550', counterparty: cp, price_source: 'none' } },
    });
    const h = fillHistory([ev(1, '1000', '1550', 'netting'), ev(2, '500', '775', 'route')] as never, A.scale);
    expect(h.rows.map((r) => [r.amount, r.price, r.pair])).toEqual([
      [1_000n, null, { amountB: 1_550n, counterparty: 'netting' }],
      [500n, null, { amountB: 775n, counterparty: 'route' }],
    ]);
    expect(h.avgPrice).toBeNull();
  });
});
