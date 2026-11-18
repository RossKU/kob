// Delivery of buy orders (bid-delivery-spec §10): the default per order class and token family, the top-up tip formula, the receive
// threshold, and the plans the simple planner builds with them (signed and validated by the script engine like every other plan).
// Each rule has a negative case that fails when the rule is removed.
import { describe, expect, it } from 'vitest';
import type { BidState } from '../types';
import type { OrderPlan } from '../plan-types';
import { errors } from '../plan-types';
import { KAS, TOK, consensusCheck, kob, makeEnv, market3x3, market8x8, marketKron } from '../../testing/fixtures';
import type { SimpleIntent } from '../intent-simple';
import { planDelivery, planSimple } from './simple';
import {
  RECV_THRESHOLD_SOMPI, TOPUP_TIP_MAX, choiceOfMode, defaultTopupTip, deliveryFixed, effectiveChoice, fundedFills, orderRecvThreshold, programTopupTip, resolveDelivery, topupOffer,
  topupTipTotal, walletThreshold, type DeliveryOrder,
} from './delivery';
import type { PlanIssue } from '../plan-types';
import { TOKEN_PROGRAMS } from '../programs.generated';
import { COVENANT_OUTPUT_MIN } from './common';

const K = kob();
const P = 250_000_000n; // 2.5 KAS per whole token
const DC = 10n * KAS; // the fixtures' delivery carrier
const order = (o: Partial<DeliveryOrder> = {}): DeliveryOrder => ({ codec: 'kcc20', program: 'KCC20Ref', cls: 'resting', carrier: DC, scale: 1_000n, minFill: 4_000n, price: P, ...o });
const codes = (xs: readonly PlanIssue[]): string[] => xs.map((i) => i.code);
const bidOf = (p: OrderPlan): BidState => p.states[0]!.state as BidState;

/** Plans, asserts success, validates the built transaction in the script engine and recovers the placement record. */
function planValid(intent: SimpleIntent, env = makeEnv()): OrderPlan {
  const p = planSimple(env, intent);
  expect(errors(p), JSON.stringify(errors(p))).toEqual([]);
  consensusCheck(K, p.built!);
  const rec = K.recoverOrders(p.built!.tx);
  expect(rec[0]!.order).toEqual(p.states[0]);
  K.checkDelivery(p.states[0]!, true);
  return p;
}

describe('the default delivery (bid-delivery-spec §10)', () => {
  it('Each fill for a resting KCC-20 buy; the wallet default and the user choice override it in that order', () => {
    expect(effectiveChoice(order(), undefined)).toBe('each');
    expect(effectiveChoice(order(), { walletDefault: 'collected' })).toBe('collected');
    expect(effectiveChoice(order(), { walletDefault: 'collected', choice: 'topup' })).toBe('topup');
    expect(effectiveChoice(order(), { walletDefault: 'topup', choice: 'collected' })).toBe('collected');
  });

  it('each fill, whatever is asked, for IOC / FOK / market orders, a repeating entry and a KRON token (negative: the rule removed gives them Collected)', () => {
    for (const o of [order({ cls: 'immediate' }), order({ cls: 'repeating' }), order({ codec: 'kron', program: 'KronToken2433' })]) {
      expect(deliveryFixed(o)).toBe(true);
      for (const choice of ['collected', 'topup'] as const) {
        expect(effectiveChoice(o, { choice, walletDefault: choice })).toBe('each');
        const pre: PlanIssue[] = [];
        expect(resolveDelivery(o, { choice }, pre).fields).toEqual({ mode: '0', recvThreshold: '0', topupTip: '0', bought: '0' });
        expect(pre).toEqual([]);
      }
    }
    expect(deliveryFixed(order())).toBe(false);
  });

  it('the modes of the choices; a mode-1 order is funded with one carrier, the others with one per fill (negative: Collected with per-fill carriers)', () => {
    expect([choiceOfMode('0'), choiceOfMode('1'), choiceOfMode('2'), choiceOfMode(undefined)]).toEqual(['each', 'collected', 'topup', 'each']);
    expect(fundedFills('collected', 3n)).toBe(1n);
    expect(fundedFills('each', 3n)).toBe(3n);
    expect(fundedFills('topup', 3n)).toBe(3n);
  });
});

describe('the top-up tip (bid-delivery-spec §10)', () => {
  it('is min(deliveryCarrier, ceil(1.25 x topup_extra)) (negative: no margin, no rounding up, no cap)', () => {
    expect(defaultTopupTip(DC, 595_400n)).toBe(744_250n);
    // 1.25 x 595_401 = 744_251.25: rounded up
    expect(defaultTopupTip(DC, 595_401n)).toBe(744_252n);
    expect(defaultTopupTip(DC, 595_401n)).not.toBe((595_401n * 5n) / 4n);
    // capped at the carrier
    expect(defaultTopupTip(1_000_000n, 2_304_000n)).toBe(1_000_000n);
    expect(defaultTopupTip(DC, null)).toBeNull();
    // from the measured table (topup_extra.json): every KCC-20 program has an extra, KRON none
    expect(programTopupTip('KCC20Ref', DC)).toBe(744_250n);
    expect(programTopupTip('KCC20Ref_8x8', DC)).toBe(defaultTopupTip(DC, 1_282_400n));
    for (const p of TOKEN_PROGRAMS) expect(programTopupTip(p.name, DC) === null, p.name).toBe(p.codec === 'kron');
    expect(programTopupTip('KronToken2433', DC)).toBeNull();
    expect(topupTipTotal(744_250n, 3n)).toBe(2_232_750n);
  });

  it('a tip above the cap or below 0 is refused, one below the default warns, up to the cap is accepted', () => {
    const run = (topupTip: bigint, carrier = DC) => {
      const pre: PlanIssue[] = [];
      const r = resolveDelivery(order({ carrier }), { choice: 'topup', topupTip }, pre);
      return { r, codes: codes(pre), pre };
    };
    expect(run(DC + 1n).codes).toEqual(['TOPUP_TIP_INVALID']);
    expect(run(-1n).codes).toEqual(['TOPUP_TIP_INVALID']);
    expect(run(744_249n).codes).toEqual(['TOPUP_TIP_LOW']);
    expect(run(744_250n)).toMatchObject({ codes: [], r: { fields: { topupTip: '744250' } } });
    // S4: below COVENANT_OUTPUT_MIN (0.08 KAS) even with a 1 KAS carrier, as kob-protocol's check_delivery
    expect(TOPUP_TIP_MAX).toBe(COVENANT_OUTPUT_MIN - 1n);
    expect(DC > TOPUP_TIP_MAX).toBe(true);
    expect(run(TOPUP_TIP_MAX)).toMatchObject({ codes: [], r: { fields: { topupTip: TOPUP_TIP_MAX.toString() } } });
    expect(run(TOPUP_TIP_MAX + 1n).codes).toEqual(['TOPUP_TIP_INVALID']);
    expect(run(TOPUP_TIP_MAX + 1n).pre[0]).toMatchObject({ params: { max: TOPUP_TIP_MAX } });
    expect(run(DC).codes).toEqual(['TOPUP_TIP_INVALID']);
    // the default never exceeds the cap, on every program
    for (const p of TOKEN_PROGRAMS) {
      const t = programTopupTip(p.name, DC);
      if (t !== null) expect(t <= TOPUP_TIP_MAX, p.name).toBe(true);
    }
    expect(defaultTopupTip(DC, 100_000_000n)).toBe(TOPUP_TIP_MAX);
  });
});

describe('the receive threshold (bid-delivery-spec §10)', () => {
  it('T = max(1, ceil(RECV_THRESHOLD_SOMPI x scale / price)): 5 KAS of the token at the order price', () => {
    expect(RECV_THRESHOLD_SOMPI).toBe(500_000_000n);
    // 5 KAS at 2.5 KAS per token: 2 tokens
    expect(walletThreshold(P, 1_000n)).toBe(2_000n);
    // rounded up
    expect(walletThreshold(300_000_000n, 1_000n)).toBe(1_667n);
    // a token worth more than 5 KAS per base unit: at least 1
    expect(walletThreshold(10n ** 15n, 1n)).toBe(1n);
    expect(walletThreshold(null, 1_000n)).toBeNull();
    expect(walletThreshold(0n, 1_000n)).toBeNull();
  });

  it("an order's recvThreshold is min(T, minFill - 1); at 0 top-up is not offered (negative: without the cap a fill of minFill could not top up)", () => {
    expect(orderRecvThreshold(2_000n, 4_000n)).toBe(2_000n);
    expect(orderRecvThreshold(2_000n, 1_500n)).toBe(1_499n);
    expect(orderRecvThreshold(2_000n, 1n)).toBe(0n);
    expect(topupOffer(order({ minFill: 1_500n }))).toMatchObject({ ok: true, threshold: 2_000n, recvThreshold: 1_499n, defaultTip: 744_250n });
    expect(topupOffer(order({ minFill: 1n }))).toEqual({ ok: false, reason: 'minFill' });
    expect(topupOffer(order({ price: null }))).toEqual({ ok: false, reason: 'price' });
    // a program row without an extra (only KRON rows; deliveryFixed catches KRON first, so a KCC-20 codec on it reaches the program rule)
    expect(topupOffer(order({ program: 'KronToken2433' }))).toEqual({ ok: false, reason: 'program' });
    expect(topupOffer(order({ program: 'KCC20Ref_8x8' }))).toMatchObject({ ok: true, defaultTip: defaultTopupTip(DC, 1_282_400n) });
    expect(topupOffer(order({ cls: 'immediate' }))).toEqual({ ok: false, reason: 'fixed' });
  });

  it('a stored T is reused whatever the price (negative: recomputing it from the price)', () => {
    expect(topupOffer(order({ price: 300_000_000n }), 2_000n)).toMatchObject({ ok: true, threshold: 2_000n, recvThreshold: 2_000n });
    expect(topupOffer(order({ price: 300_000_000n }))).toMatchObject({ ok: true, threshold: 1_667n });
  });

  it('a top-up the user chose that the order cannot have is an error; one that is only the wallet default falls back to Each fill', () => {
    const pre: PlanIssue[] = [];
    resolveDelivery(order({ minFill: 1n }), { choice: 'topup' }, pre);
    expect(pre.map((i) => [i.code, i.severity, i.field])).toEqual([['DELIVERY_TOPUP_UNAVAILABLE', 'error', 'delivery']]);
    const pre2: PlanIssue[] = [];
    const r = resolveDelivery(order({ minFill: 1n }), { walletDefault: 'topup' }, pre2);
    expect(r.fields.mode).toBe('0');
    expect(pre2.map((i) => [i.code, i.severity])).toEqual([['DELIVERY_TOPUP_FALLBACK', 'info']]);
    // negative: a wallet default the order can have is no fallback
    const pre3: PlanIssue[] = [];
    expect(resolveDelivery(order({ program: 'KCC20Ref_8x8' }), { walletDefault: 'topup' }, pre3).fields.mode).toBe('2');
    expect(pre3).toEqual([]);
  });
});

describe('plans with a delivery (signed and validated by the script engine)', () => {
  const limitBuy = (o: Partial<Extract<SimpleIntent, { type: 'limit' }>> = {}): SimpleIntent => ({ type: 'limit', side: 'buy', price: P, amount: 10n * TOK, minFill: 2_500n, ...o });

  it('a GTC limit buy is Each fill by default (mode 0, a carrier per fill); Collected is opt-in: mode 1, one carrier, maxFills does not add carriers', () => {
    const col = { delivery: { choice: 'collected' } } as const;
    const p = planValid(limitBuy(col));
    expect(bidOf(p)).toMatchObject({ mode: '1', recvThreshold: '0', topupTip: '0', bought: '0' });
    expect(BigInt(p.request!.value)).toBe(K.bidEscrow(p.states[0]!, 10n * TOK, 1n));
    expect(p.disclosure!.carriers.find((c) => c.kind === 'deliveryCarrier')).toEqual({ kind: 'deliveryCarrier', amount: DC, count: 1 });
    expect(p.disclosure!.notes).toContain('deliveryCollected');
    expect(planDelivery(p)).toMatchObject({ delivery: { choice: 'collected' }, fills: 1n });
    const more = planValid(limitBuy({ maxFills: 5n, ...col }));
    expect(more.request!.value).toBe(p.request!.value);
    // the default: each fill funds one carrier per fill (4 fills of 2.5 tokens, at most the default 3)
    const each = planValid(limitBuy());
    expect(bidOf(each).mode).toBe('0');
    expect(planDelivery(each)).toMatchObject({ delivery: { choice: 'each' } });
    expect(BigInt(each.request!.value) - BigInt(p.request!.value)).toBe(2n * DC + 2n);
  });

  it('Top-up: mode 2 with the threshold and the tip, funded with one carrier per fill', () => {
    const p = planValid(limitBuy({ delivery: { choice: 'topup' } }));
    // T = 2 tokens at 2.5 KAS (5 KAS), below minFill - 1
    expect(bidOf(p)).toMatchObject({ mode: '2', recvThreshold: '2000', topupTip: '744250', bought: '0' });
    expect(BigInt(p.request!.value)).toBe(K.bidEscrow(p.states[0]!, 10n * TOK, 3n));
    expect(p.disclosure!.notes).toContain('deliveryTopup');
    expect(planDelivery(p)).toMatchObject({ delivery: { choice: 'topup', threshold: 2_000n }, fills: 3n });
    // a stored threshold and a tip raised to the cap (S4: below 0.08 KAS)
    const q = planValid(limitBuy({ delivery: { choice: 'topup', threshold: 1_000n, topupTip: TOPUP_TIP_MAX } }));
    expect(bidOf(q)).toMatchObject({ recvThreshold: '1000', topupTip: TOPUP_TIP_MAX.toString() });
    // the threshold is capped at minFill - 1
    expect(bidOf(planValid(limitBuy({ minFill: 1_200n, delivery: { choice: 'topup' } })))).toMatchObject({ recvThreshold: '1199' });
  });

  it('Top-up on the 8x8 program with its measured tip, refused for a minimum fill of 1', () => {
    const on8x8 = planValid(limitBuy({ delivery: { choice: 'topup' } }), makeEnv({ market: market8x8() }));
    expect(bidOf(on8x8)).toMatchObject({ mode: '2', topupTip: defaultTopupTip(DC, 1_282_400n)!.toString() });
    expect(codes(on8x8.issues)).not.toContain('DELIVERY_TOPUP_UNAVAILABLE');
    const one = planSimple(makeEnv(), limitBuy({ minFill: 1n, delivery: { choice: 'topup' } }));
    expect(codes(one.issues)).toContain('DELIVERY_TOPUP_UNAVAILABLE');
  });

  it('IOC and market buys (a multi-fill one too) deliver at each fill, with a carrier per fill (negative: the class rule removed)', () => {
    const ioc = planValid({ type: 'ioc', side: 'buy', price: P + 1_100_000n, amount: 2n * TOK });
    expect(bidOf(ioc).mode).toBe('0');
    const market = planValid({ type: 'market', side: 'buy', amount: 3n * TOK, maxFills: 3n });
    expect(bidOf(market).mode).toBe('0');
    expect(planDelivery(market)).toMatchObject({ delivery: { choice: 'each' } });
    expect(planDelivery(market)!.fills).toBeGreaterThan(1n);
    expect(market.disclosure!.carriers.find((c) => c.kind === 'deliveryCarrier')!.count).toBe(Number(planDelivery(market)!.fills));
  });

  it('a KRON buy delivers at each fill (negative: Collected on a KRON token)', () => {
    const p = planValid(limitBuy({ delivery: { choice: 'collected' } }), makeEnv({ market: marketKron() }));
    expect(bidOf(p).mode).toBe('0');
    expect(p.disclosure!.carriers.find((c) => c.kind === 'deliveryCarrier')!.count).toBe(3);
  });

  it('DCA and a rising bid are Each fill by default; picking Collected gives one carrier', () => {
    const dca: SimpleIntent = { type: 'dca', amount: 5n * TOK, sliceAmount: TOK, interval: { daa: 600n }, price: 245_000_000n };
    const p = planValid({ ...dca, delivery: { choice: 'collected' } } as SimpleIntent);
    expect(bidOf(p).mode).toBe('1');
    expect(BigInt(p.request!.value)).toBe(K.bidEscrow(p.states[0]!, 5n * TOK, 1n));
    const each = planValid(dca);
    expect(bidOf(each).mode).toBe('0');
    expect(BigInt(each.request!.value)).toBe(K.bidEscrow(each.states[0]!, 5n * TOK, 5n));
    const risingIntent: SimpleIntent = { type: 'dutch', side: 'buy', amount: 5n * TOK, price: 200_000_000n, priceEnd: 240_000_000n, duration: { daa: 4_000n } };
    expect(bidOf(planValid(risingIntent)).mode).toBe('0');
    expect(bidOf(planValid({ ...risingIntent, delivery: { choice: 'collected' } } as SimpleIntent)).mode).toBe('1');
  });

  it('a crossing Collected buy is not warned for its fills (one carrier serves every fill)', () => {
    const p = planValid(limitBuy({ price: P + 1_100_000n, amount: 20n * TOK, minFill: undefined, delivery: { choice: 'collected' } }));
    expect(bidOf(p).mode).toBe('1');
    expect(codes(p.issues)).not.toContain('FILLS_BUDGET_SHORT');
    expect(p.fills).toMatchObject({ carrierDeposit: DC, scope: 'crossing' });
  });

  it('a sell has no delivery', () => {
    const p = planValid({ type: 'limit', side: 'sell', price: P, amount: 10n * TOK }, makeEnv({ market: market3x3() }));
    expect(planDelivery(p)).toBeNull();
  });
});
