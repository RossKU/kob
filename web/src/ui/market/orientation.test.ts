import { describe, expect, it } from 'vitest';
import type { DepthView, LevelView, StatsView } from '../../data/indexer-types';
import { cmpRational, parseDecimal, ratToNumber } from '../../kob/pair';
import { safeRounding } from '../../kob/units';
import { buildBookModel } from './book-model';
import { statsModel, type Candle } from './market-model';
import {
  defaultInverted, depthFromBookFlipped, depthFromViewFlipped, flipDp, flippedBookView, flippedChartSeries, flippedPriceText, invertedToNativePriceText,
  invertedRegrouper, marketKey, nativeBookView, nativeToInvertedPriceText, oppositeSide, pairLabel, readInverted, statsModelFlipped, tokensPerKas, writeInverted,
} from './orientation';
import { depthFromView } from './market-model';
import { buildIntent, layoutOf, FIELDS, setSide, type TicketForm } from '../ticket/form-state';
import { CASES, CTX, form, json } from '../ticket/ticket-fixtures';
import { sideOfField } from '../ticket/TicketFields';

// 8 decimals, scale 1e8 (one whole token): a book price of 2_500_000 sompi per token is 0.025 KAS per token = 40 tokens per KAS
const S = 100_000_000n;
const lv = (price: string, tokens: number, orders = 1): LevelView => ({ price, amount: String(BigInt(tokens) * S), orders, amount_estimated: false, scale: Number(S) });
const CTX_BOOK = { name: 'EXKCC', decimals: 8, scale: S, tick: 100_000n, dp: 2, labels: { price: 'p', size: 's', total: 't' } };
const CTX_NATIVE = { ...CTX_BOOK, dp: 4 };
const view = () => ({
  asks: [lv('2500000', 7, 2), lv('5000000', 3)],
  bids: [lv('2000000', 4), lv('1000000', 6)],
});

describe('conventions', () => {
  it('a token is TOKEN/KAS, a USD reference token is KAS/<its ticker>, and a flip inverts either', () => {
    expect(defaultInverted(false)).toBe(false);
    expect(defaultInverted(true)).toBe(true);
    expect(pairLabel('BTC', false)).toBe('BTC/KAS');
    expect(pairLabel('BTC', true)).toBe('KAS/BTC');
    expect(pairLabel('TUSD', true)).toBe('KAS/TUSD');
    expect(pairLabel('TUSD', false)).toBe('TUSD/KAS');
    expect(oppositeSide('buy')).toBe('sell');
    expect(oppositeSide(oppositeSide('buy'))).toBe('buy');
  });

  it('the remembered choice is per market, optional, and never throws without storage', () => {
    const store = new Map<string, string>();
    const g = globalThis as unknown as { localStorage?: unknown };
    const before = g.localStorage;
    g.localStorage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) };
    try {
      expect(readInverted(marketKey('token', 'AA'))).toBeNull();
      writeInverted(marketKey('token', 'AA'), true);
      writeInverted(marketKey('token', 'kas'), false);
      expect(readInverted('token:aa')).toBe(true);
      expect(readInverted('token:kas')).toBe(false);
      expect(readInverted('token:bb')).toBeNull();
    } finally {
      g.localStorage = before;
    }
    // storage that throws (blocked cookies / private mode): reads give "no choice", writes are ignored
    g.localStorage = { getItem: () => { throw new Error('denied'); }, setItem: () => { throw new Error('denied'); } };
    try {
      expect(readInverted('token:aa')).toBeNull();
      expect(() => writeInverted('token:aa', true)).not.toThrow();
    } finally {
      g.localStorage = before;
    }
  });
});

describe('prices', () => {
  it('tokens per KAS is the exact inverse of KAS per token', () => {
    expect(ratToNumber(tokensPerKas(2_500_000n, 8, 100_000_000n)!)).toBe(40);
    // a price per 0.5 token (scale 5e7): 1_000_000 sompi per half token = 0.02 KAS per token = 50 tokens per KAS
    expect(ratToNumber(tokensPerKas(1_000_000n, 8, 50_000_000n)!)).toBe(50);
    // 12 decimals, scale 1e9 (a thousandth of a token): 2_500 sompi per 1e9 base units = 0.025 KAS per token
    expect(ratToNumber(tokensPerKas(2_500n, 12, 1_000_000_000n)!)).toBe(40);
    expect(tokensPerKas(0n, 8, 100_000_000n)).toBeNull();
    expect(flippedPriceText(2_500_000n, 8, 100_000_000n, 2)).toBe('40.00');
    expect(flippedPriceText(3_000_000n, 8, 100_000_000n, 4)).toBe('33.3333');
    expect(flipDp(tokensPerKas(2_500_000n, 8, 100_000_000n))).toBe(3);
  });

  it('a typed inverted price becomes the native text exactly when it can, else rounds the way that never makes the limit worse', () => {
    expect(invertedToNativePriceText('40', 'up')).toBe('0.025');
    expect(invertedToNativePriceText('0.4', 'down')).toBe('2.5');
    expect(invertedToNativePriceText('400000', 'up')).toBe('0.0000025');
    // 1/3 has no finite expansion: a sell (round up) is never below the true price, a buy (down) never above
    expect(invertedToNativePriceText('3', 'up')).toBe('0.33333334');
    expect(invertedToNativePriceText('3', 'down')).toBe('0.33333333');
    // text that is not a positive decimal goes through for the form to report; zero stays zero (the form says "positive")
    expect(invertedToNativePriceText('abc', 'up')).toBe('abc');
    expect(invertedToNativePriceText('', 'up')).toBe('');
    expect(invertedToNativePriceText('0', 'up')).toBe('0');
    expect(invertedToNativePriceText('-5', 'up')).toBe('-5');
    expect(nativeToInvertedPriceText('0.025')).toBe('40');
    expect(nativeToInvertedPriceText('')).toBe('');
    expect(nativeToInvertedPriceText('abc')).toBe('abc');
  });
});

describe('order book', () => {
  const model = buildBookModel(view(), { scale: S });
  const native = nativeBookView(model, CTX_NATIVE);
  const flipped = flippedBookView(model, CTX_BOOK);

  it('native: asks listed highest first, sizes in tokens, totals in KAS', () => {
    expect(native.asks.map((r) => r.priceText)).toEqual(['0.0500', '0.0250']);
    expect(native.bids.map((r) => r.priceText)).toEqual(['0.0200', '0.0100']);
    expect(native.asks.at(-1)).toMatchObject({ sizeText: '7', priceValue: '2500000', amount: 700_000_000n });
    expect(native.mid).toEqual({ text: '0.0225', value: '2250000' });
  });

  it('inverted: native asks become bids (best = highest price) and native bids become asks', () => {
    expect(flipped.bids.map((r) => r.priceText)).toEqual(['40.00', '20.00']);
    // asks listed highest first: 100 then 50 (the best ask 50 directly above the spread)
    expect(flipped.asks.map((r) => r.priceText)).toEqual(['100.00', '50.00']);
    // the book is not crossed: best bid below best ask
    expect(Number(flipped.bids[0]!.priceValue)).toBeLessThan(Number(flipped.asks.at(-1)!.priceValue));
    // size is KAS (amount x price / scale): 7 tokens at 0.025 KAS = 0.175 KAS; total is the cumulative tokens
    expect(flipped.bids[0]).toMatchObject({ sizeText: '0.175', totalText: '7', amount: 700_000_000n });
    expect(flipped.bids[1]).toMatchObject({ sizeText: '0.150', totalText: '10' });
    // mid and spread of the inverted touch: bid 40, ask 50
    expect(flipped.mid!.text).toBe('45.00');
    expect(flipped.spread).toEqual({ text: '10.00', pct: '22.22' });
  });

  it('the same level is the same click in either orientation: the ticket prefill is native', () => {
    const key = (v: typeof native) => [...v.asks, ...v.bids].map((r) => `${r.pick}@${r.nativePrice}`).sort();
    expect(key(flipped)).toEqual(key(native));
    // an ask level (a seller of the token) is bought, a bid level sold, whichever side of the screen it is drawn on
    expect(native.asks.every((r) => r.pick === 'buy') && native.bids.every((r) => r.pick === 'sell')).toBe(true);
    expect(flipped.bids.every((r) => r.pick === 'buy') && flipped.asks.every((r) => r.pick === 'sell')).toBe(true);
  });

  it('depth bars are relative to the deeper side in KAS', () => {
    const all = [...flipped.bids, ...flipped.asks].map((r) => r.depthPct);
    expect(Math.max(...all)).toBe(100);
    expect(all.every((p) => p >= 0 && p <= 100)).toBe(true);
  });

  it('empty and one-sided books do not break', () => {
    const one = flippedBookView(buildBookModel({ asks: [lv('2500000', 1)], bids: [] }, { scale: S }), CTX_BOOK);
    expect(one.asks).toEqual([]);
    expect(one.bids).toHaveLength(1);
    expect(one.mid).toBeNull();
    expect(one.spread).toBeNull();
  });
});

describe('24 h stats', () => {
  const stats = (o: Partial<StatsView>): StatsView => ({
    token: 'aa'.repeat(32), price_basis: '100000000', ts: 0, last: '2500000', last_ts: 1, last_side: 'buy', open_24h: '2000000', high_24h: '5000000', low_24h: '1000000',
    change_24h_bps: 2500, volume_24h: '300000000', quote_volume_24h: '900000000', trades_24h: 12, best_bid: null, best_ask: null, mid: null, spread_bps: 40, open_asks: 1, open_bids: 1, ...o,
  });

  it('native stats are unchanged', () => {
    const m = statsModel(stats({}), 8, 4);
    expect(m).toMatchObject({ last: '0.0250', high: '0.0500', low: '0.0100', change: '+25.00%', lastSide: 'buy', volume: '3', quoteVolume: '9' });
  });

  it('inverted: last 1/last, high 1/low, low 1/high, change 1/(1+c)-1, side flips, the volumes swap units', () => {
    const m = statsModelFlipped(stats({}), 8, 2);
    expect(m.last).toBe('40.00');
    expect(m.high).toBe('100.00');
    expect(m.low).toBe('20.00');
    expect(m.change).toBe('-20.00%'); // up 25 % one way is down 20 % the other
    expect(m.changeTone).toBe('down');
    expect(m.lastSide).toBe('sell');
    expect(m.volume).toBe('9'); // KAS turnover is the base volume now
    expect(m.quoteVolume).toBe('3'); // the token volume
    expect(m.spread).toBe('0.40%');
    expect(m.trades).toBe(12);
  });

  it('inverted stats of an indexer without figures are dashes', () => {
    const m = statsModelFlipped(null, 8, 2);
    expect([m.last, m.high, m.low, m.change, m.volume, m.quoteVolume]).toEqual([null, null, null, null, null, null]);
    expect(statsModelFlipped(stats({ change_24h_bps: -10_000 }), 8, 2).change).toBeNull();
  });
});

describe('candles', () => {
  const candle = (t: number, o: bigint, h: bigint, l: bigint, c: bigint, volume: bigint, quote: bigint, filled = false): Candle => ({ t, o, h, l, c, volume, quote, trades: 1, filled });

  it('open and close invert, high and low swap, the volume is the KAS turnover, a rising price is a falling inverse', () => {
    // 0.02 -> 0.025 KAS per token (up): open 2_000_000, close 2_500_000, range 1_000_000..5_000_000 sompi per token
    const s = flippedChartSeries([candle(60_000, 2_000_000n, 5_000_000n, 1_000_000n, 2_500_000n, 300_000_000n, 900_000_000n)], 8, 100_000_000n, 2);
    expect(s.bars[0]).toMatchObject({ time: 60, open: 50, high: 100, low: 20, close: 40, filled: false });
    expect(s.bars[0]!.high).toBeGreaterThanOrEqual(Math.max(s.bars[0]!.open, s.bars[0]!.close));
    expect(s.bars[0]!.low).toBeLessThanOrEqual(Math.min(s.bars[0]!.open, s.bars[0]!.close));
    expect(s.volumes[0]).toEqual({ time: 60, value: 9, up: false });
    expect(s.text(0)).toEqual({ o: '50.00', h: '100.00', l: '20.00', c: '40.00' });
    expect(s.text(5)).toBeNull();
  });

  it('a filled bucket stays filled and flat', () => {
    const s = flippedChartSeries([candle(0, 2_500_000n, 2_500_000n, 2_500_000n, 2_500_000n, 0n, 0n, true)], 8, 100_000_000n, 2);
    expect(s.bars[0]).toMatchObject({ open: 40, high: 40, low: 40, close: 40, filled: true });
    expect(s.volumes[0]!.value).toBe(0);
  });
});

describe('depth', () => {
  const dv: DepthView = {
    token: 'aa'.repeat(32), price_basis: '100000000', ts: 0, daa: 0,
    bids: [
      { price: '2000000', amount: '400000000', orders: 1, cum_amount: '0', cum_quote: '0', estimated: false },
      { price: '1000000', amount: '600000000', orders: 1, cum_amount: '0', cum_quote: '0', estimated: false },
    ],
    asks: [
      { price: '2500000', amount: '700000000', orders: 2, cum_amount: '0', cum_quote: '0', estimated: false },
      { price: '5000000', amount: '300000000', orders: 1, cum_amount: '0', cum_quote: '0', estimated: false },
    ],
  };

  it('native asks are the inverted bids, the cumulative amount is KAS, the inverted book is uncrossed with a mid between the touches', () => {
    const f = depthFromViewFlipped(dv, 8)!;
    expect(f.bids.map((p) => p.price)).toEqual([40, 20]);
    expect(f.asks.map((p) => p.price)).toEqual([50, 100]);
    // 7 tokens at 0.025 KAS = 0.175 KAS, then 3 at 0.05 = 0.15 more
    expect(f.bids[0]!.cum).toBeCloseTo(0.175, 12);
    expect(f.bids[1]!.cum).toBeCloseTo(0.325, 12);
    expect(f.mid).toBe(45);
    expect(f.bids[0]!.price).toBeLessThan(f.asks[0]!.price);
    expect(f.bidTotal).toBe(32_500_000n);
    // the native series is untouched
    expect(depthFromView(dv, 8)!.bids.map((p) => p.price)).toEqual([0.02, 0.01]);
  });

  it('the book fallback gives the same inverted series', () => {
    const m = buildBookModel(view(), { depth: 60, scale: S });
    const f = depthFromBookFlipped(m, 8, 100_000_000n);
    expect(f.bids.map((p) => p.price)).toEqual([40, 20]);
    expect(f.asks.map((p) => p.price)).toEqual([50, 100]);
    expect(f.bids[1]!.cum).toBeCloseTo(0.325, 12);
    expect(f.mid).toBe(45);
  });
});

describe('the ticket: the order is the same whichever way it is displayed', () => {
  // the form is NATIVE (KAS per token, token amounts); an inverted display only converts what is typed and swaps the side labels
  const priceFieldIds = (f: TicketForm): string[] => {
    const l = layoutOf(f);
    return [...l.main, ...l.advanced].filter((id) => FIELDS[id]?.kind === 'price');
  };
  /** the form after the user typed, in an inverted ticket, the inverse of every native price of `f` */
  const viaInvertedInput = (f: TicketForm): TicketForm => {
    const values = { ...f.values };
    for (const id of priceFieldIds(f)) {
      if ((values[id] ?? '') === '') continue;
      const typed = nativeToInvertedPriceText(values[id]!);
      values[id] = invertedToNativePriceText(typed, safeRounding(sideOfField(f, id)) as 'up' | 'down');
    }
    return { ...f, values };
  };
  // prices whose inverse is a finite decimal: what a user types in an inverted box is then exactly the inverse, so the order is bit-identical
  const NICE = ['2.5', '4', '2', '5', '1.25', '8', '0.5'];
  const nice = (f: TicketForm): TicketForm => {
    const values = { ...f.values };
    priceFieldIds(f).forEach((id, i) => {
      if ((values[id] ?? '') !== '') values[id] = NICE[i % NICE.length]!;
    });
    return { ...f, values };
  };

  it.each(CASES)('%s: typing the inverse of every price builds the identical intent', (_name, type, side, values) => {
    const f = nice(form(type, side, values));
    const native = buildIntent(f, CTX);
    const flipped = buildIntent(viaInvertedInput(f), CTX);
    expect(native.errors).toEqual(flipped.errors);
    expect(json(flipped.intent)).toBe(json(native.intent));
  });

  it.each(CASES)('%s: whatever is typed, the native price is the exact inverse rounded the safe way (a sell never below it, a buy never above) by less than a sompi per token', (_name, type, side, values) => {
    const f = form(type, side, values);
    for (const id of priceFieldIds(f)) {
      if ((f.values[id] ?? '') === '') continue;
      const typed = nativeToInvertedPriceText(f.values[id]!); // what a user would type: the inverse to 8 significant digits
      const sell = sideOfField(f, id) === 'sell';
      const native = parseDecimal(invertedToNativePriceText(typed, safeRounding(sideOfField(f, id)) as 'up' | 'down'))!;
      const t = parseDecimal(typed)!;
      const exact = { num: t.den, den: t.num }; // 1 / typed
      const c = cmpRational(native, exact);
      expect(sell ? c >= 0 : c <= 0, `${type}/${id}`).toBe(true);
      const diff = native.num * exact.den - exact.num * native.den;
      expect((diff < 0n ? -diff : diff) * 100_000_000n < native.den * exact.den, `${type}/${id} within 1 sompi`).toBe(true);
    }
  });

  it('the side buttons are the shown pair\'s: Buy in KAS/TOKEN is a sale of the token, and flipping twice is the identity', () => {
    const f = form('limit', 'sell', { amount: '5', price: '2.5' });
    // an inverted ticket whose shown side is "buy" holds the native side "sell"
    expect(oppositeSide('buy')).toBe(f.side);
    const toggled = setSide(setSide(f, 'buy'), oppositeSide('buy'));
    expect(toggled.side).toBe('sell');
    // the native intent a shown Buy at 0.4 builds is the sell at 2.5 a native ticket builds
    const shownBuy = viaInvertedInput(setSide(f, oppositeSide('buy')));
    expect(json(buildIntent(shownBuy, CTX).intent)).toBe(json(buildIntent(f, CTX).intent));
    expect((buildIntent(shownBuy, CTX).intent as { side: string }).side).toBe('sell');
  });
});

describe('grouping of the inverted book', () => {
  // 8 decimals, scale 1e8: 2_510_000 sompi per token = 0.0251 KAS per token = 39.8406 tokens per KAS
  const c = { decimals: 8, scale: S, dp: 3 };
  const lvl = (price: bigint, tokens = 1n) => ({ price, amount: tokens * S, orders: 1, estimated: false });

  it('no grouping at 1x', () => {
    expect(invertedRegrouper({ ...c, mult: 1n })).toBeUndefined();
  });

  it('displayed bids (native asks) round DOWN on the inverted price, the bucket keeps its exact inverted price and a native price that still reaches all of it', () => {
    const regroup = invertedRegrouper({ ...c, mult: 100n })!; // step 0.1 tokens per KAS
    // 39.8406, 39.8089 -> bucket 39.8; 39.7614 -> bucket 39.7
    const out = regroup([lvl(2_510_000n, 2n), lvl(2_512_000n, 3n), lvl(2_515_000n)], 'ask').sort((a, b) => (a.price < b.price ? -1 : 1));
    expect(out).toHaveLength(2);
    const b398 = out.find((l) => l.amount === 5n * S)!;
    expect(ratToNumber(b398.inv!)).toBeCloseTo(39.8, 10);
    expect(b398.orders).toBe(2);
    // the buy a click prefills must be at or above the highest native price of the bucket (2_512_000) and still be at 39.8 tokens per KAS or better
    expect(b398.price).toBeGreaterThanOrEqual(2_512_000n);
    expect(ratToNumber(tokensPerKas(b398.price, 8, 100_000_000n)!)).toBeLessThanOrEqual(39.8 + 1e-9);
    const b397 = out.find((l) => l.amount === S)!;
    expect(ratToNumber(b397.inv!)).toBeCloseTo(39.7, 10);
  });

  it('displayed asks (native bids) round UP; the native price of the bucket is at or below its lowest native bid', () => {
    const regroup = invertedRegrouper({ ...c, mult: 100n })!;
    // 40.0 (2_500_000) and 39.8406 (2_510_000) as native BIDS: inverted 40.0 -> bucket 40.0, 39.8406 -> bucket 39.9
    const out = regroup([lvl(2_500_000n), lvl(2_510_000n)], 'bid');
    expect(out).toHaveLength(2);
    const up = out.find((l) => ratToNumber(l.inv!) < 39.95)!;
    expect(ratToNumber(up.inv!)).toBeCloseTo(39.9, 10);
    // a sell at the bucket's native price reaches this bid: its native price is not above the bid's (2_510_000)
    expect(up.price).toBeLessThanOrEqual(2_510_000n);
  });

  it('the grouped book keeps every base unit and displays the bucket prices; a click still identifies a native level', () => {
    const regroup = invertedRegrouper({ ...c, mult: 10n })!;
    const m = buildBookModel(view(), { regroup, scale: S });
    const v = flippedBookView(m, { ...CTX_BOOK, dp: 3 });
    const sum = (rows: { amount: bigint }[]) => rows.reduce((a, r) => a + r.amount, 0n);
    expect(sum(v.bids)).toBe(10n * S); // the native asks: 7 + 3 tokens
    expect(sum(v.asks)).toBe(10n * S);
    for (const r of [...v.bids, ...v.asks]) expect(r.nativePrice > 0n).toBe(true);
    // the best displayed bid is the displayed bucket of the best native ask and its text is the bucket's exact inverted price
    expect(v.bids[0]!.priceText).toBe('40.000');
  });

  it('a price below one step is not rounded to zero', () => {
    const regroup = invertedRegrouper({ decimals: 8, scale: S, dp: 1, mult: 1000n })!; // step 100 tokens per KAS
    const out = regroup([lvl(2_500_000n)], 'ask'); // 40 tokens per KAS < 100
    expect(out).toHaveLength(1);
    expect(out[0]!.price).toBe(2_500_000n);
  });
});
