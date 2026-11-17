// The executor tip of the ticket's "Tip executors" option (kob/fill-tip.ts): the measured match fee x 1.25, per token over the minimum fill.
import { describe, expect, it } from 'vitest';
import { errors } from './plan-types';
import { planOrder } from './plan';
import { TIP_FEE_MARGIN_PCT, executorTip, measureMatchFee, tipForFee } from './fill-tip';
import { KAS, TOK, makeEnv, marketKron } from '../testing/fixtures';

const empty = { asks: [], bids: [] };

describe('tip conversion', () => {
  it('per token over the minimum fill: a minimum fill earns the fee x 1.25', () => {
    // a 0.012 KAS match, a minimum fill of 4 tokens of 1000 base units
    const tip = tipForFee(1_200_000n, 4n * TOK, TOK);
    expect(tip).toBe(375_000n);
    expect((tip * 4n * TOK) / TOK).toBe((1_200_000n * TIP_FEE_MARGIN_PCT) / 100n);
    // rounded up: never short of the fee
    expect(tipForFee(1n, 3n * TOK, TOK) * 3n).toBeGreaterThanOrEqual((1n * TIP_FEE_MARGIN_PCT) / 100n);
    expect(tipForFee(0n, TOK, TOK)).toBe(0n);
  });
});

describe('the measured match fee', () => {
  for (const side of ['buy', 'sell'] as const) {
    it(`a resting ${side}: a match of it is built and its fee measured`, () => {
      const env = makeEnv({ book: empty });
      const p = planOrder(env, { type: 'limit', side, amount: 20n * TOK, price: 250_000_000n, minFill: 4n * TOK });
      expect(errors(p)).toEqual([]);
      const fee = measureMatchFee(env, p);
      expect(fee).not.toBeNull();
      // a two-order match at the relay floor: well above one placement's floor, below 0.1 KAS
      expect(fee!).toBeGreaterThan(1_000_000n);
      expect(fee!).toBeLessThan(KAS / 10n);
      const t = executorTip(env, p)!;
      expect(t.fee).toBe(fee);
      expect(t.tip).toBe(tipForFee(fee!, 4n * TOK, TOK));
      expect(t.total).toBe((20n * TOK * t.tip + TOK - 1n) / TOK);
    });
  }

  it('a KRON token is measured too', () => {
    const env = makeEnv({ market: marketKron(), book: empty });
    const p = planOrder(env, { type: 'limit', side: 'sell', amount: 5n * TOK, price: 250_000_000n });
    expect(measureMatchFee(env, p)).not.toBeNull();
  });

  it('a higher fee rate measures a higher fee', () => {
    const lo = makeEnv({ book: empty, feeRate: 100n });
    const hi = makeEnv({ book: empty, feeRate: 400n });
    const i = { type: 'limit' as const, side: 'buy' as const, amount: 10n * TOK, price: 250_000_000n };
    expect(measureMatchFee(hi, planOrder(hi, i))!).toBeGreaterThan(measureMatchFee(lo, planOrder(lo, i))!);
  });
});
