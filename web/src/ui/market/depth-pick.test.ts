import { describe, expect, it } from 'vitest';
import { pairDepthPick, tokenDepthPick } from './depth-pick';

const NATIVE = { inverted: false, decimals: 2, scale: 100n, tick: 1n };

describe('tokenDepthPick', () => {
  it('an ask is a buy, a bid a sell, the price in sompi per scale units', () => {
    // 0.025 KAS per token = 2 500 000 sompi per whole token (scale 10^decimals: the same per `scale` units)
    expect(tokenDepthPick('ask', 0.025, NATIVE)).toEqual({ side: 'buy', price: 2_500_000n });
    expect(tokenDepthPick('bid', 0.025, NATIVE)).toEqual({ side: 'sell', price: 2_500_000n });
  });
  it('rounds a buy up and a sell down to the tick', () => {
    const c = { ...NATIVE, tick: 1000n };
    expect(tokenDepthPick('ask', 0.0250004, c)!.price).toBe(2_501_000n); // 2 500 400 up to the next multiple
    expect(tokenDepthPick('bid', 0.0250004, c)!.price).toBe(2_500_000n);
    expect(tokenDepthPick('bid', 0.025, c)!.price).toBe(2_500_000n); // already on a step: unchanged
  });
  it('never rounds to zero: a price under a step is one step', () => {
    expect(tokenDepthPick('bid', 1e-9, { ...NATIVE, tick: 1000n })!.price).toBe(1000n);
  });
  it('a flipped market: the axis is tokens per KAS, the order is native, and the chart\'s bid side is the native ask side', () => {
    const c = { ...NATIVE, inverted: true };
    // 40 tokens per KAS = 0.025 KAS per token
    expect(tokenDepthPick('bid', 40, c)).toEqual({ side: 'buy', price: 2_500_000n });
    expect(tokenDepthPick('ask', 40, c)).toEqual({ side: 'sell', price: 2_500_000n });
  });
  it('a scale other than the whole token (decimals above 9: a state price is per `scale` units)', () => {
    // 1 KAS per token with 12 decimals and scale 10^9: 1e8 sompi per 10^12 base units = 1e5 sompi per 10^9 base units
    expect(tokenDepthPick('ask', 1, { inverted: false, decimals: 12, scale: 10n ** 9n, tick: 1n })!.price).toBe(100_000n);
  });
  it('no price, no pick', () => {
    expect(tokenDepthPick('ask', 0, NATIVE)).toBeNull();
    expect(tokenDepthPick('ask', NaN, NATIVE)).toBeNull();
    expect(tokenDepthPick('ask', -1, NATIVE)).toBeNull();
  });
});

describe('pairDepthPick', () => {
  it('whole QUOTE per whole BASE to QUOTE base units per scale(BASE) base units', () => {
    // BASE 2 decimals (scale 100), QUOTE 6 decimals: 1.5 QUOTE per BASE = 1 500 000 QUOTE units per whole BASE
    expect(pairDepthPick('ask', 1.5, { decimals: 2, scale: 100n }, { decimals: 6 })).toEqual({ side: 'buy', price: 1_500_000n });
    expect(pairDepthPick('bid', 1.5, { decimals: 2, scale: 100n }, { decimals: 6 })).toEqual({ side: 'sell', price: 1_500_000n });
    expect(pairDepthPick('ask', 1.5000004, { decimals: 2, scale: 100n }, { decimals: 6 })!.price).toBe(1_500_001n);
    expect(pairDepthPick('bid', 1.5000004, { decimals: 2, scale: 100n }, { decimals: 6 })!.price).toBe(1_500_000n);
  });
});
