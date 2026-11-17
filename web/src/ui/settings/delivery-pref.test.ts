// The wallet's delivery settings in localStorage: the default of buy orders and the Top-up receive threshold per (wallet key, token, extension
// commitment), stored once and reused until it is reset (bid-delivery-spec §10).
import { beforeEach, describe, expect, it } from 'vitest';
import {
  DELIVERY_PREF_KEY, listThresholds, readDeliveryPref, readThreshold, rememberThreshold, resetDeliveryPrefMemory, resetThreshold, thresholdKey, writeDeliveryPref,
} from './delivery-pref';

class MemStore {
  m = new Map<string, string>();
  get length(): number {
    return this.m.size;
  }
  key(i: number): string | null {
    return [...this.m.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.m.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.m.set(k, v);
  }
  removeItem(k: string): void {
    this.m.delete(k);
  }
}

const MAKER = 'aa'.repeat(32);
const TOKEN = 'bb'.repeat(32);
const EXT = 'cc'.repeat(32);
const key = { maker: MAKER, tokenCovId: TOKEN, extensionCommitment: EXT };

describe('the default delivery', () => {
  beforeEach(resetDeliveryPrefMemory);
  it('is Each fill unless Collected or Top-up is stored; Each fill is stored as no key', () => {
    const s = new MemStore();
    expect(readDeliveryPref(s)).toBe('each');
    expect(writeDeliveryPref('topup', s)).toBe(true);
    expect(s.getItem(DELIVERY_PREF_KEY)).toBe('topup');
    expect(readDeliveryPref(s)).toBe('topup');
    writeDeliveryPref('collected', s);
    expect(readDeliveryPref(s)).toBe('collected');
    writeDeliveryPref('each', s);
    expect(s.getItem(DELIVERY_PREF_KEY)).toBeNull();
    // a stored Each fill (saved before this default) still reads as Each fill
    s.setItem(DELIVERY_PREF_KEY, 'each');
    expect(readDeliveryPref(s)).toBe('each');
    s.setItem(DELIVERY_PREF_KEY, 'bogus');
    expect(readDeliveryPref(s)).toBe('each');
    expect(readDeliveryPref(null)).toBe('each');
  });
});

describe('the Top-up receive threshold', () => {
  it('is stored at the first top-up order of a token and kept: a later value does not replace it (negative: overwriting it)', () => {
    const s = new MemStore();
    expect(readThreshold(key, s)).toBeUndefined();
    expect(rememberThreshold({ ...key, threshold: 2_000n, ticker: 'KOBT', decimals: 3 }, s)).toBe(true);
    expect(readThreshold(key, s)).toBe(2_000n);
    expect(rememberThreshold({ ...key, threshold: 9_999n, ticker: 'KOBT', decimals: 3 }, s)).toBe(false);
    expect(readThreshold(key, s)).toBe(2_000n);
    // per wallet key, per token, per extension commitment
    expect(readThreshold({ ...key, maker: 'dd'.repeat(32) }, s)).toBeUndefined();
    expect(readThreshold({ ...key, extensionCommitment: '00'.repeat(32) }, s)).toBeUndefined();
    expect(thresholdKey(key)).toBe(`kob.delivery.threshold.${MAKER}.${TOKEN}.${EXT}`);
  });

  it('changes only by a reset; a zero or unreadable value is none', () => {
    const s = new MemStore();
    rememberThreshold({ ...key, threshold: 2_000n, ticker: 'KOBT', decimals: 3 }, s);
    resetThreshold(key, s);
    expect(readThreshold(key, s)).toBeUndefined();
    expect(rememberThreshold({ ...key, threshold: 0n, ticker: 'KOBT', decimals: 3 }, s)).toBe(false);
    s.setItem(thresholdKey(key), '{"threshold":"0"}');
    expect(readThreshold(key, s)).toBeUndefined();
    s.setItem(thresholdKey(key), 'not json');
    expect(readThreshold(key, s)).toBeUndefined();
  });

  it('lists the thresholds of one wallet key, by ticker', () => {
    const s = new MemStore();
    rememberThreshold({ ...key, tokenCovId: 'ee'.repeat(32), threshold: 5n, ticker: 'ZED', decimals: 0 }, s);
    rememberThreshold({ ...key, threshold: 2_000n, ticker: 'KOBT', decimals: 3 }, s);
    rememberThreshold({ ...key, maker: 'dd'.repeat(32), threshold: 7n, ticker: 'OTHER', decimals: 0 }, s);
    s.setItem('kob.delivery.threshold.junk', '{}');
    expect(listThresholds(MAKER, s).map((r) => [r.ticker, r.threshold, r.decimals])).toEqual([['KOBT', 2_000n, 3], ['ZED', 5n, 0]]);
  });
});
