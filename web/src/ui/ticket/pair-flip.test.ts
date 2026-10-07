// The flip of a pair page carries the order across: A/B Buy N A at p is B/A Sell N x p B at 1/p (pair-flip.ts).
import { describe, expect, it } from 'vitest';
import { buildIntent, initialForm } from './form-state';
import { flipPairForm, rememberPairForm, requestPairFlip, takePairFlip } from './pair-flip';
import { form } from './ticket-fixtures';

// A = EXKCC (8 decimals), B = EXUSD (6 decimals)
const DEC = { base: 8, quote: 6 };

describe('flipPairForm', () => {
  it('Buy 2 A at 0.0515 B per A is Sell 0.103 B at 1/0.0515 A per B', () => {
    const f = flipPairForm(form('limit', 'buy', { amount: '2', price: '0.0515', tip: '0.001' }), DEC);
    expect(f.side).toBe('sell');
    expect(f.type).toBe('limit');
    expect(f.values.amount).toBe('0.103');
    // 1 / 0.0515 = 19.4174757281..., at most A's 8 decimals
    expect(f.values.price).toBe('19.417476');
    // a tip of 0.001 KAS per A is 0.001 / 0.0515 KAS per B
    expect(f.values.tip).toBe('0.01941748');
    // and it builds on B/A (B = 6 decimals, prices in A: 8 decimals)
    const r = buildIntent(f, { decimals: 6, scale: 1_000_000n, tick: 1n, quoteDecimals: 8 });
    expect(r.errors).toEqual([]);
    expect((r.intent as { amount: bigint }).amount).toBe(103_000n);
  });

  it('Sell turns into Buy; the amount rounds down to B; an empty price leaves the amount out', () => {
    const f = flipPairForm(form('limit', 'sell', { amount: '1', price: '0.0333333' }), DEC);
    expect(f.side).toBe('buy');
    expect(f.values.amount).toBe('0.033333');
    const g = flipPairForm(form('limit', 'sell', { amount: '1' }), DEC);
    expect(g.values.amount ?? '').toBe('');
  });

  it('TWAP and DCA are each other seen from the other side; close becomes a market buy', () => {
    expect(flipPairForm(form('twap', 'sell', { amount: '10', sliceAmount: '1', interval: '5', price: '0.05' }), DEC)).toMatchObject({ type: 'dca', side: 'buy' });
    expect(flipPairForm(form('dca', 'buy', { amount: '10', sliceAmount: '1', interval: '5', price: '0.05' }), DEC)).toMatchObject({ type: 'twap', side: 'sell' });
    expect(flipPairForm(form('close', 'sell', {}), DEC)).toMatchObject({ type: 'market', side: 'buy' });
  });

  it('every price field is inverted; price distances and exit minimum fills return to their defaults', () => {
    const f = flipPairForm(
      form('ifd', 'buy', { amount: '4', price: '0.05', 'exit.kind': 'stop', 'exit.stop': '0.04', 'exit.trail': 'true', 'exit.trail.step': '0.001', 'exit.trail.gap': '0.002', 'exit.minFill': '1', lifetime: 'day' }),
      DEC,
    );
    expect(f.side).toBe('sell');
    expect(f.values.price).toBe('20');
    expect(f.values['exit.stop']).toBe('25');
    expect(f.values['exit.trail.step']).toBeUndefined();
    expect(f.values['exit.trail.gap']).toBeUndefined();
    expect(f.values['exit.minFill']).toBeUndefined();
    expect(f.values.lifetime).toBe('day');
    expect(f.values.amount).toBe('0.2');
  });

  it('the hand-over is taken once, only by the pair the flip leads to', () => {
    rememberPairForm('aa', 'bb', form('limit', 'buy', { amount: '2', price: '0.0515' }));
    requestPairFlip('aa', 'bb');
    expect(takePairFlip('cc', 'dd', DEC)).toBeNull();
    requestPairFlip('aa', 'bb');
    expect(takePairFlip('bb', 'aa', DEC)).toMatchObject({ side: 'sell', values: { amount: '0.103' } });
    expect(takePairFlip('bb', 'aa', DEC)).toBeNull();
    expect(initialForm().side).toBe('sell');
  });
});
