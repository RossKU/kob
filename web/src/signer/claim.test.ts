// The signer frame on claims (bid-delivery-spec §7) and on delivery terms: it decodes a claim
// itself, names it like a merge (title and lines: what is claimed, where it goes, the carrier
// added, the fee), refuses a tampered one, signs a valid one only on Approve, and binds the
// delivery terms of an order (a re-time or an update that changes a tip or threshold is shown
// again).
import { describe, expect, it } from 'vitest';
import { createSignerCore } from './core';
import { SIGNER_PROTOCOL, parseAppMessage, type SignerToApp } from './protocol';
import { sameOrBetter } from './retime';
import { summarize } from './summary';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { decodeSigning } from '../kob/decode';
import type { BuiltTx, OrderState } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { MAKER, OTHER, TOKEN, goldenRequest, nodeFactsOf } from '../testing/chain-fixtures';
import { BOUGHT, askOf, claim } from '../testing/claim-fixtures';

const kob = loadKobNode();
const HINTS = [{ covenantId: TOKEN.covenantId, ticker: 'TST', decimals: 3 }];
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const line = (s: ReturnType<typeof summarize>, label: RegExp) => s.lines.find((l) => label.test(l.label))?.value;

/**
 * The claim's continuation re-derived from a changed price: the order would continue with other
 * terms.
 */
function tampered(built: BuiltTx, os: OrderState): BuiltTx {
  const b = clone(built);
  const next = clone(os);
  Object.assign(next.state, { bought: '0', price: (BigInt((next.state as unknown as { price: string }).price) + 1n).toString() });
  b.tx.outputs[0]!.scriptPublicKey = kob.scriptPublicKey(next);
  return b;
}

describe('signer summary: claims', () => {
  it('a claim says what is claimed, from which order, where it goes, the carrier added and the fee', () => {
    const { built, snap } = claim('create.bid.acc');
    const s = summarize(kob, built, MAKER.pk, HINTS);
    expect(s.problems).toBe(0);
    expect(s.title).toBe('Claim bought tokens');
    expect(line(s, /^Claims from order$/)).toBe(`${snap.covenantId.slice(0, 8)}…${snap.covenantId.slice(-8)}`);
    expect(line(s, /bought by the order$/)).toBe('0.7 TST');
    expect(line(s, /^Goes to$/)).toBe('you, as one UTXO');
    expect(line(s, /carrier of its next custody/)).toMatch(/ KAS$/);
    expect(s.fee).toMatch(/ KAS$/);
    // the continuation is no new order, and nothing closes
    expect(s.orders).toEqual([]);
    expect(s.lines.some((l) => l.label === 'Closes order')).toBe(false);
  });

  it('claim and merge, claim and place: their titles and where the tokens go', () => {
    const merge = summarize(kob, claim('create.bid.acc', { tokens: [{ ...claimTokens()[0]! }] }).built, MAKER.pk, HINTS);
    expect(merge.title).toBe('Claim bought tokens and merge');
    expect(line(merge, /^Goes to$/)).toBe('you, as one UTXO');
    const place = summarize(kob, claim('create.bid.acc', { place: askOf(500n) }).built, MAKER.pk, HINTS);
    expect(place.problems).toBe(0);
    expect(place.title).toBe('Claim bought tokens and place an order');
    expect(line(place, /^Goes to$/)).toBe('0.5 TST into your new order, 0.2 TST to you');
    expect(place.orders).toHaveLength(1);
    const all = summarize(kob, claim('create.bid.acc', { place: askOf(BOUGHT) }).built, MAKER.pk, HINTS);
    expect(line(all, /^Goes to$/)).toBe('your new order');
  });

  it('a tampered claim is a problem, and the same claim read for another key is no claim of its own', () => {
    const { built, snap } = claim('create.bid.acc');
    const bad = summarize(kob, tampered(built, snap.order.state), MAKER.pk, HINTS);
    expect(bad.problems).toBeGreaterThan(0);
    expect(bad.problemCodes).toContain('claim-invalid');
    const other = summarize(kob, built, OTHER.pk, HINTS);
    expect(other.problemCodes).toContain('order-not-maker');
    expect(other.lines.some((l) => /bought by the order$/.test(l.label))).toBe(false);
  });
});

/** A plain token UTXO of the maker to merge. */
function claimTokens() {
  return [{ transactionId: '4e'.repeat(32), index: 1, amount: '20000000', blockDaaScore: '1200', covenantId: TOKEN.covenantId, state: { amount: '300', owner: MAKER.pk, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: TOKEN.ext } }];
}

describe('signer summary and re-time: delivery terms', () => {
  const topup = kob.build(goldenRequest('create.bid.topup', MAKER.pk));
  const otherTip = (() => {
    const req = goldenRequest('create.bid.topup', MAKER.pk) as unknown as { order: { state: Record<string, string> } };
    req.order.state.topupTip = (BigInt(req.order.state.topupTip!) - 1n).toString();
    return kob.build(req as never);
  })();

  it('a placement says its delivery; a top-up its tip and threshold', () => {
    const rows = summarize(kob, topup, MAKER.pk, HINTS).orders[0]!.rows;
    expect(rows.find((r) => r.id === 'delivery')?.value).toBe('Added to one UTXO');
    expect(rows.find((r) => r.id === 'topupTip')?.value).toMatch(/ KAS$/);
    expect(rows.find((r) => r.id === 'recvThreshold')).toBeDefined();
    const acc = summarize(kob, kob.build(goldenRequest('create.bid.acc', MAKER.pk)), MAKER.pk, HINTS).orders[0]!.rows;
    expect(acc.find((r) => r.id === 'delivery')?.value).toBe('Collected, sent at the end');
    expect(acc.some((r) => r.id === 'topupTip')).toBe(false);
  });

  it('another tip reads differently and is never taken as a re-time of the approved one', () => {
    const a = summarize(kob, topup, MAKER.pk, HINTS);
    const b = summarize(kob, otherTip, MAKER.pk, HINTS);
    expect(b.orders[0]!.rows.find((r) => r.id === 'topupTip')!.value).not.toBe(a.orders[0]!.rows.find((r) => r.id === 'topupTip')!.value);
    const d = (x: BuiltTx) => decodeSigning({ kob, built: x, maker: MAKER.pk, registry: null, nodeInputs: nodeFactsOf(x) });
    expect(sameOrBetter(d(topup), d(topup))).toBe(true);
    expect(sameOrBetter(d(topup), d(otherTip))).toBe(false);
  });

  it('a re-timed claim whose tokens go elsewhere is not the approved claim', () => {
    const d = (x: BuiltTx) => decodeSigning({ kob, built: x, maker: MAKER.pk, registry: null, nodeInputs: nodeFactsOf(x) });
    const plain = claim('create.bid.acc', { place: askOf(500n) }).built;
    const more = claim('create.bid.acc', { place: askOf(600n) }).built;
    expect(sameOrBetter(d(plain), d(plain))).toBe(true);
    expect(sameOrBetter(d(plain), d(more))).toBe(false);
  });
});

describe('signer core: a claim request', () => {
  const sdk = loadKaspaSdkNode();
  const setup = () => {
    const sent: SignerToApp[] = [];
    const core = createSignerCore({ sdk, kob, post: (m) => sent.push(structuredClone(m)) });
    core.importKey(MAKER.sk);
    const send = (m: Record<string, unknown>) => core.receive(parseAppMessage(structuredClone({ p: SIGNER_PROTOCOL, ...m }))!);
    return { core, sent, send };
  };
  const req = (built: BuiltTx, id = 'c1') => ({ type: 'sign', id, built, tokens: HINTS });

  it('is shown decoded as a claim and signed only on approval', () => {
    const { built } = claim('create.bid.acc');
    const { core, sent, send } = setup();
    const p = send(req(built))!;
    expect(p.summary.title).toBe('Claim bought tokens');
    expect(p.summary.problems).toBe(0);
    expect(sent.some((m) => m.type === 'signed')).toBe(false);
    expect(core.approve('c1')).toBe(true);
    expect(sent.find((m) => m.type === 'signed')).toMatchObject({ id: 'c1', txid: built.tx.id });
  });

  it('a tampered claim cannot be approved, and an update that tampers with a shown claim is held', () => {
    const { built, snap } = claim('create.bid.acc');
    const bad = tampered(built, snap.order.state);
    const a = setup();
    a.send(req(bad));
    expect(a.core.approve('c1')).toBe(false);
    expect(a.sent.some((m) => m.type === 'signed')).toBe(false);
    const b = setup();
    b.send(req(built));
    const u = b.send({ type: 'update', id: 'c1', built: bad, tokens: HINTS })!;
    expect(u.change).toBe('shown');
    expect(u.summary.problemCodes).toContain('claim-invalid');
    expect(b.core.approve('c1')).toBe(false);
  });
});
