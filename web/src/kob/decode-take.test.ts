// The pre-sign decoder on take transactions (kob/decode.ts): a crossing order filled from the wallet. The wallet receives exactly the tokens
// bought, pays at most its limit, and nothing goes to a third party beyond the resting makers' payouts.
import { describe, expect, it } from 'vitest';
import type { BookOrderUtxo, BookView, OrderPlan, PlanEnv } from './plan-types';
import { errors } from './plan-types';
import { planOrder } from './plan';
import { decodeSigning } from './decode';
import { expectedFromPlan } from '../ui/ticket/plan-expected';
import { CLOCK, KAS, MAKER_PK, OTHER_PK, TOK, keyUtxo, kob, makeEnv, market3x3, tokenUtxo } from '../testing/fixtures';
import { snapshotOfRecovered } from '../testing/chain-fixtures';
import type { TokenMarket } from './plan-types';

const K = kob();
const M = market3x3();

/** A resting order of OTHER_PK, placed for real and read back as the node would confirm it. */
function resting(side: 'sell' | 'buy', price: bigint, amount: bigint, o: { tip?: bigint; minFill?: bigint; market?: TokenMarket } = {}): { row: { covenantId: string; price: bigint; amount: bigint; tip: bigint }; utxo: BookOrderUtxo } {
  const market = o.market ?? M;
  const env: PlanEnv = {
    ...makeEnv({ market, book: { asks: [], bids: [] } }), maker: OTHER_PK,
    funding: [keyUtxo(10_000n * KAS, OTHER_PK)], tokenUtxos: [tokenUtxo(market, 1_000n * TOK, 10n * KAS, OTHER_PK)],
  };
  // a minimum fill of one token unless told otherwise: partial takes are possible
  const p = planOrder(env, { type: 'limit', side, amount, price, ...(o.tip ? { tip: o.tip } : {}), minFill: o.minFill ?? TOK });
  expect(errors(p)).toEqual([]);
  const rec = K.recoverOrders(p.built!.tx)[0]!;
  const snap = snapshotOfRecovered(rec, CLOCK.daa - 1000n);
  // the book lists a bid at its amount (its escrow could buy more)
  return { row: { covenantId: snap.covenantId, price, amount, tip: o.tip ?? 0n }, utxo: { covenantId: snap.covenantId, order: snap.order, custody: snap.custody } };
}

/** The taker's env (MAKER_PK) with `asks` / `bids` as the node-confirmed book, best first. */
function takerEnv(asks: ReturnType<typeof resting>[], bids: ReturnType<typeof resting>[] = [], o: { tokens?: bigint; funding?: bigint[] } = {}): PlanEnv {
  const level = (r: ReturnType<typeof resting>) => ({ price: r.row.price, amount: r.row.amount, orders: 1 });
  const book: BookView = { asks: asks.map(level), bids: bids.map(level), askOrders: asks.map((r) => r.row), bidOrders: bids.map((r) => r.row) };
  const env = makeEnv({ book, tokenAmounts: [o.tokens ?? 100n * TOK], ...(o.funding ? { funding: o.funding } : {}) });
  return { ...env, bookOrders: new Map([...asks, ...bids].map((r) => [r.row.covenantId.toLowerCase(), r.utxo])) };
}

const take = (env: PlanEnv, side: 'buy' | 'sell', amount: bigint, price: bigint): OrderPlan => planOrder(env, { type: 'limit', side, amount, price, crossing: 'take' });

const decode = (p: OrderPlan, expected: Parameters<typeof decodeSigning>[0]['expected'] | undefined = expectedFromPlan(p), built = p.built!) =>
  decodeSigning({ kob: K, built, maker: MAKER_PK, ...(expected ? { expected } : {}) });
const codes = (s: ReturnType<typeof decode>) => s.blocking.map((b) => b.code);
const THIRD = '33'.repeat(32);
const clone = <T,>(x: T): T => JSON.parse(JSON.stringify(x));

describe('a take decodes as what it is', () => {
  it('a buy: the asks filled, their payouts and continuations, the tokens to the wallet', () => {
    const a1 = resting('sell', 250_000_000n, 2n * TOK, { tip: 1_000_000n });
    const a2 = resting('sell', 251_000_000n, 3n * TOK);
    const p = take(takerEnv([a1, a2]), 'buy', 4n * TOK, 255_000_000n);
    const s = decode(p);
    expect(codes(s)).toEqual([]);
    expect(s.kind).toBe('take');
    expect(s.take!.side).toBe('buy');
    expect(s.take!.amount).toBe(4n * TOK);
    expect(s.take!.fills.map((f) => f.covenantId)).toEqual([a1.row.covenantId, a2.row.covenantId]);
    // the wallet pays the makers exactly what the plan said, the fee on top
    expect(s.take!.kas).toBe(p.take!.kas);
    expect(s.take!.kas).toBeLessThanOrEqual(p.take!.bound);
    expect(s.net.tokens[0]!.walletDelta).toBe(4n * TOK);
    expect(s.outputs.filter((o) => o.kind === 'fill-payout').map((o) => o.recipient)).toEqual([OTHER_PK, OTHER_PK]);
    // the partly filled ask continues with its custody: the order's, not an unknown script
    expect(s.outputs.filter((o) => o.kind === 'fill-continue')).toHaveLength(2);
    expect(s.outputs.some((o) => o.kind === 'unknown' || o.kind === 'unknown-covenant' || o.kind === 'payment-out')).toBe(false);
    // nothing of the orders taken is "released" to the wallet
    expect(s.net.kas.released).toBe(0n);
  });

  it('a sell: the bids filled, the tokens delivered to their makers, the KAS to the wallet', () => {
    const b = resting('buy', 250_000_000n, 3n * TOK, { tip: 100_000n });
    const p = take(takerEnv([], [b]), 'sell', 2n * TOK, 249_000_000n);
    const s = decode(p);
    expect(codes(s)).toEqual([]);
    expect(s.kind).toBe('take');
    expect(s.take!.side).toBe('sell');
    expect(s.take!.kas).toBe(2n * 250_100_000n);
    expect(s.take!.kas).toBeGreaterThanOrEqual(p.take!.bound);
    expect(s.net.tokens[0]!.walletDelta).toBe(-2n * TOK);
    expect(s.outputs.filter((o) => o.kind === 'fill-payout' && o.token).map((o) => o.token!.amount)).toEqual([2n * TOK]);
  });

  it('the private-key signer decodes it without the plan: no blocking finding, a take', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = take(takerEnv([a]), 'buy', TOK, 255_000_000n);
    const s = decode(p, undefined);
    expect(codes(s)).toEqual([]);
    expect(s.kind).toBe('take');
    expect(s.take!.avgPrice).toBe(250_000_000n);
  });
});

describe('the take checks', () => {
  const a = () => resting('sell', 250_000_000n, 5n * TOK);

  it('blocks a transaction that delivers other than exactly the tokens bought', () => {
    const p = take(takerEnv([a()]), 'buy', 2n * TOK, 255_000_000n);
    const e = expectedFromPlan(p);
    expect(codes(decode(p, { ...e, take: { ...e.take!, amount: 3n * TOK } }))).toContain('take-amount');
  });

  it('blocks paying more than the limit', () => {
    const p = take(takerEnv([a()]), 'buy', 2n * TOK, 255_000_000n);
    const e = expectedFromPlan(p);
    expect(codes(decode(p, { ...e, take: { ...e.take!, bound: p.take!.kas - 1n } }))).toContain('take-price');
  });

  it('blocks a sell that gets less than its limit', () => {
    const b = resting('buy', 250_000_000n, 3n * TOK);
    const p = take(takerEnv([], [b]), 'sell', 2n * TOK, 249_000_000n);
    const e = expectedFromPlan(p);
    expect(codes(decode(p, { ...e, take: { ...e.take!, bound: p.take!.kas + 1n } }))).toContain('take-price');
  });

  it('blocks filling other orders than planned', () => {
    const p = take(takerEnv([a()]), 'buy', 2n * TOK, 255_000_000n);
    const e = expectedFromPlan(p);
    expect(codes(decode(p, { ...e, take: { ...e.take!, orders: ['ab'.repeat(32)] } }))).toContain('take-orders');
  });

  it('blocks a payout to a third party: only the makers of the orders filled are paid', () => {
    const p = take(takerEnv([a()]), 'buy', 2n * TOK, 255_000_000n);
    const built = clone(p.built!);
    const j = built.tx.outputs.findIndex((o) => o.scriptPublicKey === `000020${OTHER_PK}ac`);
    expect(j).toBeGreaterThanOrEqual(0);
    built.tx.outputs[j]!.scriptPublicKey = `000020${THIRD}ac`;
    const s = decode(p, expectedFromPlan(p), built);
    expect(codes(s)).toContain('payment-out');
    // in the signer frame too: the payout is no longer a fill's
    expect(decode(p, undefined, built).outputs[j]!.kind).toBe('payment-out');
  });

  it('a placement plan never accepts a take transaction, nor a take plan a placement', () => {
    const p = take(takerEnv([a()]), 'buy', 2n * TOK, 255_000_000n);
    expect(codes(decode(p, { orders: [] }))).toContain('take-orders');
    const rest = planOrder(takerEnv([a()]), { type: 'limit', side: 'buy', amount: TOK, price: 240_000_000n, crossing: 'take' });
    expect(rest.take).toBeUndefined();
    expect(codes(decode(rest, { take: { side: 'buy', token: M.covenantId, amount: TOK, bound: 0n, orders: [] } }))).toContain('take-orders');
  });
});
