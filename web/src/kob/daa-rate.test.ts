// The DAA rate behind expiries and timed activations (matcher.md 10.10): measured over an hour or more, never over seconds. Block arrivals
// at 10 BPS are Poisson: 20 s of them carry a +-7 % (1 sigma) rate error, which a day order or a 90-day GTD date would extrapolate.
// The wallet's own samples give no rate before they span an hour (the nominal 10 DAA/s is used), and a user's date converts on the side
// of the user's intent (`unixToDaaBound`).
import { describe, expect, it } from 'vitest';
import { DaaRateEstimator } from '../data/node-rpc';
import { clampRate, expiryFor, unixToDaaBound } from './daa';

const TRUE_RATE = 10; // DAA per second over the long run (difficulty adjustment holds the block rate)

// deterministic PRNG (mulberry32)
function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The wallet's estimate after `spanS` seconds of 10 s polls of a Poisson 10 DAA/s chain. */
function measuredRate(spanS: number, r: () => number): number | null {
  const est = new DaaRateEstimator();
  let daa = 1_000_000n;
  let next = -Math.log(1 - r()) / TRUE_RATE;
  for (let s = 0; s <= spanS; s += 10) {
    while (next <= s) {
      daa += 1n;
      next += -Math.log(1 - r()) / TRUE_RATE;
    }
    est.add(daa, s * 1000);
  }
  return est.rateMilli();
}

/** kob_protocol::defaults::day_order (estimate + 1 % margin), as the wasm computes it. */
function dayOrderExpiry(d0: bigint, delta: bigint, rateMilli: number | null): bigint {
  const r = BigInt(clampRate(rateMilli));
  return d0 + (delta * r + 999n) / 1000n + (delta * r + 99_999n) / 100_000n;
}

describe('DAA rate used for expiries', () => {
  it('a rate from 20-30 s of samples is not used: the expiries convert at the nominal rate, within 1 % of the true one', () => {
    const r = rng(7);
    let off = 0;
    const N = 2000;
    for (let i = 0; i < N; i++) {
      const raw = measuredRate(20 + 10 * (i % 2), r);
      expect(raw).toBeNull();
      if (Math.abs(clampRate(raw) / 10_000 - 1) > 0.01) off++;
    }
    expect(off / N).toBeLessThan(0.05);
  });

  it('a rate from an hour of samples is off by 0.5 % (1 sigma), never near the clamp', () => {
    const r = rng(11);
    let off = 0;
    let worst = 0;
    const N = 40;
    for (let i = 0; i < N; i++) {
      const raw = measuredRate(3_600, r);
      expect(raw).not.toBeNull();
      const err = Math.abs(clampRate(raw) / 10_000 - 1);
      worst = Math.max(worst, err);
      if (err > 0.01) off++;
    }
    expect(off / N).toBeLessThan(0.1);
    expect(worst).toBeLessThan(0.025);
  });

  it('a day order placed at 00:00:01 UTC is not refundable more than 15 min before 00:00 UTC', () => {
    // 190 DAA in 20 s (-5 %, 0.7 sigma): no rate yet, the nominal one is used
    const est = new DaaRateEstimator();
    est.add(1_000_000n, 0);
    est.add(1_000_095n, 10_000);
    est.add(1_000_190n, 20_000);
    const rate = est.rateMilli();
    expect(rate).toBeNull();
    const delta = 86_399n;
    const expiry = dayOrderExpiry(1_000_190n, delta, rate);
    const refundOpensS = Number(expiry - 1_000_190n) / TRUE_RATE;
    expect(refundOpensS).toBeGreaterThanOrEqual(Number(delta) - 15 * 60);
  });

  it('a GTD date 89 days ahead never converts past the date by more than the true-rate drift', () => {
    const t0 = 1_790_640_001n;
    const at = t0 + 89n * 86_400n;
    // the measured rate is the true one (an hour or more): the expiry lands on the date
    const g = expiryFor('gtd', { daa: 1_000_000n, unixSeconds: t0, rateMilli: 10_000 }, { at });
    expect(Math.abs(Number(g.expiryDaa - 1_000_000n) / TRUE_RATE - Number(at - t0))).toBeLessThanOrEqual(6 * 3600);
    // a high (wrong) rate cannot move the end past the date: the nominal bound is used
    const high = expiryFor('gtd', { daa: 1_000_000n, unixSeconds: t0, rateMilli: 10_500 }, { at });
    expect(high.expiryDaa).toBe(g.expiryDaa);
    // a timed activation never opens before the date: the later bound
    const low = { daa: 1_000_000n, unixSeconds: t0, rateMilli: 9_500 };
    expect(unixToDaaBound(low, at, 'start')).toBe(g.expiryDaa);
    expect(unixToDaaBound(low, at, 'end')).toBeLessThan(g.expiryDaa);
  });
});
