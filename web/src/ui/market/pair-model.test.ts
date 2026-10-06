import { describe, expect, it } from 'vitest';
import type { PairBookView, PairLevelView } from '../../data/indexer-types';
import { sanitizePairBook } from '../../data/indexer';
import type { PairToken } from '../../kob/pair';
import { t } from '../../i18n';
import { buildPairBook, pickOf, spreadLine } from './pair-model';

// BASE: 8 decimals, scale 1e8; QUOTE: 6 decimals, scale 1e6. price_num / price_den = QUOTE base units per BASE base unit.
const BASE: PairToken = { covenantId: 'aa'.repeat(32), ticker: 'BTC', decimals: 8, scale: 100_000_000n };
const QUOTE: PairToken = { covenantId: 'bb'.repeat(32), ticker: 'USDT', decimals: 6, scale: 1_000_000n };
const lv = (source: 'direct' | 'entry' | 'route', num: string, den: string, amount: string, orders = 1): PairLevelView => ({ source, price_num: num, price_den: den, amount, orders });
const view = (asks: PairLevelView[], bids: PairLevelView[]): PairBookView => ({ base: BASE.covenantId, quote: QUOTE.covenantId, daa_score: 1, asks, bids });

describe('pair book model', () => {
  it('prices are exact rationals shown per WHOLE base token in quote units (decimals honoured), asks rounded up, bids down', () => {
    // 52_000 USDT units per 1e8 BTC units = 0.052 USDT per BTC; 1/3 of a unit ratio shows the rounding
    const m = buildPairBook(view([lv('direct', '52000', '100000000', '300000000', 2), lv('route', '1', '1875', '100000000')], [lv('route', '48000', '100000000', '500000000')]), BASE, QUOTE)!;
    expect(m.asks.map((r) => [r.source, r.priceText, r.amountText, r.orders])).toEqual([
      ['direct', '0.0520000', '3', 2], // fixed decimals: every row of a column has the same number (trailing zeros kept)
      ['route', '0.0533334', '1', 1], // 1/1875 x 100 = 0.0533333... rounded UP for an ask (6 significant digits)
    ]);
    expect(m.bids.map((r) => [r.source, r.priceText, r.amountText])).toEqual([['route', '0.0480000', '5']]);
    expect(m.asks[0]!.price).toEqual({ num: 13n, den: 250n });
    expect(m.asks[0]!.totalText).toBe('0.156000');
    expect(m.spreadText).toBe('0.0040000');
    expect(m.midText).toBe('0.0500000');
    expect(m.spreadPct).toBe('8.00');
    expect(m.dp).toBe(7);
  });

  it('sorts asks ascending and bids descending by exact price (cross multiplication), direct before route on ties, and cuts to depth', () => {
    const m = buildPairBook(
      view(
        [lv('route', '6', '100', '1'), lv('route', '5', '100', '1'), lv('direct', '1', '20', '1'), lv('route', '7', '100', '1')],
        [lv('route', '3', '100', '1'), lv('direct', '4', '100', '1'), lv('route', '1', '25', '1')],
      ),
      BASE,
      QUOTE,
      3,
    )!;
    expect(m.asks.map((r) => `${r.source}:${r.unitPrice.num}/${r.unitPrice.den}`)).toEqual(['direct:1/20', 'route:1/20', 'route:3/50']);
    expect(m.bids.map((r) => `${r.source}:${r.unitPrice.num}/${r.unitPrice.den}`)).toEqual(['direct:1/25', 'route:1/25', 'route:3/100']);
    expect(m.asks[0]!.bar).toBe(1);
  });

  it('a crossed book has a negative spread, handles one-sided and empty books, refuses a view of another pair', () => {
    const crossed = buildPairBook(view([lv('route', '4', '100', '1')], [lv('route', '5', '100', '1')]), BASE, QUOTE)!;
    expect(crossed.spreadText).toBe('-1.00000');
    const one = buildPairBook(view([lv('route', '4', '100', '1')], []), BASE, QUOTE)!;
    expect(one.spreadText).toBeNull();
    expect(one.spreadPct).toBeNull();
    expect(one.midText).toBeNull();
    expect(buildPairBook(view([], []), BASE, QUOTE)!.empty).toBe(true);
    expect(buildPairBook({ ...view([], []), base: QUOTE.covenantId, quote: BASE.covenantId }, BASE, QUOTE)).toBeNull();
  });

  it('quote decimals above base decimals scale the other way', () => {
    const b: PairToken = { ...BASE, decimals: 2, scale: 100n };
    const q: PairToken = { ...QUOTE, decimals: 8, scale: 100_000_000n };
    // 3 quote units per base unit = 3 x 10^(2-8) whole quote per whole base
    const m = buildPairBook(view([lv('route', '3', '1', '100')], []), b, q)!;
    expect(m.asks[0]!.priceText).toBe('0.00000300000'); // 5 significant digits of a tiny price: 11 fixed decimals
  });

  it('the client drops malformed levels before any bigint math', () => {
    const bad = view(
      [lv('route', '1', '0', '5'), lv('route', 'x', '1', '5'), { ...lv('route', '1', '1', '5'), source: 'other' as 'route' }, lv('direct', '1', '1', '0'), lv('direct', '2', '3', '4')],
      [lv('route', '-1', '1', '1')],
    );
    const s = sanitizePairBook(bad);
    expect(s.asks).toEqual([lv('direct', '2', '3', '4')]);
    expect(s.bids).toEqual([]);
  });
});

describe('pair book crossings: pair orders net each other and fill through the KAS route; a crossed book is shown as it is, with a negative spread', () => {
  const L = '100000000';
  it('route levels crossing each other (a crossed KAS book behind them): the same spread line with a negative spread and percent', () => {
    const m = buildPairBook(view([lv('route', '4', '100', '1')], [lv('route', '5', '100', '1')]), BASE, QUOTE)!;
    expect(t(spreadLine(m).key, spreadLine(m).params)).toBe('Mid 4.50000 · Spread -1.00000 (-22.22%)');
  });

  it('resting pair orders crossing each other (direct and entry levels) are fillable by netting: a negative spread, on either side', () => {
    // a direct ask at 4 under a direct bid at 6 (USDT per BTC), the route uncrossed
    const d = buildPairBook(view([lv('direct', '4', '100', L, 2), lv('route', '7', '100', L)], [lv('direct', '6', '100', L), lv('route', '3', '100', L)]), BASE, QUOTE)!;
    expect(spreadLine(d)).toEqual({ key: 'pair.book.spread', params: { mid: '5.00000', spread: '-2.00000', pct: '-40.00' } });
    // an if-done entry bid above a direct ask
    const e = buildPairBook(view([lv('direct', '4', '100', L)], [lv('entry', '5', '100', L)]), BASE, QUOTE)!;
    expect(e.spreadText).toBe('-1.00000');
    // a pair order through a route level
    const r = buildPairBook(view([lv('direct', '4', '100', L)], [lv('route', '5', '100', L)]), BASE, QUOTE)!;
    expect(r.spreadText).toBe('-1.00000');
  });

  it('ties at one price: direct, then entry, then route; the entry source is kept for the label', () => {
    const m = buildPairBook(view([lv('route', '5', '100', L), lv('entry', '5', '100', L), lv('direct', '5', '100', L)], [lv('entry', '4', '100', L, 3)]), BASE, QUOTE)!;
    expect(m.asks.map((r) => r.source)).toEqual(['direct', 'entry', 'route']);
    expect(m.bids[0]).toMatchObject({ source: 'entry', orders: 3 });
  });

  it('an uncrossed book has a positive spread; a one-sided one the one-sided line', () => {
    const m = buildPairBook(view([lv('direct', '6', '100', '1')], [lv('direct', '4', '100', '1')]), BASE, QUOTE)!;
    expect(spreadLine(m)).toEqual({ key: 'pair.book.spread', params: { mid: '5.00000', spread: '2.00000', pct: '40.00' } });
    expect(spreadLine(buildPairBook(view([lv('direct', '6', '100', '1')], []), BASE, QUOTE)!)).toEqual({ key: 'pair.book.oneSided', params: {} });
  });
});

describe('a click on a level prefills the ticket with a state price (QUOTE base units per whole BASE)', () => {
  it('an ask prefills a buy rounded up, a bid a sell rounded down (both reach the level)', () => {
    // 1 / 3 USDT unit per BTC unit = 33,333,333.33 USDT units per whole BTC (1e8 units)
    const m = buildPairBook(view([lv('direct', '1', '3', '100')], [lv('route', '1', '3', '100')]), BASE, QUOTE)!;
    expect(pickOf(m.asks[0]!, BASE)).toEqual({ side: 'buy', price: 33_333_334n });
    expect(pickOf(m.bids[0]!, BASE)).toEqual({ side: 'sell', price: 33_333_333n });
  });
});
