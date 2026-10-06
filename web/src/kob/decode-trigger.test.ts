// Pre-sign decode of the trigger evidence (touch, protocol v2.6): a conditional order or stop entry armed or trailed by a transaction shows the
// input its `ev` argument names, that input's side / quote / size / rest, and whether it satisfies the order's rule. The transactions are the
// golden vectors' own built transactions (crates/kob-protocol/vectors/golden.json, consensus-checked by the Rust suite); the tamper cases edit
// the entry arguments of the built plans the way a buggy or hostile builder could.
import { describe, expect, it } from 'vitest';
import { decodeSigning, fillArg, type SigningSummary } from './decode';
import { loadKobNode } from './wasm.node';
import type { BuiltTx, SigPlan } from './types';
import { goldenTx, nodeFactsOf } from '../testing/chain-fixtures';

const kob = loadKobNode();
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const builtOf = (name: string): BuiltTx => clone(goldenTx(name).built as BuiltTx);
/** A matcher batch pays other makers, so it is never "ok" for an arbitrary wallet key: these tests look at the trigger findings only. */
const decode = (built: BuiltTx): SigningSummary => decodeSigning({ kob, built, maker: built.sign[0]?.pubkey ?? '00'.repeat(32), registry: null, nodeInputs: nodeFactsOf(built) });
const triggerCodes = (s: SigningSummary): string[] => [...s.blocking, ...s.warnings, ...s.info].map((i) => i.code).filter((c) => c.startsWith('trigger-'));
const entryPlan = (b: BuiltTx, i: number): Extract<SigPlan, { kind: 'entry' }> => {
  const p = b.plans[i]!;
  if (p.kind !== 'entry') throw new Error(`input ${i} is not a covenant entry`);
  return p;
};
const setInt = (b: BuiltTx, input: number, arg: number, value: bigint | number) => {
  entryPlan(b, input).args[arg] = { kind: 'int', value: String(value) };
};

describe('trigger evidence of a fill that arms (settle / fill of an unarmed stop leg or stop entry)', () => {
  it('a stop sell armed in its fill shows the evidence ask, its custody, quote, size and rest, and satisfies the rule', () => {
    const b = builtOf('cond.ask.stop.trigger');
    const s = decode(b);
    expect(s.triggers).toHaveLength(1);
    const t = s.triggers[0]!;
    expect(t).toMatchObject({ input: 0, kind: 'KobCondAsk', path: 'fill', effect: 'arm', trail: null, ok: true });
    expect(t.rule.sides).toEqual(['ask']);
    // inputs: the order, the evidence ask, the order's custody (the token leader), the evidence's custody
    expect(t.evidence).toMatchObject({ input: 1, custodyInput: 3, kind: 'KobAsk', side: 'ask' });
    expect(t.evidence.covenantId).toBe(b.tx.inputs[1]!.utxo.covenantId);
    expect(Object.values(t.checks).every(Boolean)).toBe(true);
    // the evidence quote is at or below the stop; its size is the fill argument n of the evidence (base units) and reaches minTouch
    expect(t.evidence.price! <= t.rule.stop).toBe(true);
    expect(t.evidence.amount).toBe(fillArg(entryPlan(b, 1).args[0]));
    expect(t.evidence.amount! >= t.rule.minTouch).toBe(true);
    // rested long enough before the lock time (CLTV)
    expect(t.evidence.exposedSince! + t.rule.minRestDaa <= BigInt(b.tx.lockTime)).toBe(true);
    // on the spend of that order, and as an info finding (never blocking)
    expect(s.spends.find((x) => x.input === 0)!.trigger).toEqual(t);
    expect(triggerCodes(s)).toEqual(['trigger-evidence']);
    expect(s.info.find((i) => i.code === 'trigger-evidence')).toMatchObject({ input: 0, params: { evidence: 1 } });
  });

  it('every arm-in-fill vector (both families: stop sell / buy, stop-limit, stop entries) decodes to a satisfied trigger', () => {
    const cases: [string, string, 'ask' | 'bid'][] = [
      ['cond.ask.stopLimit.trigger', 'KobCondAsk', 'ask'], ['cond.bid.stop.trigger', 'KobCondBid', 'bid'], ['ifd.bid.stopEntry.trigger', 'KobIfdBid', 'bid'],
      ['ifd.ask.stopEntry.trigger', 'KobIfdAsk', 'ask'], ['rpt.bid.stopLoss', 'KobCondAsk', 'ask'], ['kron.cond.ask.stop.trigger', 'KobCondAskKron', 'ask'],
      ['kron.cond.bid.stop.trigger', 'KobCondBidKron', 'bid'], ['kron.ifd.bid.stopEntry.trigger', 'KobIfdBidKron', 'bid'], ['kron.ifd.ask.stopEntry.trigger', 'KobIfdAskKron', 'ask'],
    ];
    for (const [name, kind, side] of cases) {
      const s = decode(builtOf(name));
      expect(s.triggers, name).toHaveLength(1);
      expect(s.triggers[0], name).toMatchObject({ input: 0, kind, path: 'fill', ok: true, evidence: { input: 1, side, custodyInput: side === 'ask' ? 3 : null } });
      expect(triggerCodes(s), name).toEqual(['trigger-evidence']);
    }
  });

  it('a take-profit fill, an armed stop and a plain fill arm nothing', () => {
    for (const name of ['cond.ask.takeProfit.partial', 'cond.ask.stop.auction', 'take.ask.partial', 'ifd.bid.partial']) {
      const s = decode(builtOf(name));
      expect(s.triggers, name).toEqual([]);
      expect(triggerCodes(s), name).toEqual([]);
    }
  });
});

describe('trigger evidence of an update (arm or trail next to the evidence fill, no fill of the order)', () => {
  it('an arm by update names the evidence ask and its custody', () => {
    const b = builtOf('cond.ask.update.arm');
    const s = decode(b);
    expect(s.triggers).toHaveLength(1);
    expect(s.triggers[0]).toMatchObject({
      input: 2, kind: 'KobCondAsk', path: 'update', effect: 'arm', ok: true, rule: { sides: ['ask', 'bid'] },
      evidence: { input: 0, custodyInput: 1, kind: 'KobAsk', side: 'ask' },
    });
    expect(s.spends.find((x) => x.input === 2)).toMatchObject({ action: 'update', trigger: { input: 2 } });
    expect(triggerCodes(s)).toEqual(['trigger-evidence']);
  });

  it('a trail by update reads a resting bid (sell stop) or ask (buy stop) and moves the stop by whole steps', () => {
    const up = decode(builtOf('cond.ask.update.trail')).triggers[0]!;
    expect(up).toMatchObject({ kind: 'KobCondAsk', path: 'update', effect: 'trail', ok: true, evidence: { input: 0, side: 'bid', custodyInput: null } });
    expect(up.trail!.steps >= 1n).toBe(true);
    expect(up.trail!.newStop > up.rule.stop).toBe(true);
    const down = decode(builtOf('cond.bid.update.trail')).triggers[0]!;
    expect(down).toMatchObject({ kind: 'KobCondBid', path: 'update', effect: 'trail', ok: true, evidence: { input: 0, side: 'ask', custodyInput: 1 } });
    expect(down.trail!.newStop < down.rule.stop).toBe(true);
  });

  it('stop entries are armed by update on their own side only; several stops may read one batch', () => {
    const bid = decode(builtOf('ifd.bid.update.arm')).triggers[0]!;
    expect(bid).toMatchObject({ kind: 'KobIfdBid', path: 'update', effect: 'arm', ok: true, rule: { sides: ['bid'] }, evidence: { side: 'bid', custodyInput: null } });
    const ask = decode(builtOf('ifd.ask.update.arm')).triggers[0]!;
    expect(ask).toMatchObject({ kind: 'KobIfdAsk', path: 'update', effect: 'arm', ok: true, rule: { sides: ['ask'] }, evidence: { side: 'ask', custodyInput: 1 } });
    for (const name of ['cond.ask.update.armAndTrail.oneBatch', 'kron.cond.ask.update.armAndTrail.oneBatch']) {
      const s = decode(builtOf(name));
      expect(s.triggers.map((t) => [t.input, t.effect, t.evidence.input, t.ok]), name).toEqual([[3, 'arm', 0, true], [4, 'trail', 1, true]]);
    }
    const batch = decode(builtOf('match.batch.3x5.8x8.arm'));
    expect(batch.triggers).toHaveLength(1);
    expect(batch.triggers[0]).toMatchObject({ input: 13, path: 'update', effect: 'arm', ok: true, evidence: { side: 'ask' } });
  });
});

describe('invalid trigger evidence is a blocking finding', () => {
  it('an evidence index that is not a plain resting order of the token (a token input, the order itself, out of range) is blocking', () => {
    for (const ev of [1, 2, 99, -1]) {
      const b = builtOf('cond.ask.update.arm');
      setInt(b, 2, 0, ev);
      const s = decode(b);
      expect(s.triggers[0], `ev ${ev}`).toMatchObject({ ok: false, checks: { plain: false } });
      expect(s.blocking.map((i) => i.code), `ev ${ev}`).toContain('trigger-evidence-invalid');
      expect(s.ok).toBe(false);
    }
    // a stop entry filled with its evidence index on the stop entry's own token input
    const e = builtOf('ifd.bid.stopEntry.trigger');
    setInt(e, 0, 5, 2);
    expect(decode(e).blocking.map((i) => i.code)).toContain('trigger-evidence-invalid');
  });

  it('an ask evidence with a custody index that is not its custody is blocking', () => {
    const b = builtOf('cond.ask.stop.trigger');
    // input 2 is the conditional order's OWN custody (owned by the order, not by the evidence ask, whose custody is input 3)
    setInt(b, 0, 5, 2);
    const s = decode(b);
    const t = s.triggers[0]!;
    expect(t.checks.custody).toBe(false);
    expect(s.blocking.map((i) => i.code)).toContain('trigger-evidence-invalid');
  });

  it('a wrong-side evidence (a resting bid where the order needs a resting ask) is blocking', () => {
    const b = builtOf('match.batch.3x5.8x8.arm');
    // the arm reads input 0, a KobBid fill of the batch, as if it were an ask (its tk still names a custody)
    setInt(b, 13, 0, 0);
    const s = decode(b);
    const t = s.triggers[0]!;
    expect(t).toMatchObject({ input: 13, ok: false, evidence: { input: 0, side: 'bid', kind: 'KobBid' }, checks: { plain: true, side: false } });
    expect(s.blocking.map((i) => i.code)).toContain('trigger-rule-unmet');
    expect(s.blocking.find((i) => i.code === 'trigger-rule-unmet')!.message).toMatch(/resting bid, but the order needs a resting ask/);
  });

  it('a stop entry fill whose evidence is on the wrong side (a sell-stop entry reading a resting bid) is blocking', () => {
    const b = builtOf('ifd.ask.stopEntry.trigger');
    // KobIfdAsk.settle(nb, tokenIn, tokOut, exitOut, cPre, cSuf, ev, tk, t): tk < 0 names a bid, which a sell-stop entry never accepts
    setInt(b, 0, 7, -1);
    const s = decode(b);
    expect(s.triggers[0]!.checks.side).toBe(false);
    expect(s.blocking.map((i) => i.code)).toContain('trigger-rule-unmet');
  });

  it('evidence below the order threshold (minTouch) is blocking; exactly minTouch is enough', () => {
    const b = builtOf('cond.ask.update.arm');
    const p = entryPlan(b, 2);
    const st = kob.decodeState(p.template, p.state);
    (st.state as unknown as Record<string, string>).minTouch = '1000000000';
    p.state = kob.encodeState(st);
    const s = decode(b);
    expect(s.triggers[0]!.checks.amount).toBe(false);
    expect(s.blocking.map((i) => i.code)).toContain('trigger-rule-unmet');
    // the boundary: a minTouch of exactly the evidence fill passes, one base unit more does not
    const n = decode(builtOf('cond.ask.update.arm')).triggers[0]!.evidence.amount!;
    for (const [minTouch, ok] of [[n, true], [n + 1n, false]] as const) {
      const b2 = builtOf('cond.ask.update.arm');
      const p2 = entryPlan(b2, 2);
      const st2 = kob.decodeState(p2.template, p2.state);
      (st2.state as unknown as Record<string, string>).minTouch = minTouch.toString();
      p2.state = kob.encodeState(st2);
      expect(decode(b2).triggers[0]!.checks.amount, String(minTouch)).toBe(ok);
    }
  });
});

describe('fillArg', () => {
  it('reads the 8-byte little-endian fill argument; a merge argument (top bit) or refund (0) is no fill', () => {
    expect(fillArg({ kind: 'bytes', value: '0300000000000000' })).toBe(3n);
    expect(fillArg({ kind: 'bytes', value: '0000000000000000' })).toBe(0n);
    expect(fillArg({ kind: 'bytes', value: '0500000001000080' })! < 0n).toBe(true);
    expect(fillArg({ kind: 'int', value: '3' })).toBeNull();
    expect(fillArg({ kind: 'bytes', value: '03' })).toBeNull();
  });
});
