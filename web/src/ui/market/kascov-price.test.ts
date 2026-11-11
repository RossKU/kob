import { describe, expect, it } from 'vitest';
import type { Rational } from '../../kob/pair';
import { KASCOV_PRICE_DIVERGENCE_BPS, kascovPriceNote, ownReferenceKas } from './kascov-price';

/** KAS per token as a rational from a decimal text */
const kas = (n: number, d: number): Rational => ({ num: BigInt(n), den: BigInt(d) });

describe('kascovPriceNote: a third-party price more than 20% from KOB\'s own reference', () => {
  const ref = kas(1, 50); // 0.02 KAS per token

  it('says what kascov lists and how far it is, signed in whole percent, labelled as kascov\'s', () => {
    const low = kascovPriceNote(0.013, ref)!;
    expect(low.text).toBe('kascov lists 0.013 KAS (-35%)');
    expect(low.percent).toBe('-35%');
    expect(low.title).toContain('Third-party reference');
    expect(low.title).toContain('its prices come from its own order book');
    // a float price a hair below its decimal still reads whole percent
    expect(kascovPriceNote(0.0125, kas(1, 40))!.percent).toBe('-50%');
    expect(kascovPriceNote(0.05, ref)!.text).toBe('kascov lists 0.05 KAS (+150%)');
  });

  it('is nothing within 20% (the limit itself included), without a listed price, without a reference, or for a price that is not usable', () => {
    expect(KASCOV_PRICE_DIVERGENCE_BPS).toBe(2_000n);
    expect(kascovPriceNote(0.02, ref)).toBeNull();
    expect(kascovPriceNote(0.024, ref)).toBeNull(); // exactly +20%
    expect(kascovPriceNote(0.016, ref)).toBeNull(); // exactly -20%
    expect(kascovPriceNote(0.0241, ref)).not.toBeNull();
    expect(kascovPriceNote(undefined, ref)).toBeNull();
    expect(kascovPriceNote(null, ref)).toBeNull();
    expect(kascovPriceNote(0.012, null)).toBeNull();
    expect(kascovPriceNote(0.012, kas(0, 1))).toBeNull();
    for (const bad of [0, -1, NaN, Infinity]) expect(kascovPriceNote(bad, ref)).toBeNull();
  });

  it('shows the price with about five significant digits, at most 6 decimals, never a bare zero', () => {
    expect(kascovPriceNote(0.0123456789, kas(1, 100))!.price).toBe('0.012346');
    expect(kascovPriceNote(33.4567, kas(1, 1))!.price).toBe('33.457');
    expect(kascovPriceNote(0.0000004, kas(1, 1000))!.price).toBe('<0.000001');
  });
});

describe('ownReferenceKas: the last trade, else the mid of the book', () => {
  const v = { price_basis: '100000000', last: '2000000', mid: '2500000' };
  it('is the last price in KAS per whole token (8 decimals: basis 10^8), the mid without a last, null without either', () => {
    expect(ownReferenceKas(v, 8)).toEqual({ num: 1n, den: 50n });
    expect(ownReferenceKas({ ...v, last: null }, 8)).toEqual({ num: 1n, den: 40n });
    expect(ownReferenceKas({ ...v, last: null, mid: null }, 8)).toBeNull();
    expect(ownReferenceKas(null, 8)).toBeNull();
    expect(ownReferenceKas({ ...v, price_basis: '0' }, 8)).toBeNull();
  });
});
