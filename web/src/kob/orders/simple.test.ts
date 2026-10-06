// Planner tests for every simple order type, side and tif, on the 3x3 reference program and the 8x8 KOB program.
// Oracle: each plan is signed locally, finalized with tightened budgets and validated by the script engine (consensus rules), the
// placement record recovered from the transaction equals the planned state, and the disclosure numbers reconcile with the built tx.
// Amounts are base units of the 3-decimal test token (TOK = 1000 = one whole token = the scale), prices sompi per whole token.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { AskState, BidState } from '../types';
import type { OrderPlan, PlanEnv, TokenMarket } from '../plan-types';
import { errors } from '../plan-types';
import { planSimple } from './simple';
import { planOrder } from '../plan';
import type { SimpleIntent } from '../intent-simple';
import {
  CLOCK, KAS, MAKER_PK, TOK, bookOrder, consensusCheck, kob, level, makeEnv, market3x3, market8x8, marketKron, tokenUtxo,
} from '../../testing/fixtures';
import { MAX_IDLE_DAA } from '../daa';
import { baseKind } from '../order-facts';
import { ceilDiv, quoteOf } from '../units';

const golden = JSON.parse(readFileSync(fileURLToPath(new URL('../../../../crates/kob-protocol/vectors/golden.json', import.meta.url)), 'utf8'));
const goldenState = (name: string): Record<string, string> => golden.transactions.find((t: { name: string }) => t.name === name).request.order.state;

const K = kob();
const P = 250_000_000n; // 2.50 KAS per token: on the tick (100) and inside the default book
const SCALE = 1_000n;
const DC = 10n * KAS;
const ask = (p: OrderPlan): AskState => {
  const s = p.states[0];
  if (baseKind(s.kind) !== 'KobAsk') throw new Error('not an ask');
  return s.state as AskState;
};
const bid = (p: OrderPlan): BidState => {
  const s = p.states[0];
  if (baseKind(s.kind) !== 'KobBid') throw new Error('not a bid');
  return s.state as BidState;
};
const sum = (xs: bigint[]): bigint => xs.reduce((a, b) => a + b, 0n);
/** BidState::escrow by the protocol rule: used(amount) = ceil(amount x rate / scale), one sompi per extra fill, a carrier per fill, the reserve. */
const escrow = (amount: bigint, rate: bigint, fills: bigint, dc = DC, reserve = 0n): bigint => ceilDiv(amount * rate, SCALE) + (fills - 1n) + fills * dc + reserve;
/** The wallet's default minimum fill: the amount worth 10 KAS at `price`, clamped to 1..amount (kob-protocol defaults::default_min_fill). */
const defMin = (amount: bigint, price: bigint): bigint => {
  const want = ceilDiv(10n * KAS * SCALE, price);
  return want < 1n ? 1n : want > amount ? amount : want;
};

/**
 * Plans, asserts success and runs every oracle: consensus validity, recovered placement record, fee and carrier accounting.
 */
function planValid(env: PlanEnv, intent: SimpleIntent): OrderPlan {
  const p = planSimple(env, intent);
  expect(errors(p), JSON.stringify(errors(p))).toEqual([]);
  expect(p.ok).toBe(true);
  const built = p.built!;
  const d = p.disclosure!;
  expect(p.request!.action).toBe('createOrder');
  consensusCheck(K, built);

  // the placement record recovered from the transaction (trusting nothing) is exactly the planned order
  const rec = K.recoverOrders(built.tx);
  expect(rec).toHaveLength(1);
  expect(K.encodeState(rec[0].order)).toBe(K.encodeState(p.states[0]));
  expect(rec[0].order).toEqual(p.states[0]);
  expect(BigInt(rec[0].value)).toBe(BigInt(p.request!.value));
  // the numeric gate of the protocol holds for every planned order
  K.checkNumbers(p.states[0]);

  // accounting: inputs = outputs + fee; kasLocked = inputs - KAS change - fee - the wallet-owned token-change carrier
  const inSum = sum(built.tx.inputs.map((i) => BigInt(i.utxo.amount)));
  const outSum = sum(built.tx.outputs.map((o) => BigInt(o.value)));
  const fee = BigInt(built.fee.fee);
  expect(inSum - outSum).toBe(fee);
  expect(d.fee).toBe(fee);
  const change = built.fee.changeOutput == null ? 0n : BigInt(built.tx.outputs[built.fee.changeOutput].value);
  const kept = sum(d.carriers.filter((c) => c.kept).map((c) => c.amount * BigInt(c.count)));
  const locked = sum(d.carriers.filter((c) => !c.kept).map((c) => c.amount * BigInt(c.count)));
  expect(d.kasLocked).toBe(locked);
  expect(d.kasLocked).toBe(inSum - change - fee - kept);
  const orderValue = BigInt(built.tx.outputs[0].value);
  const st = d.side === 'sell' ? ask(p) : bid(p);
  if (d.side === 'buy') {
    expect(d.kasLocked).toBe(orderValue);
    expect(d.tokensEscrowed).toBe(0n);
    // the escrow is exactly kob-wasm bidEscrow for the disclosed amount and the budgeted fills
    const fills = BigInt(d.carriers.find((c) => c.kind === 'deliveryCarrier')!.count);
    expect(orderValue).toBe(K.bidEscrow(p.states[0], d.tokenAmount, fills));
  } else {
    expect(d.kasLocked).toBe(orderValue + BigInt(built.tx.outputs[1].value));
    expect(d.tokensEscrowed).toBe(BigInt(rec[0].custody!.state.amount));
    expect(d.tokenAmount).toBe(BigInt(ask(p).amountLeft));
  }
  // all-in arithmetic derives from the state, not from the form; the total follows the quote rule (a seller receives the ceil, a buyer pays the floor)
  if (d.limitPrice !== null) {
    expect(d.allInPrice).toBe(d.side === 'sell' ? d.limitPrice - d.tip : d.limitPrice + d.tip);
    expect(d.allInTotal).toBe(quoteOf(d.tokenAmount, d.allInPrice!, d.scale, d.side === 'sell' ? 'up' : 'down'));
  }
  expect(d.scale).toBe(BigInt(st.scale));
  expect(d.minFill).toBe(BigInt(st.minFill));
  expect(d.minTouch).toBeNull();
  expect(d.expiry.daa).toBe(BigInt(st.expiryDaa));
  return p;
}

const markets: [string, TokenMarket][] = [['3x3 KCC20Ref', market3x3()], ['8x8 KCC20Ref_8x8', market8x8()], ['KRON 2433', marketKron()]];

describe.each(markets)('limit orders on %s', (_name, market) => {
  const env = () => makeEnv({ market });

  it('GTC sell: KobAsk with custody, expiry = placement + 90 days, refund tip of the program, default minimum fill', () => {
    const p = planValid(env(), { type: 'limit', side: 'sell', price: 300_000_000n, amount: 10n * TOK, tip: 100_000n });
    const s = ask(p);
    expect(s).toMatchObject({
      maker: MAKER_PK, tokenCovId: market.covenantId, tokenTplHash: market.templateHash, tplPrefixLen: String(market.prefixLen), tplSuffixLen: String(market.suffixLen),
      scale: '1000', price: '300000000', tip: '100000', tif: '0', activeFrom: '0', expiryDaa: String(CLOCK.daa + MAX_IDLE_DAA),
      refundTip: market.refundTip.toString(), interval: '0', maxFill: '0', slope: '0', priceEnd: '0', amountLeft: '10000',
    });
    // the amount worth 10 KAS at 3 KAS per token: ceil(10 / 3 tokens) = 3334 base units
    expect(s.minFill).toBe('3334');
    expect(BigInt(s.minFill)).toBe(K.defaultMinFill(10n * TOK, 300_000_000n, SCALE));
    const d = p.disclosure!;
    expect(d.side).toBe('sell');
    expect(d.limitPrice).toBe(300_000_000n);
    expect(d.allInPrice).toBe(299_900_000n);
    expect(d.allInTotal).toBe(2_999_000_000n);
    expect(d.tokensEscrowed).toBe(10_000n);
    expect(d.minFill).toBe(3_334n);
    expect(d.expiry).toMatchObject({ kind: 'gtc', daa: CLOCK.daa + MAX_IDLE_DAA, deadlineUnixSeconds: null });
    expect(d.expiry.approxUnixSeconds).toBe(CLOCK.unixSeconds + 90n * 86_400n);
    expect(d.refundTip).toBe(market.refundTip);
    expect(d.notes).toContain('gtc');
    // carriers: order 10 KAS + custody 10 KAS locked; the token change carrier (100 tokens held, 10 sold) stays with the wallet
    expect(d.carriers).toEqual([
      { kind: 'orderCarrier', amount: 10n * KAS, count: 1 },
      { kind: 'tokenCarrier', amount: 10n * KAS, count: 1 },
      { kind: 'tokenChangeCarrier', amount: 10n * KAS, count: 1, kept: true },
    ]);
    expect(d.kasLocked).toBe(20n * KAS);
    expect(p.built!.covenants).toHaveLength(1);
  });

  it('an amount that is not a multiple of the scale: the custody is exact and the total is rounded for the maker', () => {
    // 1.234 tokens at 2.5 KAS less a 0.000001 KAS tip: ceil(1234 x 249_999_900 / 1000)
    const s = planValid(env(), { type: 'limit', side: 'sell', price: P, amount: 1_234n, tip: 100n });
    expect(ask(s).amountLeft).toBe('1234');
    expect(s.disclosure!.allInTotal).toBe(ceilDiv(1_234n * (P - 100n), SCALE));
    expect(s.disclosure!.allInTotal).toBe(K.askProceedsAt(s.states[0], 1_234n, P));
    // a bid of 1.234 tokens: pays at most floor(1234 x (P + tip) / 1000); its escrow budgets the ceil
    const b = planValid(env(), { type: 'limit', side: 'buy', price: P, amount: 1_234n, tip: 100n });
    expect(b.disclosure!.allInTotal).toBe((1_234n * (P + 100n)) / SCALE);
    expect(b.disclosure!.allInTotal).toBe(K.bidSpendAt(b.states[0], 1_234n, P));
    expect(BigInt(b.request!.value)).toBe(escrow(1_234n, P + 100n, 1n));
  });

  it('GTC sell of the whole token UTXO has no token change and no kept carrier', () => {
    const p = planValid(makeEnv({ market, tokenAmounts: [10n * TOK] }), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK });
    expect(p.disclosure!.carriers.map((c) => c.kind)).toEqual(['orderCarrier', 'tokenCarrier']);
    expect(p.built!.tx.outputs.filter((o) => o.covenant?.covenantId === market.covenantId)).toHaveLength(1);
  });

  it('GTC buy: KobBid whose value is kob-wasm bidEscrow (golden create.bid: 5451 KAS + 2 sompi of rounding)', () => {
    const p = planValid(env(), { type: 'limit', side: 'buy', price: 245_000_000n, amount: 10n * TOK, tip: 100_000n });
    const s = bid(p);
    expect(s).toMatchObject({
      maker: MAKER_PK, extensionCommitment: market.extensionCommitment, price: '245000000', tip: '100000', tif: '0', activeFrom: '0', scale: '1000',
      expiryDaa: String(CLOCK.daa + MAX_IDLE_DAA), reserve: '0', deliveryCarrier: '1000000000', refundTip: market.refundTip.toString(),
    });
    // minimum fill: 10 KAS at 2.45 KAS per token = ceil(4081.6) base units; fills = min(ceil(10000 / 4082), 3) = 3
    expect(s.minFill).toBe('4082');
    // used(10000) + 2 sompi (each of 3 fills rounds its budget up) + 3 delivery carriers: exactly the Rust golden vector
    expect(BigInt(p.request!.value)).toBe(5_451_000_002n);
    expect(BigInt(p.request!.value)).toBe(K.bidEscrow(p.states[0], 10n * TOK, 3n));
    expect(p.request!.tokens).toBeUndefined();
    const d = p.disclosure!;
    expect(d.allInPrice).toBe(245_100_000n);
    expect(d.allInTotal).toBe(2_451_000_000n);
    expect(d.carriers).toEqual([
      { kind: 'escrow', amount: 2_451_000_002n, count: 1 },
      { kind: 'deliveryCarrier', amount: 10n * KAS, count: 3 },
    ]);
    expect(d.kasLocked).toBe(5_451_000_002n);
  });

  it('buy fills default to min(ceil(amount / minFill), 3) and can be raised; fewer than one is refused', () => {
    // 2 tokens at 2.5 KAS: the 10-KAS minimum fill (4 tokens) is clamped to the amount: one fill
    expect(BigInt(planValid(env(), { type: 'limit', side: 'buy', price: P, amount: 2n * TOK }).request!.value)).toBe(escrow(2n * TOK, P, 1n));
    // an explicit minimum fill of half a token allows the default 3 fills
    expect(BigInt(planValid(env(), { type: 'limit', side: 'buy', price: P, amount: 2n * TOK, minFill: 500n }).request!.value)).toBe(escrow(2n * TOK, P, 3n));
    expect(BigInt(planValid(env(), { type: 'limit', side: 'buy', price: P, amount: 10n * TOK, maxFills: 5n }).request!.value)).toBe(escrow(10n * TOK, P, 5n));
    const bad = planSimple(env(), { type: 'limit', side: 'buy', price: P, amount: 10n * TOK, maxFills: 0n });
    expect(bad.issues.map((i) => i.code)).toContain('MAX_FILLS_INVALID');
    expect(planSimple(env(), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK, maxFills: 2n }).issues.map((i) => i.code)).toContain('SIDE_INVALID');
  });

  it('the minimum fill: wallet default, an explicit value, and invalid values are refused', () => {
    const d = planValid(env(), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK });
    expect(ask(d).minFill).toBe(String(defMin(10n * TOK, P)));
    expect(ask(d).minFill).toBe('4000');
    // a cheap token: the 10-KAS amount exceeds the order, clamped to the amount
    const cheap = planValid(env(), { type: 'limit', side: 'sell', price: 1_000_000n, amount: 10n * TOK });
    expect(ask(cheap).minFill).toBe('10000');
    expect(ask(planValid(env(), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK, minFill: 7n })).minFill).toBe('7');
    for (const minFill of [0n, 10n * TOK + 1n]) {
      expect(planSimple(env(), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK, minFill }).issues.map((i) => i.code)).toContain('MIN_FILL_INVALID');
    }
  });

  it('GTD: expiry from the user date, refundable then (90-day cap enforced)', () => {
    const at = CLOCK.unixSeconds + 3_600n;
    for (const side of ['sell', 'buy'] as const) {
      const p = planValid(env(), { type: 'limit', side, price: P, amount: 3n * TOK, lifetime: { kind: 'gtd', at } });
      expect(BigInt(side === 'sell' ? ask(p).expiryDaa : bid(p).expiryDaa)).toBe(CLOCK.daa + 36_000n);
      expect(p.disclosure!.expiry).toMatchObject({ kind: 'gtd', daa: CLOCK.daa + 36_000n, approxUnixSeconds: at });
    }
    expect(planSimple(env(), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds + 91n * 86_400n } }).issues.map((i) => i.code)).toContain('EXPIRY_TOO_FAR');
    expect(planSimple(env(), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds - 5n } }).issues.map((i) => i.code)).toContain('EXPIRY_TOO_SOON');
    // exactly 90 days is allowed
    expect(planSimple(env(), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds + 90n * 86_400n } }).ok).toBe(true);
  });

  it('Day: until 00:00 UTC via kob.dayOrder, deadline in the placement record (recovered from the tx)', () => {
    for (const side of ['sell', 'buy'] as const) {
      const p = planValid(env(), { type: 'limit', side, price: P, amount: 3n * TOK, lifetime: { kind: 'day' } });
      const expected = K.dayOrder(CLOCK.daa, CLOCK.unixSeconds, 10_000n);
      expect(expected).toEqual({ expiryDaa: '1327240', deadline: '1790726400' });
      expect(BigInt(side === 'sell' ? ask(p).expiryDaa : bid(p).expiryDaa)).toBe(1_327_240n);
      expect(p.request!.deadline).toBe('1790726400');
      expect(K.recoverOrders(p.built!.tx)[0].deadline).toBe('1790726400');
      expect(p.disclosure!.expiry).toMatchObject({ kind: 'day', daa: 1_327_240n, deadlineUnixSeconds: 1_790_726_400n });
      expect(p.disclosure!.notes).toContain('dayOrder');
    }
    // the measured rate moves expiryDaa (clamped), never the deadline
    const fast = planValid(makeEnv({ market, clock: { ...CLOCK, rateMilli: 10_500 } }), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, lifetime: { kind: 'day' } });
    expect(fast.request!.deadline).toBe('1790726400');
    expect(BigInt(ask(fast).expiryDaa)).toBeGreaterThan(1_327_240n);
  });

  it('Day order placed seconds before midnight warns that it ends soon', () => {
    const late = makeEnv({ market, clock: { ...CLOCK, unixSeconds: 1_790_726_400n - 120n } });
    const p = planSimple(late, { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, lifetime: { kind: 'day' } });
    expect(p.ok).toBe(true);
    expect(p.issues.find((i) => i.code === 'DAY_ORDER_ENDS_SOON')).toMatchObject({ severity: 'warning', params: { minutes: 2 } });
  });

  it('timed activation sets activeFrom (from a date or a DAA) and reports when it starts', () => {
    for (const side of ['sell', 'buy'] as const) {
      const p = planValid(env(), { type: 'limit', side, price: P, amount: 3n * TOK, activeFrom: { unixSeconds: CLOCK.unixSeconds + 600n } });
      expect(BigInt(side === 'sell' ? ask(p).activeFrom : bid(p).activeFrom)).toBe(CLOCK.daa + 6_000n);
      expect(p.disclosure!.activatesAt).toEqual({ daa: CLOCK.daa + 6_000n, approxUnixSeconds: CLOCK.unixSeconds + 600n });
    }
    const byDaa = planValid(env(), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, activeFrom: { daa: CLOCK.daa + 123n } });
    expect(ask(byDaa).activeFrom).toBe(String(CLOCK.daa + 123n));
    // in the past: activates at once, informational
    const past = planSimple(env(), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, activeFrom: { daa: CLOCK.daa - 5n } });
    expect(past.ok).toBe(true);
    expect(ask(past).activeFrom).toBe('0');
    expect(past.disclosure!.activatesAt).toBeNull();
    expect(past.issues.find((i) => i.code === 'ACTIVE_IN_PAST')?.severity).toBe('info');
    // after the expiry / beyond 90 days
    const late = planSimple(env(), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK, activeFrom: { unixSeconds: CLOCK.unixSeconds + 7_200n }, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds + 3_600n } });
    expect(late.issues.map((i) => i.code)).toContain('ACTIVE_AFTER_EXPIRY');
    expect(planSimple(env(), { type: 'limit', side: 'buy', price: P, amount: 3n * TOK, activeFrom: { daa: CLOCK.daa + MAX_IDLE_DAA + 1n } }).issues.map((i) => i.code)).toContain('ACTIVE_TOO_FAR');
  });

  it('priority tip is carried into the state and the all-in numbers (sell receives less, buy pays more)', () => {
    const s = planValid(env(), { type: 'limit', side: 'sell', price: P, amount: 4n * TOK, tip: 1_000_000n });
    const b = planValid(env(), { type: 'limit', side: 'buy', price: P, amount: 4n * TOK, tip: 1_000_000n });
    expect(ask(s).tip).toBe('1000000');
    expect(s.disclosure!.allInPrice).toBe(P - 1_000_000n);
    expect(bid(b).tip).toBe('1000000');
    expect(b.disclosure!.allInPrice).toBe(P + 1_000_000n);
    expect(b.disclosure!.tip).toBe(1_000_000n);
    // the bid budget includes the tip; the minimum fill is worth 10 KAS at the LIMIT (4 tokens at 2.5 KAS: the whole order, one fill)
    expect(bid(b).minFill).toBe('4000');
    expect(BigInt(b.request!.value)).toBe(escrow(4n * TOK, P + 1_000_000n, 1n));
  });
});

describe.each(markets)('IOC / FOK on %s', (_name, market) => {
  const env = () => makeEnv({ market });

  it('IOC sell / buy: tif 1, life 300 DAA from now, no decay, minimum fill 1 base unit', () => {
    for (const side of ['sell', 'buy'] as const) {
      const p = planValid(env(), { type: 'ioc', side, price: side === 'sell' ? 245_000_000n : 250_000_000n, amount: 4n * TOK });
      const s = side === 'sell' ? ask(p) : bid(p);
      expect(s).toMatchObject({ tif: '1', activeFrom: '0', expiryDaa: String(CLOCK.daa + 300n), slope: '0', priceEnd: '0', minFill: '1' });
      expect(p.disclosure!.expiry).toMatchObject({ kind: 'ioc', daa: CLOCK.daa + 300n });
      expect(p.disclosure!.notes).toContain('iocRemainderReturned');
    }
    // an IOC bid keeps one delivery carrier (the remainder rides back on the delivery)
    expect(BigInt(planValid(env(), { type: 'ioc', side: 'buy', price: P, amount: 4n * TOK }).request!.value)).toBe(escrow(4n * TOK, P, 1n));
  });

  it('IOC life is configurable within 1..600 DAA (the covenant kill)', () => {
    const p = planValid(env(), { type: 'ioc', side: 'sell', price: P, amount: 2n * TOK, life: { seconds: 45n } });
    expect(ask(p).expiryDaa).toBe(String(CLOCK.daa + 450n));
    expect(planSimple(env(), { type: 'ioc', side: 'sell', price: P, amount: 2n * TOK, life: { daa: 601n } }).issues.map((i) => i.code)).toContain('LIFE_OUT_OF_RANGE');
    expect(planSimple(env(), { type: 'ioc', side: 'sell', price: P, amount: 2n * TOK, life: { daa: 0n } }).issues.map((i) => i.code)).toContain('LIFE_OUT_OF_RANGE');
    expect(planSimple(env(), { type: 'ioc', side: 'sell', price: P, amount: 2n * TOK, life: { daa: 600n } }).ok).toBe(true);
  });

  it('IOC with timed activation counts its life from the activation', () => {
    const p = planValid(env(), { type: 'ioc', side: 'buy', price: P, amount: 2n * TOK, activeFrom: { daa: CLOCK.daa + 1_000n } });
    expect(bid(p)).toMatchObject({ activeFrom: String(CLOCK.daa + 1_000n), expiryDaa: String(CLOCK.daa + 1_300n) });
  });

  it('FOK sell / buy: tif 2 when the visible book can fill it in one transaction', () => {
    const fokSell = planValid(env(), { type: 'fok', side: 'sell', price: 243_000_000n, amount: 6n * TOK }); // bids: 5 @245, 10 @243
    expect(ask(fokSell)).toMatchObject({ tif: '2', expiryDaa: String(CLOCK.daa + 300n), minFill: '1' });
    expect(fokSell.disclosure!.expiry.kind).toBe('fok');
    expect(fokSell.disclosure!.notes).toContain('fokAllOrNothing');
    const fokBuy = planValid(env(), { type: 'fok', side: 'buy', price: 250_000_000n, amount: 4n * TOK }); // asks: 5 @250 in 2 orders
    expect(bid(fokBuy).tif).toBe('2');
    expect(BigInt(fokBuy.request!.value)).toBe(escrow(4n * TOK, P, 1n));
  });

  it('FOK is refused when the visible book cannot fill it (depth) or needs too many counterparties', () => {
    const noDepth = planSimple(env(), { type: 'fok', side: 'sell', price: 245_000_000n, amount: 6n * TOK }); // only 5 tokens reach 245
    expect(noDepth.ok).toBe(false);
    expect(noDepth.built).toBeNull();
    expect(noDepth.issues.map((i) => i.code)).toEqual(['FOK_INSUFFICIENT_DEPTH']);
    expect(noDepth.issues[0].params).toMatchObject({ amount: 6_000n, available: 5_000n });
    // 20 tokens @240 + 10 @243 + 5 @245 in 3 + 2 + 1 = 6 orders; filling 30 tokens needs all six
    const wide = planSimple(env(), { type: 'fok', side: 'sell', price: 240_000_000n, amount: 30n * TOK });
    if (market.slots.outputs < 6) expect(wide.issues.map((i) => i.code)).toEqual(['FOK_TOO_MANY_COUNTERPARTIES']);
    else expect(wide.issues.filter((i) => i.severity === 'error')).toEqual([]);
  });

  it('FOK with per-order book entries is exact; an order whose minimum fill exceeds the FOK cannot take part', () => {
    const book = { asks: [], bids: [level(245_000_000n, 30n * TOK, 6)], bidOrders: [bookOrder(245_000_000n, 25n * TOK), ...Array.from({ length: 5 }, () => bookOrder(245_000_000n, TOK))] };
    const p = planValid(makeEnv({ market, book }), { type: 'fok', side: 'sell', price: 240_000_000n, amount: 20n * TOK });
    expect(ask(p).tif).toBe('2');
    // the 25-token bid wants at least 21 tokens per fill: only the five 1-token bids remain
    const strict = { ...book, bidOrders: [bookOrder(245_000_000n, 25n * TOK, 0n, 21n * TOK), ...Array.from({ length: 5 }, () => bookOrder(245_000_000n, TOK))] };
    const r = planSimple(makeEnv({ market, book: strict }), { type: 'fok', side: 'sell', price: 240_000_000n, amount: 20n * TOK });
    expect(r.issues[0]).toMatchObject({ code: 'FOK_INSUFFICIENT_DEPTH', params: { available: 5n * TOK } });
  });
});

describe('market orders (matcher.md 10.2), golden cross-check on 3x3', () => {
  it('sell: auction from the best bid down 3% over 200 DAA, IOC, activeFrom +30, expiry +300; equals the Rust golden vector', () => {
    const env = makeEnv({ clock: { ...CLOCK, daa: 999_870n }, book: { asks: [], bids: [level(260_000_000n, 40n * TOK, 2)] } });
    // the wallet's market minimum fill is 1 base unit; the golden vector was built with one token
    expect(ask(planValid(env, { type: 'market', side: 'sell', amount: 10n * TOK, tip: 100_000n })).minFill).toBe('1');
    const p = planValid(env, { type: 'market', side: 'sell', amount: 10n * TOK, tip: 100_000n, minFill: TOK });
    const g = goldenState('create.ask.market');
    expect({ ...ask(p), maker: g.maker }).toEqual(g);
    expect(ask(p)).toMatchObject({ price: '260000000', priceEnd: '252200000', slope: '39000', decayStep: '1', tif: '1', activeFrom: '999900', expiryDaa: '1000200' });
    const d = p.disclosure!;
    expect(d.expectedPrice).toBe(260_000_000n); // the touch
    expect(d.worstPrice).toBe(252_200_000n); // the bound
    expect(d.limitPrice).toBe(252_200_000n);
    expect(d.allInPrice).toBe(252_100_000n);
    expect(d.expiry.kind).toBe('ioc');
    expect(d.notes).toEqual(expect.arrayContaining(['auction', 'market']));
  });

  it('buy: rising bid up 3% to the cap, escrow sized at the cap; equals the Rust golden vector', () => {
    const env = makeEnv({ clock: { ...CLOCK, daa: 999_870n }, book: { bids: [], asks: [level(240_000_000n, 40n * TOK, 2)] } });
    const p = planValid(env, { type: 'market', side: 'buy', amount: 4n * TOK, tip: 100_000n, minFill: TOK });
    const g = goldenState('create.bid.market');
    expect({ ...bid(p), maker: g.maker }).toEqual(g);
    expect(BigInt(p.request!.value)).toBe(1_989_200_000n); // ceil(4000 x (247.2M + 0.1M) / 1000) + 10 KAS
    const d = p.disclosure!;
    expect(d.expectedPrice).toBe(240_000_000n);
    expect(d.worstPrice).toBe(247_200_000n);
    expect(d.allInPrice).toBe(247_300_000n);
    expect(d.carriers[0]).toEqual({ kind: 'escrow', amount: 4n * 247_300_000n, count: 1 });
  });

  it.each(markets)('sell and buy validate in the script engine on %s; slope reaches the bound within the auction length', (_n, market) => {
    for (const side of ['sell', 'buy'] as const) {
      const p = planValid(makeEnv({ market }), { type: 'market', side, amount: 3n * TOK });
      const s = side === 'sell' ? ask(p) : bid(p);
      const start = BigInt(s.price);
      const end = BigInt(s.priceEnd);
      expect(start).toBe(side === 'sell' ? 245_000_000n : 250_000_000n);
      expect(side === 'sell' ? end < start : end > start).toBe(true);
      // after 200 DAA the quote has reached the bound; after 199 it has not overshot
      const slope = BigInt(s.slope);
      const at = (t: bigint): bigint => (side === 'sell' ? (start - slope * t < end ? end : start - slope * t) : start + slope * t > end ? end : start + slope * t);
      expect(at(200n)).toBe(end);
      expect(at(0n)).toBe(start);
      // the bound is on the tick and never worse than 3%
      expect(end % market.tick).toBe(0n);
      const move = side === 'sell' ? start - end : end - start;
      expect(move * 10_000n).toBeLessThanOrEqual(start * 300n + 10_000n * market.tick);
      expect(BigInt(s.activeFrom)).toBe(CLOCK.daa + 30n);
      expect(BigInt(s.expiryDaa)).toBe(CLOCK.daa + 330n);
      expect(s.minFill).toBe('1');
    }
  });

  it('custom slippage, auction length and activation; tick rounding is in the safe direction', () => {
    const book = { asks: [level(250_000_055n, 9n * TOK, 1)], bids: [level(245_000_055n, 9n * TOK, 1)] };
    const s = planValid(makeEnv({ book }), { type: 'market', side: 'sell', amount: 2n * TOK, slippageBps: 100n, auction: { seconds: 10n }, activation: 50n });
    // 245_000_055 - floor(1%) = 242_550_055 -> rounded UP to the tick: never below the user's bound
    expect(ask(s).priceEnd).toBe('242550100');
    expect(ask(s).slope).toBe(String((245_000_055n - 242_550_100n + 99n) / 100n));
    expect(ask(s).activeFrom).toBe(String(CLOCK.daa + 50n));
    const b = planValid(makeEnv({ book }), { type: 'market', side: 'buy', amount: 2n * TOK, slippageBps: 100n });
    // 250_000_055 + 2_500_000 = 252_500_055 -> rounded DOWN
    expect(bid(b).priceEnd).toBe('252500000');
  });

  it('needs liquidity on the opposite side and a sane slippage', () => {
    const empty = makeEnv({ book: { asks: [], bids: [] } });
    expect(planSimple(empty, { type: 'market', side: 'sell', amount: TOK }).issues.map((i) => i.code)).toEqual(['NO_LIQUIDITY']);
    expect(planSimple(empty, { type: 'market', side: 'buy', amount: TOK }).issues[0].params).toMatchObject({ counterparty: 'asks' });
    const env = makeEnv();
    expect(planSimple(env, { type: 'market', side: 'sell', amount: TOK, slippageBps: 0n }).issues.map((i) => i.code)).toContain('SLIPPAGE_INVALID');
    expect(planSimple(env, { type: 'market', side: 'sell', amount: TOK, slippageBps: 10_000n }).issues.map((i) => i.code)).toContain('SLIPPAGE_INVALID');
    expect(planSimple(env, { type: 'market', side: 'sell', amount: TOK, slippageBps: 2_000n }).issues.find((i) => i.code === 'SLIPPAGE_HIGH')?.severity).toBe('warning');
    expect(planSimple(env, { type: 'market', side: 'sell', amount: TOK, auction: { daa: 0n } }).issues.map((i) => i.code)).toContain('DURATION_INVALID');
    expect(planSimple(env, { type: 'market', side: 'sell', amount: TOK, life: { daa: 700n } }).issues.map((i) => i.code)).toContain('LIFE_OUT_OF_RANGE');
  });

  it('warns when the visible book within the bound is thinner than the order', () => {
    const p = planSimple(makeEnv(), { type: 'market', side: 'sell', amount: 40n * TOK }); // bids within 3% of 245: 5 + 10 + 20 tokens
    expect(p.ok).toBe(true);
    expect(p.issues.find((i) => i.code === 'MARKET_DEPTH_INSUFFICIENT')).toMatchObject({ severity: 'warning', params: { available: 35n * TOK, amount: 40n * TOK } });
  });

  it('market FOK is the same auction with tif 2 and the FOK pre-check against the bound', () => {
    const p = planValid(makeEnv({ market: market8x8() }), { type: 'market', side: 'sell', amount: 12n * TOK, allOrNothing: true });
    expect(ask(p).tif).toBe('2');
    expect(p.disclosure!.expiry.kind).toBe('fok');
    const thin = planSimple(makeEnv(), { type: 'market', side: 'sell', amount: 100n * TOK, allOrNothing: true });
    expect(thin.issues.filter((i) => i.severity !== 'info').map((i) => i.code)).toEqual(['FOK_INSUFFICIENT_DEPTH']);
  });

  it('a tip larger than the worst price is refused', () => {
    expect(planSimple(makeEnv(), { type: 'market', side: 'sell', amount: TOK, tip: 300_000_000n }).issues.map((i) => i.code)).toContain('TIP_EXCEEDS_PRICE');
  });
});

describe('streaming / quote-and-execute (matcher.md 10.3)', () => {
  it('starts from the displayed price with the user tolerance, IOC or FOK', () => {
    const book = { asks: [level(250_000_000n, 30n * TOK, 2)], bids: [level(245_000_000n, 30n * TOK, 2)] };
    const env = makeEnv({ market: market8x8(), book });
    const ioc = planValid(env, { type: 'streaming', side: 'buy', amount: 5n * TOK, displayedPrice: 251_000_000n, toleranceBps: 50n });
    expect(bid(ioc)).toMatchObject({ price: '251000000', priceEnd: '252255000', tif: '1', minFill: '1' });
    expect(ioc.disclosure!.expectedPrice).toBe(251_000_000n);
    expect(ioc.disclosure!.worstPrice).toBe(252_255_000n);
    expect(ioc.disclosure!.notes).toContain('streaming');
    const fok = planValid(env, { type: 'streaming', side: 'sell', amount: 5n * TOK, displayedPrice: 244_000_000n, toleranceBps: 100n, allOrNothing: true });
    expect(ask(fok)).toMatchObject({ price: '244000000', priceEnd: '241560000', tif: '2' });
    expect(planSimple(env, { type: 'streaming', side: 'buy', amount: 5n * TOK, displayedPrice: 0n, toleranceBps: 50n }).issues.map((i) => i.code)).toContain('REFERENCE_PRICE_INVALID');
    expect(planSimple(env, { type: 'streaming', side: 'buy', amount: 5n * TOK, displayedPrice: 251_000_000n, toleranceBps: 0n }).issues.map((i) => i.code)).toContain('SLIPPAGE_INVALID');
  });
});

describe('close', () => {
  it('sells the whole balance as a market auction, any amount (no remainder stays behind)', () => {
    const market = market8x8();
    const env = makeEnv({ market, tokenAmounts: [7n * TOK] });
    env.tokenUtxos.push(tokenUtxo(market, 3n * TOK + 250n)); // 10.25 tokens
    const p = planValid(env, { type: 'close' });
    expect(ask(p)).toMatchObject({ amountLeft: '10250', tif: '1', minFill: '1' });
    expect(p.disclosure!.tokenAmount).toBe(10_250n);
    expect(p.disclosure!.notes).toContain('close');
    expect(p.built!.tx.inputs.filter((i) => i.utxo.covenantId === market.covenantId)).toHaveLength(2);
  });

  it('a partial close sells the requested amount; holding nothing is an error', () => {
    const p = planValid(makeEnv(), { type: 'close', amount: 4n * TOK });
    expect(ask(p).amountLeft).toBe('4000');
    const dust = makeEnv({ tokenAmounts: [999n] });
    expect(ask(planValid(dust, { type: 'close' })).amountLeft).toBe('999');
    expect(planSimple(makeEnv({ tokenAmounts: [] }), { type: 'close' }).issues.map((i) => i.code)).toEqual(['CLOSE_NOTHING_TO_SELL']);
    expect(planSimple(makeEnv({ tokenAmounts: [3n * TOK] }), { type: 'close', amount: 4n * TOK }).issues.map((i) => i.code)).toContain('INSUFFICIENT_TOKENS');
  });
});

describe('marketable limit (matcher.md 10.5)', () => {
  it('sell limit at or below the best bid: auction from the touch down to the limit, then resting (default)', () => {
    const p = planValid(makeEnv(), { type: 'limit', side: 'sell', price: 244_000_000n, amount: 3n * TOK });
    expect(ask(p)).toMatchObject({ price: '245000000', priceEnd: '244000000', tif: '0', decayStep: '1', activeFrom: String(CLOCK.daa + 30n) });
    expect(ask(p).slope).toBe('5000'); // 1_000_000 / 200 DAA
    expect(p.issues.find((i) => i.code === 'MARKETABLE_AUCTION')).toMatchObject({ severity: 'info', params: { touch: 245_000_000n } });
    const d = p.disclosure!;
    expect(d.expectedPrice).toBe(245_000_000n);
    expect(d.worstPrice).toBe(244_000_000n);
    expect(d.limitPrice).toBe(244_000_000n);
    expect(d.notes).toEqual(expect.arrayContaining(['auction', 'marketable']));
    expect(d.expiry.kind).toBe('gtc');
  });

  it('buy limit at or above the best ask: rising auction from the touch up to the limit, escrow at the limit', () => {
    const p = planValid(makeEnv(), { type: 'limit', side: 'buy', price: 252_000_000n, amount: 3n * TOK });
    expect(bid(p)).toMatchObject({ price: '250000000', priceEnd: '252000000', tif: '0' });
    expect(BigInt(bid(p).slope) * 200n).toBeGreaterThanOrEqual(2_000_000n);
    // the minimum fill (the amount worth 10 KAS at the 2.52 limit, 3969 base units) is clamped to the 3-token order: one fill
    expect(BigInt(p.request!.value)).toBe(escrow(3n * TOK, 252_000_000n, 1n));
  });

  it('exactly at the touch there is nothing to improve: a plain limit', () => {
    const p = planValid(makeEnv(), { type: 'limit', side: 'sell', price: 245_000_000n, amount: 3n * TOK });
    expect(ask(p).slope).toBe('0');
    expect(p.issues.map((i) => i.code)).not.toContain('MARKETABLE_AUCTION');
  });

  it("policy 'limit' places a plain crossing limit with a warning; 'reject' refuses", () => {
    const plain = planValid(makeEnv(), { type: 'limit', side: 'sell', price: 240_000_000n, amount: 3n * TOK, crossing: 'limit' });
    expect(ask(plain)).toMatchObject({ price: '240000000', slope: '0', priceEnd: '0' });
    expect(plain.issues.find((i) => i.code === 'MARKETABLE_LIMIT_FILLS_AT_LIMIT')).toMatchObject({ severity: 'warning', params: { touch: 245_000_000n } });
    const rej = planSimple(makeEnv(), { type: 'limit', side: 'buy', price: 251_000_000n, amount: 3n * TOK, crossing: 'reject' });
    expect(rej.ok).toBe(false);
    expect(rej.issues.map((i) => i.code)).toContain('MARKETABLE_REJECTED');
  });

  it('a timed limit that is not active yet is not marketable', () => {
    const p = planValid(makeEnv(), { type: 'limit', side: 'sell', price: 240_000_000n, amount: 3n * TOK, activeFrom: { daa: CLOCK.daa + 5_000n } });
    expect(ask(p).slope).toBe('0');
    expect(p.issues.map((i) => i.code)).not.toContain('MARKETABLE_AUCTION');
  });

  it('a passive limit gets no marketable handling; a limit far through the market warns', () => {
    expect(planValid(makeEnv(), { type: 'limit', side: 'sell', price: 300_000_000n, amount: 10n * TOK }).issues).toEqual([]);
    const far = planSimple(makeEnv(), { type: 'limit', side: 'sell', price: 100_000_000n, amount: 3n * TOK });
    expect(far.issues.map((i) => i.code)).toEqual(expect.arrayContaining(['PRICE_AGGRESSIVE_VS_MARKET', 'MARKETABLE_AUCTION']));
    expect(planSimple(makeEnv(), { type: 'limit', side: 'sell', price: 900_000_000n, amount: 3n * TOK }).issues.map((i) => i.code)).toContain('PRICE_FAR_FROM_MARKET');
  });
});

describe.each(markets)('TWAP / DCA on %s', (_name, market) => {
  it('TWAP sell: interval and maxFill in the state (golden shape), whole size in custody, minimum fill at most the slice', () => {
    const p = planValid(makeEnv({ market }), { type: 'twap', amount: 10n * TOK, sliceAmount: 2n * TOK, interval: { seconds: 60n }, price: P, tip: 100_000n });
    // the 10-KAS minimum at the limit (4 tokens at 2.5 KAS) is above the 2-token slice: capped at the slice
    expect(ask(p)).toMatchObject({ interval: '600', maxFill: '2000', minFill: '2000', slope: '0', priceEnd: '0', tif: '0', amountLeft: '10000', tip: '100000', price: String(P) });
    expect(p.disclosure!.tokensEscrowed).toBe(10_000n);
    expect(p.disclosure!.notes).toContain('twap');
  });

  it('TWAP with a slice auction: every slice descends from price to priceEnd', () => {
    const p = planValid(makeEnv({ market }), { type: 'twap', amount: 6n * TOK, sliceAmount: 3n * TOK, interval: { daa: 1_200n }, price: 260_000_000n, priceEnd: 250_000_000n, sliceAuction: { daa: 100n } });
    expect(ask(p)).toMatchObject({ interval: '1200', maxFill: '3000', price: '260000000', priceEnd: '250000000', slope: '100000', decayStep: '1' });
    expect(p.disclosure!.worstPrice).toBe(250_000_000n);
    expect(p.disclosure!.expectedPrice).toBe(260_000_000n);
    expect(p.disclosure!.notes).toContain('auction');
  });

  it('DCA buy: one delivery carrier per slice (golden bid.dca: 5 tokens, 1 per slice = 6225.5 KAS + 4 sompi of rounding)', () => {
    const p = planValid(makeEnv({ market }), { type: 'dca', amount: 5n * TOK, sliceAmount: TOK, interval: { daa: 600n }, price: 245_000_000n, tip: 100_000n });
    expect(bid(p)).toMatchObject({ interval: '600', maxFill: '1000', minFill: '1000', slope: '0', tif: '0' });
    expect(BigInt(p.request!.value)).toBe(6_225_500_004n);
    expect(p.disclosure!.carriers).toEqual([
      { kind: 'escrow', amount: 5n * 245_100_000n + 4n, count: 1 },
      { kind: 'deliveryCarrier', amount: 10n * KAS, count: 5 },
    ]);
  });

  it('DCA with a rising slice auction budgets at the cap; extra fills add carriers', () => {
    const p = planValid(makeEnv({ market }), { type: 'dca', amount: 4n * TOK, sliceAmount: 2n * TOK, interval: { daa: 600n }, price: 245_000_000n, priceEnd: 250_000_000n, maxFills: 6n });
    expect(bid(p)).toMatchObject({ price: '245000000', priceEnd: '250000000', slope: '25000' });
    expect(BigInt(p.request!.value)).toBe(escrow(4n * TOK, 250_000_000n, 6n));
    expect(planSimple(makeEnv({ market }), { type: 'dca', amount: 4n * TOK, sliceAmount: 2n * TOK, interval: { daa: 600n }, price: P, maxFills: 1n }).issues.map((i) => i.code)).toContain('MAX_FILLS_INVALID');
  });

  it('slice larger than the order is one slice; schedule must fit the order life; bad inputs are errors', () => {
    const one = planValid(makeEnv({ market }), { type: 'twap', amount: 3n * TOK, sliceAmount: 10n * TOK, interval: { daa: 600n }, price: P });
    expect(ask(one).maxFill).toBe('3000');
    expect(one.issues.map((i) => i.code)).toContain('TWAP_SINGLE_SLICE');
    const tooLong = planSimple(makeEnv({ market }), { type: 'twap', amount: 100n * TOK, sliceAmount: TOK, interval: { seconds: 3_600n }, price: P, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds + 86_400n } });
    expect(tooLong.issues.map((i) => i.code)).toContain('TWAP_EXCEEDS_LIFE');
    expect(planSimple(makeEnv({ market }), { type: 'twap', amount: 5n * TOK, sliceAmount: 0n, interval: { daa: 600n }, price: P }).issues.map((i) => i.code)).toContain('SLICE_AMOUNT_INVALID');
    expect(planSimple(makeEnv({ market }), { type: 'twap', amount: 5n * TOK, sliceAmount: TOK, interval: { daa: 0n }, price: P }).issues.map((i) => i.code)).toContain('DURATION_INVALID');
    expect(planSimple(makeEnv({ market }), { type: 'twap', amount: 5n * TOK, sliceAmount: TOK, interval: { daa: 10n }, price: P, priceEnd: P + 100n }).issues.map((i) => i.code)).toContain('PRICE_END_INVALID');
    expect(planSimple(makeEnv({ market }), { type: 'dca', amount: 5n * TOK, sliceAmount: TOK, interval: { daa: 10n }, price: P, priceEnd: P - 100n }).issues.map((i) => i.code)).toContain('PRICE_END_INVALID');
    expect(planSimple(makeEnv({ market }), { type: 'twap', side: 'buy' as never, amount: 5n * TOK, sliceAmount: TOK, interval: { daa: 10n }, price: P }).issues.map((i) => i.code)).toContain('SIDE_INVALID');
  });
});

describe.each(markets)('Dutch decay / rising bid on %s', (_name, market) => {
  it('sell: price descends from start to end over the duration; origin = activeFrom = placement + 30', () => {
    const p = planValid(makeEnv({ market }), { type: 'dutch', side: 'sell', amount: 5n * TOK, price: 300_000_000n, priceEnd: 200_000_000n, duration: { seconds: 600n } });
    const s = ask(p);
    expect(s).toMatchObject({ price: '300000000', priceEnd: '200000000', decayStep: '1', activeFrom: String(CLOCK.daa + 30n), tif: '0' });
    // the minimum fill is the amount worth 10 KAS at the floor (2 KAS per token): 5 tokens, the whole order
    expect(s.minFill).toBe('5000');
    expect(BigInt(s.slope) * 6_000n).toBeGreaterThanOrEqual(100_000_000n);
    expect(BigInt(s.slope) * 5_999n).toBeLessThan(100_000_000n + BigInt(s.slope));
    expect(p.disclosure!).toMatchObject({ expectedPrice: 300_000_000n, worstPrice: 200_000_000n, limitPrice: 200_000_000n });
    expect(p.disclosure!.notes).toEqual(expect.arrayContaining(['auction', 'dutch']));
  });

  it('golden dutch shape: step 1000 DAA, slope 1_000_000 over 100_000 DAA', () => {
    const p = planValid(makeEnv({ market }), { type: 'dutch', side: 'sell', amount: 10n * TOK, price: 300_000_000n, priceEnd: 200_000_000n, duration: { daa: 100_000n }, stepDaa: 1_000n, tip: 100_000n });
    expect(ask(p)).toMatchObject({ slope: '1000000', decayStep: '1000', price: '300000000', priceEnd: '200000000' });
  });

  it('buy (rising bid): escrow at the cap, cap is the worst price', () => {
    const p = planValid(makeEnv({ market }), { type: 'dutch', side: 'buy', amount: 5n * TOK, price: 200_000_000n, priceEnd: 240_000_000n, duration: { daa: 4_000n }, stepDaa: 10n, maxFills: 2n });
    expect(bid(p)).toMatchObject({ price: '200000000', priceEnd: '240000000', slope: '100000', decayStep: '10' });
    expect(BigInt(p.request!.value)).toBe(escrow(5n * TOK, 240_000_000n, 2n));
    expect(p.disclosure!).toMatchObject({ expectedPrice: 200_000_000n, worstPrice: 240_000_000n, allInPrice: 240_000_000n });
  });

  it('timed start; end price must be on the right side; bad duration / step', () => {
    const p = planValid(makeEnv({ market }), { type: 'dutch', side: 'sell', amount: 2n * TOK, price: 300_000_000n, priceEnd: 290_000_000n, duration: { daa: 100n }, activeFrom: { daa: CLOCK.daa + 999n } });
    expect(ask(p).activeFrom).toBe(String(CLOCK.daa + 999n));
    expect(p.disclosure!.activatesAt?.daa).toBe(CLOCK.daa + 999n);
    const codes = (i: SimpleIntent): string[] => planSimple(makeEnv({ market }), i).issues.map((x) => x.code);
    expect(codes({ type: 'dutch', side: 'sell', amount: 2n * TOK, price: 300_000_000n, priceEnd: 300_000_000n, duration: { daa: 100n } })).toContain('PRICE_END_INVALID');
    expect(codes({ type: 'dutch', side: 'buy', amount: 2n * TOK, price: 300_000_000n, priceEnd: 290_000_000n, duration: { daa: 100n } })).toContain('PRICE_END_INVALID');
    expect(codes({ type: 'dutch', side: 'sell', amount: 2n * TOK, price: 300_000_000n, priceEnd: 290_000_000n, duration: { daa: 0n } })).toContain('DURATION_INVALID');
    expect(codes({ type: 'dutch', side: 'sell', amount: 2n * TOK, price: 300_000_000n, priceEnd: 290_000_000n, duration: { daa: 10n }, stepDaa: 0n })).toContain('DURATION_INVALID');
  });

  it('a decaying order can rest at its end price as day or GTD', () => {
    const p = planValid(makeEnv({ market }), { type: 'dutch', side: 'sell', amount: 2n * TOK, price: 300_000_000n, priceEnd: 290_000_000n, duration: { daa: 100n }, lifetime: { kind: 'day' } });
    expect(p.request!.deadline).toBe('1790726400');
  });
});

describe('validation and guards through the planner', () => {
  it('never throws for user input: unknown types, zero amounts, bad prices come back as error issues', () => {
    const env = makeEnv();
    const codes = (i: unknown): string[] => planSimple(env, i as SimpleIntent).issues.map((x) => x.code);
    expect(codes({ type: 'nonsense', side: 'sell', amount: 1n })).toEqual(['INTENT_UNKNOWN_TYPE']);
    expect(codes({ type: 'limit', side: 'sell', amount: 0n, price: P })).toEqual(['AMOUNT_NOT_POSITIVE']);
    expect(codes({ type: 'limit', side: 'sell', amount: -1n, price: P })).toEqual(['AMOUNT_NOT_POSITIVE']);
    expect(codes({ type: 'limit', side: 'sell', amount: 1n << 63n, price: P })).toEqual(['AMOUNT_TOO_LARGE']);
    expect(codes({ type: 'limit', side: 'sell', amount: TOK, price: 0n })).toEqual(['PRICE_NOT_POSITIVE']);
    expect(codes({ type: 'limit', side: 'sell', amount: TOK, price: -5n })).toEqual(['PRICE_NOT_POSITIVE']);
    expect(codes({ type: 'limit', side: 'sell', amount: TOK, price: 1n << 63n })).toEqual(['PRICE_TOO_LARGE']);
    expect(codes({ type: 'limit', side: 'sell', amount: TOK, price: P, tip: -1n })).toContain('TIP_NEGATIVE');
    expect(codes({ type: 'limit', side: 'sell', amount: TOK, price: P, tip: P })).toContain('TIP_EXCEEDS_PRICE');
    for (const bad of [{ type: 'limit', side: 'sell', amount: 0n, price: P }, { type: 'x', amount: 1n }]) {
      const p = planSimple(env, bad as SimpleIntent);
      expect(p).toMatchObject({ ok: false, built: null, request: null, disclosure: null });
      expect(errors(p).length).toBeGreaterThan(0);
    }
  });

  it('a KRON custody above the token output limit is refused; a bid of that size is not a custody', () => {
    const env = makeEnv({ market: marketKron() });
    expect(planSimple(env, { type: 'limit', side: 'sell', amount: 1_000_000_001n, price: P }).issues.map((i) => i.code)).toEqual(['AMOUNT_TOO_LARGE']);
    expect(planSimple(env, { type: 'limit', side: 'buy', amount: 1_000_000_001n, price: 1_000_000n }).issues.map((i) => i.code)).not.toContain('AMOUNT_TOO_LARGE');
  });

  it('off-tick prices are refused with the neighbouring ticks; every issue carries a message', () => {
    const p = planSimple(makeEnv(), { type: 'limit', side: 'sell', amount: TOK, price: P + 30n });
    expect(p.ok).toBe(false);
    expect(p.issues[0]).toMatchObject({ code: 'PRICE_NOT_ON_TICK', severity: 'error', field: 'price', params: { tick: 100n, below: P, above: P + 100n } });
    expect(p.issues[0].message).toContain('100');
  });

  it('the 2^62 notional gate: the full fill at every rate the order carries must stay below 2^62 sompi', () => {
    // amount x price / scale just below and just above 2^62
    const price = 1_000_000_000n;
    const below = ((1n << 62n) * SCALE) / price - 1n;
    expect(planSimple(makeEnv({ tokenAmounts: [] }), { type: 'limit', side: 'sell', amount: below, price }).issues.map((i) => i.code)).not.toContain('NOTIONAL_TOO_LARGE');
    expect(planSimple(makeEnv(), { type: 'limit', side: 'sell', amount: below + 2n, price }).issues.map((i) => i.code)).toContain('NOTIONAL_TOO_LARGE');
    // a bid's budget rate includes the tip
    expect(planSimple(makeEnv(), { type: 'limit', side: 'buy', amount: 1n << 40n, price: 10n ** 15n }).issues.map((i) => i.code)).toContain('NOTIONAL_TOO_LARGE');
  });

  it('self-trade: a new order that would cross an own resting order is refused (both sides, tips, auctions)', () => {
    const ownBid = { covenantId: 'cd'.repeat(32), side: 'buy' as const, price: 240_000_000n, tip: 0n, amountLeft: 3n * TOK, active: true };
    const env = makeEnv({ ownOrders: [ownBid], book: { asks: [level(290_000_000n, 5n * TOK)], bids: [level(200_000_000n, 5n * TOK)] } });
    const cross = planSimple(env, { type: 'limit', side: 'sell', price: 240_000_000n, amount: TOK, crossing: 'limit' });
    expect(cross.ok).toBe(false);
    expect(cross.issues.map((i) => i.code)).toContain('SELF_TRADE');
    expect(cross.built).toBeNull();
    expect(planSimple(env, { type: 'limit', side: 'sell', price: 240_000_100n, amount: TOK }).ok).toBe(true);
    // an own bid's tip lets it reach a higher sell
    const tipped = makeEnv({ ownOrders: [{ ...ownBid, tip: 1_000_000n }], book: { asks: [], bids: [] } });
    expect(planSimple(tipped, { type: 'limit', side: 'sell', price: 241_000_000n, amount: TOK }).issues.map((i) => i.code)).toContain('SELF_TRADE');
    expect(planSimple(tipped, { type: 'limit', side: 'sell', price: 241_000_100n, amount: TOK }).ok).toBe(true);
    // a market sell auctions down to its bound: judged at the bound (3% below the touch 200 = 194)
    const mkt = makeEnv({ ownOrders: [{ ...ownBid, price: 195_000_000n }], book: { asks: [], bids: [level(200_000_000n, 5n * TOK)] } });
    expect(planSimple(mkt, { type: 'market', side: 'sell', amount: TOK }).issues.map((i) => i.code)).toContain('SELF_TRADE');
    // buy side against an own ask; an inactive own order or one with nothing left does not block
    const ownAsk = { ...ownBid, side: 'sell' as const, price: 250_000_000n };
    expect(planSimple(makeEnv({ ownOrders: [ownAsk] }), { type: 'limit', side: 'buy', price: 250_000_000n, amount: TOK, crossing: 'limit' }).issues.map((i) => i.code)).toContain('SELF_TRADE');
    expect(planSimple(makeEnv({ ownOrders: [{ ...ownAsk, active: false }] }), { type: 'limit', side: 'buy', price: 250_000_000n, amount: TOK, crossing: 'limit' }).ok).toBe(true);
    expect(planSimple(makeEnv({ ownOrders: [{ ...ownAsk, amountLeft: 0n }] }), { type: 'limit', side: 'buy', price: 250_000_000n, amount: TOK, crossing: 'limit' }).ok).toBe(true);
  });

  it('carriers dominating a tiny order raise a warning', () => {
    const p = planSimple(makeEnv(), { type: 'limit', side: 'sell', price: 100n, amount: TOK });
    expect(p.ok).toBe(true);
    expect(p.issues.find((i) => i.code === 'CARRIERS_DOMINATE')?.severity).toBe('warning');
  });
});

describe('funding and token selection', () => {
  it('not enough tokens: clear shortfall in base units', () => {
    const p = planSimple(makeEnv({ tokenAmounts: [5n * TOK] }), { type: 'limit', side: 'sell', price: P, amount: 8n * TOK });
    expect(p.ok).toBe(false);
    expect(p.issues).toHaveLength(1);
    expect(p.issues[0]).toMatchObject({ code: 'INSUFFICIENT_TOKENS', params: { needed: 8_000n, have: 5_000n, shortfall: 3_000n } });
  });

  it('tokens spread over more UTXOs than the program allows: 3x3 needs a merge, 8x8 does not', () => {
    const amounts = [TOK, TOK, TOK, TOK];
    const m3 = planSimple(makeEnv({ market: market3x3(), tokenAmounts: amounts }), { type: 'limit', side: 'sell', price: P, amount: 4n * TOK });
    expect(m3.issues.map((i) => i.code)).toEqual(['TOKEN_UTXOS_FRAGMENTED']);
    expect(m3.issues[0].params).toMatchObject({ max: 3 });
    const m8 = planValid(makeEnv({ market: market8x8(), tokenAmounts: amounts }), { type: 'limit', side: 'sell', price: P, amount: 4n * TOK });
    expect(m8.built!.tx.inputs.filter((i) => i.utxo.covenantId)).toHaveLength(4);
    // 9 UTXOs exceed even the 8-input covenant limit on 8x8
    const nine = planSimple(makeEnv({ market: market8x8(), tokenAmounts: Array(9).fill(TOK) }), { type: 'limit', side: 'sell', price: P, amount: 9n * TOK });
    expect(nine.issues.map((i) => i.code)).toEqual(['TOKEN_UTXOS_FRAGMENTED']);
  });

  it('several token UTXOs are combined largest-first and the change returns to the maker', () => {
    const p = planValid(makeEnv({ market: market3x3(), tokenAmounts: [4n * TOK, 3n * TOK, 2n * TOK] }), { type: 'limit', side: 'sell', price: P, amount: 6n * TOK });
    const tokenIns = p.built!.tx.inputs.filter((i) => i.utxo.covenantId);
    expect(tokenIns).toHaveLength(2); // 4 + 3 = 7 >= 6
    expect(p.disclosure!.carriers.find((c) => c.kind === 'tokenChangeCarrier')).toMatchObject({ kept: true });
  });

  it('other tokens, foreign owners and custody UTXOs are never spent', () => {
    const env = makeEnv({ tokenAmounts: [] });
    const m = env.token;
    const foreign = tokenUtxo(m, 100n * TOK, 10n * KAS, 'ab'.repeat(32));
    const wrongExt = { ...tokenUtxo(m, 100n * TOK), state: { ...tokenUtxo(m, 100n * TOK).state, extension_commitment: 'dd'.repeat(32) } };
    const custody = { ...tokenUtxo(m, 100n * TOK), state: { ...tokenUtxo(m, 100n * TOK).state, owner_scheme: 4 } };
    const other = { ...tokenUtxo(m, 100n * TOK), covenantId: '99'.repeat(32) };
    env.tokenUtxos.push(foreign, wrongExt, custody, other);
    expect(planSimple(env, { type: 'limit', side: 'sell', price: P, amount: TOK }).issues[0].code).toBe('INSUFFICIENT_TOKENS');
  });

  it('not enough KAS: a clear shortfall (boundary exact); a wallet with only just enough succeeds with a smaller reserve', () => {
    // sell 10 of 10 tokens: needs order carrier + custody carrier - the token input's own 10 KAS = 10 KAS + fee
    const tokenAmounts = [10n * TOK];
    const fee = 1_192_300n;
    const enough = planValid(makeEnv({ tokenAmounts, funding: [10n * KAS + 2n * fee] }), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK });
    expect(BigInt(enough.built!.fee.fee)).toBeGreaterThan(0n);
    const short = planSimple(makeEnv({ tokenAmounts, funding: [10n * KAS - 1n] }), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK });
    expect(short.issues[0]).toMatchObject({ code: 'INSUFFICIENT_KAS', params: { needed: 10n * KAS, have: 10n * KAS - 1n, shortfall: 1n } });
    // enough for the outputs but not for the network fee
    const noFee = planSimple(makeEnv({ tokenAmounts, funding: [10n * KAS + 1_000n] }), { type: 'limit', side: 'sell', price: P, amount: 10n * TOK });
    expect(noFee.ok).toBe(false);
    expect(noFee.issues.map((i) => i.code)).toEqual(['INSUFFICIENT_KAS']);
    expect(noFee.issues[0].params!.shortfall as bigint).toBeGreaterThan(0n);
    expect(noFee.issues[0].params!.shortfall as bigint).toBeLessThan(fee * 2n);
    // buy: escrow + carriers + fee (2 tokens: one fill)
    const need = escrow(2n * TOK, P, 1n);
    expect(planSimple(makeEnv({ funding: [need + 400_000n] }), { type: 'limit', side: 'buy', price: P, amount: 2n * TOK }).ok).toBe(true);
    expect(planSimple(makeEnv({ funding: [need - 1n] }), { type: 'limit', side: 'buy', price: P, amount: 2n * TOK }).issues.map((i) => i.code)).toEqual(['INSUFFICIENT_KAS']);
  });

  it('funding is largest-first and can span several UTXOs; a wallet of dust UTXOs is reported as fragmented', () => {
    const p = planValid(makeEnv({ funding: [3n * KAS, 2n * KAS, 90n * KAS, 30n * KAS] }), { type: 'limit', side: 'buy', price: P, amount: 30n * TOK });
    // needs 30 x 2.5 + 3 x 10 KAS (+ 2 sompi) = 105 KAS: 90 + 30
    expect(p.built!.tx.inputs.filter((i) => !i.utxo.covenantId)).toHaveLength(2);
    const dust = planSimple(makeEnv({ funding: Array(100).fill(3n * KAS) }), { type: 'limit', side: 'buy', price: P, amount: 100n * TOK });
    expect(dust.issues.map((i) => i.code)).toEqual(['FUNDING_FRAGMENTED']);
  });

  it('honours the fee rate and the change address of the environment', () => {
    const env = makeEnv({ feeRate: 200n });
    env.changeTo = 'ab'.repeat(32);
    const p = planValid(env, { type: 'limit', side: 'buy', price: P, amount: TOK });
    expect(p.request!.fee).toEqual({ feeRate: '200' });
    expect(BigInt(p.built!.fee.feeRate)).toBe(200n);
    expect(p.request!.change).toBe('ab'.repeat(32));
  });

  it('carrier override changes every carrier consistently', () => {
    const p = planValid(makeEnv({ carrier: 5n * KAS }), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK });
    expect(p.disclosure!.carriers[0].amount).toBe(5n * KAS);
    expect(p.disclosure!.kasLocked).toBe(10n * KAS);
    const b = planValid(makeEnv({ carrier: 5n * KAS }), { type: 'limit', side: 'buy', price: P, amount: 3n * TOK });
    expect(BigInt(b.request!.value)).toBe(escrow(3n * TOK, P, 1n, 5n * KAS));
    expect(bid(b).deliveryCarrier).toBe(String(5n * KAS));
  });

  it('a protocol refusal by kob-wasm surfaces as an error issue, not as a throw', () => {
    // a zero token carrier cannot host the custody token UTXO
    const p = planSimple(makeEnv({ carrier: 0n }), { type: 'limit', side: 'sell', price: P, amount: 3n * TOK });
    expect(p.ok).toBe(false);
    expect(p.built).toBeNull();
    expect(errors(p).length).toBeGreaterThan(0);
  });
});

describe('planOrder dispatcher', () => {
  it('routes simple intents to planSimple and returns the same plan', () => {
    const env = makeEnv();
    const a = planOrder(env, { type: 'limit', side: 'sell', price: P, amount: 2n * TOK });
    expect(a.ok).toBe(true);
    expect(a.states).toEqual(planSimple(env, { type: 'limit', side: 'sell', price: P, amount: 2n * TOK }).states);
  });
});

describe('property sweep: every combination validates in the script engine', () => {
  // deterministic pseudo-random sweep over side, size (any base-unit amount), price, tip and time-in-force on both programs
  let seed = 0x2545f491;
  const rnd = (n: number): number => {
    seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
    return (seed >>> 8) % n;
  };
  it('40 random limit / IOC / TWAP / Dutch orders', () => {
    for (let n = 0; n < 40; n++) {
      const market = rnd(2) ? market3x3() : market8x8();
      const env = makeEnv({ market, tokenAmounts: [200n * TOK], funding: [10_000_000n * KAS] });
      const side = rnd(2) ? 'sell' : 'buy';
      const price = BigInt(100 + rnd(4_000)) * 1_000_000n; // multiples of the tick
      const amount = BigInt(1 + rnd(40_000));
      const tip = BigInt(rnd(50)) * 100_000n;
      const lifetime = [{ kind: 'gtc' as const }, { kind: 'day' as const }, { kind: 'gtd' as const, at: CLOCK.unixSeconds + BigInt(600 + rnd(86_400 * 80)) }][rnd(3)];
      let intent: SimpleIntent;
      switch (rnd(4)) {
        case 0: intent = { type: 'limit', side, price, amount, tip, lifetime, crossing: 'limit' }; break;
        case 1: intent = { type: 'ioc', side, price, amount, tip }; break;
        case 2: intent = side === 'sell'
          ? { type: 'twap', amount, sliceAmount: BigInt(1 + rnd(5)) * TOK + BigInt(rnd(1_000)), interval: { daa: BigInt(600 + rnd(6_000)) }, price, tip }
          : { type: 'dca', amount, sliceAmount: BigInt(1 + rnd(5)) * TOK + BigInt(rnd(1_000)), interval: { daa: BigInt(600 + rnd(6_000)) }, price, tip };
          break;
        default: intent = { type: 'dutch', side, amount, price: side === 'sell' ? price + 5_000_000n : price, priceEnd: side === 'sell' ? price : price + 5_000_000n, duration: { daa: BigInt(100 + rnd(5_000)) }, tip };
      }
      const p = planSimple(env, intent);
      const hard = errors(p).filter((i) => !['SELF_TRADE'].includes(i.code));
      expect(hard, JSON.stringify(intent, (_k, v) => (typeof v === 'bigint' ? v.toString() : v))).toEqual([]);
      planValid(env, intent);
    }
  });
});
