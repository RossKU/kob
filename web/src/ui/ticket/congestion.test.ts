import { beforeEach, describe, expect, it } from 'vitest';
import { RELAY_FEE_FLOOR, isContended, parseFeeEstimate, type FeeEstimate } from '../../kob/fee-policy';
import { CONGESTION_POLL_MS, CongestionProbe, congestionDismissed, dismissCongestion, resetCongestionDismissal } from './congestion';

const est = (normal: number, priority = normal + 5, low = 100): FeeEstimate => ({ priority, normal, low });

describe('isContended: the executor\'s reading of getFeeEstimate (kob_protocol::fee_estimate::FeeEstimate::contended)', () => {
  it('the first normal bucket, rounded up, at least 10 percent above the relay floor', () => {
    // the TN10 flood of 2026-10-02: normal buckets 140 to 194, low 115, floor 100
    expect(isContended(est(194))).toBe(true);
    expect(isContended(est(140))).toBe(true);
    expect(isContended(est(110))).toBe(true);
    expect(isContended(est(109.2))).toBe(true); // ceil -> 110, as the policy rounds the rate it pays
    expect(isContended(est(109))).toBe(false);
    expect(isContended(est(100))).toBe(false); // an idle node: every bucket at the floor
    expect(isContended(est(1))).toBe(false); // below the floor
  });

  it('only the normal bucket counts', () => {
    expect(isContended({ priority: 900, normal: 100, low: 100 })).toBe(false);
    expect(isContended({ priority: 100, normal: 100, low: 900 })).toBe(false);
  });

  it('measured against the caller\'s floor, never below the relay minimum', () => {
    expect(isContended(est(105), 1n)).toBe(false);
    expect(isContended(est(110), 1n)).toBe(true);
    expect(isContended(est(164), 150n)).toBe(false);
    expect(isContended(est(165), 150n)).toBe(true);
    expect(RELAY_FEE_FLOOR).toBe(100n);
  });

  it('no estimate or an unusable one: not contended', () => {
    expect(isContended(null)).toBe(false);
    expect(isContended(undefined)).toBe(false);
    expect(isContended(est(Number.NaN))).toBe(false);
    expect(isContended(est(-5))).toBe(false);
    expect(isContended(est(Number.POSITIVE_INFINITY))).toBe(false);
  });

  it('reads the node\'s own answer (wRPC shape)', () => {
    const busy = parseFeeEstimate({
      estimate: {
        priorityBucket: { feerate: 211.4, estimatedSeconds: 0.9 },
        normalBuckets: [{ feerate: 194, estimatedSeconds: 30 }, { feerate: 140.2, estimatedSeconds: 50 }],
        lowBuckets: [{ feerate: 115, estimatedSeconds: 3000 }],
      },
    });
    expect(isContended(busy)).toBe(true);
    const idle = parseFeeEstimate({ priorityBucket: { feerate: 100 }, normalBuckets: [{ feerate: 100 }], lowBuckets: [{ feerate: 100 }] });
    expect(isContended(idle)).toBe(false);
  });
});

describe('CongestionProbe', () => {
  const make = (answers: (FeeEstimate | null | Error)[]) => {
    let t = 1_000_000;
    let calls = 0;
    const read = async () => {
      const a = answers[Math.min(calls, answers.length - 1)];
      calls += 1;
      if (a instanceof Error) throw a;
      return a;
    };
    const probe = new CongestionProbe(read, RELAY_FEE_FLOOR, () => t);
    return { probe, calls: () => calls, advance: (ms: number) => (t += ms) };
  };

  it('asks the node at most once per 30 s however often it is called', async () => {
    const s = make([est(150), est(100)]);
    expect(await s.probe.probe()).toBe(true);
    for (let i = 0; i < 20; i++) expect(await s.probe.probe()).toBe(true);
    expect(s.calls()).toBe(1);
    s.advance(CONGESTION_POLL_MS - 1);
    expect(await s.probe.probe()).toBe(true);
    expect(s.calls()).toBe(1);
    s.advance(1);
    expect(await s.probe.probe()).toBe(false); // the second answer: quiet again
    expect(s.calls()).toBe(2);
    expect(s.probe.contended).toBe(false);
  });

  it('concurrent callers share one ask', async () => {
    const s = make([est(150)]);
    const all = await Promise.all([s.probe.probe(), s.probe.probe(), s.probe.probe()]);
    expect(all).toEqual([true, true, true]);
    expect(s.calls()).toBe(1);
  });

  it('a node without the estimate, or one that errors, says nothing (and is not asked again within the period)', async () => {
    const none = make([null]);
    expect(await none.probe.probe()).toBe(false);
    const bad = make([new Error('method not found')]);
    expect(await bad.probe.probe()).toBe(false);
    expect(await bad.probe.probe()).toBe(false);
    expect(bad.calls()).toBe(1);
  });

  it('a later error clears the notice (no stale "contended")', async () => {
    const s = make([est(150), new Error('down')]);
    expect(await s.probe.probe()).toBe(true);
    s.advance(CONGESTION_POLL_MS);
    expect(await s.probe.probe()).toBe(false);
  });
});

describe('dismissal', () => {
  beforeEach(resetCongestionDismissal);
  it('lasts for the page load', () => {
    expect(congestionDismissed()).toBe(false);
    dismissCongestion();
    expect(congestionDismissed()).toBe(true);
  });
});
