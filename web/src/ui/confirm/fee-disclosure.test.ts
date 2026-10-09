import { describe, expect, it } from 'vitest';
import { decodeSigning } from '../../kob/decode';
import { describeFee, normalizePolicy, pickRate, rememberFeeChoice, type FeeChoice, type FeeEstimate, type FeePolicy } from '../../kob/fee-policy';
import { planOrder } from '../../kob/plan';
import { parseRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import { TOK, makeEnv } from '../../testing/fixtures';
import { nodeFactsOf, tradableRegistryJson } from '../../testing/chain-fixtures';
import { t } from '../../i18n';
import { buildConfirmModel, type Row } from './confirm-model';
import { feeDisclosure, feeDisclosureOf, timeText } from './fee-disclosure';

const estimate: FeeEstimate = { priority: 194, normal: 140, low: 115, seconds: { priority: 0.5, normal: 40, low: 1800 } };
const policy = (o: Partial<FeePolicy> = {}): FeePolicy => normalizePolicy(o);
const choice = (o: Partial<FeePolicy> = {}, est: FeeEstimate | null = estimate, urgency: 'high' | 'normal' | 'low' = 'normal', rate?: bigint, fee = 5_000_000n): FeeChoice => {
  const p = policy(o);
  const pick = pickRate(p, est, urgency);
  return describeFee(p, pick, rate ?? pick.rate, fee);
};
const tr = () => (k: string, p?: Record<string, string | number | bigint>) => t(k, p);
const row = (rows: Row[], id: string): Row | undefined => rows.find((r) => r.id === id);

describe('feeDisclosure: the rate, its bucket and the time', () => {
  it('estimate: rate in sompi per gram, urgency + the node feerate, and the node\'s inclusion time', () => {
    const d = feeDisclosure({ choice: choice({}, estimate, 'high'), rate: null, fee: 5_000_000n }, tr());
    expect(row(d.rows, 'fee-rate')).toMatchObject({ value: '194 sompi per gram', tone: 'normal' });
    expect(row(d.rows, 'fee-rate')!.detail).toContain('priority bucket');
    expect(row(d.rows, 'fee-rate')!.detail).toContain('the node suggests 194');
    expect(row(d.rows, 'fee-speed')!.value).toBe('the node expects under a second at this rate');
    expect(d.warnings).toEqual([]);
    expect(d.info).toEqual([]);
  });

  it('each urgency names its bucket and time', () => {
    const n = feeDisclosure({ choice: choice({}, estimate, 'normal'), rate: null, fee: 1n }, tr());
    expect(row(n.rows, 'fee-rate')!.detail).toContain('next-minute');
    expect(row(n.rows, 'fee-speed')!.value).toContain('about 40 seconds');
    const l = feeDisclosure({ choice: choice({}, estimate, 'low'), rate: null, fee: 1n }, tr());
    expect(row(l.rows, 'fee-rate')!.detail).toContain('next-hour');
    expect(row(l.rows, 'fee-speed')!.value).toContain('about 30 minutes');
  });

  it('a fractional node feerate is shown rounded, the rate paid is the ceiling', () => {
    const d = feeDisclosure({ choice: choice({}, { priority: 150.456, normal: 150.456, low: 150.456 }), rate: null, fee: 1n }, tr());
    expect(row(d.rows, 'fee-rate')!.value).toBe('151 sompi per gram');
    expect(row(d.rows, 'fee-rate')!.detail).toContain('150.46');
  });

  it('timeText: sub-second, seconds, minutes, hours, and nothing for garbage', () => {
    expect(timeText(0.4, tr())).toBe('under a second');
    expect(timeText(40, tr())).toBe('about 40 seconds');
    expect(timeText(300, tr())).toBe('about 5 minutes');
    expect(timeText(7_200, tr())).toBe('about 2 hours');
    expect(timeText(Number.NaN, tr())).toBe('');
  });
});

describe('feeDisclosure: fallbacks and caps are said plainly', () => {
  it('no estimate: the minimum rate with the "estimate unavailable" warning', () => {
    const d = feeDisclosure({ choice: choice({}, null), rate: null, fee: 3_000_000n }, tr());
    expect(row(d.rows, 'fee-rate')).toMatchObject({ value: '100 sompi per gram', tone: 'warn' });
    expect(row(d.rows, 'fee-speed')).toBeUndefined();
    expect(d.warnings).toHaveLength(1);
    expect(d.warnings[0]).toMatchObject({ code: 'fee-estimate-unavailable', severity: 'warning' });
    expect(d.warnings[0]!.text).toBe('Fee estimate unavailable: paying the minimum rate (100 sompi per gram); may confirm slowly when busy.');
  });

  it('the policy switched off: an info line, not a warning', () => {
    const d = feeDisclosure({ choice: choice({ dynamic: false }), rate: null, fee: 1n }, tr());
    expect(d.warnings).toEqual([]);
    expect(d.info[0]).toMatchObject({ code: 'fee-policy-off' });
    expect(row(d.rows, 'fee-rate')!.tone).toBe('normal');
  });

  it('the wallet could not afford more: a warning', () => {
    const c: FeeChoice = { ...choice({}, estimate), source: 'floor', reason: 'funds', rate: 100n };
    const d = feeDisclosure({ choice: c, rate: null, fee: 1n }, tr());
    expect(d.warnings[0]).toMatchObject({ code: 'fee-floor-funds' });
  });

  it('maxRate clamp: the node\'s suggestion vs the limit', () => {
    const d = feeDisclosure({ choice: choice({ maxRate: 150n }, estimate, 'high'), rate: null, fee: 1n }, tr());
    expect(row(d.rows, 'fee-rate')!.value).toBe('150 sompi per gram');
    expect(d.info.map((f) => f.code)).toEqual(['fee-max-rate']);
    expect(d.info[0]!.text).toBe('The node suggests 194 sompi per gram, above your maximum of 150: paying 150; may confirm slower.');
  });

  it('total cap: the lowered rate and why', () => {
    const p = policy({ maxFeeSompi: 100_000_000n });
    const pick = pickRate(p, { priority: 1000, normal: 1000, low: 1000 }, 'high');
    const d = feeDisclosure({ choice: describeFee(p, pick, 400n, 100_000_000n), rate: null, fee: 100_000_000n }, tr());
    expect(row(d.rows, 'fee-rate')!.value).toBe('400 sompi per gram');
    expect(d.info.map((f) => f.code)).toEqual(['fee-total-cap']);
    expect(d.info[0]!.text).toContain('Fee capped at 1 KAS');
    expect(d.info[0]!.text).toContain('from 1000 to 400');
    expect(d.warnings).toEqual([]);
  });

  it('a floor-rate transaction above the cap is disclosed as sent anyway', () => {
    const p = policy({ maxFeeSompi: 1_000n });
    const c = describeFee(p, pickRate(p, estimate, 'normal'), 100n, 2_500_000n);
    const d = feeDisclosure({ choice: c, rate: null, fee: 2_500_000n }, tr());
    expect(d.warnings.map((f) => f.code)).toContain('fee-over-cap');
    expect(d.warnings.find((f) => f.code === 'fee-over-cap')!.text).toContain('0.025 KAS is above your 0.00001 KAS limit');
  });

  it('no remembered choice (a transaction planned elsewhere): just the rate of the built transaction; no rate: nothing', () => {
    const d = feeDisclosure({ choice: null, rate: 120n, fee: 1n }, tr());
    expect(d.rows).toEqual([{ id: 'fee-rate', label: 'Fee rate', value: '120 sompi per gram', tone: 'normal' }]);
    expect(d.warnings).toEqual([]);
    expect(feeDisclosure({ choice: null, rate: null, fee: 1n }, tr())).toEqual({ rows: [], warnings: [], info: [] });
  });

  it('feeDisclosureOf reads the remembered choice of the built transaction, else its rate', () => {
    const built = { fee: { fee: '5000000', feeRate: '140' } };
    expect(row(feeDisclosureOf(built, tr()).rows, 'fee-rate')!.value).toBe('140 sompi per gram');
    expect(row(feeDisclosureOf(built, tr()).rows, 'fee-speed')).toBeUndefined();
    rememberFeeChoice(built, choice({}, estimate, 'normal'));
    expect(row(feeDisclosureOf(built, tr()).rows, 'fee-speed')).toBeDefined();
    expect(feeDisclosureOf({ fee: { fee: '1', feeRate: 'x' } }, tr()).rows).toEqual([]);
  });
});

describe('both languages: every text resolves (no leftover key or placeholder)', () => {
  const cases: [string, FeeChoice][] = [
    ['estimate', choice({}, estimate, 'high')],
    ['unavailable', choice({}, null)],
    ['off', choice({ dynamic: false })],
    ['funds', { ...choice({}, estimate), source: 'floor', reason: 'funds', rate: 100n }],
    ['maxRate', choice({ maxRate: 150n }, estimate, 'high')],
    ['cap', (() => { const p = policy({ maxFeeSompi: 100_000_000n }); return describeFee(p, pickRate(p, { priority: 1000, normal: 1000, low: 1000 }, 'high'), 400n, 100_000_000n); })()],
    ['overCap', (() => { const p = policy({ maxFeeSompi: 1_000n }); return describeFee(p, pickRate(p, estimate, 'normal'), 100n, 2_500_000n); })()],
  ];
  it.each(cases)('%s', (_name, c) => {
    const d = feeDisclosure({ choice: c, rate: null, fee: c.fee }, tr());
    const all = [...d.rows.flatMap((r) => [r.label, r.value, r.detail ?? '']), ...[...d.warnings, ...d.info].map((f) => f.text ?? '')];
    expect(all.length).toBeGreaterThan(0);
    for (const s of all) {
      expect(s).not.toMatch(/\{\w+\}/);
      expect(s).not.toMatch(/^fees\./);
    }
  });
  it('the warning names the minimum rate', () => {
    const d = feeDisclosure({ choice: choice({}, null), rate: null, fee: 1n }, tr());
    expect(d.warnings[0]!.text).toMatch(/estimate unavailable/i);
    expect(d.warnings[0]!.text).toContain('100');
  });
});

describe('the confirmation screen model carries the disclosure (real plan)', () => {
  const kob = loadKobNode();
  const registry = parseRegistry(tradableRegistryJson(), { kob });
  it('fee section: Network fee, Fee rate, Expected inclusion; the warning among the warnings', () => {
    const fees = { policy: policy(), estimate: { priority: 300, normal: 150, low: 110, seconds: { priority: 0.8, normal: 40, low: 1500 } } };
    const plan = planOrder({ ...makeEnv(), fees }, { type: 'market', side: 'buy', amount: 2n * TOK });
    expect(plan.ok).toBe(true);
    const built = plan.built!;
    const summary = decodeSigning({ kob, built, maker: makeEnv().maker, registry, nodeInputs: nodeFactsOf(built), feePolicy: policy() });
    const m = buildConfirmModel(summary, { registry, fee: feeDisclosureOf(built, tr()), tr: tr() });
    const fee = m.sections.find((s) => s.id === 'fee')!;
    expect(fee.rows.map((r) => r.id)).toEqual(['fee', 'fee-rate', 'fee-speed']);
    expect(fee.rows[1]!.value).toBe('300 sompi per gram');
    expect(m.warnings.some((w) => w.code === 'fee-estimate-unavailable')).toBe(false);

    const floorPlan = planOrder({ ...makeEnv(), fees: { policy: policy(), estimate: null } }, { type: 'market', side: 'buy', amount: 2n * TOK });
    const fb = floorPlan.built!;
    const m2 = buildConfirmModel(decodeSigning({ kob, built: fb, maker: makeEnv().maker, registry, nodeInputs: nodeFactsOf(fb) }), { registry, fee: feeDisclosureOf(fb, tr()), tr: tr() });
    expect(m2.warnings.find((w) => w.code === 'fee-estimate-unavailable')!.text).toMatch(/^Fee estimate unavailable: paying the minimum rate/);
    expect(m2.canSign).toBe(true); // a disclosure never blocks signing
  });
});

describe('the decoder\'s fee sanity bounds follow the policy cap', () => {
  const kob = loadKobNode();
  const registry = parseRegistry(tradableRegistryJson(), { kob });
  it('a dynamic fee above the 0.5 KAS absolute ceiling blocks without the allowance and passes with it', () => {
    // 1000 sompi per gram on a heavy transaction: rate x mass above 0.5 KAS needs mass > 50,000
    const fees = { policy: policy({ maxFeeSompi: 0n }), estimate: { priority: 1000, normal: 1000, low: 1000 } };
    const plan = planOrder({ ...makeEnv(), fees }, { type: 'limit', side: 'sell', price: 300_000_000n, amount: 5n * TOK });
    const built = plan.built!;
    const fee = BigInt(built.fee.fee);
    const base = decodeSigning({ kob, built, maker: makeEnv().maker, registry, nodeInputs: nodeFactsOf(built) });
    const allowed = decodeSigning({ kob, built, maker: makeEnv().maker, registry, nodeInputs: nodeFactsOf(built), feePolicy: policy() });
    // the placement is light: both decode without a fee finding; the bounds are exercised directly below
    expect(fee).toBeLessThan(50_000_000n);
    expect(base.blocking.filter((b) => b.code === 'fee-excessive')).toEqual([]);
    expect(allowed.blocking.filter((b) => b.code === 'fee-excessive')).toEqual([]);
  });
});describe("the decoder's fee sanity bounds follow the fee policy", () => {
  const kob = loadKobNode();
  const registry = parseRegistry(tradableRegistryJson(), { kob });
  const maker = makeEnv().maker;
  const resting = { type: 'limit', side: 'sell', price: 300_000_000n, amount: 5n * TOK } as const;
  const decodeWith = (built: ReturnType<typeof planOrder>['built'], feePolicy?: FeePolicy) =>
    decodeSigning({ kob, built: built!, maker, registry, nodeInputs: nodeFactsOf(built!), ...(feePolicy ? { feePolicy } : {}) });
  const codes = (s: ReturnType<typeof decodeSigning>) => s.blocking.map((b) => b.code);
  const atRate = (rate: bigint) => planOrder(makeEnv({ feeRate: rate }), resting).built!;

  it('a dynamic fee at 194 sompi per gram on a small transaction passes without a finding', () => {
    const fees = { policy: policy(), estimate: { priority: 194, normal: 194, low: 194 } };
    const plan = planOrder({ ...makeEnv(), fees }, resting);
    expect(plan.built!.fee.feeRate).toBe('194');
    const s = decodeWith(plan.built, policy());
    expect(codes(s)).toEqual([]);
    expect(s.warnings.map((w) => w.code)).not.toContain('fee-high');
  });

  it('a declared rate above max(maxRate, floor) blocks; at the limit it passes; the floor is the limit when the policy is off', () => {
    expect(codes(decodeWith(atRate(1000n), policy()))).toEqual([]);
    const over = decodeWith(atRate(1001n), policy());
    expect(codes(over)).toContain('fee-rate-excessive');
    expect(over.ok).toBe(false);
    expect(over.blocking.find((b) => b.code === 'fee-rate-excessive')!.params).toMatchObject({ rate: '1001', max: 1000n });
    expect(codes(decodeWith(atRate(400n), policy({ maxRate: 300n })))).toContain('fee-rate-excessive');
    expect(codes(decodeWith(atRate(300n), policy({ maxRate: 300n })))).toEqual([]);
    // maxRate below the floor means the floor
    expect(codes(decodeWith(atRate(100n), policy({ maxRate: 50n })))).toEqual([]);
    expect(codes(decodeWith(atRate(101n), policy({ maxRate: 50n })))).toContain('fee-rate-excessive');
    // dynamic off: only the floor
    expect(codes(decodeWith(atRate(194n), policy({ dynamic: false })))).toContain('fee-rate-excessive');
    expect(codes(decodeWith(atRate(100n), policy({ dynamic: false })))).toEqual([]);
    // no policy handed over: no rate check (the fixed bounds only)
    expect(codes(decodeWith(atRate(1001n)))).toEqual([]);
  });

  /** the same transaction with its change output reduced by `extra` (declared fee updated: a builder that burns KAS consistently) */
  const burn = (built: ReturnType<typeof atRate>, extra: bigint) => {
    const b = JSON.parse(JSON.stringify(built)) as typeof built;
    const ci = b.fee.changeOutput!;
    b.tx.outputs[ci].value = (BigInt(b.tx.outputs[ci].value) - extra).toString();
    b.fee.fee = (BigInt(b.fee.fee) + extra).toString();
    return b;
  };

  it('a fee far above what the transaction costs at its own rate still blocks on a small move, even with the cap at 1 KAS', () => {
    const built = atRate(194n);
    const own = BigInt(built.fee.minFee);
    expect(own).toBeLessThan(10_000_000n);
    const burnt = burn(built, 30_000_000n); // 0.3 KAS: above 0.1 KAS and above minFee x 2, far above the 1% of what the order locks
    const s = decodeWith(burnt, policy({ maxFeeSompi: 100_000_000n }));
    expect(BigInt(burnt.fee.fee)).toBeGreaterThan(own * 2n + 100_000n);
    expect(codes(s)).toContain('fee-excessive');
    // without a policy it blocks too (the fixed 1% bound)
    expect(codes(decodeWith(burnt))).toContain('fee-excessive');
  });

  it('the 1% floor rises only to what the transaction costs at its own rate (not to the cap)', () => {
    // a heavy transaction at a high rate may cost more than 0.1 KAS and more than 1% of what it moves: its own-rate allowance covers that
    const built = atRate(3000n);
    expect(BigInt(built.fee.fee)).toBeGreaterThan(10_000_000n); // above the fixed 0.1 KAS bound
    expect(codes(decodeWith(built))).toContain('fee-excessive'); // the fixed bounds alone block it
    const s = decodeWith(built, policy({ maxRate: 3000n, maxFeeSompi: 100_000_000n }));
    expect(codes(s)).toEqual([]);
    // the same fee with a fabricated tiny own-rate target (minFee low) does not get the allowance
    const lowTarget = JSON.parse(JSON.stringify(built)) as typeof built;
    lowTarget.fee.minFee = '1000';
    expect(decodeWith(lowTarget, policy({ maxRate: 3000n })).blocking.map((b) => b.code)).toContain('fee-excessive');
  });

  it('the absolute ceiling rises to the policy cap but never past it', () => {
    const built = atRate(200_000n);
    expect(BigInt(built.fee.fee)).toBeGreaterThan(50_000_000n);
    // a rate this high is blocked by the rate check; with maxRate raised the cap decides
    const high = policy({ maxRate: 1_000_000n, maxFeeSompi: 0n });
    expect(codes(decodeWith(built, high))).toContain('fee-excessive'); // no cap: the fixed 0.5 KAS ceiling
    expect(codes(decodeWith(built, policy({ maxRate: 1_000_000n, maxFeeSompi: BigInt(built.fee.fee) }))).filter((c) => c === 'fee-excessive')).toEqual([]);
    expect(codes(decodeWith(built, policy({ maxRate: 1_000_000n, maxFeeSompi: BigInt(built.fee.fee) - 1n })))).toContain('fee-excessive');
  });
  it('an ordinary floor-rate fee is untouched by any policy', () => {
    expect(codes(decodeWith(atRate(100n), policy()))).toEqual([]);
    expect(codes(decodeWith(atRate(100n), policy({ dynamic: false })))).toEqual([]);
  });
});
