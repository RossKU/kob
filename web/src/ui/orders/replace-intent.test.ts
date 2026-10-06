// The replace form (R-5): a live order turned back into its ticket intent re-plans to the SAME order (round trip), and an edited intent becomes the
// replacement of one atomic cancel + new order transaction that the script engine accepts.
import { describe, expect, it } from 'vitest';
import { planCancelReplace, type OrderSnapshot } from '../../kob/cancel';
import type { CondIntent } from '../../kob/intent-cond';
import { planOrder } from '../../kob/plan';
import type { OrderState } from '../../kob/types';
import { CLOCK, KAS, MAKER_PK, MAKER_SK, TOK, consensusCheck, keyUtxo, makeEnv } from '../../testing/fixtures';
import { planOk } from '../../kob/orders/cond-testkit';
import { intentFromOrder } from './replace-intent';
import { replaceEnv, specOfPlan } from './ReplaceDialog';
import type { OrderRowModel } from './orders-model';

const env = makeEnv();
const ctx = { typeKey: 'stop' as const, expiryKind: 'gtc' as const, nowDaa: CLOCK.daa };

/** Places `intent` (consensus checked) and returns the live order as a snapshot. */
function placed(intent: CondIntent): { state: OrderState; snap: OrderSnapshot } {
  const { signed, recovered } = planOk(env, intent);
  const r = recovered[0]!;
  const state = r.order as OrderState;
  const custody = r.custody
    ? { transactionId: signed.tx.id, index: r.custody.output, amount: r.custody.value, covenantId: env.token.covenantId, blockDaaScore: '1000', state: r.custody.state }
    : null;
  const snap: OrderSnapshot = {
    covenantId: r.covenantId, order: { transactionId: signed.tx.id, index: r.output, amount: r.value, covenantId: r.covenantId, blockDaaScore: '1000', state },
    custody, strays: [], refundDueDaa: null, deadline: null, source: 'chain',
  };
  return { state, snap };
}

const rowOf = (snap: OrderSnapshot, typeKey: OrderRowModel['typeKey']): OrderRowModel =>
  ({ id: snap.covenantId, typeKey, entry: { id: snap.covenantId, view: null, record: null, resolved: { order: snap.order } } } as unknown as OrderRowModel);

const roundTrip = (intent: CondIntent, typeKey: OrderRowModel['typeKey']) => {
  const { state, snap } = placed(intent);
  const back = intentFromOrder(state, { ...ctx, typeKey }, env.kob);
  expect(back.ok).toBe(true);
  if (!back.ok) throw new Error('no intent');
  const p = planOrder(replaceEnv(env, rowOf(snap, typeKey), state), back.intent);
  expect(p.issues.filter((i) => i.severity === 'error')).toEqual([]);
  // the same order again: every term but the GTC expiry (counted from now, the same clock here) is what the order had
  expect(env.kob.encodeState(p.states[0]!)).toBe(env.kob.encodeState(state));
  return { state, snap, intent: back.intent, plan: p };
};

describe('intentFromOrder round trip (the replace form opens on the order as it is)', () => {
  it('stop-market, stop-limit, take-profit, OCO, trailing (sell and buy)', () => {
    roundTrip({ type: 'stopMarket', side: 'sell', amount: 5n * TOK, stop: 230_000_000n }, 'stop');
    roundTrip({ type: 'stopLimit', side: 'sell', amount: 5n * TOK, stop: 230_000_000n, limit: 220_000_000n }, 'stop');
    roundTrip({ type: 'takeProfit', side: 'buy', amount: 4n * TOK, price: 230_000_000n }, 'take-profit');
    roundTrip({ type: 'oco', side: 'sell', amount: 6n * TOK, takeProfit: 270_000_000n, stop: 230_000_000n, minTouch: 2n * TOK }, 'oco');
    roundTrip({ type: 'trailingStop', side: 'sell', amount: 3n * TOK, stop: 230_000_000n, trail: { step: 1_000_000n, gap: 2_000_000n } }, 'trailing');
    roundTrip({ type: 'stopMarket', side: 'buy', amount: 3n * TOK, stop: 270_000_000n }, 'stop');
  });

  it('IFD / IFO entries, buy first and sell first, stop entries', () => {
    roundTrip({ type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n } }, 'ifd');
    roundTrip({ type: 'ifd', side: 'sell', amount: 10n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n } }, 'ifd');
    roundTrip({ type: 'ifo', side: 'buy', amount: 8n * TOK, entry: { price: 240_000_000n, stop: 235_000_000n }, exit: { takeProfit: 300_000_000n, stop: 220_000_000n } }, 'ifo');
  });

  it('repeat entries are not replaced (their exits re-arm them)', () => {
    const { state } = placed({ type: 'repeatIfd', side: 'buy', amount: 4n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 260_000_000n }, repeat: { count: 2n } });
    expect(intentFromOrder(state, { ...ctx, typeKey: 'repeat' }, env.kob)).toEqual({ ok: false, reason: 'repeat' });
  });
});

describe('an edited intent becomes one atomic cancel + replacement (consensus valid)', () => {
  const cancelEnv = { kob: env.kob, maker: MAKER_PK, funding: [keyUtxo(100n * KAS)], tokenUtxos: env.tokenUtxos, clock: { daa: CLOCK.daa } };

  it('a stop-limit moved with its limit, a band and trigger rule change', () => {
    const { state, snap, intent } = roundTrip({ type: 'stopLimit', side: 'sell', amount: 5n * TOK, stop: 230_000_000n, limit: 220_000_000n }, 'stop');
    const edited = { ...intent, stop: 232_000_000n, limit: 225_000_000n, minRestDaa: 100n } as CondIntent;
    const p = planOrder(replaceEnv(env, rowOf(snap, 'stop'), state), edited);
    expect(p.ok).toBe(true);
    const spec = specOfPlan(p)!;
    const plan = planCancelReplace(cancelEnv, snap, spec);
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    expect(plan.request!.action).toBe('cancelOrder');
    consensusCheck(env.kob, plan.built!, [MAKER_SK]);
  });

  it('an IFD entry with a new limit and take-profit, buy first (escrow re-planned)', () => {
    const { state, snap, intent } = roundTrip({ type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n } }, 'ifd');
    const edited = { ...intent, entry: { price: 242_000_000n }, exit: { ...(intent as { exit: object }).exit, takeProfit: 310_000_000n } } as CondIntent;
    const p = planOrder(replaceEnv(env, rowOf(snap, 'ifd'), state), edited);
    expect(p.ok).toBe(true);
    const plan = planCancelReplace(cancelEnv, snap, specOfPlan(p)!);
    expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
    consensusCheck(env.kob, plan.built!, [MAKER_SK]);
  });

  it('the replace environment counts the order\'s own custody and KAS and drops it from the self-trade check', () => {
    const { state, snap } = placed({ type: 'stopMarket', side: 'sell', amount: 5n * TOK, stop: 230_000_000n });
    const own = { covenantId: snap.covenantId, side: 'sell' as const, price: 1n, tip: 0n, amountLeft: 5n * TOK, active: true };
    const e = replaceEnv({ ...env, ownOrders: [own], tokenUtxos: [], funding: [] }, rowOf(snap, 'stop'), state);
    expect(e.ownOrders).toEqual([]);
    expect(e.tokenUtxos.reduce((a, u) => a + BigInt(u.state.amount), 0n)).toBe(5n * TOK);
    expect(e.funding.map((f) => f.amount)).toEqual([snap.order.amount]);
  });
});
