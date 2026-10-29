// The amount slider: percent of Max <-> amount (base units, any decimals), the minimum fill, and a buy's Max found by the planner search.
import { describe, expect, it } from 'vitest';
import { planOrder, type Intent } from '../../kob/plan';
import { errors, type PlanEnv } from '../../kob/plan-types';
import { PAGE_STEP, SLIDER_POINTS, SNAP_RANGE, amountPercent, percentAmount, pickAmount, pointEnabled, snapPct } from './amount-slider';
import { CTX, ticketEnv } from './ticket-fixtures';
import { formatAmount } from './form-state';
import { MAX_KAS_RESERVE, maxBuyAmount } from './max-amount';

const KAS = 100_000_000n;

describe('percent of Max -> amount', () => {
  it('rounds down to a whole base unit whatever the decimals; 100 % is exactly Max', () => {
    // 0 decimals: 3 whole tokens
    expect(SLIDER_POINTS.map((p) => percentAmount(3n, p))).toEqual([0n, 0n, 1n, 2n, 3n]);
    expect(formatAmount(percentAmount(3n, 50), 0)).toBe('1');
    // 8 decimals: 1.23456789 tokens
    const m8 = 123_456_789n;
    expect(SLIDER_POINTS.map((p) => percentAmount(m8, p))).toEqual([0n, 30_864_197n, 61_728_394n, 92_592_591n, m8]);
    expect(formatAmount(percentAmount(m8, 50), 8)).toBe('0.61728394');
    // 18 decimals: 1.5 tokens plus one base unit
    const m18 = 1_500_000_000_000_000_001n;
    expect(percentAmount(m18, 50)).toBe(750_000_000_000_000_000n);
    expect(formatAmount(percentAmount(m18, 50), 18)).toBe('0.75');
    expect(percentAmount(m18, 100)).toBe(m18);
    expect(percentAmount(m18, 33)).toBe(495_000_000_000_000_000n);
    // nothing to place
    expect(percentAmount(0n, 50)).toBe(0n);
  });

  it('a typed amount moves the slider to its nearest whole percent (above Max reads 100)', () => {
    expect(amountPercent(null, 200n)).toBe(0);
    expect(amountPercent(100n, 200n)).toBe(50);
    expect(amountPercent(66n, 200n)).toBe(33);
    expect(amountPercent(67n, 200n)).toBe(34);
    expect(amountPercent(999n, 200n)).toBe(100);
    expect(amountPercent(5n, null)).toBeNull();
    expect(amountPercent(5n, 0n)).toBeNull();
    // every slider step comes back to itself
    for (const max of [3n, 123_456_789n, 1_500_000_000_000_000_001n]) {
      for (let p = 0; p <= 100; p++) {
        const a = pickAmount(p, max, null);
        if (a !== null && max >= 100n) expect(amountPercent(a, max), `${max} ${p}`).toBe(p);
      }
    }
  });
});

describe('the minimum fill', () => {
  it('a point below the minimum fill is disabled; a drag below it sets the minimum fill; 0 % empties the box', () => {
    const max = 10n * 100_000_000n;
    const min = 3n * 100_000_000n; // 30 % of Max
    expect(SLIDER_POINTS.map((p) => pointEnabled(p, max, min))).toEqual([true, false, true, true, true]);
    expect(pickAmount(0, max, min)).toBeNull();
    expect(pickAmount(10, max, min)).toBe(min);
    expect(pickAmount(25, max, min)).toBe(min);
    expect(pickAmount(30, max, min)).toBe(min);
    expect(pickAmount(31, max, min)).toBe(310_000_000n);
    // a minimum above Max: nothing fits
    expect(pickAmount(50, 2n, 3n)).toBeNull();
    expect(pointEnabled(100, 2n, 3n)).toBe(false);
    // no Max: every point is off
    expect(SLIDER_POINTS.map((p) => pointEnabled(p, null, null))).toEqual([false, false, false, false, false]);
    // a tiny Max of a 0-decimal token: 25 % is 0 base units, disabled
    expect(SLIDER_POINTS.map((p) => pointEnabled(p, 3n, null))).toEqual([true, false, true, true, true]);
  });
});

describe('a buy: Max from the planner search, the points as shares of it', () => {
  const PRICE = 230_000_000n;
  const limit = (amount: bigint): Intent => ({ type: 'limit', side: 'buy', amount, price: PRICE });
  const balanceOf = (env: PlanEnv) => env.funding.reduce((a, u) => a + BigInt(u.amount), 0n);

  it('100 % is the searched Max; 50 % is half of it rounded down and plans', () => {
    const env = ticketEnv({ funding: [40n * KAS] });
    const balance = balanceOf(env);
    const max = maxBuyAmount({ upper: (balance * CTX.scale) / PRICE + 1n, conservative: 0n, kasBalance: balance, planAt: (n) => planOrder(env, limit(n)) });
    expect(max).toBeGreaterThan(0n);
    expect(pickAmount(100, max, null)).toBe(max);
    const half = pickAmount(50, max, null)!;
    expect(half).toBe(max / 2n);
    const p = planOrder(env, limit(half));
    expect(errors(p)).toEqual([]);
    expect(p.disclosure!.kasLocked + (p.disclosure!.fee ?? 0n) + MAX_KAS_RESERVE).toBeLessThan(balance);
  });
});

describe('the magnetic snap of a dragged thumb', () => {
  it('snaps to a tick within 3 %, leaves the rest alone, and keeps the ticks themselves', () => {
    expect(snapPct(27)).toBe(25);
    expect(snapPct(22)).toBe(25);
    expect(snapPct(21)).toBe(21);
    expect(snapPct(28)).toBe(25);
    expect(snapPct(29)).toBe(29);
    expect(snapPct(73)).toBe(75);
    expect(snapPct(98)).toBe(100);
    expect(snapPct(2)).toBe(0);
    expect(snapPct(40)).toBe(40);
    for (const p of SLIDER_POINTS) expect(snapPct(p)).toBe(p);
    expect(SNAP_RANGE).toBe(3);
    expect(PAGE_STEP).toBe(25);
  });
});
