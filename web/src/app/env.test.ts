import { describe, expect, it } from 'vitest';
import type { BookOrderView, LevelView, OrderView } from '../data/indexer-types';
import type { Services } from './services';
import { CLOCK, MAKER_PK, kob, market3x3 } from '../testing/fixtures';
import { tradableRegistryJson } from '../testing/chain-fixtures';
import { parseRegistry } from '../kob/registry';
import { planOrder } from '../kob/plan';
import { isPairEnv } from '../kob/plan-types';
import { bookFromIndexer, buildPairPlanEnv, kasReferenceOf, ownOrderRefs } from './env';

const m = market3x3(); // 3 decimals: scale 1000 base units = one whole token
const row = (price: string, over: Partial<BookOrderView> = {}): BookOrderView => ({
  covenant_id: 'aa'.repeat(32), contract: 'KobAsk', maker: null, price, tip: null, min_fill: '1', scale: 1000, amount_left: '2000', amount_estimated: false,
  status: 'open', cur_value: null, expiry_daa: null, expired: false, genesis_daa: 1, confirmations: 10, settled: true, ...over,
});
const lv = (price: string, amount: string, scale: number, orders = 1): LevelView => ({ price, amount, amount_estimated: false, orders, scale });

describe('bookFromIndexer', () => {
  it('keeps prices per whole token as posted and sums the base units of equal prices', () => {
    const book = bookFromIndexer(m, {
      asks: [row('5000'), row('5000', { amount_left: '1500', covenant_id: 'bb'.repeat(32) }), row('6000', { amount_left: '7', covenant_id: 'cc'.repeat(32) })],
      bids: [],
    });
    expect(book.asks).toEqual([{ price: 5000n, amount: 3500n, orders: 2 }, { price: 6000n, amount: 7n, orders: 1 }]);
    expect(book.askOrders).toEqual([
      { covenantId: 'aa'.repeat(32), price: 5000n, amount: 2000n, minFill: 1n },
      { covenantId: 'bb'.repeat(32), price: 5000n, amount: 1500n, minFill: 1n },
      { covenantId: 'cc'.repeat(32), price: 6000n, amount: 7n, minFill: 1n },
    ]);
  });

  it('carries the tip and the minimum fill of every order (exact FOK pre-check)', () => {
    const book = bookFromIndexer(m, { asks: [row('5000', { tip: '30', min_fill: '250' })], bids: [] });
    expect(book.askOrders).toEqual([{ covenantId: 'aa'.repeat(32), price: 5000n, amount: 2000n, tip: 30n, minFill: 250n }]);
  });

  it('skips orders of another scale (their price quotes another whole token); a row without a scale is taken as the standard one', () => {
    const book = bookFromIndexer(m, { asks: [row('5000', { scale: 100 }), row('5100', { scale: null, covenant_id: 'dd'.repeat(32) })], bids: [] });
    expect(book.asks).toEqual([{ price: 5100n, amount: 2000n, orders: 1 }]);
  });

  it('aggregated levels: equal prices merge, levels of another scale are skipped', () => {
    const book = bookFromIndexer(m, { asks: [lv('5000', '2000', 1000), lv('5000', '3000', 1000, 2), lv('6000', '1000', 1000), lv('9', '5000', 100)], bids: [] });
    expect(book.asks).toEqual([{ price: 5000n, amount: 5000n, orders: 3 }, { price: 6000n, amount: 1000n, orders: 1 }]);
    expect(book.askOrders).toBeUndefined();
  });
});

describe('ownOrderRefs', () => {
  it('keeps open orders of the token at the standard scale, with prices per whole token and the amount left', () => {
    const base = { covenant_id: 'cc'.repeat(32), side: 1, token: m.covenantId, status: 'open', price: '5000', quote: null, tip: '0', scale: 1000, amount_left: '2000', initial_amount: '2000', expired: false } as unknown as OrderView;
    const other = { ...base, covenant_id: 'dd'.repeat(32), token: 'ee'.repeat(32) } as OrderView;
    const otherScale = { ...base, covenant_id: 'ef'.repeat(32), scale: 100 } as OrderView;
    const refs = ownOrderRefs(m, [base, other, otherScale]);
    expect(refs).toHaveLength(1);
    expect(refs[0]).toMatchObject({ side: 'sell', price: 5000n, amountLeft: 2000n, tip: 0n, active: true });
  });

  it('a pair order listed under the token (no KAS price) is never a self-trade reference of the KAS book', () => {
    const pairAsk = {
      covenant_id: 'c1'.repeat(32), contract: 'KobPair', side: 1, token: m.covenantId, status: 'open', price: null, quote: null, tip: '0', scale: 1000,
      amount_left: '3000', initial_amount: '3000', expired: false, pair: { base: m.covenantId, quote: 'bb'.repeat(32), side: 'ask', price: '100', quote_now: '100' },
    } as unknown as OrderView;
    const condPair = { ...pairAsk, covenant_id: 'c2'.repeat(32), contract: 'KobCondPair', pair: undefined } as unknown as OrderView;
    expect(ownOrderRefs(m, [pairAsk, condPair])).toEqual([]);
  });
});

// ------------------------------------------------------------------------------------------------ buildPairPlanEnv

const K = kob();
const registry = parseRegistry(tradableRegistryJson(), { kob: K });
const A = registry.tokens[0]!; // 3x3, 3 decimals
const B = registry.tokens[1]!; // 8x8, 8 decimals
const H = (s: string) => s.repeat(32);

interface Fakes {
  pairBook?: () => Promise<unknown>;
  allOrders?: () => Promise<unknown>;
  book?: (t: string) => Promise<unknown>;
  trades?: (t: string) => Promise<unknown>;
  indexer?: boolean;
}
const services = (f: Fakes = {}) =>
  ({
    kob: K,
    registry,
    node: { getClock: async () => ({ daa: CLOCK.daa, unixSeconds: CLOCK.unixSeconds, rateMilli: 10_000 }) },
    utxos: { fundingFor: async () => [] },
    tracker: { tokenUtxosFor: async (_pk: string, t: { covenantId: string }) => [{ covenantId: t.covenantId, marker: t.covenantId }] },
    indexer: f.indexer === false ? null : {
      pairBook: f.pairBook ?? (async () => ({ base: A.covenantId, quote: B.covenantId, daa_score: 1, asks: [], bids: [] })),
      allOrders: f.allOrders ?? (async () => []),
      book: f.book ?? (async () => ({ asks: [], bids: [] })),
      trades: f.trades ?? (async () => null),
    },
    config: { features: {} },
  }) as unknown as Services;

describe('buildPairPlanEnv', () => {
  it('A is the env token, B the pair quote, the UTXOs of both tokens, the pair book in B per whole A (asks up, bids down)', async () => {
    const pv = {
      base: A.covenantId, quote: B.covenantId, daa_score: 5,
      // 1 B base unit per 3 A base units: per whole A (1000) = 333.33 -> ask 334 (up), bid 333 (down); a direct, an entry and a route level
      asks: [{ source: 'direct', price_num: '1', price_den: '3', amount: '500', orders: 1 }, { source: 'entry', price_num: '1', price_den: '2', amount: '100', orders: 1 }],
      bids: [{ source: 'route', price_num: '1', price_den: '3', amount: '700', orders: 2 }],
    };
    const env = await buildPairPlanEnv(services({ pairBook: async () => pv }), { pubkey: MAKER_PK }, A, B);
    expect(isPairEnv(env)).toBe(true);
    expect(env.token.covenantId).toBe(A.covenantId);
    expect(env.pair.quote.covenantId).toBe(B.covenantId);
    expect((env.tokenUtxos[0] as unknown as { marker: string }).marker).toBe(A.covenantId);
    expect((env.pair.quoteTokenUtxos[0] as unknown as { marker: string }).marker).toBe(B.covenantId);
    expect(env.pair.view).toBe(pv);
    expect(env.book).toEqual({ asks: [{ price: 334n, amount: 500n, orders: 1 }, { price: 500n, amount: 100n, orders: 1 }], bids: [{ price: 333n, amount: 700n, orders: 2 }] });
    expect(env.guardsUnavailable).toBeUndefined();
  });

  it('own pair orders of THIS pair (oriented) are the self-trade references, at their quote now; others are left out', async () => {
    const own = (id: string, pair: object, over: object = {}) => ({ covenant_id: H(id), contract: 'KobPair', status: 'open', expired: false, amount_left: '10', pair, ...over });
    const mine = [
      own('a1', { base: A.covenantId, quote: B.covenantId, side: 'ask', price: '900', quote_now: '880', amount_left: '10' }),
      own('a2', { base: A.covenantId, quote: B.covenantId, side: 'bid', price: '700', quote_now: null, amount_left: '20' }),
      own('a3', { base: B.covenantId, quote: A.covenantId, side: 'ask', price: '1', quote_now: '1' }),
      own('a4', { base: A.covenantId, quote: B.covenantId, side: 'ask', price: null, quote_now: null }, { contract: 'KobCondPair' }),
      own('a5', { base: A.covenantId, quote: B.covenantId, side: 'ask', price: '5', quote_now: '5' }, { status: 'filled' }),
      { covenant_id: H('a6'), contract: 'KobAsk', status: 'open', token: A.covenantId, price: '100', side: 1, expired: false },
    ];
    const env = await buildPairPlanEnv(services({ allOrders: async () => mine }), { pubkey: MAKER_PK }, A, B);
    expect(env.ownOrders).toEqual([
      { covenantId: H('a1'), side: 'sell', price: 880n, tip: 0n, amountLeft: 10n, active: true },
      { covenantId: H('a2'), side: 'buy', price: 700n, tip: 0n, amountLeft: 20n, active: true },
    ]);
  });

  it('KAS references: the KAS book midpoint, else the newest trade; the rate the newest trades imply is the cross-check', async () => {
    const lvl = (price: string) => ({ price, amount: '1000', amount_estimated: false, orders: 1, scale: 1000 });
    const book = async (t: string) => (t === A.covenantId ? { asks: [lvl('310000000')], bids: [lvl('290000000')] } : { asks: [], bids: [] });
    const trades = async (t: string) => (t === A.covenantId ? { price_basis: '1000', items: [{ price: '300000000' }] } : { price_basis: '100000000', items: [{ price: '20000000' }] });
    const env = await buildPairPlanEnv(services({ book, trades }), { pubkey: MAKER_PK }, A, B);
    expect(env.pair.kasPerWholeA).toBe(300_000_000n);
    // B has no KAS book: its newest trade, 0.2 KAS per whole B
    expect(env.pair.kasPerWholeB).toBe(20_000_000n);
    // 3 KAS / 0.2 KAS = 15 whole B per whole A = 15 x 10^8 B base units
    expect(env.lastFillPrice).toBe(1_500_000_000n);
    const failing = { book: async () => { throw new Error('x'); }, trades: async () => null } as unknown as Parameters<typeof kasReferenceOf>[0];
    expect(await kasReferenceOf(failing, env.token)).toEqual({ book: null, last: null });
  });

  it('KAS references: an ask nobody has to take does not move the reference (the default minimum fill of a pair order)', async () => {
    // 10 whole tokens a level (29 KAS at 2.9): one level is worth a default minimum fill (10 KAS)
    const lvl = (price: string, amount = '10000') => ({ price, amount, amount_estimated: false, orders: 1, scale: 1000 });
    const trades = async () => ({ price_basis: '1000', items: [] });
    const m = (await buildPairPlanEnv(services(), { pubkey: MAKER_PK }, A, B)).token;
    // only an ask, far off: no book reference (the newest trade, here none)
    const onlyAsk = async () => ({ asks: [lvl('900000000000000')], bids: [] });
    expect(await kasReferenceOf({ book: onlyAsk, trades } as unknown as Parameters<typeof kasReferenceOf>[0], m)).toMatchObject({ book: null });
    // a bid and a far-off ask: the bid
    const wide = async () => ({ asks: [lvl('900000000000000')], bids: [lvl('290000000')] });
    expect(await kasReferenceOf({ book: wide, trades } as unknown as Parameters<typeof kasReferenceOf>[0], m)).toMatchObject({ book: 290_000_000n });
    // a narrow spread: the midpoint
    const narrow = async () => ({ asks: [lvl('310000000')], bids: [lvl('290000000')] });
    expect(await kasReferenceOf({ book: narrow, trades } as unknown as Parameters<typeof kasReferenceOf>[0], m)).toMatchObject({ book: 300_000_000n });
    // a bid of one base unit at 3,000 times the book's price (worth 9 KAS) does not set it: the bids worth 10 KAS together do
    const tiny = async () => ({ asks: [], bids: [lvl('900000000000', '1'), lvl('290000000')] });
    expect(await kasReferenceOf({ book: tiny, trades } as unknown as Parameters<typeof kasReferenceOf>[0], m)).toMatchObject({ book: 290_000_000n });
  });

  it('a failed or unsupported pair book and a failed own-order read are reported (guards unavailable), never silently off', async () => {
    const env = await buildPairPlanEnv(services({ pairBook: async () => null, allOrders: async () => { throw new Error('503'); } }), { pubkey: MAKER_PK }, A, B);
    expect(env.guardsUnavailable).toEqual(['book', 'own-orders']);
    expect(env.pair.view).toBeNull();
    expect(env.book).toEqual({ asks: [], bids: [] });
    expect(env.pair.kasPerWholeA).toBeNull();
    const offline = await buildPairPlanEnv(services({ indexer: false }), { pubkey: MAKER_PK }, A, B);
    expect(offline.guardsUnavailable).toBeUndefined();
  });

  it('refuses one token as both sides', async () => {
    await expect(buildPairPlanEnv(services(), { pubkey: MAKER_PK }, A, A)).rejects.toThrow(/two different tokens/);
  });

  it('the env plans a pair order end to end (planOrder dispatches to the pair planner)', async () => {
    const env = await buildPairPlanEnv(services(), { pubkey: MAKER_PK }, A, B);
    const p = planOrder({ ...env, tokenUtxos: [] }, { type: 'limit', side: 'sell', amount: 1_000n, price: 1_500n });
    // the fake wallet holds no A: the pair planner ran and refused with the pair code naming A
    expect(p.issues.find((i) => i.code === 'PAIR_INSUFFICIENT_TOKENS')?.params).toMatchObject({ ticker: A.ticker });
  });
});

