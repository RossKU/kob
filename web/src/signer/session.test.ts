// The private-key signer's one-approval merge session (session.ts, core.ts): one Allow covers exactly the plan the frame decoded, each step
// is signed without a prompt only as the next link of that plan, and anything else ends the session and is shown for confirmation as usual.
// Every link is built by kob-wasm (kob/merge.ts), signed by the frame with the official SDK and run through the script engine.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createSignerCore } from './core';
import { SIGNER_PROTOCOL, parseAppMessage, parseSignerMessage, type SignerToApp } from './protocol';
import {
  SESSION_FEE_CAP, SESSION_MAX_MS, SESSION_MAX_TXS, SESSION_TX_FEE_CAP, admitStep, mergeFacts, openState, recordStep, sessionTerms, stepsLeft,
  type MergeFacts, type SessionTerms,
} from './session';
import { summarize } from './summary';
import { tableHash, type RegistryRow } from './table';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAX_MERGE_TXS, buildMergeLink, mergeCarry, planMerge, type MergeLink, type MergePlan, type MergeToken } from '../kob/merge';
import { keyState } from '../kob/token-state';
import type { BuiltTx, Kcc20State, TokenUtxo } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { MAKER, OTHER, TOKEN, goldenRequest, keyUtxo, tokenUtxo } from '../testing/chain-fixtures';

const kob = loadKobNode();
const sdk = loadKaspaSdkNode();
const P = SIGNER_PROTOCOL;
const KAS = 100_000_000n;
const STD: MergeToken = { covenantId: TOKEN.covenantId, program: TOKEN.program, templateHash: TOKEN.templateHash, extensionCommitment: TOKEN.ext, slots: { inputs: 3 } };
const HINTS = [{ covenantId: TOKEN.covenantId, ticker: 'TUSD', decimals: 8 }];
// the registry table the bundle pins: the frame shows the ticker of a listed token only from it
const TABLE: RegistryRow[] = [{ covenantId: TOKEN.covenantId, ticker: 'TUSD', decimals: 8 }];
const FUNDING = [keyUtxo(MAKER.pk, 50n * KAS, 201)];
const env = (funding = FUNDING) => ({ kob, maker: MAKER.pk, funding });
/** `n` of the maker's UTXOs, 1, 2, ... tokens, 2 KAS carriers, outpoints 0x40.. */
const utxos = (n: number, at = 0x40): TokenUtxo[] => Array.from({ length: n }, (_, i) => tokenUtxo(BigInt(i + 1) * KAS, MAKER.pk, at + i, 2n * KAS));
const clone = <T>(v: T): T => structuredClone(v);

function plan7(): MergePlan {
  const p = planMerge(env(), STD, utxos(7));
  expect(p.ok, JSON.stringify(p.issues)).toBe(true);
  return p;
}
const facts = (built: BuiltTx): MergeFacts | { refused: string } => mergeFacts(kob, summarize(kob, built, MAKER.pk, HINTS).decoded, built, MAKER.pk);
const ok = (built: BuiltTx): MergeFacts => {
  const f = facts(built);
  if ('refused' in f) throw new Error(`refused: ${f.refused}`);
  return f;
};
const termsOf = (p: MergePlan): SessionTerms => {
  const t = sessionTerms(p.links.map((l) => ({ built: l.built, facts: ok(l.built) })));
  if ('refused' in t) throw new Error(t.refused);
  return t;
};
/** the merge link `l` built again from `inputs` (as the app does at its turn), with the request edited by `edit` */
const rebuilt = (l: MergeLink, inputs: TokenUtxo[], edit: (r: MergeLink['request']) => void = () => undefined): BuiltTx => {
  const req = clone({ ...l.request, tokens: inputs });
  edit(req);
  return kob.build(req);
};

describe('session rules: what a merge link is', () => {
  it('every link of a planned merge is admitted; the terms are derived from the plan', () => {
    const p = plan7();
    expect(p.links).toHaveLength(3);
    const t = termsOf(p);
    expect(t.links.map((l) => l.parents)).toEqual([[], [], [0, 1]]);
    expect(t.utxos).toBe(7);
    expect(t.outputs).toBe(1);
    expect(t.amount).toBe(28n * KAS);
    expect(t.plannedFee).toBe(p.fee);
    const largest = p.links.reduce((m, l) => (l.fee > m ? l.fee : m), 0n);
    expect(t.perTxFeeMax).toBe(2n * largest + 100_000n);
    expect(t.feeMax).toBe(2n * p.fee + 3n * 100_000n);
    expect(t.ttlMs).toBe(120_000 + 3 * 10_000);
  });

  it('the hard caps hold whatever the plan, and the transaction cap is the merge planner\'s', () => {
    expect(SESSION_MAX_TXS).toBe(MAX_MERGE_TXS);
    expect(SESSION_TX_FEE_CAP).toBe(20_000_000n);
    expect(SESSION_FEE_CAP).toBe(500_000_000n);
    expect(SESSION_MAX_MS).toBe(600_000);
    const p = plan7();
    const links = p.links.map((l) => ({ built: l.built, facts: ok(l.built) }));
    // a plan whose fee is above the per-transaction hard cap is refused, not capped
    const rich = links.map((l) => ({ ...l, facts: { ...l.facts, fee: SESSION_TX_FEE_CAP + 1n } }));
    expect(sessionTerms(rich)).toEqual({ refused: 'plan-fee' });
    expect(sessionTerms([])).toEqual({ refused: 'plan-size' });
    expect(sessionTerms(Array.from({ length: SESSION_MAX_TXS + 1 }, () => links[0]!))).toEqual({ refused: 'plan-size' });
  });

  it('refuses an inconsistent plan: an outpoint spent twice, two tokens, a parent that is not an earlier link', () => {
    const p = plan7();
    const links = p.links.map((l) => ({ built: l.built, facts: ok(l.built) }));
    expect(sessionTerms([links[0]!, links[0]!])).toEqual({ refused: 'plan-reuse' });
    const other = { ...links[1]!, facts: { ...links[1]!.facts, covenantId: 'ab'.repeat(32) } };
    expect(sessionTerms([links[0]!, other])).toEqual({ refused: 'plan-token' });
    // the level-2 link before its parents
    expect(sessionTerms([links[2]!, links[0]!, links[1]!])).toEqual({ refused: 'plan-parent' });
  });

  it('refuses what is not a pure self-merge: a token send to another key, an order cancel, a KRON merge (unsigned token inputs)', () => {
    const l = plan7().links[0]!;
    const toOther = rebuilt(l, l.inputs, (r) => {
      r.recipients = [{ pubkey: OTHER.pk, amount: (1n * KAS).toString(), carrier: (1n * KAS).toString() }];
    });
    expect(facts(toOther)).toEqual({ refused: 'blocking' });
    expect(facts(kob.build(goldenRequest('cancel.ask', MAKER.pk)))).toHaveProperty('refused');
    // KRON: one approval per transaction (its token inputs carry no signature, so their amounts are not committed)
    const kronTpl = kob.templates().find((x) => x.name === 'KronToken2433')!;
    const KRON: MergeToken = { covenantId: 'e6'.repeat(32), program: 'KronToken2433', templateHash: kronTpl.hash, extensionCommitment: null, slots: { inputs: kronTpl.tokenSlots![0] } };
    const kron = Array.from({ length: 3 }, (_, i) => ({
      transactionId: (0xa0 + i).toString(16).repeat(32), index: 0, amount: (2n * KAS).toString(), blockDaaScore: '500', covenantId: KRON.covenantId,
      state: keyState('kron', BigInt(100 * (i + 1)), MAKER.pk, null),
    })) as TokenUtxo[];
    const kp = planMerge(env([keyUtxo(MAKER.pk, 5n * KAS, 211)]), KRON, kron);
    expect(kp.ok, JSON.stringify(kp.issues)).toBe(true);
    expect(mergeFacts(kob, summarize(kob, kp.links[0]!.built, MAKER.pk, []).decoded, kp.links[0]!.built, MAKER.pk)).toEqual({ refused: 'unsigned' });
  });

  it('refuses a carrier above the merge rule, a payload, a changed token amount and a fee much above the minimum', () => {
    const l = plan7().links[0]!;
    // the merged UTXO would keep 1.5 KAS of the wallet (the rule: the largest input carrier, at most 1 KAS)
    expect(facts(rebuilt(l, l.inputs, (r) => void (r.tokenChangeCarrier = (150_000_000n).toString())))).toEqual({ refused: 'carrier' });
    const withPayload = clone(l.built);
    withPayload.tx.payload = 'aa';
    expect(facts(withPayload)).toHaveProperty('refused');
    const fewer = clone(l.built);
    const leader = fewer.plans.find((x) => x.kind === 'tokenLeader') as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' }>;
    leader.nextStates[0] = { ...leader.nextStates[0]!, amount: '1' };
    expect(facts(fewer)).toHaveProperty('refused');
    // a fee much above the minimum (fee-high), or any other warning but the two a frame without a node or registry always gets
    const d = summarize(kob, l.built, MAKER.pk, HINTS).decoded;
    expect(d.warnings.map((w) => w.code).sort()).toEqual(['inputs-unverified', 'token-unlisted']);
    expect(mergeFacts(kob, { ...d, warnings: [...d.warnings, { code: 'fee-high', severity: 'warning', message: '' }] }, l.built, MAKER.pk)).toEqual({ refused: 'warning' });
  });

  it('option A: a step whose token inputs carry two borrow settings is blocked by the decoder and never admitted', () => {
    const held = (n: number): TokenUtxo => {
      const u = tokenUtxo(BigInt(n) * KAS, MAKER.pk, 0x70 + n, 2n * KAS);
      return { ...u, state: { ...u.state, borrow_scheme: 2, borrow_guard: OTHER.pk } as Kcc20State };
    };
    const p = planMerge(env(), STD, [held(1), held(2)]);
    expect(p.ok).toBe(true);
    // one setting: a merge (the change keeps it)
    expect(facts(p.links[0]!.built)).not.toHaveProperty('refused');
    const mixed = clone(p.links[0]!.built);
    const i = mixed.tx.inputs.findIndex((x) => x.utxo.covenantId === TOKEN.covenantId);
    const plan = mixed.plans[i] as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' }>;
    const plain = { ...plan.state, borrow_scheme: 0, borrow_guard: '00'.repeat(32) };
    plan.state = plain;
    mixed.tx.inputs[i]!.utxo.scriptPublicKey = kob.tokenScriptPublicKey(plan.template, plain);
    expect(summarize(kob, mixed, MAKER.pk, HINTS).decoded.blocking.map((b) => b.code)).toContain('token-state-mixed');
    expect(facts(mixed)).toEqual({ refused: 'blocking' });
  });
});

describe('session rules: each step against the allowed plan', () => {
  const run = () => {
    const p = plan7();
    const s = openState('aa'.repeat(16), MAKER.pk, termsOf(p), 1000);
    return { p, s };
  };

  it('admits the plan link by link, with the outputs of the links signed before', () => {
    const { p, s } = run();
    for (let k = 0; k < 2; k++) {
      const f = ok(p.links[k]!.built);
      expect(admitStep(s, f, 1000)).toBeNull();
      recordStep(s, f, `${k}`.repeat(64));
    }
    const third = p.links[2]!;
    const inputs = [mergeCarry(p.links[0]!, '0'.repeat(64), STD), mergeCarry(p.links[1]!, '1'.repeat(64), STD), ...third.fresh];
    const f = ok(rebuilt(third, inputs));
    expect(admitStep(s, f, 1000)).toBeNull();
    recordStep(s, f, 'ff'.repeat(32));
    expect(stepsLeft(s)).toBe(0);
    // the count is exhausted: nothing more
    expect(admitStep(s, ok(p.links[0]!.built), 1000)).toBe('count');
  });

  it('a step that deviates from the plan is refused: another UTXO after a re-read, another parent, another carrier', () => {
    const { p, s } = run();
    const first = p.links[0]!;
    // the re-read found another UTXO of the same token in place of an allowed one
    const swapped = [...first.inputs.slice(0, 2), tokenUtxo(9n * KAS, MAKER.pk, 0x99, 2n * KAS)];
    expect(admitStep(s, ok(rebuilt(first, swapped)), 1000)).toBe('deviates');
    // the allowed UTXO with another KAS amount than at approval
    const moved = [...first.inputs.slice(0, 2), { ...first.inputs[2]!, amount: (3n * KAS).toString() }];
    expect(admitStep(s, ok(rebuilt(first, moved)), 1000)).toBe('deviates');
    // the second link first (the run keeps the plan's order)
    expect(admitStep(s, ok(p.links[1]!.built), 1000)).toBe('deviates');
    // a lower carrier on the merged output than planned
    expect(admitStep(s, ok(rebuilt(first, first.inputs, (r) => void (r.tokenChangeCarrier = (50_000_000n).toString()))), 1000)).toBe('deviates');
    for (let k = 0; k < 2; k++) recordStep(s, ok(p.links[k]!.built), `${k}`.repeat(64));
    // the level-2 link spending another output than the ones this session signed
    const third = p.links[2]!;
    const forged = [mergeCarry(p.links[0]!, '0'.repeat(64), STD), mergeCarry(p.links[1]!, '7'.repeat(64), STD), ...third.fresh];
    expect(admitStep(s, ok(rebuilt(third, forged)), 1000)).toBe('deviates');
  });

  it('a replayed step is refused (its inputs are spent), as is a step after expiry or past the fee caps', () => {
    const { p, s } = run();
    const f = ok(p.links[0]!.built);
    recordStep(s, f, '0'.repeat(64));
    expect(admitStep(s, f, 1000)).toBe('consumed');
    const g = ok(p.links[1]!.built);
    expect(admitStep(s, g, s.expiresAt)).toBe('expired');
    expect(admitStep(s, { ...g, fee: s.terms.perTxFeeMax + 1n }, 1000)).toBe('fee-cap');
    s.feeLeft = g.fee - 1n;
    expect(admitStep(s, g, 1000)).toBe('fee-budget');
  });
});

// ------------------------------------------------------------------------------------------------ the frame's core

function setup(at = { now: 0 }) {
  const sent: SignerToApp[] = [];
  const core = createSignerCore({ sdk, kob, post: (m) => sent.push(structuredClone(m)), now: () => at.now, registryHash: tableHash(TABLE) });
  const send = (m: Record<string, unknown>) => {
    const parsed = parseAppMessage(structuredClone({ p: P, ...m }));
    if (!parsed) throw new Error(`not protocol: ${JSON.stringify(m).slice(0, 80)}`);
    return core.receive(parsed);
  };
  const app = () => sent.map((m) => parseSignerMessage(m));
  const last = () => sent.at(-1)!;
  core.importKey(MAKER.sk);
  return { core, sent, send, app, last, at };
}
const open = (p: MergePlan, id = 'o1') => ({ type: 'session-open', id, plan: p.links.map((l) => l.built), tokens: HINTS, registry: TABLE });
const step = (built: BuiltTx, id: string, session?: string) => ({ type: 'sign', id, built, tokens: HINTS, ...(session ? { session } : {}) });
type Signed = Extract<SignerToApp, { type: 'signed' }>;
const finalId = (built: BuiltTx, m: SignerToApp): string => kob.finalize(built, (m as Signed).signatures, { tightenBudgets: true }).tx.id;

/** opens a session over a 7-UTXO plan; returns its id */
function allowed(c: ReturnType<typeof setup>, p: MergePlan): string {
  const req = c.send(open(p));
  expect(req?.session).toBeDefined();
  expect(c.sent.filter((m) => m.type === 'signed' || m.type === 'session-opened')).toEqual([]);
  expect(c.core.approve('o1')).toBe(true);
  const opened = c.last() as Extract<SignerToApp, { type: 'session-opened' }>;
  expect(opened.type).toBe('session-opened');
  expect(c.app().at(-1)).toEqual(opened); // the app's parser takes it
  return opened.sid;
}

describe('signer core: one-approval session', () => {
  it('Allow signs nothing; then the whole plan is signed step by step without a prompt, and the session closes when done', () => {
    const c = setup();
    const p = plan7();
    const req = c.send(open(p))!;
    // the screen: the frame's own terms of the plan
    expect(req.summary.title).toBe('Merge TUSD UTXOs without asking each time');
    expect(req.summary.lines.find((l) => l.label === 'What')?.value).toBe('7 of your UTXOs into 1, to yourself. 28 TUSD in total, unchanged.');
    expect(req.summary.lines.find((l) => l.label === 'Transactions')?.value).toBe('at most 3');
    expect(req.summary.lines.find((l) => l.label === 'Valid for')?.value).toBe('2 min 30 s, in this tab only');
    expect(req.summary.fee).toMatch(/^at most [0-9.]+ KAS in total \(planned [0-9.]+ KAS\)$/);
    expect(c.core.approve('o1')).toBe(true);
    const sid = (c.last() as Extract<SignerToApp, { type: 'session-opened' }>).sid;
    expect(sid).toMatch(/^[0-9a-f]{32}$/);
    expect(c.sent.some((m) => m.type === 'signed')).toBe(false);

    const ids: string[] = [];
    for (let k = 0; k < 2; k++) {
      expect(c.send(step(p.links[k]!.built, `s${k}`, sid))).toBeNull();
      expect(c.core.pending()).toBeNull();
      const m = c.last();
      expect(m.type).toBe('signed');
      ids.push(finalId(p.links[k]!.built, m));
    }
    const third = p.links[2]!;
    const built = buildMergeLink(env(), STD, [mergeCarry(p.links[0]!, ids[0]!, STD), mergeCarry(p.links[1]!, ids[1]!, STD), ...third.fresh], third.fresh, new Set(), third);
    if ('error' in built) throw new Error(built.error.message);
    expect(c.send(step(built.built, 's2', sid))).toBeNull();
    const signed = c.sent.at(-2) as Signed;
    expect(signed.type).toBe('signed');
    expect(() => kob.validate(kob.finalize(built.built, signed.signatures, { tightenBudgets: true }))).not.toThrow();
    expect(c.last()).toEqual({ p: P, type: 'session-closed', sid, reason: 'done' });
    expect(c.core.session()).toBeNull();
    // the approval is not reusable: the same sid now gets a normal confirmation
    expect(c.send(step(p.links[0]!.built, 'again', sid))?.id).toBe('again');
  });

  it('a deviating step ends the session and is shown for confirmation; the next step prompts too', () => {
    const c = setup();
    const p = plan7();
    const sid = allowed(c, p);
    const first = p.links[0]!;
    const swapped = rebuilt(first, [...first.inputs.slice(0, 2), tokenUtxo(9n * KAS, MAKER.pk, 0x99, 2n * KAS)]);
    const req = c.send(step(swapped, 's0', sid));
    expect(req?.id).toBe('s0');
    expect(c.sent.at(-1)).toEqual({ p: P, type: 'session-closed', sid, reason: 'refused' });
    expect(c.sent.some((m) => m.type === 'signed')).toBe(false);
    c.core.reject('s0');
    expect(c.send(step(p.links[0]!.built, 's1', sid))?.id).toBe('s1');
    expect(c.sent.some((m) => m.type === 'signed')).toBe(false);
  });

  it('a replayed step (the same transaction twice) is refused and prompts', () => {
    const c = setup();
    const p = plan7();
    const sid = allowed(c, p);
    c.send(step(p.links[0]!.built, 's0', sid));
    expect(c.last().type).toBe('signed');
    expect(c.send(step(p.links[0]!.built, 's0b', sid))?.id).toBe('s0b');
    expect(c.sent.at(-1)).toEqual({ p: P, type: 'session-closed', sid, reason: 'refused' });
  });

  it('a wrong session id, a request without one, an expired session: each prompts and ends the session', () => {
    const at = { now: 0 };
    const c = setup(at);
    const p = plan7();
    const sid = allowed(c, p);
    expect(c.send(step(p.links[0]!.built, 'w', 'ab'.repeat(16)))?.id).toBe('w');
    expect(c.sent.at(-1)).toEqual({ p: P, type: 'session-closed', sid, reason: 'refused' });
    c.core.reject('w');

    const d = setup();
    const sid2 = allowed(d, p);
    expect(d.send(step(p.links[0]!.built, 'n'))?.id).toBe('n');
    expect(d.sent.at(-1)).toEqual({ p: P, type: 'session-closed', sid: sid2, reason: 'closed' });

    const e = setup(at);
    const sid3 = allowed(e, p);
    at.now = 150_000;
    expect(e.send(step(p.links[0]!.built, 'x', sid3))?.id).toBe('x');
    expect(e.sent.at(-1)).toEqual({ p: P, type: 'session-closed', sid: sid3, reason: 'expired' });
  });

  it('a plan that is not a merge (KRON, a send to another key) is refused: the run then asks per step', () => {
    const c = setup();
    const l = plan7().links[0]!;
    const toOther = rebuilt(l, l.inputs, (r) => void (r.recipients = [{ pubkey: OTHER.pk, amount: (1n * KAS).toString(), carrier: (1n * KAS).toString() }]));
    expect(c.send({ type: 'session-open', id: 'o1', plan: [toOther], tokens: HINTS })).toBeNull();
    expect(c.last()).toEqual({ p: P, type: 'rejected', id: 'o1', reason: 'invalid' });
    expect(c.core.session()).toBeNull();
  });

  it('the session is cleared by a reject, forget, a new key and the app\'s close; busy while the screen is open', () => {
    const p = plan7();
    const a = setup();
    a.send(open(p));
    expect(a.send(step(p.links[0]!.built, 'b'))).toBeNull();
    expect(a.last()).toEqual({ p: P, type: 'rejected', id: 'b', reason: 'busy' });
    a.core.reject('o1');
    expect(a.last()).toEqual({ p: P, type: 'rejected', id: 'o1', reason: 'user' });
    expect(a.core.session()).toBeNull();

    for (const end of ['reject', 'forget', 'import', 'close'] as const) {
      const c = setup();
      const sid = allowed(c, p);
      if (end === 'reject') {
        // a normal request rejected while the session is live ends it
        c.send(step(kob.build(goldenRequest('cancel.ask', MAKER.pk)), 'q'));
        c.core.reject('q');
      } else if (end === 'forget') c.send({ type: 'forget' });
      else if (end === 'import') c.core.importKey(OTHER.sk);
      else c.send({ type: 'session-close', sid });
      expect(c.core.session(), end).toBeNull();
      expect(c.sent.some((m) => m.type === 'session-closed' && m.sid === sid), end).toBe(true);
    }
  });
});

describe('signer core: a session touches no storage and never sends the key', () => {
  const touched: string[] = [];
  const names = ['localStorage', 'sessionStorage', 'indexedDB', 'caches'] as const;
  const saved: Record<string, PropertyDescriptor | undefined> = {};
  beforeEach(() => {
    touched.length = 0;
    for (const n of names) {
      saved[n] = Object.getOwnPropertyDescriptor(globalThis, n);
      Object.defineProperty(globalThis, n, { configurable: true, get: () => new Proxy({}, { get: (_t, k) => (touched.push(`${n}.${String(k)}`), () => null) }) });
    }
  });
  afterEach(() => {
    for (const n of names) {
      if (saved[n]) Object.defineProperty(globalThis, n, saved[n]!);
      else Reflect.deleteProperty(globalThis, n);
    }
  });

  it('open, allow, steps, close', () => {
    const spy = vi.spyOn(console, 'log');
    const c = setup();
    const p = plan7();
    const sid = allowed(c, p);
    c.send(step(p.links[0]!.built, 's0', sid));
    c.send({ type: 'session-close', sid });
    expect(touched).toEqual([]);
    const wire = JSON.stringify(c.sent);
    expect(wire).not.toContain(MAKER.sk);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });
});
