// The take planner (kob/take.ts): a crossing limit filled directly from the wallet. Every take transaction is signed with the taker's key
// only, finalized with tightened budgets and validated by the script engine; the resting orders are real placements of another key.
import { describe, expect, it } from 'vitest';
import type { BookOrderUtxo, BookView, OrderPlan, PlanEnv } from './plan-types';
import { errors } from './plan-types';
import { planOrder } from './plan';
import { bookWithout, takeBound, takeLockTime, takesPerTx } from './take';
import { CLOCK, KAS, MAKER_PK, MAKER_SK, OTHER_PK, TOK, consensusCheck, keyUtxo, kob, makeEnv, market3x3, market8x8, tokenUtxo } from '../testing/fixtures';
import { snapshotOfRecovered } from '../testing/chain-fixtures';
import type { TokenMarket } from './plan-types';

const K = kob();
const M = market3x3();

/** A resting order of OTHER_PK, placed for real and read back as the node would confirm it. */
function resting(side: 'sell' | 'buy', price: bigint, amount: bigint, o: { tip?: bigint; minFill?: bigint; market?: TokenMarket; maxFills?: bigint } = {}): { row: { covenantId: string; price: bigint; amount: bigint; tip: bigint }; utxo: BookOrderUtxo } {
  const market = o.market ?? M;
  const env: PlanEnv = {
    ...makeEnv({ market, book: { asks: [], bids: [] } }), maker: OTHER_PK,
    funding: [keyUtxo(10_000n * KAS, OTHER_PK)], tokenUtxos: [tokenUtxo(market, 1_000n * TOK, 10n * KAS, OTHER_PK)],
  };
  // a minimum fill of one token unless told otherwise: partial takes are possible
  const p = planOrder(env, { type: 'limit', side, amount, price, ...(o.tip ? { tip: o.tip } : {}), minFill: o.minFill ?? TOK, ...(o.maxFills ? { maxFills: o.maxFills } : {}) });
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

describe('crossing detection', () => {
  it('a buy at or above the best ask is a take; below it is a plain resting limit', () => {
    const a = resting('sell', 250_000_000n, 5n * TOK);
    const env = takerEnv([a]);
    const at = take(env, 'buy', 2n * TOK, 250_000_000n);
    expect(at.take).toBeDefined();
    expect(at.states).toEqual([]);
    const below = take(env, 'buy', 2n * TOK, 249_000_000n);
    expect(below.take).toBeUndefined();
    expect(below.states).toHaveLength(1);
  });

  it('the tester case: a limit 0.0005 KAS above the ask takes it at the ask, paying only the network fee', () => {
    // kUSDT/KAS: ask 0.25 KAS, limit 0.2505 KAS, about 5 tokens
    const a = resting('sell', 25_000_000n, 5n * TOK);
    const p = take(takerEnv([a]), 'buy', 5n * TOK, 25_050_000n);
    expect(errors(p)).toEqual([]);
    expect(p.take!.amount).toBe(5n * TOK);
    expect(p.take!.kas).toBe(125_000_000n); // 5 x 0.25 KAS, at the ask's price, not the limit
    expect(p.take!.rest).toBe(0n);
    expect(p.issues.map((i) => i.code)).toContain('TAKE_NOW');
    consensusCheck(K, p.built!, [MAKER_SK]);
  });

  it('the ask tip is the taker\'s: it pays the maker price less tip', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK, { tip: 1_000_000n });
    const p = take(takerEnv([a]), 'buy', 2n * TOK, 250_000_000n);
    expect(p.take!.kas).toBe(2n * 249_000_000n);
    consensusCheck(K, p.built!, [MAKER_SK]);
  });

  it('a timed limit never takes', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = planOrder(takerEnv([a]), { type: 'limit', side: 'buy', amount: TOK, price: 260_000_000n, crossing: 'take', activeFrom: { daa: CLOCK.daa + 5_000n } });
    expect(p.take).toBeUndefined();
  });
});

describe('multi-order take', () => {
  it('takes several asks in one transaction, best price first', () => {
    const a1 = resting('sell', 250_000_000n, 2n * TOK);
    const a2 = resting('sell', 251_000_000n, 2n * TOK);
    const p = take(takerEnv([a1, a2]), 'buy', 3n * TOK, 255_000_000n);
    expect(errors(p)).toEqual([]);
    expect(p.take!.steps).toHaveLength(1);
    expect(p.take!.steps[0]!.fills.map((f) => [f.covenantId, f.amount])).toEqual([[a1.row.covenantId, 2n * TOK], [a2.row.covenantId, TOK]]);
    expect(p.take!.kas).toBe(2n * 250_000_000n + 251_000_000n);
    consensusCheck(K, p.built!, [MAKER_SK]);
  });

  it('more crossing orders than one transaction carries: the next transaction takes the rest', () => {
    const per = takesPerTx({ token: M }, 'buy');
    const asks = Array.from({ length: per + 1 }, (_, i) => resting('sell', 250_000_000n + BigInt(i) * 100_000n, TOK));
    const p = take(takerEnv(asks), 'buy', BigInt(per + 1) * TOK, 260_000_000n);
    expect(errors(p)).toEqual([]);
    expect(p.take!.steps).toHaveLength(2);
    expect(p.take!.amount).toBe(BigInt(per) * TOK);
    expect(p.take!.total.amount).toBe(BigInt(per + 1) * TOK);
    expect(p.take!.rest).toBe(0n);
    consensusCheck(K, p.built!, [MAKER_SK]);
  });

  it('a sell crossing bids delivers its tokens and receives price plus tip', () => {
    const b1 = resting('buy', 250_000_000n, 2n * TOK, { tip: 100_000n });
    const b2 = resting('buy', 249_000_000n, 2n * TOK);
    const p = take(takerEnv([], [b1, b2]), 'sell', 3n * TOK, 249_000_000n);
    expect(errors(p)).toEqual([]);
    const fills = p.take!.steps[0]!.fills;
    expect(fills.map((f) => [f.side, f.amount])).toEqual([['bid', 2n * TOK], ['bid', TOK]]);
    expect(fills[0]!.kas).toBe(2n * 250_100_000n);
    consensusCheck(K, p.built!, [MAKER_SK]);
  });
});

describe('limit protection', () => {
  it('never takes an ask above the limit: the rest rests at the limit', () => {
    const a1 = resting('sell', 250_000_000n, 2n * TOK);
    const a2 = resting('sell', 260_000_000n, 5n * TOK);
    const p = take(takerEnv([a1, a2]), 'buy', 5n * TOK, 255_000_000n);
    expect(p.take!.steps[0]!.fills.map((f) => f.covenantId)).toEqual([a1.row.covenantId]);
    expect(p.take!.amount).toBe(2n * TOK);
    for (const f of p.take!.steps.flatMap((s) => s.fills)) expect(f.price - f.tip).toBeLessThanOrEqual(255_000_000n);
    expect(p.take!.kas).toBeLessThanOrEqual(takeBound('buy', p.take!.amount, 255_000_000n, M.scale));
  });

  it('never sells into a bid below the limit', () => {
    const b1 = resting('buy', 250_000_000n, 2n * TOK);
    const b2 = resting('buy', 240_000_000n, 5n * TOK);
    const p = take(takerEnv([], [b1, b2]), 'sell', 5n * TOK, 245_000_000n);
    expect(p.take!.steps[0]!.fills.map((f) => f.covenantId)).toEqual([b1.row.covenantId]);
    expect(p.take!.rest).toBe(3n * TOK);
  });

  it('the user\'s own resting order is never taken: a self-trade stays refused', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const env = { ...takerEnv([a]), ownOrders: [{ covenantId: a.row.covenantId, side: 'sell' as const, price: 250_000_000n, tip: 0n, amountLeft: 2n * TOK, active: true }] };
    expect(errors(take(env, 'buy', TOK, 255_000_000n)).map((i) => i.code)).toContain('SELF_TRADE');
  });

  it('an order the node did not confirm (no UTXO in the env) is not taken: the order is placed as an auction', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const env = { ...takerEnv([a]), bookOrders: new Map() };
    const p = take(env, 'buy', TOK, 255_000_000n);
    expect(p.take).toBeUndefined();
    expect(p.issues.map((i) => i.code)).toContain('MARKETABLE_AUCTION');
  });

  it('a minimum fill the rest of the order cannot meet skips that order', () => {
    const a = resting('sell', 250_000_000n, 10n * TOK, { minFill: 5n * TOK });
    const p = take(takerEnv([a]), 'buy', 2n * TOK, 255_000_000n);
    expect(p.take).toBeUndefined();
  });
});

describe('dust', () => {
  it('a partial fill whose rest would be dust (KIP-9) is not taken; the whole order is', () => {
    // 8 decimals, a bid of 4 tokens at 0.0249 KAS: 1 token left would be 0.0249 KAS on a covenant output, below its 0.08 KAS floor
    const m = market8x8({ decimals: 8 } as never);
    // its escrow budgets one delivery (as the mock's seeded bids)
    const b = resting('buy', 2_490_000n, 4n * m.scale, { minFill: 1n, market: m, maxFills: 1n });
    const env = { ...takerEnv([], [b], { tokens: 100n * m.scale }), token: m, tokenUtxos: [tokenUtxo(m, 100n * m.scale)] };
    const part = planOrder(env, { type: 'ioc', side: 'sell', amount: 3n * m.scale, price: 2_480_000n, take: true });
    expect(part.take).toBeUndefined();
    const whole = planOrder(env, { type: 'ioc', side: 'sell', amount: 4n * m.scale, price: 2_480_000n, take: true });
    expect(errors(whole)).toEqual([]);
    expect(whole.take!.amount).toBe(4n * m.scale);
    consensusCheck(K, whole.built!, [MAKER_SK]);
  });
});

describe('remainder resting', () => {
  it('what the book cannot fill rests as a plain limit, planned against the book without the taken orders', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = take(takerEnv([a]), 'buy', 5n * TOK, 252_000_000n);
    expect(p.take!.amount).toBe(2n * TOK);
    expect(p.take!.rest).toBe(3n * TOK);
    const r = p.take!.restPlan!;
    expect(errors(r)).toEqual([]);
    expect(r.states).toHaveLength(1);
    // a plain limit at the user's price, never an auction
    expect(r.issues.map((i) => i.code)).not.toContain('MARKETABLE_AUCTION');
    expect((r.states[0]!.state as { price: string }).price).toBe('252000000');
  });

  it('bookWithout drops the taken orders and sums the levels again', () => {
    const book: BookView = {
      asks: [{ price: 1n, amount: 5n, orders: 2 }], bids: [],
      askOrders: [{ covenantId: 'aa', price: 1n, amount: 2n }, { covenantId: 'bb', price: 1n, amount: 3n }], bidOrders: [],
    };
    const b = bookWithout(book, new Set(['aa']));
    expect(b.asks).toEqual([{ price: 1n, amount: 3n, orders: 1 }]);
    expect(b.askOrders).toEqual([{ covenantId: 'bb', price: 1n, amount: 3n }]);
    expect(bookWithout(book, new Set())).toBe(book);
  });
});

describe('the take request', () => {
  it('locks at the node clock less the taker margin and pays the tokens and the change to the wallet only', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = take(takerEnv([a]), 'buy', TOK, 255_000_000n);
    expect(BigInt(p.built!.tx.lockTime)).toBe(takeLockTime({ clock: CLOCK }));
    expect(p.built!.sign.every((s) => s.pubkey === MAKER_PK)).toBe(true);
  });

  it('a wallet short of KAS cannot take', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = take(takerEnv([a], [], { funding: [KAS] }), 'buy', 2n * TOK, 255_000_000n);
    expect(p.ok).toBe(false);
  });
});

describe('time in force', () => {
  it('IOC: takes what crosses and places nothing for the rest', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = planOrder(takerEnv([a]), { type: 'ioc', side: 'buy', amount: 5n * TOK, price: 255_000_000n, take: true });
    expect(errors(p)).toEqual([]);
    expect(p.take!.amount).toBe(2n * TOK);
    expect(p.take!.rest).toBe(3n * TOK);
    expect(p.take!.restKind).toBe('none');
    expect(p.take!.restPlan).toBeNull();
    expect(p.issues.map((i) => i.code)).toContain('TAKE_REST_NOT_PLACED');
    consensusCheck(K, p.built!, [MAKER_SK]);
  });

  it('IOC without the direct fill is the covenant IOC order', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = planOrder(takerEnv([a]), { type: 'ioc', side: 'buy', amount: 2n * TOK, price: 255_000_000n });
    expect(p.take).toBeUndefined();
    expect(p.states).toHaveLength(1);
  });

  it('FOK: the whole amount in one transaction, all or nothing', () => {
    const a1 = resting('sell', 250_000_000n, 2n * TOK);
    const a2 = resting('sell', 251_000_000n, 2n * TOK);
    const env = takerEnv([a1, a2]);
    const ok = planOrder(env, { type: 'fok', side: 'buy', amount: 4n * TOK, price: 255_000_000n, take: true });
    expect(errors(ok)).toEqual([]);
    expect(ok.take!.amount).toBe(4n * TOK);
    expect(ok.take!.rest).toBe(0n);
    consensusCheck(K, ok.built!, [MAKER_SK]);
    // more than the book holds at the price: refused, nothing placed
    const short = planOrder(env, { type: 'fok', side: 'buy', amount: 5n * TOK, price: 255_000_000n, take: true });
    expect(short.ok).toBe(false);
    expect(errors(short).map((i) => i.code)).toEqual(['TAKE_FOK_UNFILLED']);
    expect(short.built).toBeNull();
  });

  it('FOK that needs more than one transaction is refused', () => {
    const per = takesPerTx({ token: M }, 'buy');
    const asks = Array.from({ length: per + 1 }, (_, i) => resting('sell', 250_000_000n + BigInt(i) * 100_000n, TOK));
    const p = planOrder(takerEnv(asks), { type: 'fok', side: 'buy', amount: BigInt(per + 1) * TOK, price: 260_000_000n, take: true });
    expect(errors(p).map((i) => i.code)).toEqual(['TAKE_FOK_UNFILLED']);
  });

  it('market buy: takes within its worst price (the slippage bound), the rest is not placed', () => {
    const a1 = resting('sell', 250_000_000n, 2n * TOK);
    const a2 = resting('sell', 251_000_000n, 2n * TOK);
    const far = resting('sell', 300_000_000n, 5n * TOK);
    // 1% slippage from 2.50: worst 2.525, so the ask at 3.00 is never taken
    const p = planOrder(takerEnv([a1, a2, far]), { type: 'market', side: 'buy', amount: 6n * TOK, slippageBps: 100n, take: true });
    expect(errors(p)).toEqual([]);
    expect(p.take!.steps.flatMap((s) => s.fills).map((f) => f.covenantId)).toEqual([a1.row.covenantId, a2.row.covenantId]);
    expect(p.take!.limit).toBe(252_500_000n);
    expect(p.take!.rest).toBe(2n * TOK);
    expect(p.take!.restKind).toBe('none');
    consensusCheck(K, p.built!, [MAKER_SK]);
  });

  it('market sell: takes the bids within its worst price', () => {
    const b1 = resting('buy', 250_000_000n, 2n * TOK);
    const b2 = resting('buy', 200_000_000n, 2n * TOK);
    const p = planOrder(takerEnv([], [b1, b2]), { type: 'market', side: 'sell', amount: 3n * TOK, slippageBps: 300n, take: true });
    expect(errors(p)).toEqual([]);
    expect(p.take!.steps[0]!.fills.map((f) => f.covenantId)).toEqual([b1.row.covenantId]);
    expect(p.take!.amount).toBe(2n * TOK);
    consensusCheck(K, p.built!, [MAKER_SK]);
  });

  it('market with all or nothing is a FOK take', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = planOrder(takerEnv([a]), { type: 'market', side: 'buy', amount: 3n * TOK, allOrNothing: true, take: true });
    expect(errors(p).map((i) => i.code)).toEqual(['TAKE_FOK_UNFILLED']);
  });

  it('a market order without the direct fill is the auction', () => {
    const a = resting('sell', 250_000_000n, 2n * TOK);
    const p = planOrder(takerEnv([a]), { type: 'market', side: 'buy', amount: TOK });
    expect(p.take).toBeUndefined();
    expect(p.states).toHaveLength(1);
  });
});
