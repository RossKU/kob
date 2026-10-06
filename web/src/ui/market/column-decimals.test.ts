// Fixed decimals per column: every row of a numeric column shows the SAME number of decimals (trailing zeros kept), chosen once per market / view, so
// the decimal points line up (right-aligned, tabular figures). Covers the token order book (native and inverted), the trade tape (native and
// inverted) and the pair (implied) book with the values of the TBTC/TUSD pair-book screenshot.
import { describe, expect, it } from 'vitest';
import type { PairBookView, PairLevelView } from '../../data/indexer-types';
import type { PairToken } from '../../kob/pair';
import { buildBookModel } from './book-model';
import { buildPairBook } from './pair-model';
import { plainPrice } from './PairBook';
import { flippedBookView, nativeBookView, tapeColumns } from './orientation';
import type { Tape, TapeRow } from './market-model';

const decimalsOf = (text: string): number => (text.includes('.') ? text.split('.')[1]!.length : 0);
const sameDecimals = (texts: string[]): boolean => new Set(texts.map(decimalsOf)).size === 1;

// ---- token order book: 8 decimals, scale 1e8 (prices per whole token), tick = 100_000 sompi per token = 0.001 KAS per token
const S = 100_000_000n;
const CTX = { name: 'TBTC', decimals: 8, scale: S, tick: 100_000n, dp: 3, labels: { price: 'p', size: 's', total: 't' } };
const lv = (price: string, amount: bigint) => ({ price, amount: amount.toString(), amount_estimated: false, orders: 1, scale: Number(S) });
// amounts in base units: 0.007, 0.03, 1.2 tokens on the asks; 0.004, 0.025, 123.456 on the bids
const BOOK = {
  asks: [lv('2500000000', 700_000n), lv('2500100000', 3_000_000n), lv('2600000000', 120_000_000n)],
  bids: [lv('2400000000', 400_000n), lv('2399900000', 2_500_000n), lv('2300000000', 12_345_600_000n)],
};
const model = () => buildBookModel(BOOK, { depth: 12, scale: S });

describe('token book: fixed decimals per column', () => {
  const v = nativeBookView(model(), CTX);
  const rows = [...v.asks, ...v.bids];

  it('prices: the tick fixes the decimals (3), trailing zeros kept', () => {
    expect(v.asks.map((r) => r.priceText)).toEqual(['26.000', '25.001', '25.000']);
    expect(v.bids.map((r) => r.priceText)).toEqual(['24.000', '23.999', '23.000']);
    expect(sameDecimals(rows.map((r) => r.priceText))).toBe(true);
  });

  it('sizes: the finest amount shown fixes the decimals (3), thousands grouped', () => {
    expect(v.asks.map((r) => r.sizeText)).toEqual(['1.200', '0.030', '0.007']);
    expect(v.bids.map((r) => r.sizeText)).toEqual(['0.004', '0.025', '123.456']);
    expect(sameDecimals(rows.map((r) => r.sizeText))).toBe(true);
  });

  it('totals: one fixed KAS precision for the column, not compacted', () => {
    expect(sameDecimals(rows.map((r) => r.totalText))).toBe(true);
    expect(v.asks.at(-1)!.totalText).toBe('0.175'); // 0.007 token x 25 KAS; the tick (0.001 KAS) fixes 3 decimals
    expect(rows.every((r) => !/[KMB]$/.test(r.totalText))).toBe(true);
  });

  it('inverted: tokens per KAS at one fixed precision, sizes in KAS from the tick, totals in tokens from the finest cumulative amount', () => {
    const f = flippedBookView(model(), { ...CTX, dp: 2 });
    const all = [...f.asks, ...f.bids];
    expect(sameDecimals(all.map((r) => r.priceText))).toBe(true);
    expect(decimalsOf(f.bids[0]!.priceText)).toBe(2);
    expect(sameDecimals(all.map((r) => r.sizeText))).toBe(true);
    expect(decimalsOf(f.bids[0]!.sizeText)).toBe(3); // tick 100_000 sompi = 3 KAS decimals
    expect(f.bids[0]!.sizeText).toBe('0.175');
    expect(sameDecimals(all.map((r) => r.totalText))).toBe(true);
    expect(decimalsOf(f.bids[0]!.totalText)).toBe(3); // the finest cumulative amount: 0.007 token
    expect(f.bids[0]!.totalText).toBe('0.007');
  });

  it('whole-token amounts show integer sizes in both views (no decimals to keep)', () => {
    const ctx = { ...CTX, tick: 100_000n, dp: 2 };
    const whole = buildBookModel({ asks: [lv('2500000000', 7n * S), lv('2500100000', 30n * S), lv('2600000000', 1200n * S)], bids: [lv('2400000000', 4n * S)] }, { scale: S });
    const n = nativeBookView(whole, ctx);
    expect(n.asks.map((r) => r.sizeText)).toEqual(['1,200', '30', '7']);
    const f = flippedBookView(whole, ctx);
    expect(sameDecimals([...f.asks, ...f.bids].map((r) => r.sizeText))).toBe(true);
  });
});

// ---- trade tape
const trade = (id: string, price: bigint, amount: bigint, quote: bigint): TapeRow => ({ id, timeMs: null, daa: 1, side: 'buy', price, amount, quote, settled: true, fills: 1 });

describe('trade tape: fixed decimals per column', () => {
  // KAS per token 84_392.43 / 80_607.2 / 80_482: price = sompi per 1e8 base units
  const tape: Tape = {
    basis: 100_000_000n,
    source: 'trades',
    rows: [trade('1', 8_439_243_000_000n, 600n, 50_600n), trade('2', 8_060_720_000_000n, 1130n, 91_100n), trade('3', 8_048_200_000_000n, 2365n, 190_300n), trade('4', 8_048_200_000_000n, 21_240n, 1_709_500n)],
  };

  it('native: one price precision and one size precision for every row (the finest amount shown)', () => {
    const c = tapeColumns(tape, { decimals: 8, dp: 2, inverted: false });
    expect(c.map((r) => r.price)).toEqual(['84,392.43', '80,607.20', '80,482.00', '80,482.00']);
    expect(c.map((r) => r.size)).toEqual(['0.00000600', '0.00001130', '0.00002365', '0.00021240']);
  });

  it('native: coarse amounts keep a short size column (the finest row decides)', () => {
    const t2: Tape = { ...tape, rows: [trade('1', 8_439_243_000_000n, 1_000_000n, 0n), trade('2', 8_439_243_000_000n, 25_000_000n, 0n)] };
    const c = tapeColumns(t2, { decimals: 8, dp: 2, inverted: false });
    expect(c.map((r) => r.size)).toEqual(['0.01', '0.25']);
  });

  it('inverted: tokens per KAS and the KAS size, each at one fixed precision', () => {
    const c = tapeColumns(tape, { decimals: 8, dp: 8, inverted: true, tick: 100n });
    expect(sameDecimals(c.map((r) => r.price))).toBe(true);
    expect(decimalsOf(c[0]!.price)).toBe(8);
    expect(sameDecimals(c.map((r) => r.size))).toBe(true);
    expect(decimalsOf(c[0]!.size)).toBe(6);
  });

  it('inverted: a row finer than the tick does not widen the KAS column (rounded to the tick precision)', () => {
    const fine: Tape = { ...tape, rows: [trade('1', 8_439_243_000_000n, 150_000_000n, 14_351_840_000n), trade('2', 8_439_243_000_000n, 150_000_000n, 2_392_470_001n)] };
    const c = tapeColumns(fine, { decimals: 8, dp: 4, inverted: true, tick: 10_000n });
    expect(c.map((r) => r.size)).toEqual(['143.5184', '23.9247']);
  });

  it('rows without a value show a dash and do not change the column precision', () => {
    const t2: Tape = { ...tape, rows: [trade('1', 8_439_243_000_000n, 150_000_000n, 1n), { ...trade('2', 8_439_243_000_000n, 0n, 0n), amount: null, price: null }] };
    const c = tapeColumns(t2, { decimals: 8, dp: 2, inverted: false });
    expect(c.map((r) => r.size)).toEqual(['1.5', '—']);
    expect(c[1]!.price).toBe('—');
  });
});

// ---- pair (implied) book
const TBTC: PairToken = { covenantId: 'aa'.repeat(32), ticker: 'TBTC', decimals: 8, scale: 100_000_000n };
const TUSD: PairToken = { covenantId: 'bb'.repeat(32), ticker: 'TUSD', decimals: 8, scale: 100_000_000n };
const pl = (source: 'direct' | 'route', price: string, amount: string): PairLevelView => {
  // `price` = QUOTE per whole BASE as a decimal with at most 2 fraction digits; both tokens have 8 decimals, so units per unit = the same number
  const [i, f = ''] = price.split('.');
  return { source, price_num: `${i}${f.padEnd(2, '0')}`, price_den: '100', amount, orders: 1 };
};
const pview = (asks: PairLevelView[], bids: PairLevelView[]): PairBookView => ({ base: TBTC.covenantId, quote: TUSD.covenantId, daa_score: 1, asks, bids });

describe('pair book: fixed decimals per column (the TBTC/TUSD screenshot)', () => {
  // prices that used to show as 84,392.43 / 80,607.2 / 80,482 and amounts as 0.000006 / 0.0000113 / 0.00002365 / 0.0002124
  const m = buildPairBook(pview([pl('route', '84392.43', '600'), pl('route', '85000', '1130')], [pl('route', '80607.2', '2365'), pl('direct', '80482', '21240')]), TBTC, TUSD)!;
  const rows = [...m.asks, ...m.bids];

  it('every price has the same decimals, zeros kept', () => {
    expect(m.dp).toBe(2);
    expect(m.asks.map((r) => r.priceText)).toEqual(['84,392.43', '85,000.00']);
    expect(m.bids.map((r) => r.priceText)).toEqual(['80,607.20', '80,482.00']);
  });

  it('every amount has the decimals of the finest amount shown', () => {
    expect(m.amountDp).toBe(8);
    expect(rows.map((r) => r.amountText).sort()).toEqual(['0.00000600', '0.00001130', '0.00002365', '0.00021240']);
    expect(sameDecimals(rows.map((r) => r.amountText))).toBe(true);
  });

  it('the spread and totals use fixed decimals too', () => {
    expect(m.spreadText).toBe('3,785.23');
    expect(sameDecimals(rows.map((r) => r.totalText))).toBe(true);
  });

  it('whole amounts stay whole: the precision follows the finest amount, not the token decimals', () => {
    const w = buildPairBook(pview([pl('route', '2', '300000000')], [pl('route', '1', '500000000')]), TBTC, TUSD)!;
    expect([...w.asks, ...w.bids].map((r) => r.amountText)).toEqual(['3', '5']);
  });
});

describe('pair book click prefill', () => {
  it('hands the ticket a plain number: no separators, no padding zeros', () => {
    expect(plainPrice('84,392.43')).toBe('84392.43');
    expect(plainPrice('80,482.00')).toBe('80482');
    expect(plainPrice('0.0505000')).toBe('0.0505');
    expect(plainPrice('1,200')).toBe('1200');
    expect(plainPrice('0.10')).toBe('0.1');
  });
});
