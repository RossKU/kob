// Pre-sign decoding of pair orders (KobPair, KobCondPair, KobIfdPair; token A for token B, either family each): placements (custodies of A and
// of B, a sell-first entry's B prefund, escrows per role against the plan), cancels / refunds / sweeps / position cancels (what returns of A, of B
// and of KAS), the registry pins of both tokens, and the trigger evidence of pair stops in both modes (two KAS-book fills, or a resting pair
// order). Every transaction is real: the golden pair requests re-keyed to the test maker, built, signed and run through the script engine.
import { describe, expect, it } from 'vitest';
import { decodeSigning, fillArg, type SigningSummary } from './decode';
import { planCancel, planCancelAll, planRefund, planSweep, type CancelEnv, type OrderSnapshot } from './cancel';
import { custodiesOf, describeOrder, pairFactsOf } from './order-facts';
import { parseRegistry, type TokenRegistry } from './registry';
import { loadKobNode } from './wasm.node';
import type { BuiltTx, OrderState, SigPlan, TokenUtxo } from './types';
import type { KobWasm } from './wasm';
import { MAKER, ZERO32, goldenRequest, goldenTx, keyUtxo, nodeFactsOf, placeGolden, signAndValidate, tradableRegistryJson } from '../testing/chain-fixtures';
import { PAIR_CREATES, PAIR_MIXES, buildGolden, legState, snapshotAfter, withFields } from '../testing/pair-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
const A = '70'.repeat(32);
const B = '71'.repeat(32);
const env: CancelEnv = { kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 1000n } };
const codes = (xs: { code: string }[]) => xs.map((x) => x.code);
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const decode = (built: BuiltTx, extra: Partial<Parameters<typeof decodeSigning>[0]> = {}): SigningSummary =>
  decodeSigning({ kob, built, maker: MAKER.pk, registry: null, nodeInputs: nodeFactsOf(built), ...extra });

/** A registry listing the golden KCC-20 pair tokens A (70..) and B (71..) on the reference program; `bExt` overrides B's extension commitment. */
function registry(bExt?: string): TokenRegistry {
  const doc = tradableRegistryJson();
  const t = doc.tokens[0];
  doc.tokens = [{ ...t, ticker: 'TSTA', covenant_id: A }, { ...t, ticker: 'TSTB', covenant_id: B, ...(bExt ? { extension_commitment: bExt } : {}) }];
  return parseRegistry(doc, { kob });
}

/** A stray of `token` owned by the order (KCC-20, the golden extension). */
function strayOf(snap: OrderSnapshot, token: string, amount: bigint, n: number): TokenUtxo {
  return {
    transactionId: n.toString(16).padStart(2, '0').repeat(32), index: n, amount: (10n * KAS).toString(), blockDaaScore: '1200', covenantId: token,
    state: { amount: amount.toString(), owner: snap.covenantId, owner_scheme: 4, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: 'ee'.repeat(32) },
  };
}

describe('pair placements of every kind and family mix', () => {
  for (const mix of PAIR_MIXES) {
    for (const [suffix, kind, side] of PAIR_CREATES) {
      const name = `pair.${mix}${suffix}`;
      it(`${name}: no blocking finding; A / B / KAS re-derived from the transaction, the custodies those of kob-wasm`, () => {
        const p = placeGolden(kob, name);
        const st = p.recovered[0]!.order as OrderState;
        const cs = custodiesOf(st, kob);
        const base = cs.filter((c) => c.role === 'base').reduce((a, c) => a + c.amount, 0n);
        const quote = cs.filter((c) => c.role === 'quote').reduce((a, c) => a + c.amount, 0n);
        const s = decode(p.built, { expected: { orders: [st], tokensEscrowed: base, quoteEscrowed: quote } });
        expect(s.blocking).toEqual([]);
        expect(s.kind).toBe('create');
        expect(s.orders).toHaveLength(1);
        const o = s.orders[0]!;
        expect(o.verified).toBe(true);
        expect(o.description).toMatchObject({ kind, side, tokenCovId: A, pair: { kind, side, base: { covId: A }, quote: { covId: B } } });
        // every custody of the record, of its own token, with the amount kob-wasm says the state holds
        expect(o.custodies.map((c) => [c.ref.covenantId, c.amount, c.role])).toEqual(cs.filter((c) => c.amount > 0n).map((c) => [c.token, c.amount, c.role]));
        if (suffix === 'create.ifdAsk') expect(o.custodies.map((c) => c.role)).toEqual(['base', 'quote']);
        // per token: what leaves the wallet goes into the custodies (a pair order holds no KAS escrow: its KAS is carriers and tip)
        for (const c of o.custodies) expect(s.net.tokens.find((t) => t.ref.covenantId === c.ref.covenantId)!.escrowed).toBe(c.amount);
        expect(o.locked.escrow).toBe(0n);
        expect(o.locked.total).toBe(o.value);
        expect(s.net.kas.toOthers).toBe(0n);
      });
    }
  }

  it('a plan whose escrow of B (or of A) differs from the transaction is blocking', () => {
    const p = placeGolden(kob, 'pair.create.ifdAsk');
    const st = p.recovered[0]!.order as OrderState;
    const cs = custodiesOf(st, kob);
    const s = decode(p.built, { expected: { tokensEscrowed: cs[0]!.amount, quoteEscrowed: cs[1]!.amount + 1n } });
    expect(codes(s.blocking)).toEqual(['expected-quote-escrowed']);
    const t = decode(p.built, { expected: { tokensEscrowed: cs[0]!.amount + cs[1]!.amount } });
    expect(codes(t.blocking)).toEqual(['expected-tokens-escrowed']);
  });

  it('both tokens are checked against the registry: listed pins pass, a B pinned to another extension than its entry is blocking, an unknown B warns', () => {
    const p = placeGolden(kob, 'pair.create.ask');
    const ok = decode(p.built, { registry: registry() });
    expect(ok.blocking).toEqual([]);
    expect(ok.warnings.filter((w) => w.code !== 'inputs-unverified')).toEqual([]);
    const bad = decode(p.built, { registry: registry('dd'.repeat(32)) });
    expect(codes(bad.blocking)).toContain('pair-token-mismatch');
    expect(bad.blocking.find((x) => x.code === 'pair-token-mismatch')!.params).toMatchObject({ covenantId: B, token: 'B' });
    const doc = tradableRegistryJson();
    doc.tokens = [{ ...doc.tokens[0], ticker: 'TSTA', covenant_id: A }];
    const unknownB = decode(p.built, { registry: parseRegistry(doc, { kob }) });
    expect(unknownB.blocking).toEqual([]);
    expect(unknownB.warnings.filter((w) => w.code === 'token-unlisted').map((w) => w.params?.covenantId)).toEqual([B]);
  });

  it('terms kob-wasm refuses for a NEW order are blocking (pair-terms-invalid)', () => {
    const p = placeGolden(kob, 'pair.create.bid');
    const hostile: KobWasm = { ...kob, checkNewOrder: () => { throw new Error('price must be positive'); } };
    const s = decode(p.built, { kob: hostile });
    expect(codes(s.blocking)).toEqual(['pair-terms-invalid']);
    expect(s.blocking[0]!.message).toContain('price must be positive');
  });
});

describe('pair cancels, refunds, sweeps and position cancels', () => {
  for (const mix of PAIR_MIXES) {
    for (const [suffix] of PAIR_CREATES) {
      const name = `pair.${mix}${suffix}`;
      it(`${name}: the maker's cancel returns every custody of A and of B; consensus-valid; the decoder shows each token`, () => {
        const p = placeGolden(kob, name);
        const snap = p.snapshots[0]!;
        const plan = planCancel(env, snap);
        expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
        expect(plan.ok).toBe(true);
        signAndValidate(kob, plan.built!);
        const st = snap.order.state;
        const cs = custodiesOf(st, kob);
        expect(plan.tokensReturned).toBe(cs.filter((c) => c.role === 'base').reduce((a, c) => a + c.amount, 0n));
        expect(plan.quoteReturned).toBe(cs.filter((c) => c.role === 'quote').reduce((a, c) => a + c.amount, 0n));
        const s = decode(plan.built!, { expected: plan.expected });
        expect(s.blocking).toEqual([]);
        expect(s.kind).toBe('cancel');
        const sp = s.spends[0]!;
        expect(sp.pairTokens!.map((t) => [t.role, t.released, t.strays])).toEqual([
          ['base', cs.filter((c) => c.role === 'base').reduce((a, c) => a + c.amount, 0n), 0n],
          ['quote', cs.filter((c) => c.role === 'quote').reduce((a, c) => a + c.amount, 0n), 0n],
        ]);
        expect(sp.otherStrays).toEqual([]);
        for (const c of cs.filter((x) => x.amount > 0n)) expect(s.net.tokens.find((t) => t.ref.covenantId === c.token)!.toMaker).toBe(c.amount);
      });
    }
  }

  it('a cancel sweeps strays of BOTH tokens (A strays and B strays each in its own transfer) and reports them', () => {
    const p = placeGolden(kob, 'pair.create.ask');
    const snap = { ...p.snapshots[0]!, strays: [] as TokenUtxo[] };
    snap.strays = [strayOf(snap, A, 7n, 90), strayOf(snap, B, 11n, 91), strayOf(snap, B, 13n, 92)];
    const plan = planCancel(env, snap);
    expect(plan.ok).toBe(true);
    signAndValidate(kob, plan.built!);
    expect(plan.tokensReturned).toBe(10_000n + 7n);
    expect(plan.quoteReturned).toBe(24n);
    const s = decode(plan.built!, { expected: plan.expected });
    expect(s.blocking).toEqual([]);
    const sp = s.spends[0]!;
    expect(sp.strays).toBe(7n);
    expect(sp.pairTokens!.map((t) => [t.role, t.released, t.custody, t.strays])).toEqual([['base', 10_007n, 10_000n, 7n], ['quote', 24n, 0n, 24n]]);
    // B is the order's own token: its strays are not foreign
    expect(sp.otherStrays.map((x) => [x.ref.covenantId, x.amount, x.foreign])).toEqual([[B, 24n, false]]);
    expect(codes(s.info)).toEqual(expect.arrayContaining(['strays-swept', 'other-strays-swept']));
  });

  it('a sell-first entry refunds both custodies (A and the B prefund) once due; a refund before that is refused', () => {
    const p = placeGolden(kob, 'pair.create.ifdAsk');
    const snap = p.snapshots[0]!;
    expect(snap.prefund).toBeTruthy();
    const early = planRefund(env, snap);
    expect(early.ok).toBe(false);
    expect(codes(early.issues)).toContain('refund.not-yet');
    const plan = planRefund({ ...env, clock: { daa: 400_000_000n } }, snap);
    expect(plan.ok).toBe(true);
    expect(plan.request).toMatchObject({ action: 'refundOrder', prefund: { index: snap.prefund!.index } });
    signAndValidate(kob, plan.built!, []);
    const s = decode(plan.built!, { expected: plan.expected });
    expect(s.kind).toBe('refund');
    expect(s.blocking).toEqual([]);
    expect(s.spends[0]!.pairTokens!.map((t) => t.released)).toEqual(custodiesOf(snap.order.state, kob).map((c) => c.amount));
  });

  it('a sweep in place returns B strays of a pair bid and leaves its B escrow untouched', () => {
    const p = placeGolden(kob, 'pair.create.bid');
    const snap = { ...p.snapshots[0]! };
    snap.strays = [strayOf(snap, B, 5n, 93), strayOf(snap, A, 3n, 94)];
    const plan = planSweep(env, snap);
    expect(plan.ok).toBe(true);
    signAndValidate(kob, plan.built!);
    expect(plan.sweep!.tokens.map((t) => [t.covenantId, t.amount, t.foreign])).toEqual([[A, 3n, false], [B, 5n, false]]);
    expect(plan.tokensReturned).toBe(3n);
    expect(plan.quoteReturned).toBe(5n);
    const s = decode(plan.built!, { expected: plan.expected });
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('sweep');
    expect(s.spends[0]!.pairTokens!.map((t) => [t.role, t.strays])).toEqual([['base', 3n], ['quote', 5n]]);
  });

  it('an if-done pair entry and the exit its fill created are cancelled together in ONE position cancel (one pair)', () => {
    const { built, signed } = buildGolden(kob, 'pair.ifd.bid.cont');
    const entry0 = legState(goldenRequest('pair.ifd.bid.cont', MAKER.pk), 0, 'KobIfdPair');
    const n = fillArg((built.plans[0] as Extract<SigPlan, { kind: 'entry' }>).args[0])!;
    const p = BigInt((entry0.state as { price: string }).price);
    const spend = kob.ifdPairAmounts(entry0, n, p).spend!;
    const entryCov = built.tx.inputs[0]!.utxo.covenantId!;
    const left = BigInt((entry0.state as { amountLeft: string }).amountLeft) - n;
    const custody = BigInt((entry0.state as { custody: string }).custody) - spend;
    const entry = snapshotAfter(kob, built, signed.tx, entryCov, [withFields(entry0, { amountLeft: left, custody })]);
    expect(entry).not.toBeNull();
    const exitCov = built.covenants.find((c) => c.template === 'KobCondPair')!.covenantId;
    const exit = snapshotAfter(kob, built, signed.tx, exitCov, [kob.ifdPairExitFor(entry0, n, n)]);
    expect(exit).not.toBeNull();
    expect(exit!.custody!.covenantId).toBe(A);
    expect(entry!.custody!.covenantId).toBe(B);
    const plans = planCancelAll(env, [entry!, exit!]);
    expect(plans).toHaveLength(1);
    expect(plans[0]!.ok).toBe(true);
    expect(plans[0]!.request).toMatchObject({ action: 'cancelPosition' });
    signAndValidate(kob, plans[0]!.built!);
    expect(plans[0]!.tokensReturned).toBe(n);
    expect(plans[0]!.quoteReturned).toBe(custody);
    const s = decode(plans[0]!.built!, { expected: plans[0]!.expected });
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('cancel-position');
    expect(describeOrder(exit!.order.state, kob).pair).toMatchObject({ side: 'sell', escrowA: n, escrowB: 0n });
  });
});

describe('trigger evidence of pair stops (both modes)', () => {
  const builtOf = (name: string): BuiltTx => clone(goldenTx(name).built as BuiltTx);
  const dec = (b: BuiltTx) => decodeSigning({ kob, built: b, maker: b.sign[0]?.pubkey ?? '00'.repeat(32), registry: null, nodeInputs: nodeFactsOf(b) });
  const triggerCodes = (s: SigningSummary): string[] => [...s.blocking, ...s.warnings, ...s.info].map((i) => i.code).filter((c) => c.startsWith('trigger-'));
  const entry = (b: BuiltTx, i: number) => b.plans[i] as Extract<SigPlan, { kind: 'entry' }>;
  const setInt = (b: BuiltTx, input: number, arg: number, value: bigint | number) => {
    entry(b, input).args[arg] = { kind: 'int', value: String(value) };
  };
  const orderInput = (b: BuiltTx) => b.plans.findIndex((p) => p.kind === 'entry' && (p.template === 'KobCondPair' || p.template === 'KobIfdPair'));

  const cases: [string, 'fill' | 'update', 'arm' | 'trail', 'kasBooks' | 'pair'][] = [
    ['pair.cond.ask.stop.ev0', 'fill', 'arm', 'kasBooks'], ['pair.cond.ask.stop.ev1', 'fill', 'arm', 'pair'],
    ['pair.cond.bid.stop.ev0', 'fill', 'arm', 'kasBooks'], ['pair.cond.bid.stop.ev1', 'fill', 'arm', 'pair'],
    ['pair.update.ask.arm.ev0', 'update', 'arm', 'kasBooks'], ['pair.update.ask.arm.ev1', 'update', 'arm', 'pair'],
    ['pair.update.bid.arm.ev0', 'update', 'arm', 'kasBooks'], ['pair.update.ask.trail.ev0', 'update', 'trail', 'kasBooks'],
    ['pair.update.ask.trail.ev1', 'update', 'trail', 'pair'], ['pair.update.bid.trail.ev1', 'update', 'trail', 'pair'],
    ['pair.ifd.bid.stop.ev1', 'fill', 'arm', 'pair'], ['pair.ifd.ask.stop.ev0', 'fill', 'arm', 'kasBooks'], ['pair.update.ifdBid.arm.ev1', 'update', 'arm', 'pair'],
  ];
  for (const [name, path, effect, mode] of cases) {
    it(`${name}: the evidence (${mode}) satisfies the order's rule and is shown`, () => {
      const b = builtOf(name);
      const s = dec(b);
      expect(s.triggers).toHaveLength(1);
      const t = s.triggers[0]!;
      expect(t).toMatchObject({ input: orderInput(b), path, effect, ok: true, pair: { mode } });
      expect(Object.values(t.checks).every(Boolean)).toBe(true);
      expect(triggerCodes(s)).toEqual(['trigger-evidence']);
      if (mode === 'kasBooks') {
        expect(t.pair!.evidenceB).not.toBeNull();
        expect(t.evidence.side).not.toBe(t.pair!.evidenceB!.side);
        expect(t.pair!.minTouchB).not.toBeNull();
        expect(t.pair!.evidenceB!.amount! >= t.pair!.minTouchB!).toBe(true);
      } else {
        expect(t.evidence.kind).toBe('KobPair');
        expect(t.pair!.evidenceB).toBeNull();
      }
      expect(t.evidence.amount! >= t.rule.minTouch).toBe(true);
      if (effect === 'trail') expect(t.trail!.steps >= 1n).toBe(true);
    });
  }

  it('evidence that is not of the pair, not filled or of an unknown mode is blocking', () => {
    // mode 1 pointing at the order's own custody input (not a pair order)
    const b = builtOf('pair.cond.ask.stop.ev1');
    const i = orderInput(b);
    setInt(b, i, 7, 3);
    expect(triggerCodes(dec(b))).toEqual(['trigger-evidence-invalid']);
    // an evidence mode other than 0 / 1
    const c = builtOf('pair.cond.ask.stop.ev1');
    setInt(c, orderInput(c), 10, 2);
    const sc = dec(c);
    expect(triggerCodes(sc)).toEqual(['trigger-evidence-invalid']);
    expect(sc.triggers[0]!.pair!.mode).toBe('invalid');
    // mode 0 with the B leg pointing at the A leg (the same token twice)
    const d = builtOf('pair.cond.ask.stop.ev0');
    const j = orderInput(d);
    setInt(d, j, 8, Number(entry(d, j).args[7]!.value));
    expect(triggerCodes(dec(d))).toEqual(['trigger-evidence-invalid']);
  });

  it('a trailing ratchet the evidence does not justify (one step more than maximal) is blocking', () => {
    const b = builtOf('pair.update.ask.trail.ev1');
    const i = orderInput(b);
    const k = BigInt(entry(b, i).args[12]!.value);
    setInt(b, i, 12, k + 1n);
    const s = dec(b);
    expect(triggerCodes(s)).toEqual(['trigger-rule-unmet']);
    expect(s.triggers[0]!.checks.price).toBe(false);
  });

  it('a resting pair order on the wrong side does not arm a pair stop (side rule)', () => {
    // the buy stop's evidence is a pair BID; naming the ASK leg of the same transaction instead breaks the side rule
    const b = builtOf('pair.cond.bid.stop.ev1');
    const i = orderInput(b);
    const s0 = dec(b);
    const ev = s0.triggers[0]!.evidence.input;
    const other = b.plans.findIndex((p, k) => k !== ev && k !== i && p.kind === 'entry' && p.template === 'KobPair');
    setInt(b, i, 7, other);
    const s = dec(b);
    expect(s.triggers[0]!.checks.side === false || s.triggers[0]!.checks.custody === false).toBe(true);
    expect(triggerCodes(s).some((c) => c === 'trigger-rule-unmet' || c === 'trigger-evidence-invalid')).toBe(true);
  });

  it('describes the pair trigger rule of a stop from kob-wasm', () => {
    const st = (goldenTx('pair.create.condAsk').request as unknown as { order: OrderState }).order;
    const d = describeOrder(st, kob);
    expect(d.pair!.triggerRule).toMatchObject({ kind: 'KobCondPair', direction: 'fallsTo' });
    expect(pairFactsOf(st)!.side).toBe('sell');
  });
});
