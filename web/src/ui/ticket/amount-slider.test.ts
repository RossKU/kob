// The amount slider: percent of Max <-> amount (base units, any decimals), the minimum fill, and a buy's Max found by the planner search.
import { describe, expect, it } from 'vitest';
import { planOrder, type Intent } from '../../kob/plan';
import { errors, type PlanEnv } from '../../kob/plan-types';
import { PAGE_STEP, SLIDER_POINTS, SNAP_RANGE, amountPercent, percentAmount, pickAmount, picked, pointEnabled, snapPct, trackPercent } from './amount-slider';
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


describe('the thumb keeps its position through ticks (founder, 2026-10-30: it jumped between 3 % and 50 % with the amount unchanged)', () => {
  const AMOUNT = 395_800_000n; // 3.958 tokens at 8 decimals
  const settle = (max: bigint | null) => trackPercent(null, AMOUNT, max);

  it('Max moving, going unknown while the book reloads and coming back does not move it', () => {
    let mem = settle(1_000_000_000n); // 3.958 of 10 -> 40 %
    expect(mem).toEqual({ amount: AMOUNT, pct: 40, settled: true });
    // ticks: Max a little different each time, a transient empty book (no Max), Max wildly different for a moment, then back
    for (const max of [1_000_000_000n, 980_000_000n, null, 0n, null, 8_000_000n, 1200_000_000n, null, 1_000_000_000n, 970_000_000n]) {
      mem = trackPercent(mem, AMOUNT, max);
      expect(mem.pct, `Max ${max}`).toBe(40);
    }
  });

  it('a new amount is converted once, against the Max known then, and keeps that position', () => {
    let mem = settle(1_000_000_000n);
    const typed = 500_000_000n; // 5 of 10 -> 50 %
    mem = trackPercent(mem, typed, 1_000_000_000n);
    expect(mem.pct).toBe(50);
    for (const max of [null, 200_000_000n, 600_000_000n, null]) expect(trackPercent(mem, typed, max).pct).toBe(50);
    // emptied: 0 %, whatever Max is
    expect(trackPercent(mem, null, 1_000_000_000n).pct).toBe(0);
    expect(trackPercent(mem, 0n, null).pct).toBe(0);
  });

  it('an amount typed while Max is unknown leaves the thumb where it was, and converts once Max is known', () => {
    const mem = trackPercent(settle(1_000_000_000n), 200_000_000n, null);
    expect(mem).toEqual({ amount: 200_000_000n, pct: 40, settled: false });
    // still unknown: unchanged; known: converted once (2 of 10 -> 20 %) and fixed from then on
    expect(trackPercent(mem, 200_000_000n, null)).toEqual(mem);
    const known = trackPercent(mem, 200_000_000n, 1_000_000_000n);
    expect(known).toEqual({ amount: 200_000_000n, pct: 20, settled: true });
    expect(trackPercent(known, 200_000_000n, 400_000_000n).pct).toBe(20);
  });

  it('a drag sets the position directly, and the amount it produced keeps it', () => {
    const mem = picked(30, 300_000_000n);
    for (const max of [1_000_000_000n, null, 5_000_000_000n]) expect(trackPercent(mem, 300_000_000n, max).pct).toBe(30);
    expect(picked(0, null)).toEqual({ amount: null, pct: 0, settled: true });
    expect(trackPercent(picked(0, null), null, null).pct).toBe(0);
  });

  it('with nothing in the box it reads 0 % before there is a Max', () => {
    expect(trackPercent(null, null, null)).toEqual({ amount: null, pct: 0, settled: true });
  });
});
