// The executor tip of the ticket's "Tip executors" option (kob/fill-tip.ts): the measured match fee x 1.25, per token over the minimum fill.
import { describe, expect, it } from 'vitest';
import { errors } from './plan-types';
import { planOrder } from './plan';
import { TIP_FEE_MARGIN_PCT, executorTip, measureMatchFee, tipForFee } from './fill-tip';
import { KAS, TOK, makeEnv, marketKron } from '../testing/fixtures';

const empty = { asks: [], bids: [] };

describe('tip conversion', () => {
  it('per token over the whole amount: one fill of the whole order earns the fee x 1.25', () => {
    // a 0.012 KAS match, an order of 4 tokens of 1000 base units
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
      // over the whole 20 tokens, not the 4-token minimum fill: the tip in all is about one match fee x 1.25
      expect(t.tip).toBe(tipForFee(fee!, 20n * TOK, TOK));
      expect(t.total).toBe((20n * TOK * t.tip + TOK - 1n) / TOK);
      expect(t.total).toBeGreaterThanOrEqual((fee! * TIP_FEE_MARGIN_PCT) / 100n);
      expect(t.total).toBeLessThanOrEqual((fee! * TIP_FEE_MARGIN_PCT) / 100n + 20n);
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

describe('the founder case (2026-11-17)', () => {
  it('a 306 KAS buy of about 8.3 tokens: the tip in all is about 1.25 x one fill, not 1.25 x per minimum fill', () => {
    // a 0.0172 KAS match; 8.3 tokens; the old rule (per 0.27-token minimum fill) asked about 30 x as much
    const fee = 1_720_000n;
    const amount = 8_300n;
    const tip = tipForFee(fee, amount, TOK);
    const total = (amount * tip + TOK - 1n) / TOK;
    expect(total).toBeGreaterThanOrEqual(2_150_000n);
    expect(total).toBeLessThan(2_160_000n);
    expect(tipForFee(fee, 270n, TOK) * amount / TOK).toBeGreaterThan(25n * total);
  });
});
