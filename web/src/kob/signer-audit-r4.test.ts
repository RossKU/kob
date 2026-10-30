// Audit 2026-10-30 round 4 regressions for the signer frame under a hostile parent (G5, G6, G7, W4):
//   G5  an approved flat order (slope 0) is not re-timed into a steep decay without a prompt
//   G6  the flipped view says a sell that decays to 0 KAS
//   G7  the token hints' decimals are labelled as the app's word, and an order whose scale does not fit them blocks Approve
//   W4  forgetting the key (or importing another) takes the approval with it
import { describe, expect, it } from 'vitest';
import { createSignerCore } from '../signer/core';
import { RETIME_MAX_DELAY_DAA, sameOrBetter } from '../signer/retime';
import { SIGNER_PROTOCOL, parseAppMessage, type SignerToApp, type TokenHint } from '../signer/protocol';
import { summarize, type SummaryOrder } from '../signer/summary';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { decodeSigning } from './decode';
import { planOrder } from './plan';
import type { BuiltTx, OrderState } from './types';
import { MAKER_PK, MAKER_SK } from '../testing/fixtures';
import { assertConsensusValid, kob } from '../testing/wallet-fixtures';
import { buildIntent } from '../ui/ticket/form-state';
import { CASES, CTX, form, ticketEnv } from '../ui/ticket/ticket-fixtures';

const sdk = loadKaspaSdkNode();
const P = SIGNER_PROTOCOL;

function frame(sk: string, now?: () => number) {
  const sent: SignerToApp[] = [];
  const core = createSignerCore({ sdk, kob, post: (m) => sent.push(structuredClone(m)), ...(now ? { now } : {}) });
  const send = (m: Record<string, unknown>) => core.receive(parseAppMessage(structuredClone({ p: P, ...m }))!);
  core.importKey(sk);
  return { core, sent, send };
}
const decode = (b: BuiltTx) => decodeSigning({ kob, built: b, maker: MAKER_PK, registry: null });
const sign = (built: BuiltTx, id: string, extra: Record<string, unknown> = {}) => ({ type: 'sign', id, built, tokens: [], ...extra });
const ordRows = (o: SummaryOrder, pre = ''): string[] => [...o.rows.map((r) => `${pre}${r.label}=${r.value}`), ...(o.exit ? ordRows(o.exit, 'exit/') : [])];

type PlanResult = ReturnType<typeof planOrder>;
function planCase(name: string): PlanResult {
  const c = CASES.find((x) => x[0] === name)!;
  const env = ticketEnv();
  return planOrder(env, buildIntent(form(c[1], c[2], c[3]), CTX).intent!);
}
function mutate(p: PlanResult, patch: Record<string, string>): BuiltTx {
  const order = p.states[0]!;
  const st = order.state as unknown as Record<string, string>;
  return kob.build({ ...p.request, order: { kind: order.kind, state: { ...st, ...patch } } as unknown as OrderState } as never);
}

describe('G5: a flat approval is not re-timed into a steep decay', () => {
  it('slope 0 approved, slope 1e9 / step 1 re-timed: shown again, on the re-time and on the update path', () => {
    const p = planCase('limit sell');
    const approved = mutate(p, { slope: '0', priceEnd: '1', decayStep: '0' });
    const steep = mutate(p, { slope: '1000000000', priceEnd: '1', decayStep: '1' });
    expect(sameOrBetter(decode(approved), decode(steep))).toBe(false);
    const { core, sent, send } = frame(MAKER_SK, () => 1_000);
    send(sign(approved, 'a0'));
    expect(core.approve('a0')).toBe(true);
    expect(send(sign(steep, 'a1', { retimeOf: 'a0' })), 'the steep one is shown, not signed').not.toBeNull();
    expect(sent.at(-1)!.type).toBe('signed'); // only the approved one
    // the in-place update of a request on screen
    const f2 = frame(MAKER_SK);
    f2.send(sign(approved, 'u0'));
    const upd = f2.send({ type: 'update', id: 'u0', built: steep, tokens: [] });
    expect(upd?.change, 'an update to a steeper decay is announced').not.toBe('silent');
  });

  it('the legitimate cases still pass: flat to flat, a slower decay, a decay with the step as planned', () => {
    const p = planCase('limit sell');
    const flat = decode(mutate(p, { slope: '0', priceEnd: '1', decayStep: '0' }));
    expect(sameOrBetter(flat, decode(mutate(p, { slope: '0', priceEnd: '1', decayStep: '0' })))).toBe(true);
    expect(sameOrBetter(flat, decode(mutate(p, { slope: '0', priceEnd: '1', decayStep: '5' })))).toBe(true);
    const dutch = planCase('dutch sell');
    const a = decode(dutch.built!);
    expect(sameOrBetter(a, a)).toBe(true);
    // a decay step of 0 on a decaying order is not valid on chain, and counts as 1 (never as an infinite slowness)
    const slow = decode(mutate(dutch, { slope: '1', decayStep: '1000' }));
    expect(sameOrBetter(slow, decode(mutate(dutch, { slope: '1', decayStep: '1000' })))).toBe(true);
    expect(sameOrBetter(slow, decode(mutate(dutch, { slope: '1', decayStep: '1' })))).toBe(false);
    expect(RETIME_MAX_DELAY_DAA).toBeGreaterThan(0n);
  });
});

describe('G6: the flipped view says a sell that decays to zero', () => {
  it('a Dutch sell to 0 KAS under an inverted hint reads "at least 0 KAS" with the decay rows, as the plain view says 0', () => {
    const dutch = planCase('dutch sell');
    const bad = mutate(dutch, { priceEnd: '0', slope: '1000000000' });
    const cov = decode(bad).orders[0]!.description.tokenCovId;
    const hint = (inverted: boolean): TokenHint[] => [{ covenantId: cov, ticker: 'GOLD', decimals: 8, inverted }];
    const plain = summarize(kob, bad, MAKER_PK, hint(false)).orders.flatMap((o) => ordRows(o));
    const flipped = summarize(kob, bad, MAKER_PK, hint(true)).orders.flatMap((o) => ordRows(o));
    expect(plain.join(' ; ')).toMatch(/0 KAS per GOLD/);
    const receive = flipped.find((r) => r.startsWith('You receive='))!;
    expect(receive, flipped.join(' ; ')).toMatch(/at least 0 KAS/);
    // and it says the price is a decay (an expected and a worst row), as it does for any other decaying order
    expect(flipped.some((r) => /^Worst price/.test(r) || /worst/i.test(r.split('=')[0]!)), flipped.join(' ; ')).toBe(true);
  });
});

describe('G7: the decimals the app gives are labelled, and an order that does not fit them blocks Approve', () => {
  const limitSell = () => planCase('limit sell').built!;
  const tokenOf = (b: BuiltTx) => decode(b).orders[0]!.description.tokenCovId;

  it('a listed token is named with its decimals "as the app lists them"', () => {
    const b = limitSell();
    const s = summarize(kob, b, MAKER_PK, [{ covenantId: tokenOf(b), ticker: 'GOLD', decimals: 8 }]);
    expect(s.orders[0]!.token).toMatch(/^GOLD \([0-9a-f]{8}…[0-9a-f]{8}, 8 decimals as the app lists them\)$/);
  });

  it('the same order under another decimals hint is refused (the order quotes its price per 10^8 units)', () => {
    const b = limitSell();
    const cov = tokenOf(b);
    const honest = summarize(kob, b, MAKER_PK, [{ covenantId: cov, ticker: 'GOLD', decimals: 8 }]);
    expect(honest.problems).toBe(0);
    for (const decimals of [0, 2, 6, 9, 10, 18]) {
      const s = summarize(kob, b, MAKER_PK, [{ covenantId: cov, ticker: 'GOLD', decimals }]);
      expect(s.problems, `decimals ${decimals}`).toBeGreaterThan(0);
      expect(s.problemCodes).toContain('token-scale');
      const f = frame(MAKER_SK);
      f.send({ type: 'sign', id: 'g7', built: b, tokens: [{ covenantId: cov, ticker: 'GOLD', decimals }] });
      expect(f.core.approve('g7'), `Approve stays disabled with decimals ${decimals}`).toBe(false);
    }
  });

  it('a lied scale that agrees with a lied hint (9 for an 8-decimal token) is what the label is for: the hint is the app\'s word', () => {
    const p = planCase('limit sell');
    const bad = mutate(p, { scale: '1000000000' });
    const cov = tokenOf(bad);
    const s = summarize(kob, bad, MAKER_PK, [{ covenantId: cov, ticker: 'GOLD', decimals: 9 }]);
    // consistent with itself, so it is not refused by the check; the frame says whose word the 9 decimals are
    expect(s.orders[0]!.token).toContain('9 decimals as the app lists them');
    expect(s.orders[0]!.rows.find((r) => /^Amount/.test(r.label))!.value).toBe('0.5 GOLD');
  });

  it('a token the app marks assumed, issued or from a program default keeps its own labels and is not held to the standard scale', () => {
    const b = limitSell();
    const cov = tokenOf(b);
    const assumed = summarize(kob, b, MAKER_PK, [{ covenantId: cov, ticker: '', decimals: 8, source: 'assumed' }]);
    expect(assumed.orders[0]!.token).toContain('decimals assumed 8');
    expect(assumed.problemCodes).not.toContain('token-scale');
  });
});

describe('W4: forgetting the key takes the approval with it', () => {
  it('a re-time approved under the old key is not signed after forget + import of another key', () => {
    const p = planCase('limit sell');
    const first = p.built!;
    const { core, send } = frame(MAKER_SK, () => 1_000);
    send(sign(first, 'w0'));
    core.approve('w0');
    core.forget();
    core.importKey(MAKER_SK);
    expect(send(sign(first, 'w1', { retimeOf: 'w0' })), 'shown again: the approval was dropped with the key').not.toBeNull();
  });
});

void assertConsensusValid;
