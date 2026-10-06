import { describe, expect, it } from 'vitest';
import {
  DEFAULT_FEE_POLICY, FeeOracle, RELAY_FEE_FLOOR, buildWithCap, capForTotal, describeFee, feeChoiceOf, normalizePolicy, parseFeeEstimate, pickRate,
  readFeeContext, rememberFeeChoice, type FeeEstimate, type FeePolicy,
} from './fee-policy';

const policy = (o: Partial<FeePolicy> = {}): FeePolicy => normalizePolicy(o);
const est = (priority: number, normal: number, low: number, seconds?: FeeEstimate['seconds']): FeeEstimate => ({ priority, normal, low, ...(seconds ? { seconds } : {}) });

describe('defaults', () => {
  it('floor 100, maxRate 1000, 1 KAS total cap, refresh 10 s, max age 60 s, dynamic on', () => {
    expect(DEFAULT_FEE_POLICY).toEqual({ dynamic: true, floor: 100n, maxRate: 1000n, maxFeeSompi: 100_000_000n, refreshMs: 10_000, maxAgeMs: 60_000 });
    expect(RELAY_FEE_FLOOR).toBe(100n);
  });
  it('a floor below the relay minimum is raised to it', () => {
    expect(normalizePolicy({ floor: 10n }).floor).toBe(100n);
    expect(normalizePolicy({ floor: 150n }).floor).toBe(150n);
  });
});

describe('parseFeeEstimate', () => {
  const node = { estimate: { priorityBucket: { feerate: 194.2, estimatedSeconds: 0.5 }, normalBuckets: [{ feerate: 140, estimatedSeconds: 30 }, { feerate: 120, estimatedSeconds: 90 }], lowBuckets: [{ feerate: 115, estimatedSeconds: 1800 }] } };
  it('reads the SDK shape (wrapped) and the unwrapped inner object', () => {
    const want = { priority: 194.2, normal: 140, low: 115, seconds: { priority: 0.5, normal: 30, low: 1800 } };
    expect(parseFeeEstimate(node)).toEqual(want);
    expect(parseFeeEstimate(node.estimate)).toEqual(want);
  });
  it('takes the FIRST normal / low bucket and tolerates snake case and a missing time', () => {
    const r = parseFeeEstimate({ priority_bucket: { fee_rate: 100 }, normal_buckets: [{ feerate: 101 }, { feerate: 5 }], low_buckets: [{ feerate: 100 }] });
    expect(r).toEqual({ priority: 100, normal: 101, low: 100 });
  });
  it('accepts an estimate it already parsed (re-validated)', () => {
    const e = { priority: 150, normal: 120, low: 100, seconds: { priority: 1 } };
    expect(parseFeeEstimate(e)).toEqual(e);
    expect(parseFeeEstimate({ priority: 150, normal: 0, low: 100 })).toBeNull();
    expect(parseFeeEstimate({ priority: 150, normal: Number.NaN, low: 100 })).toBeNull();
  });
  it('null for anything unusable', () => {
    const b = (f: unknown) => ({ feerate: f });
    const mk = (p: unknown, n: unknown, l: unknown) => ({ estimate: { priorityBucket: p, normalBuckets: n, lowBuckets: l } });
    for (const bad of [
      null, undefined, 5, 'x', [], {}, { estimate: {} }, { estimate: null },
      mk(b(1), [], [b(1)]), mk(b(1), [b(1)], []), mk(undefined, [b(1)], [b(1)]),
      mk(b(Number.NaN), [b(1)], [b(1)]), mk(b(Infinity), [b(1)], [b(1)]), mk(b(0), [b(1)], [b(1)]), mk(b(-3), [b(1)], [b(1)]),
      mk(b(1), [b(null)], [b(1)]), mk(b(1), [b(1)], [b('abc')]), mk(b(1), 'no', [b(1)]),
    ]) expect(parseFeeEstimate(bad), JSON.stringify(bad)).toBeNull();
  });
});

describe('pickRate: bucket per urgency', () => {
  const e = est(194, 140, 115, { priority: 0.4, normal: 40, low: 1500 });
  it('high -> priority, normal -> normal[0], low -> low[0]', () => {
    expect(pickRate(policy(), e, 'high')).toMatchObject({ rate: 194n, source: 'estimate', clamped: false, bucketFeerate: 194, estimatedSeconds: 0.4, urgency: 'high' });
    expect(pickRate(policy(), e, 'normal')).toMatchObject({ rate: 140n, source: 'estimate', estimatedSeconds: 40, urgency: 'normal' });
    expect(pickRate(policy(), e, 'low')).toMatchObject({ rate: 115n, source: 'estimate', estimatedSeconds: 1500, urgency: 'low' });
  });
  it('rounds the node feerate UP', () => {
    expect(pickRate(policy(), est(150.01, 140.0001, 100.4), 'high').rate).toBe(151n);
    expect(pickRate(policy(), est(150.01, 140.0001, 100.4), 'normal').rate).toBe(141n);
    expect(pickRate(policy(), est(150.01, 140.0001, 100.4), 'low').rate).toBe(101n);
    expect(pickRate(policy(), est(200, 200, 200), 'high').rate).toBe(200n);
  });
});

describe('pickRate: clamps', () => {
  it('raises a rate below the floor to the floor (clamped, still an estimate)', () => {
    const p = pickRate(policy(), est(1, 99, 50), 'normal');
    expect(p).toMatchObject({ rate: 100n, source: 'estimate', clamped: true, clampedTo: 'floor' });
    expect(pickRate(policy(), est(1, 100, 50), 'normal')).toMatchObject({ rate: 100n, clamped: false });
  });
  it('caps a rate above maxRate', () => {
    const p = pickRate(policy({ maxRate: 500n }), est(5000, 700, 100), 'high');
    expect(p).toMatchObject({ rate: 500n, source: 'estimate', clamped: true, clampedTo: 'maxRate', bucketFeerate: 5000 });
    expect(pickRate(policy({ maxRate: 500n }), est(5000, 500, 100), 'normal')).toMatchObject({ rate: 500n, clamped: false });
    expect(pickRate(policy(), est(5000, 700, 100), 'high').rate).toBe(1000n);
  });
  it('maxRate below the floor means the floor (never an error, never below it)', () => {
    expect(pickRate(policy({ maxRate: 50n }), est(900, 900, 900), 'high')).toMatchObject({ rate: 100n, clamped: true, clampedTo: 'maxRate' });
    expect(pickRate(policy({ maxRate: 0n }), est(900, 900, 900), 'low').rate).toBe(100n);
    expect(pickRate(policy({ floor: 200n, maxRate: 150n }), est(900, 900, 900), 'low').rate).toBe(200n);
  });
  it('a custom floor is the lower bound', () => {
    expect(pickRate(policy({ floor: 120n }), est(110, 110, 110), 'normal')).toMatchObject({ rate: 120n, clampedTo: 'floor' });
  });
});

describe('pickRate: fallback to the floor, never an error', () => {
  it('no estimate (null)', () => {
    expect(pickRate(policy(), null, 'high')).toEqual({ urgency: 'high', rate: 100n, source: 'floor', reason: 'unavailable', clamped: false });
  });
  it('an unusable bucket value', () => {
    for (const bad of [Number.NaN, Infinity, 0, -1]) {
      expect(pickRate(policy(), est(bad, 150, 150), 'high')).toMatchObject({ rate: 100n, source: 'floor', reason: 'unavailable' });
      expect(pickRate(policy(), est(bad, 150, 150), 'normal')).toMatchObject({ rate: 150n, source: 'estimate' });
    }
  });
  it('dynamic: false is always the floor, whatever the estimate says', () => {
    for (const u of ['high', 'normal', 'low'] as const) {
      expect(pickRate(policy({ dynamic: false }), est(900, 900, 900), u)).toEqual({ urgency: u, rate: 100n, source: 'floor', reason: 'disabled', clamped: false });
    }
  });
});

describe('capForTotal', () => {
  const p = policy({ maxFeeSompi: 100_000_000n });
  it('null while the fee is within the cap, or no cap is set', () => {
    expect(capForTotal(p, 500n, 100_000_000n)).toBeNull();
    expect(capForTotal(p, 500n, 99_999_999n)).toBeNull();
    expect(capForTotal(policy({ maxFeeSompi: 0n }), 1000n, 10n ** 12n)).toBeNull();
  });
  it('rebuild rate = floor_div(rate x cap, fee) when above the cap', () => {
    expect(capForTotal(p, 1000n, 250_000_000n)).toBe(400n);
    expect(capForTotal(p, 333n, 150_000_000n)).toBe(222n); // 333 * 1e8 / 1.5e8 = 222
    expect(capForTotal(policy({ maxFeeSompi: 1_000n }), 777n, 3_000n)).toBe(259n); // floor(777 * 1000 / 3000) = 259
  });
  it('never below the floor, and never when the rate already is the floor (the floor-rate tx is sent over the cap)', () => {
    expect(capForTotal(p, 150n, 10_000_000_000n)).toBe(100n);
    expect(capForTotal(p, 100n, 10_000_000_000n)).toBeNull();
    expect(capForTotal(policy({ floor: 150n }), 150n, 10_000_000_000n)).toBeNull();
  });
  it('a fee just above the cap lowers a rate by one step, never raises it', () => {
    expect(capForTotal(p, 101n, 100_000_001n)).toBe(100n); // floor_div(101 * 1e8, 100_000_001) = 100
    expect(capForTotal(p, 1000n, 100_000_001n)).toBe(999n);
  });
});

describe('buildWithCap', () => {
  // a fake builder: fee = rate * mass
  const mass = 1_000_000n;
  const mk = () => {
    const calls: string[] = [];
    const build = (r: { fee?: { feeRate?: string | null } }) => {
      const rate = BigInt(r.fee?.feeRate ?? '100');
      calls.push(rate.toString());
      return { fee: { fee: (rate * mass).toString(), feeRate: rate.toString() } };
    };
    return { calls, build };
  };
  it('no rebuild while the fee is within the cap', () => {
    const { calls, build } = mk();
    const r = buildWithCap(policy({ maxFeeSompi: 200_000_000n }), { fee: { feeRate: '150' } }, build);
    expect(calls).toEqual(['150']);
    expect(r.rate).toBe(150n);
    expect(r.built.fee.fee).toBe('150000000');
  });
  it('rebuilds ONCE at floor_div(rate x cap, fee) and returns that request', () => {
    const { calls, build } = mk();
    const r = buildWithCap(policy({ maxFeeSompi: 100_000_000n }), { fee: { feeRate: '400', feeMode: 'priority' } }, build);
    // 400 x 1e6 = 4e8 > 1e8 -> 400 * 1e8 / 4e8 = 100
    expect(calls).toEqual(['400', '100']);
    expect(r.rate).toBe(100n);
    expect(r.request.fee).toEqual({ feeRate: '100', feeMode: 'priority' });
  });
  it('a floor-rate transaction above the cap is kept (one build)', () => {
    const { calls, build } = mk();
    const r = buildWithCap(policy({ maxFeeSompi: 1_000n }), { fee: { feeRate: '100' } }, build);
    expect(calls).toEqual(['100']);
    expect(r.rate).toBe(100n);
  });
  it('a request with no rate is at the floor: never rebuilt', () => {
    const { calls, build } = mk();
    buildWithCap(policy({ maxFeeSompi: 1n }), {}, build);
    expect(calls).toEqual(['100']);
  });
  it('a failing rebuild keeps the first transaction; a failing first build propagates', () => {
    let n = 0;
    const flaky = () => {
      if (n++ === 1) throw new Error('second build failed');
      return { fee: { fee: '900000000' } };
    };
    const r = buildWithCap(policy({ maxFeeSompi: 100_000_000n }), { fee: { feeRate: '500' } }, flaky);
    expect(r.rate).toBe(500n);
    expect(() => buildWithCap(policy(), {}, () => { throw new Error('boom'); })).toThrow('boom');
  });
  it('no policy: a plain build', () => {
    const { calls, build } = mk();
    buildWithCap(null, { fee: { feeRate: '9000' } }, build);
    expect(calls).toEqual(['9000']);
  });
});

describe('describeFee', () => {
  const p = policy({ maxFeeSompi: 100_000_000n });
  it('keeps the pick, notes the cap and over-cap floor transactions', () => {
    const pick = pickRate(p, est(900, 140, 115, { priority: 1 }), 'high');
    const capped = describeFee(p, pick, 400n, 100_000_000n);
    expect(capped).toMatchObject({ urgency: 'high', rate: 400n, cappedFrom: 900n, overCap: false, source: 'estimate', estimatedSeconds: 1, maxFeeSompi: 100_000_000n, floor: 100n });
    const over = describeFee(p, pickRate(p, null, 'normal'), 100n, 200_000_000n);
    expect(over).toMatchObject({ source: 'floor', reason: 'unavailable', overCap: true });
    expect(over.cappedFrom).toBeUndefined();
    expect(describeFee(policy({ maxFeeSompi: 0n }), pick, 900n, 10n ** 12n).overCap).toBe(false);
  });
  it('is remembered per built transaction', () => {
    const built = { fee: {} };
    expect(feeChoiceOf(built)).toBeNull();
    const c = describeFee(p, pickRate(p, null, 'low'), 100n, 1n);
    rememberFeeChoice(built, c);
    expect(feeChoiceOf(built)).toBe(c);
    expect(feeChoiceOf({})).toBeNull();
  });
});

describe('FeeOracle', () => {
  const raw = (n: number) => ({ estimate: { priorityBucket: { feerate: n }, normalBuckets: [{ feerate: n }], lowBuckets: [{ feerate: n }] } });
  function harness(answers: (() => unknown)[], o: { refreshMs?: number; maxAgeMs?: number } = {}) {
    let t = 1_000_000;
    let calls = 0;
    const oracle = new FeeOracle({
      fetch: async () => {
        const f = answers[Math.min(calls, answers.length - 1)];
        calls++;
        return f();
      },
      now: () => t,
      ...o,
    });
    return { oracle, advance: (ms: number) => { t += ms; }, calls: () => calls };
  }

  it('asks the node at most once per refreshMs', async () => {
    const h = harness([() => raw(150), () => raw(160)], { refreshMs: 10_000 });
    expect((await h.oracle.get())?.normal).toBe(150);
    h.advance(9_999);
    expect((await h.oracle.get())?.normal).toBe(150);
    expect(h.calls()).toBe(1);
    h.advance(1);
    expect((await h.oracle.get())?.normal).toBe(160);
    expect(h.calls()).toBe(2);
  });

  it('concurrent callers share one call', async () => {
    const h = harness([() => raw(150)]);
    const all = await Promise.all([h.oracle.get(), h.oracle.get(), h.oracle.get()]);
    expect(all.every((e) => e?.normal === 150)).toBe(true);
    expect(h.calls()).toBe(1);
  });

  it('keeps the last good estimate through failures and malformed answers, up to maxAgeMs, then null', async () => {
    const h = harness([() => raw(150), () => { throw new Error('down'); }, () => ({ estimate: {} })], { refreshMs: 10_000, maxAgeMs: 60_000 });
    expect((await h.oracle.get())?.normal).toBe(150);
    h.advance(10_000);
    expect((await h.oracle.get())?.normal).toBe(150); // call failed: the 10 s old estimate still serves
    h.advance(10_000);
    expect((await h.oracle.get())?.normal).toBe(150); // malformed: ditto (20 s)
    h.advance(40_001);
    expect(await h.oracle.get()).toBeNull(); // 60.000 s + : stale
    expect(h.oracle.peek()).toBeNull();
  });

  it('stale at exactly more than maxAge, fresh at exactly maxAge', async () => {
    const h = harness([() => raw(150), () => { throw new Error('down'); }], { refreshMs: 1_000_000, maxAgeMs: 60_000 });
    await h.oracle.get();
    h.advance(60_000);
    expect(h.oracle.peek()?.normal).toBe(150);
    h.advance(1);
    expect(h.oracle.peek()).toBeNull();
  });

  it('never rejects: a throwing or hanging node is null', async () => {
    const down = new FeeOracle({ fetch: async () => { throw new Error('method not found'); } });
    expect(await down.get()).toBeNull();
    const hang = new FeeOracle({ fetch: () => new Promise(() => undefined), timeoutMs: 20 });
    expect(await hang.get()).toBeNull();
    const sync = new FeeOracle({ fetch: () => { throw new Error('sync throw'); } });
    expect(await sync.get()).toBeNull();
  });

  it('a failed first call is retried only after refreshMs', async () => {
    const h = harness([() => { throw new Error('down'); }, () => raw(130)], { refreshMs: 10_000 });
    expect(await h.oracle.get()).toBeNull();
    h.advance(5_000);
    expect(await h.oracle.get()).toBeNull();
    expect(h.calls()).toBe(1);
    h.advance(5_000);
    expect((await h.oracle.get())?.normal).toBe(130);
  });

  it('readFeeContext: no call at all when the policy is not dynamic', async () => {
    let called = 0;
    const o = { get: async () => { called++; return est(1, 2, 3); } };
    expect(await readFeeContext(policy({ dynamic: false }), o)).toEqual({ policy: policy({ dynamic: false }), estimate: null });
    expect(called).toBe(0);
    expect((await readFeeContext(policy(), o)).estimate).toEqual(est(1, 2, 3));
    expect((await readFeeContext(policy(), null)).estimate).toBeNull();
  });
});
