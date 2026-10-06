// The pair-API adapter (data/pair-api.ts): malformed rows never reach bigint math; the documented shapes pass unchanged; the guessed envelopes
// (an `items` wrapper of /v1/pairs, missing counts, a missing volume / cursor) read as documented.
import { describe, expect, it } from 'vitest';
import type { PairBookView } from './indexer-types';
import { adaptPairCandles, adaptPairFills, adaptPairSummaries, evidenceDetailOf, pairDetailOf, sanitizePairBook } from './pair-api';

const H1 = 'a1'.repeat(32);
const H2 = 'b2'.repeat(32);

describe('sanitizePairBook', () => {
  it('keeps direct, entry and route levels; drops unknown sources, zero or malformed terms', () => {
    const ok = (source: string, num = '3', den = '2', amount = '10') => ({ source, price_num: num, price_den: den, amount, orders: 1 });
    const v = {
      base: H1, quote: H2, daa_score: 1,
      asks: [ok('direct'), ok('entry'), ok('route'), ok('cross'), ok('direct', '0'), ok('direct', '1', '0'), ok('direct', '1', '1', '0'), ok('direct', '1.5'), null],
      bids: [ok('route', '-1')],
    } as unknown as PairBookView;
    const s = sanitizePairBook(v);
    expect(s.asks.map((l) => l.source)).toEqual(['direct', 'entry', 'route']);
    expect(s.bids).toEqual([]);
  });
});

describe('adaptPairSummaries', () => {
  it('an array or an items envelope; hex32 oriented pairs only; missing counts read as 0', () => {
    const full = { base: H1, quote: H2, direct_asks: 1, direct_bids: 2, entry_asks: 3, entry_bids: 4, conditionals: 5 };
    expect(adaptPairSummaries([full])).toEqual([full]);
    expect(adaptPairSummaries({ items: [{ base: H1, quote: H2, direct_asks: 1 }] })).toEqual([
      { base: H1, quote: H2, direct_asks: 1, direct_bids: 0, entry_asks: 0, entry_bids: 0, conditionals: 0 },
    ]);
    expect(adaptPairSummaries([{ base: 'zz', quote: H2 }, { base: H1, quote: H1 }, 5])).toEqual([]);
    expect(adaptPairSummaries({})).toBeNull();
    expect(adaptPairSummaries(null)).toBeNull();
  });
});

describe('adaptPairCandles / adaptPairFills', () => {
  const rate = (v: string, den = '1') => ({ value: v, num: v, den });
  it('keeps well-formed candles only (four exact rates, a numeric time)', () => {
    const good = { t: 1, o: rate('10'), h: rate('12'), l: rate('9'), c: rate('11'), a_traded: true, b_traded: true, pair_volume_a: '0', pair_volume_b: '0', pair_fills: 0 };
    const v = adaptPairCandles({ base: H1, quote: H2, interval: '1m', price_basis: '1000', quote_price_basis: '100', decimals: 3, quote_decimals: null, price_source: 'kas_books', items: [good, { ...good, o: rate('1', '0') }, { ...good, t: 'x' }] });
    expect(v?.items).toEqual([good]);
    expect(v).toMatchObject({ price_basis: '1000', decimals: 3, quote_decimals: null });
    expect(adaptPairCandles({ items: 3 })).toBeNull();
  });

  it('fills: volume only (no price source), a missing 24 h volume reads as zeros, a missing cursor as null', () => {
    const fill = { id: 1, order: H1, side: 'bid', amount_a: '5', amount_b: '7', counterparty: 'route', price_source: 'none' };
    const v = adaptPairFills({ base: H1, quote: H2, items: [fill, { id: 'x' }, { ...fill, side: 'buy' }] });
    expect(v?.items).toEqual([fill]);
    expect(v?.volume_24h).toEqual({ amount_a: '0', amount_b: '0', fills: 0 });
    expect(v?.next_cursor).toBeNull();
    expect(adaptPairFills({ items: [], next_cursor: 12 })?.next_cursor).toBe('12');
    expect(adaptPairFills(null)).toBeNull();
  });
});

describe('event details', () => {
  it('detail.pair of a pair fill (never a price source) and detail.evidence of a pair arm', () => {
    const ev = {
      detail: {
        pair: { side: 'ask', base: H1, quote: H2, a_scale: 1000, amount_a: '4000', amount_b: '4100', price: '1025', price_num: '41', price_den: '40', tip_kas: '0', counterparty: 'netting', price_source: 'whatever' },
        evidence: { mode: 0, inputs: [1, 2], orders: [H1, null], a: '300000000', b: '20000000' },
      },
    };
    expect(pairDetailOf(ev)).toMatchObject({ side: 'ask', amount_a: '4000', amount_b: '4100', price: '1025', counterparty: 'netting', price_source: 'none' });
    expect(evidenceDetailOf(ev)).toEqual({ mode: 0, inputs: [1, 2], orders: [H1, null], a: '300000000', b: '20000000' });
    expect(evidenceDetailOf({ detail: { evidence: { mode: 1, price: '1400' } } })).toEqual({ mode: 1, price: '1400' });
    expect(pairDetailOf({ detail: { pair: { side: 'x' } } })).toBeNull();
    expect(pairDetailOf({ detail: null })).toBeNull();
    expect(evidenceDetailOf({ detail: {} })).toBeNull();
  });
});
