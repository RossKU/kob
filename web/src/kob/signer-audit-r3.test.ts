// Audit 2026-10-30 regressions for the signer frame (G1, G2, G3): the keeper tip of an if-done stop entry is a row, the seconds of an auction and
// the band / touch sizes are in the frame's values, and a silent re-time pins the decay slope and moves an expiry only with the start.
import { describe, expect, it } from 'vitest';
import { createSignerCore } from '../signer/core';
import { RETIME_MAX_DELAY_DAA, sameOrBetter } from '../signer/retime';
import { SIGNER_PROTOCOL, parseAppMessage, type SignerToApp } from '../signer/protocol';
import { summarize } from '../signer/summary';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { decodeSigning } from './decode';
import { planOrder } from './plan';
import type { BuiltTx, OrderState } from './types';
import { MAKER_PK, MAKER_SK } from '../testing/fixtures';
import { kob } from '../testing/wallet-fixtures';
import { buildIntent } from '../ui/ticket/form-state';
import { CASES, CTX, form, ticketEnv } from '../ui/ticket/ticket-fixtures';
import { makePairEnv } from './orders/pair-fixtures';

const sdk = loadKaspaSdkNode();
const P = SIGNER_PROTOCOL;

function frame(sk: string, now?: () => number) {
  const sent: SignerToApp[] = [];
  const core = createSignerCore({ sdk, kob, post: (m) => sent.push(structuredClone(m)), ...(now ? { now } : {}) });
  const send = (m: Record<string, unknown>) => core.receive(parseAppMessage(structuredClone({ p: P, ...m }))!);
  core.importKey(sk);
  return { core, sent, send };
}
const at = (daa: bigint, o: Parameters<typeof ticketEnv>[0] = {}) => {
  const env = ticketEnv(o);
  return { ...env, clock: { ...env.clock, daa: env.clock.daa + daa, unixSeconds: env.clock.unixSeconds + daa / 10n } };
};
const decode = (b: BuiltTx) => decodeSigning({ kob, built: b, maker: MAKER_PK, registry: null });
const plan = (intent: ReturnType<typeof buildIntent>['intent'], daa = 0n) => {
  const p = planOrder(at(daa), intent!);
  if (!p.built) throw new Error('plan failed: ' + JSON.stringify(p.issues.map((i) => i.code)));
  return p.built;
};
const sign = (built: BuiltTx, id: string, extra: Record<string, unknown> = {}) => ({ type: 'sign', id, built, tokens: [], ...extra });
const rowsOf = (b: BuiltTx) => summarize(kob, b, MAKER_PK, []).orders.flatMap((o) => o.rows.map((r) => `${r.label}=${r.value}`));

describe('G3: a silent re-time pins the decay slope and ties the expiry to the start', () => {
  it('a Dutch sell with another decay slope is not taken without asking', () => {
    const slow = buildIntent(form('dutch', 'sell', { amount: '5', price: '2.8', priceEnd: '2.6', duration: '10' }), CTX).intent!;
    const fast = buildIntent(form('dutch', 'sell', { amount: '5', price: '2.8', priceEnd: '2.6', duration: '1' }), CTX).intent!;
    const a = plan(slow);
    const b = plan(fast);
    expect(decode(b).orders[0]!.description.auction!.slope).toBeGreaterThan(decode(a).orders[0]!.description.auction!.slope * 5n);
    expect(sameOrBetter(decode(a), decode(b))).toBe(false);
    const { core, send } = frame(MAKER_SK, () => 1_000);
    send(sign(a, 'r0'));
    core.approve('r0');
    expect(send(sign(b, 'r1', { retimeOf: 'r0' }))).not.toBeNull(); // shown again
  });

  it('an explicit expiry that moves alone by minutes is not taken without asking; the same order planned seconds later is', () => {
    const gtd = (at_: string) => buildIntent(form('limit', 'sell', { amount: '5', price: '2.6', lifetime: 'gtd', lifetimeAt: at_ }), CTX).intent!;
    const a = plan(gtd('2026-10-05T00:00'));
    const b = plan(gtd('2026-10-05T00:59'));
    const da = decode(a).orders[0]!.description;
    const db = decode(b).orders[0]!.description;
    expect(db.activeFrom).toBe(da.activeFrom);
    expect(db.expiryDaa - da.expiryDaa).toBeGreaterThan(RETIME_MAX_DELAY_DAA);
    expect(sameOrBetter(decode(a), decode(b))).toBe(false);
    const market = buildIntent(form('market', 'buy', { amount: '2' }), CTX).intent!;
    expect(sameOrBetter(decode(plan(market)), decode(plan(market, 40n)))).toBe(true);
    expect(sameOrBetter(decode(plan(market)), decode(plan(market, RETIME_MAX_DELAY_DAA + 1n)))).toBe(false);
  });
});

describe('G1: the keeper tip of an if-done stop entry is a row of the frame, plain and pair', () => {
  const forCase = (name: string) => {
    const c = CASES.find((x) => x[0] === name)!;
    const p = planOrder(at(0n), buildIntent(form(c[1], c[2], c[3]), CTX).intent!);
    if (!p.ok || !p.built || !p.request) throw new Error('plan ' + name);
    return p;
  };
  it('KobIfdBid stop entry: raising the tip changes a row, and the row names it', () => {
    const p = forCase('ifd stop entry');
    const order = p.states[0]!;
    const st = order.state as unknown as Record<string, string>;
    const tip = (BigInt(p.request!.value) / 2n).toString();
    const built = kob.build({ ...p.request, order: { kind: order.kind, state: { ...st, keeperTip: tip } } as unknown as OrderState } as never);
    const base = rowsOf(p.built!);
    const bad = rowsOf(built);
    expect(base.some((r) => r.startsWith('Keeper tip='))).toBe(true);
    expect(bad).not.toEqual(base);
    expect(bad.find((r) => r.startsWith('Keeper tip='))).toContain('KAS');
  });

  it('KobIfdPair stop entry: the tip is a row too', () => {
    const p = planOrder(makePairEnv(), { type: 'ifd', side: 'buy', amount: 8_000n, entry: { price: 1_600n, stop: 1_550n }, exit: { stop: 1_450n } } as never);
    if (!p.ok || !p.built || !p.request || !p.states[0]) throw new Error('plan');
    const order = p.states[0];
    const st = order.state as unknown as Record<string, string>;
    const built = kob.build({ ...p.request, order: { kind: order.kind, state: { ...st, keeperTip: (BigInt(st.keeperTip ?? '0') + 12_345n).toString() } } as unknown as OrderState } as never);
    expect(rowsOf(built)).not.toEqual(rowsOf(p.built));
    expect(rowsOf(built).some((r) => r.startsWith('Keeper tip='))).toBe(true);
  });
});

describe('G2: the frame says the auction seconds and the pair trigger sizes', () => {
  it('two Dutch orders that differ only in duration read differently', () => {
    const dutch = (duration: string) => plan(buildIntent(form('dutch', 'sell', { amount: '5', price: '2.8', priceEnd: '2.6', duration }), CTX).intent!);
    const a = rowsOf(dutch('10'));
    const b = rowsOf(dutch('1'));
    expect(a).not.toEqual(b);
    expect(a.find((r) => r.startsWith('Auction ends at'))).toMatch(/price moves over \d+/);
  });

  it('a pair stop names the touch sizes in the trigger row', () => {
    const p = planOrder(makePairEnv(), { type: 'stopMarket', side: 'sell', amount: 8_000n, stop: 1_400n } as never);
    if (!p.ok || !p.built) throw new Error('plan');
    const trigger = summarize(kob, p.built, MAKER_PK, []).orders[0]!.rows.find((r) => r.id === 'pairTrigger');
    expect(trigger?.value).toMatch(/\(.*\)$/);
  });
});
