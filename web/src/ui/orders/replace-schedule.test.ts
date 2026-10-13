// Amend / renew of the scheduled plain kinds (TWAP, DCA, Dutch / rising, timed activation; KAS and pair): the live order is turned back into its
// ticket intent with its schedule, the ticket form re-plans it to the SAME order (the path the replace dialog takes: intent -> form -> intent),
// and an edit becomes one atomic cancel + replacement the script engine accepts. A Dutch auction goes on from where its decay is now.
import { describe, expect, it } from 'vitest';
import { planCancelReplace, type OrderSnapshot } from '../../kob/cancel';
import { planOrder, type Intent } from '../../kob/plan';
import { errors } from '../../kob/plan-types';
import type { OrderState } from '../../kob/types';
import { describeOrder } from '../../kob/order-facts';
import { makePairEnv } from '../../kob/orders/pair-fixtures';
import { CLOCK, KAS, MAKER_PK, MAKER_SK, TOK, consensusCheck, keyUtxo, makeEnv } from '../../testing/fixtures';
import { buildIntent, formFromIntent, formatLocalDateTime, type TicketCtx } from '../ticket/form-state';
import { intentFromOrder, type ReplaceContext } from './replace-intent';
import { replaceEnv, specOfPlan } from './ReplaceDialog';
import { amendKind, type OrderRowModel, type OrderTypeKey } from './orders-model';

const env = makeEnv();
const TICKET: TicketCtx = { decimals: 3, scale: 1000n, tick: 100n, rateMilli: CLOCK.rateMilli, tzOffsetMin: 0 };

/** Places `intent` (consensus checked) and returns the live order as a snapshot. */
function placed(intent: Intent, e = env): { state: OrderState; snap: OrderSnapshot } {
  const plan = planOrder(e, intent);
  expect(errors(plan), JSON.stringify(errors(plan), (_k, v) => (typeof v === 'bigint' ? v.toString() : v))).toEqual([]);
  const signed = consensusCheck(e.kob, plan.built!);
  const r = e.kob.recoverOrders(signed.tx)[0]!;
  const state = r.order as OrderState;
  const custody = r.custody
    ? { transactionId: signed.tx.id, index: r.custody.output, amount: r.custody.value, covenantId: e.token.covenantId, blockDaaScore: '1000', state: r.custody.state }
    : null;
  const snap: OrderSnapshot = {
    covenantId: r.covenantId, order: { transactionId: signed.tx.id, index: r.output, amount: r.value, covenantId: r.covenantId, blockDaaScore: '1000', state },
    custody, strays: [], refundDueDaa: null, deadline: null, source: 'chain',
  };
  return { state, snap };
}

const rowOf = (snap: OrderSnapshot, typeKey: OrderTypeKey): OrderRowModel =>
  ({ id: snap.covenantId, typeKey, entry: { id: snap.covenantId, view: null, record: null, resolved: { order: snap.order } } } as unknown as OrderRowModel);

const ctxOf = (typeKey: OrderTypeKey, snap: OrderSnapshot, over: Partial<ReplaceContext> = {}): ReplaceContext => ({
  typeKey, expiryKind: 'gtc', nowDaa: CLOCK.daa, clock: CLOCK, value: BigInt(snap.order.amount), tick: 100n, ...over,
});

/** intent -> the replace dialog's form -> intent again, planned against the replace environment. */
function replanned(state: OrderState, snap: OrderSnapshot, typeKey: OrderTypeKey, over: Partial<ReplaceContext> = {}, e = env, ticket = TICKET) {
  const back = intentFromOrder(state, ctxOf(typeKey, snap, over), e.kob);
  expect(back.ok, String(!back.ok && back.reason)).toBe(true);
  if (!back.ok) throw new Error('no intent');
  const form = formFromIntent(back.intent, ticket);
  const built = buildIntent(form, ticket);
  expect(built.errors).toEqual([]);
  const p = planOrder(replaceEnv(e, rowOf(snap, typeKey), state), built.intent!);
  expect(errors(p)).toEqual([]);
  return { intent: back.intent, form, plan: p };
}

describe('scheduled plain orders open in the replace form with their schedule and re-plan to the same order', () => {
  it('TWAP (sell) and DCA (buy), with and without a per-slice auction', () => {
    const cases: [OrderTypeKey, Intent][] = [
      ['twap', { type: 'twap', side: 'sell', amount: 12n * TOK, sliceAmount: 3n * TOK, interval: { seconds: 120n }, price: 260_000_000n }],
      ['twap', { type: 'twap', side: 'sell', amount: 12n * TOK, sliceAmount: 4n * TOK, interval: { seconds: 60n }, price: 270_000_000n, priceEnd: 260_000_000n, sliceAuction: { seconds: 20n } }],
      ['dca', { type: 'dca', side: 'buy', amount: 9n * TOK, sliceAmount: 3n * TOK, interval: { seconds: 300n }, price: 240_000_000n }],
      ['dca', { type: 'dca', side: 'buy', amount: 8n * TOK, sliceAmount: 2n * TOK, interval: { seconds: 60n }, price: 230_000_000n, priceEnd: 240_000_000n, sliceAuction: { seconds: 30n } }],
    ];
    for (const [key, intent] of cases) {
      const { state, snap } = placed(intent);
      expect(amendKind(state, key)).toBe('replace');
      const { form, plan } = replanned(state, snap, key);
      expect(form.type).toBe(key);
      // the same order again: slice, interval, prices, auction slope, amount, minimum fill (the GTC expiry counts from the same clock here)
      expect(env.kob.encodeState(plan.states[0]!), `${key} ${JSON.stringify(form.values)}`).toBe(env.kob.encodeState(state));
    }
  });

  it('a timed limit keeps its activation (to the next whole minute, never earlier)', () => {
    const at = CLOCK.unixSeconds + 3_600n;
    for (const side of ['sell', 'buy'] as const) {
      const { state, snap } = placed({ type: 'limit', side, amount: 5n * TOK, price: side === 'sell' ? 270_000_000n : 230_000_000n, activeFrom: { unixSeconds: at } });
      expect(amendKind(state, 'timed')).toBe('replace');
      const { form, plan } = replanned(state, snap, 'timed');
      expect(form.type).toBe('limit');
      expect(form.values.activeFrom).toBe(formatLocalDateTime(at));
      const now = describeOrder(plan.states[0]!);
      const was = describeOrder(state);
      expect(now.activeFrom).toBeGreaterThanOrEqual(was.activeFrom);
      expect(now.activeFrom - was.activeFrom).toBeLessThan(600n); // under a minute of DAA
      expect(now.price).toBe(was.price);
    }
  });

  it('a Dutch auction goes on from its current price to the same end at the same pace; an ended one rests at its end as a limit', () => {
    const { state, snap } = placed({ type: 'dutch', side: 'sell', amount: 6n * TOK, price: 300_000_000n, priceEnd: 260_000_000n, duration: { seconds: 400n }, stepDaa: 10n });
    const was = describeOrder(state);
    const a = was.auction!;
    expect(amendKind(state, 'dutch')).toBe('replace');
    // 1,000 DAA into the decay: 100 steps of `slope` taken
    const later = was.activeFrom + 1_000n + 5n;
    const clock = { ...CLOCK, daa: later };
    const back = intentFromOrder(state, ctxOf('dutch', snap, { nowDaa: later, clock }), env.kob);
    expect(back.ok).toBe(true);
    if (!back.ok) return;
    const current = 300_000_000n - 100n * a.slope;
    expect(back.intent).toMatchObject({ type: 'dutch', price: current, priceEnd: 260_000_000n, stepDaa: 10n });
    const p = planOrder(replaceEnv({ ...env, clock }, rowOf(snap, 'dutch'), state), back.intent);
    expect(errors(p)).toEqual([]);
    const next = describeOrder(p.states[0]!);
    expect(next.price).toBe(current);
    expect(next.auction).toMatchObject({ priceEnd: 260_000_000n, slope: a.slope, decayStep: 10n });
    // the decay over: a limit at the end price
    const done = intentFromOrder(state, ctxOf('dutch', snap, { nowDaa: was.activeFrom + 10_000n }), env.kob);
    expect(done).toMatchObject({ ok: true, intent: { type: 'limit', price: 260_000_000n } });
    // not started yet (a future activation): the planned terms, unchanged
    const early = intentFromOrder(state, ctxOf('dutch', snap, { nowDaa: CLOCK.daa }), env.kob);
    expect(early).toMatchObject({ ok: true, intent: { type: 'dutch', price: 300_000_000n } });
  });

  it('a rising bid (buy) goes on upward the same way', () => {
    const { state, snap } = placed({ type: 'dutch', side: 'buy', amount: 6n * TOK, price: 200_000_000n, priceEnd: 240_000_000n, duration: { seconds: 400n } });
    const was = describeOrder(state);
    const later = was.activeFrom + 50n;
    const back = intentFromOrder(state, ctxOf('dutch', snap, { nowDaa: later, clock: { ...CLOCK, daa: later } }), env.kob);
    expect(back).toMatchObject({ ok: true, intent: { type: 'dutch', side: 'buy', price: 200_000_000n + 50n * was.auction!.slope, priceEnd: 240_000_000n } });
    // the buy's amount is what its escrow buys (kob-wasm bidBuyingPower), at least the placed amount
    expect((back as { intent: { amount: bigint } }).intent.amount).toBeGreaterThanOrEqual(6n * TOK);
  });

  it('pair TWAP / Dutch / timed orders the same way (prices B per whole A)', () => {
    const pe = makePairEnv();
    const PT: TicketCtx = { decimals: 3, scale: 1000n, tick: 1n, quoteDecimals: 2, rateMilli: CLOCK.rateMilli, tzOffsetMin: 0 };
    const twap = placed({ type: 'twap', side: 'sell', amount: 10n * 1000n, sliceAmount: 2_000n, interval: { seconds: 120n }, price: 1_550n }, pe);
    expect(amendKind(twap.state, 'twap')).toBe('replace');
    const t = replanned(twap.state, twap.snap, 'twap', { tick: undefined }, pe, PT);
    expect(pe.kob.encodeState(t.plan.states[0]!)).toBe(pe.kob.encodeState(twap.state));
    const timed = placed({ type: 'limit', side: 'buy', amount: 5_000n, price: 1_400n, activeFrom: { unixSeconds: CLOCK.unixSeconds + 7_200n } }, pe);
    expect(amendKind(timed.state, 'timed')).toBe('replace');
    expect(replanned(timed.state, timed.snap, 'timed', { tick: undefined }, pe, PT).form.values.activeFrom).toBe(formatLocalDateTime(CLOCK.unixSeconds + 7_200n));
  });
});

describe('an edited scheduled order becomes one atomic cancel + replacement (consensus valid)', () => {
  const cancelEnv = { kob: env.kob, maker: MAKER_PK, funding: [keyUtxo(100n * KAS)], tokenUtxos: env.tokenUtxos, clock: { daa: CLOCK.daa } };

  it('a TWAP with a new slice and a DCA with a new price', () => {
    for (const [key, intent, edit] of [
      ['twap', { type: 'twap', side: 'sell', amount: 12n * TOK, sliceAmount: 3n * TOK, interval: { seconds: 120n }, price: 260_000_000n }, { sliceAmount: 4n * TOK }],
      ['dca', { type: 'dca', side: 'buy', amount: 9n * TOK, sliceAmount: 3n * TOK, interval: { seconds: 300n }, price: 240_000_000n }, { price: 238_000_000n }],
    ] as [OrderTypeKey, Intent, object][]) {
      const { state, snap } = placed(intent);
      const { intent: back } = replanned(state, snap, key);
      const p = planOrder(replaceEnv(env, rowOf(snap, key), state), { ...back, ...edit } as Intent);
      expect(errors(p)).toEqual([]);
      const plan = planCancelReplace(cancelEnv, snap, specOfPlan(p)!);
      expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
      consensusCheck(env.kob, plan.built!, [MAKER_SK]);
    }
  });
});

describe('amendKind of the plain kinds', () => {
  it('a plain KAS limit keeps its in-place amend; IOC / FOK / market are never replaced', () => {
    const { state } = placed({ type: 'limit', side: 'sell', amount: 5n * TOK, price: 270_000_000n });
    expect(amendKind(state, 'limit')).toBe('limit');
    for (const k of ['ioc', 'fok', 'market'] as const) expect(amendKind(state, k)).toBeNull();
    expect(intentFromOrder(state, { typeKey: 'limit', expiryKind: 'gtc', nowDaa: CLOCK.daa }, env.kob)).toEqual({ ok: false, reason: 'kind' });
  });
});
