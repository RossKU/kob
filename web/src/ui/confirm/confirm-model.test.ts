// The decoded-order vocabulary (confirm-model.ts) on the golden transactions: `orderRows` / `orderTitle` of each decoded order, token labels and
// amounts, the order type re-derived from the state, and the decoder's own verdicts the silent pre-sign checks rely on.
import { describe, expect, it } from 'vitest';
import { decodeSigning, describeInputsForWallet, type DecodedOrder, type DecodedSpend, type ExpectedSigning, type SigningSummary, type TokenRef } from '../../kob/decode';
import { parseRegistry, type TokenRegistry } from '../../kob/registry';
import type { ActionRequest, BuiltTx, Hex } from '../../kob/types';
import { MAKER, goldenRequest, goldenTx, tradableRegistryJson, nodeFactsOf } from '../../testing/chain-fixtures';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { orderRows, orderTitle, orderTypeCode, tokenAmountText, tokenLabel, type Fmt, type Row } from './confirm-model';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const CLOCK = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };
const KAS = 100_000_000n;

const build = (name: string): BuiltTx => kob.build(goldenRequest<ActionRequest>(name, MAKER.pk));
const decode = (built: BuiltTx, o: { registry?: TokenRegistry | null; expected?: ExpectedSigning } = {}): SigningSummary =>
  decodeSigning({ kob, built, maker: MAKER.pk, registry: o.registry === undefined ? registry : o.registry, expected: o.expected, nodeInputs: nodeFactsOf(built) });

/** A token as the registry knows it (or as an unknown one: no ticker, no decimals). */
function refOf(reg: TokenRegistry | null, covenantId: Hex): TokenRef {
  const info = reg?.byCovenantId.get(covenantId);
  return info
    ? { covenantId, ticker: info.ticker, decimals: info.decimals, display: '', inRegistry: true, tradable: info.tradable }
    : { covenantId, ticker: null, decimals: null, display: '', inRegistry: false, tradable: false };
}
const fmtOf = (reg: TokenRegistry | null, covenantId: Hex): Fmt => ({ tr: (k, p) => t(k, p), ref: refOf(reg, covenantId), clock: CLOCK, time: (unix) => `@${unix}`, registry: reg });

/** The rows and title of the n-th created order, as the signer frame builds them. */
function created(name: string, reg: TokenRegistry | null = registry, n = 0): { summary: SigningSummary; order: DecodedOrder; rows: Row[]; title: string; f: Fmt } {
  const summary = decode(build(name), { registry: reg });
  const order = summary.orders[n]!;
  const f = fmtOf(reg, order.description.tokenCovId);
  return { summary, order, rows: orderRows(order.description, { value: order.value, locked: order.locked, deadline: order.deadline }, f), title: orderTitle(order.description, order.deadline, f), f };
}
/** The rows and title of a spent order (cancel, refund, sweep). */
const spent = (s: DecodedSpend, reg: TokenRegistry | null = registry): { rows: Row[]; title: string } => {
  const f = fmtOf(reg, s.description!.tokenCovId);
  return { rows: orderRows(s.description!, { value: null, locked: null, deadline: null }, f), title: orderTitle(s.description!, null, f) };
};
const rowOf = (rows: Row[], id: string): Row => {
  const r = rows.find((x) => x.id === id);
  if (!r) throw new Error(`no row ${id}: ${rows.map((x) => x.id).join(',')}`);
  return r;
};
const codes = (xs: { code: string }[]) => xs.map((x) => x.code);

/** Every string orderRows / orderTitle / tokenLabel produce for a decoded summary, for leftover-key and placeholder checks. */
function strings(summary: SigningSummary, reg: TokenRegistry | null = registry): string[] {
  const out: string[] = [];
  const add = (rows: Row[], title: string) => out.push(title, ...rows.flatMap((r) => [r.label, r.value, r.detail ?? '']));
  for (const o of summary.orders) {
    const f = fmtOf(reg, o.description.tokenCovId);
    add(orderRows(o.description, { value: o.value, locked: o.locked, deadline: o.deadline }, f), orderTitle(o.description, o.deadline, f));
    if (o.description.entry?.exit) add(orderRows(o.description.entry.exit, { value: null, locked: null, deadline: null }, f), orderTitle(o.description.entry.exit, null, f));
  }
  for (const s of summary.spends) if (s.description) add(spent(s, reg).rows, spent(s, reg).title);
  for (const n of summary.net.tokens) out.push(tokenLabel(n.ref, (k, p) => t(k, p), reg), tokenAmountText(n.ref, n.walletDelta));
  return out;
}

const GOLDEN = [
  'create.ask', 'create.ask.twap', 'create.ask.dutch', 'create.ask.market', 'create.ask.day', 'create.bid', 'create.bid.dca', 'create.bid.market',
  'create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdBid.stopEntry', 'create.ifdBid.repeat', 'create.ifdAsk', 'create.ifdAsk.repeat',
  'cancel.ask', 'cancel.ask.sweepStray', 'cancel.bid', 'cancel.condAsk', 'cancel.ifdBid', 'cancel.position.repeatBuyFirst', 'cancel.position.repeatSellFirst',
  'cancelReplace.ask', 'cancelReplace.bid', 'refund.ask.expiry', 'refund.bid', 'refund.condAsk', 'refund.ifdBid', 'send.tokens',
];

describe('decoded order terms', () => {
  it('a limit sell: quantity, price, all-in, escrow and the KAS that is locked', () => {
    const { summary: s, order, rows, title } = created('create.ask');
    expect(s.kind).toBe('create');
    expect(s.ok).toBe(true);
    expect(s.blocking).toEqual([]);
    expect(title).toBe('Sell limit order');
    expect(order.verified).toBe(true);
    expect(rowOf(rows, 'amount').value).toBe('10 TST');
    expect(rowOf(rows, 'minFill').value).toMatch(/ TST$/);
    // golden token TST has 3 decimals (scale 1000 base units): 250_000_000 sompi per whole token = 2.5 KAS per token
    expect(rowOf(rows, 'price').value).toBe('2.5 KAS per TST');
    expect(rowOf(rows, 'tip').value).toBe('0.001 KAS per TST');
    expect(rowOf(rows, 'allIn').label).toBe('You receive at least per token (all-in)');
    expect(rowOf(rows, 'allIn').value).toBe('2.499 KAS per TST');
    const k = s.net.kas;
    expect(k.locked.total).toBe(20n * KAS);
    expect(k.locked.refundTips).toBe(3_500_000n);
    expect(k.locked.carriers).toBeGreaterThan(0n);
    // the wallet's KAS delta is the locked amount plus the fee
    expect(k.walletDelta).toBeLessThan(-20n * KAS);
    expect(k.walletDelta).toBeGreaterThan(-21n * KAS);
    expect(k.toOthers).toBe(0n);
    const tok = s.net.tokens.find((n) => n.ref.covenantId === order.description.tokenCovId)!;
    const text = (v: bigint) => tokenAmountText(tok.ref, v);
    expect(text(tok.escrowed)).toBe('10 TST');
    expect(text(tok.toMaker)).toBe('2 TST');
    expect(text(tok.fromWallet)).toBe('12 TST');
    expect(text(tok.walletDelta)).toBe('-10 TST');
    expect(tok.toOthers).toBe(0n);
  });

  it('a plain bid shows its KAS budget and what it covers', () => {
    const { summary, rows, title } = created('create.bid');
    expect(title).toBe('Buy limit order');
    expect(rowOf(rows, 'budget').detail).toMatch(/^Covers the amount at the limit price/);
    expect(rowOf(rows, 'allIn').label).toBe('You pay at most per token (all-in)');
    expect(summary.net.kas.locked.escrow).toBeGreaterThan(0n);
  });

  it('day orders show the 00:00 UTC deadline in UTC and JST', () => {
    const { rows, title } = created('create.ask.day');
    expect(title).toBe('Sell day limit order');
    expect(rowOf(rows, 'deadline').value).toBe('2026-09-30 00:00 UTC / 2026-09-30 09:00 JST');
  });

  it('an if-done entry shows its exit, and repeat entries say how often they re-arm', () => {
    const { order, rows, title, f } = created('create.ifdBid.repeat');
    expect(title).toMatch(/repeat IF[DO] entry/);
    const exit = order.description.entry!.exit!;
    expect(orderTitle(exit, null, f)).toMatch(/^Sell /);
    expect(rows.some((r) => r.id === 'repeat')).toBe(true);
    expect(rowOf(rows, 'minFill').value).toMatch(/ TST$/);
    const stopEntry = created('create.ifdBid.stopEntry');
    expect(stopEntry.rows.some((r) => r.id === 'entryStop')).toBe(true);
    expect(stopEntry.title).toMatch(/stop entry/);
  });

  it('conditional orders show trigger, band, exposure and keeper tip', () => {
    const { rows, title } = created('create.condAsk');
    expect(title).toMatch(/^Sell (stop order|OCO order|trailing stop|take-profit order|OCO order with trailing stop)/);
    expect(rows.some((r) => r.id === 'stop')).toBe(true);
    // R-4: the trigger direction (a sell stop arms on resting sells at or below it) and the worst fill price after the trigger
    expect(rowOf(rows, 'exposure').value).toMatch(/at least [\d.]+ TST of a sell order resting at or below the stop for \d+ s/);
    expect(rowOf(rows, 'stopWorst').detail).toMatch(/between the stop and this price/);
    const buy = created('create.condBid').rows;
    expect(rowOf(buy, 'exposure').value).toMatch(/buy order resting at or above the stop/);
    expect(rowOf(buy, 'stopWorst').value).not.toBe(rowOf(buy, 'stop').value);
  });

  it('TWAP / DCA / Dutch / market describe their schedule and auction', () => {
    expect(created('create.ask.twap').rows.some((r) => r.id === 'schedule')).toBe(true);
    expect(created('create.bid.dca').title).toMatch(/DCA/);
    expect(created('create.ask.dutch').rows.some((r) => r.id === 'auction')).toBe(true);
    const mk = created('create.ask.market');
    expect(mk.title).toMatch(/market order/);
    expect(rowOf(mk.rows, 'tif').value).toMatch(/immediate or cancel/);
  });
});

describe('decoded spends and verdicts', () => {
  it('cancel: what is closed, what comes back, nothing leaves', () => {
    const s = decode(build('cancel.ask'));
    expect(s.kind).toBe('cancel');
    expect(s.spends).toHaveLength(1);
    expect(s.spends[0]).toMatchObject({ action: 'cancel', makerIsWallet: true });
    expect(spent(s.spends[0]!).title).toMatch(/^Sell /);
    expect(s.spends[0]!.tokensReleased).toBeGreaterThan(0n);
    expect(s.net.kas.toOthers).toBe(0n);
    expect(s.net.tokens.every((n) => n.toOthers === 0n)).toBe(true);
    expect(s.orders).toHaveLength(0);
  });

  it('a cancel that sweeps stray tokens counts them once: the custody is what remains of the released tokens', () => {
    const summary = decode(build('cancel.ask.sweepStray'));
    const s = summary.spends[0]!;
    expect(s.strays).toBeGreaterThan(0n);
    expect(s.tokensReleased).toBeGreaterThan(s.strays);
    const net = summary.net.tokens.find((n) => n.ref.covenantId === s.description!.tokenCovId)!;
    expect(net.released).toBe(s.tokensReleased);
  });

  it('cancel-replace closes one order and creates another', () => {
    const s = decode(build('cancelReplace.ask'));
    expect(s.kind).toBe('cancel-replace');
    expect(s.spends).toHaveLength(1);
    expect(s.orders).toHaveLength(1);
  });

  it('a position cancel lists every order it closes', () => {
    const s = decode(build('cancel.position.repeatBuyFirst'));
    expect(s.kind).toBe('cancel-position');
    expect(s.spends.length).toBeGreaterThan(1);
  });

  it('a refund is a refund', () => {
    expect(decode(build('refund.ask.expiry')).kind).toBe('refund');
  });

  it('a token transfer to another key is blocking and counted as going to other keys', () => {
    const s = decode(build('send.tokens'));
    expect(s.ok).toBe(false);
    expect(codes(s.blocking)).toContain('transfer-out');
    expect(s.net.tokens.some((n) => n.toOthers > 0n)).toBe(true);
  });

  it('a planner claim that the transaction does not meet blocks (kasLocked mismatch)', () => {
    const s = decode(build('create.ask'), { expected: { kasLocked: 1n } });
    expect(s.ok).toBe(false);
    expect(codes(s.blocking)).toContain('expected-kas-locked');
  });

  it('the transaction identity and the wallet notices come from the decoder', () => {
    const built = build('create.ask');
    const s = decode(built);
    expect(s.inputs).toHaveLength(2);
    expect(s.txid).toMatch(/^[0-9a-f]{64}$/);
    expect(s.signatures).toBe(2);
    expect(describeInputsForWallet(built, 'kasware').notices.some((n) => n.code === 'blind-tokens')).toBe(true);
  });
});

describe('token labels and order types', () => {
  it('never shows a bare ticker: registry tokens carry their id, others are "unknown token"', () => {
    const ref = { covenantId: '70'.repeat(32), ticker: 'TST', decimals: 3, display: '', inRegistry: true, tradable: true };
    expect(tokenLabel(ref, (k, p) => t(k, p), registry)).toBe('TST (7070...7070)');
    expect(tokenLabel({ ...ref, covenantId: 'ab'.repeat(32) }, (k, p) => t(k, p), registry)).toBe('unknown token (abababab…abababab)');
    // an unknown token: whole tokens on the assumed 8 decimals, said so (never "base units")
    const { summary, rows } = created('create.ask', null);
    expect(rowOf(rows, 'amount').value).toContain('0.0001 70707070…70707070 (decimals assumed 8)');
    expect(rows.some((r) => r.value.includes(' / '))).toBe(false);
    expect(codes(summary.warnings)).toContain('token-unlisted');
    expect(tokenAmountText({ ...ref, decimals: null, ticker: null }, 500_000_000n, (k, p) => t(k, p))).toBe('5 70707070…70707070 (decimals assumed 8)');
  });

  it('classifies order types from the state, not from anything the form claimed', () => {
    const d = (name: string) => decode(build(name)).orders[0]!;
    expect(orderTypeCode(d('create.ask').description)).toBe('limit');
    expect(orderTypeCode(d('create.ask.day').description, d('create.ask.day').deadline)).toBe('limitDay');
    expect(orderTypeCode(d('create.ask.twap').description)).toBe('twap');
    expect(orderTypeCode(d('create.bid.dca').description)).toBe('dca');
    expect(orderTypeCode(d('create.ask.dutch').description)).toBe('dutch');
    expect(orderTypeCode(d('create.ask.market').description)).toBe('market');
    expect(orderTypeCode(d('create.ifdBid.stopEntry').description)).toMatch(/Stop$/);
    expect(orderTypeCode(d('create.ifdBid.repeat').description)).toMatch(/^repeat/);
  });
});

describe('translations of the order vocabulary', () => {
  it(`no raw keys or unfilled placeholders leak into any string (${GOLDEN.length} golden transactions)`, () => {
    for (const name of GOLDEN) {
      for (const s of strings(decode(build(name)))) {
        expect(s, `${name}: "${s}"`).not.toMatch(/\bconfirm\.[a-zA-Z]/);
        expect(s, `${name}: "${s}"`).not.toMatch(/\{\w+\}/);
        expect(s, `${name}: "${s}"`).not.toMatch(/undefined|NaN|\[object/);
      }
    }
  });

  it('uses the trading terms of the spec', () => {
    const text = strings(decode(build('create.ifdBid.repeat'))).join('\n');
    expect(text).toMatch(/repeat/i);
    expect(text).toMatch(/IF[DO]/);
    expect(strings(decode(build('create.condAsk'))).join('\n')).toMatch(/stop|take-profit|OCO|trailing/i);
    expect(strings(decode(build('create.ask.day'))).join('\n')).toMatch(/day order/i);
  });
});

describe('trigger evidence (touch rule) in the decoded summary', () => {
  const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
  const vector = (name: string): BuiltTx => clone(goldenTx(name).built as BuiltTx);
  const decodeVector = (built: BuiltTx): SigningSummary =>
    decodeSigning({ kob, built, maker: built.sign[0]?.pubkey ?? MAKER.pk, registry, nodeInputs: nodeFactsOf(built) });
  const armed = (s: SigningSummary): NonNullable<DecodedSpend['trigger']> => {
    const sp = s.spends.find((x) => x.trigger);
    if (!sp) throw new Error('no armed order');
    return sp.trigger!;
  };

  it('an order armed in its fill names the evidence input, its custody, its side and passes every rule', () => {
    const s = decodeVector(vector('cond.ask.stop.trigger'));
    const tr = armed(s);
    expect(tr).toMatchObject({ path: 'fill', ok: true });
    expect(tr.evidence).toMatchObject({ input: 1, custodyInput: 3, side: 'ask' });
    expect(Object.values(tr.checks).every(Boolean)).toBe(true);
    // the order's own terms still render without leftover keys
    for (const text of strings(s)) expect(text, text).not.toMatch(/\bconfirm\.[a-zA-Z]|\{\w+\}|undefined|NaN/);
  });

  it('a trail by update carries the new stop; a bad evidence index fails the rule and blocks', () => {
    const trail = armed(decodeVector(vector('cond.ask.update.trail')));
    expect(trail.effect).toBe('trail');
    expect(trail.trail!.newStop).toBeGreaterThan(0n);
    const bad = vector('cond.ask.update.arm');
    const p = bad.plans[2]!;
    if (p.kind !== 'entry') throw new Error('not an entry');
    p.args[0] = { kind: 'int', value: '1' };
    const s = decodeVector(bad);
    expect(armed(s).ok).toBe(false);
    expect(s.ok).toBe(false);
    expect(codes(s.blocking)).toContain('trigger-evidence-invalid');
  });
});
