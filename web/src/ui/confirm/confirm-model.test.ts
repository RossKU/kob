import { describe, expect, it } from 'vitest';
import { decodeSigning, describeInputsForWallet, type ExpectedSigning, type SigningSummary } from '../../kob/decode';
import { parseRegistry, type TokenRegistry } from '../../kob/registry';
import type { ActionRequest, BuiltTx } from '../../kob/types';
import { MAKER, goldenRequest, goldenTx, tradableRegistryJson, nodeFactsOf } from '../../testing/chain-fixtures';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { buildConfirmModel, orderTypeCode, tokenAmountText, tokenLabel, type ConfirmModel, type Row, type Section } from './confirm-model';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const CLOCK = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };

const build = (name: string): BuiltTx => kob.build(goldenRequest<ActionRequest>(name, MAKER.pk));
const decode = (built: BuiltTx, o: { registry?: TokenRegistry | null; expected?: ExpectedSigning } = {}): SigningSummary =>
  decodeSigning({ kob, built, maker: MAKER.pk, registry: o.registry === undefined ? registry : o.registry, expected: o.expected, nodeInputs: nodeFactsOf(built) });
const model = (name: string, reg: TokenRegistry | null = registry): ConfirmModel => {
  const built = build(name);
  return buildConfirmModel(decode(built, { registry: reg }), { registry: reg, clock: CLOCK, tr: (k, p) => t(k, p) }, describeInputsForWallet(built, 'kasware').notices);
};
const section = (m: ConfirmModel, id: Section['id']): Section => {
  const s = m.sections.find((x) => x.id === id);
  if (!s) throw new Error(`no section ${id}: ${m.sections.map((x) => x.id).join(',')}`);
  return s;
};
const rowOf = (rows: Row[], id: string): Row => {
  const r = rows.find((x) => x.id === id);
  if (!r) throw new Error(`no row ${id}: ${rows.map((x) => x.id).join(',')}`);
  return r;
};

/** Every string of a model, for leftover-key and placeholder checks. */
function strings(m: ConfirmModel): string[] {
  const out: string[] = [m.heading, m.intro];
  const rows = (rs: Row[]) => rs.forEach((r) => out.push(r.label, r.value, r.detail ?? ''));
  for (const s of m.sections) {
    out.push(s.title, s.note ?? '');
    rows(s.rows);
    const walk = (c: (typeof s.cards)[number]) => {
      out.push(c.title, c.badge);
      rows(c.rows);
      c.children.forEach(walk);
    };
    s.cards.forEach(walk);
  }
  out.push(...m.advanced.inputs.map((i) => i.text), ...m.advanced.outputs.map((o) => o.text), ...m.advanced.payload);
  return out;
}

const GOLDEN = [
  'create.ask', 'create.ask.twap', 'create.ask.dutch', 'create.ask.market', 'create.ask.day', 'create.bid', 'create.bid.dca', 'create.bid.market',
  'create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdBid.stopEntry', 'create.ifdBid.repeat', 'create.ifdAsk', 'create.ifdAsk.repeat',
  'cancel.ask', 'cancel.ask.sweepStray', 'cancel.bid', 'cancel.condAsk', 'cancel.ifdBid', 'cancel.position.repeatBuyFirst', 'cancel.position.repeatSellFirst',
  'cancelReplace.ask', 'cancelReplace.bid', 'refund.ask.expiry', 'refund.bid', 'refund.condAsk', 'refund.ifdBid', 'send.tokens',
];

describe('confirmation model', () => {
  it('a limit sell: quantity, price, all-in, escrow and the KAS that is locked', () => {
    const m = model('create.ask');
    expect(m.kind).toBe('create');
    expect(m.canSign).toBe(true);
    expect(m.blocking).toEqual([]);
    const card = section(m, 'create').cards[0]!;
    expect(card.title).toBe('Sell limit order');
    expect(card.flagged).toBe(false);
    expect(rowOf(card.rows, 'amount').value).toBe('10 TST');
    expect(rowOf(card.rows, 'minFill').value).toMatch(/ TST$/);
    // golden token TST has 3 decimals (scale 1000 base units): 250_000_000 sompi per whole token = 2.5 KAS per token
    expect(rowOf(card.rows, 'price').value).toBe('2.5 KAS / TST');
    expect(rowOf(card.rows, 'tip').value).toBe('0.001 KAS / TST');
    expect(rowOf(card.rows, 'allIn').label).toBe('You receive at least per token (all-in)');
    expect(rowOf(card.rows, 'allIn').value).toBe('2.499 KAS / TST');
    const locked = section(m, 'locked');
    expect(rowOf(locked.rows, 'locked-total').value).toBe('20 KAS');
    expect(rowOf(locked.rows, 'locked-carriers').detail).toBe('returned when the order ends');
    expect(rowOf(locked.rows, 'locked-refundTips').value).toBe('0.034 KAS');
    expect(locked.rows.find((r) => r.id.startsWith('tok-escrow'))!.value).toBe('10 TST');
    expect(section(m, 'back').rows.find((r) => r.id.startsWith('tok-back'))!.value).toBe('2 TST');
    expect(section(m, 'spend').rows.find((r) => r.id.startsWith('tok-spend'))!.value).toBe('12 TST');
    expect(section(m, 'net').rows.find((r) => r.id.startsWith('tok-net'))!.value).toBe('-10 TST');
    expect(rowOf(section(m, 'net').rows, 'net-kas').value).toMatch(/^-20\.\d+ KAS$/);
    expect(m.sections.some((x) => x.id === 'others')).toBe(false);
  });

  it('a plain bid shows its KAS budget and what it covers', () => {
    const m = model('create.bid');
    const card = section(m, 'create').cards[0]!;
    expect(card.title).toBe('Buy limit order');
    expect(rowOf(card.rows, 'budget').detail).toMatch(/^Covers the amount at the limit price/);
    expect(rowOf(card.rows, 'allIn').label).toBe('You pay at most per token (all-in)');
    expect(rowOf(section(m, 'locked').rows, 'locked-escrow').value).toMatch(/KAS$/);
  });

  it('day orders show the 00:00 UTC deadline in UTC and JST', () => {
    const card = section(model('create.ask.day'), 'create').cards[0]!;
    expect(card.title).toBe('Sell day limit order');
    expect(rowOf(card.rows, 'deadline').value).toBe('2026-09-30 00:00 UTC / 2026-09-30 09:00 JST');
  });

  it('an if-done entry shows its exit, and repeat entries say how often they re-arm', () => {
    const m = model('create.ifdBid.repeat');
    const card = section(m, 'create').cards[0]!;
    expect(card.title).toMatch(/repeat IF[DO] entry/);
    expect(card.children).toHaveLength(1);
    expect(card.children[0]!.title).toMatch(/^Exit created after each entry fill: Sell /);
    expect(card.rows.some((r) => r.id === 'repeat')).toBe(true);
    expect(rowOf(card.rows, 'minFill').value).toMatch(/ TST$/);
    const stopEntry = section(model('create.ifdBid.stopEntry'), 'create').cards[0]!;
    expect(stopEntry.rows.some((r) => r.id === 'entryStop')).toBe(true);
    expect(stopEntry.title).toMatch(/stop entry/);
  });

  it('conditional orders show trigger, band, exposure and keeper tip', () => {
    const card = section(model('create.condAsk'), 'create').cards[0]!;
    expect(card.title).toMatch(/^Sell (stop order|OCO order|trailing stop|take-profit order|OCO order with trailing stop)/);
    expect(card.rows.some((r) => r.id === 'stop')).toBe(true);
    // R-4: the trigger direction (a sell stop arms on resting sells at or below it) and the worst fill price after the trigger
    expect(rowOf(card.rows, 'exposure').value).toMatch(/at least [\d.]+ TST of a sell order resting at or below the stop for \d+ s/);
    expect(rowOf(card.rows, 'stopWorst').detail).toMatch(/between the stop and this price/);
    const buy = section(model('create.condBid'), 'create').cards[0]!;
    expect(rowOf(buy.rows, 'exposure').value).toMatch(/buy order resting at or above the stop/);
    expect(rowOf(buy.rows, 'stopWorst').value).not.toBe(rowOf(buy.rows, 'stop').value);
  });

  it('TWAP / DCA / Dutch / market describe their schedule and auction', () => {
    expect(section(model('create.ask.twap'), 'create').cards[0]!.rows.some((r) => r.id === 'schedule')).toBe(true);
    expect(section(model('create.bid.dca'), 'create').cards[0]!.title).toMatch(/DCA/);
    expect(section(model('create.ask.dutch'), 'create').cards[0]!.rows.some((r) => r.id === 'auction')).toBe(true);
    const mk = section(model('create.ask.market'), 'create').cards[0]!;
    expect(mk.title).toMatch(/market order/);
    expect(rowOf(mk.rows, 'tif').value).toMatch(/immediate or cancel/);
  });

  it('cancel: what is closed, what comes back, nothing leaves', () => {
    const m = model('cancel.ask');
    expect(m.kind).toBe('cancel');
    const close = section(m, 'close').cards[0]!;
    expect(close.title).toMatch(/^Cancel: Sell /);
    expect(close.badge).toBe('your order');
    expect(rowOf(close.rows, 'tokensReleased').tone).toBe('good');
    expect(m.sections.some((s) => s.id === 'others')).toBe(false);
    expect(m.sections.some((s) => s.id === 'create')).toBe(false);
  });

  it('a cancel that sweeps stray tokens says so', () => {
    const m = model('cancel.ask.sweepStray');
    expect(rowOf(section(m, 'close').cards[0]!.rows, 'strays').detail).toMatch(/only its maker can move them/);
  });

  it('cancel-replace closes one order and creates another', () => {
    const m = model('cancelReplace.ask');
    expect(m.kind).toBe('cancel-replace');
    expect(section(m, 'close').cards).toHaveLength(1);
    expect(section(m, 'create').cards).toHaveLength(1);
  });

  it('a position cancel lists every order it closes', () => {
    const m = model('cancel.position.repeatBuyFirst');
    expect(m.kind).toBe('cancel-position');
    expect(section(m, 'close').cards.length).toBeGreaterThan(1);
  });

  it('a refund is headed as a refund', () => {
    const m = model('refund.ask.expiry');
    expect(m.kind).toBe('refund');
    expect(m.heading).toBe('Refund an expired order');
  });

  it('a token transfer to another key is surfaced as blocking and appears under "goes to other keys"', () => {
    const built = build('send.tokens');
    const s = decode(built);
    expect(s.ok).toBe(false);
    const m = buildConfirmModel(s, { registry, clock: CLOCK });
    expect(m.canSign).toBe(false);
    expect(m.blocking.map((b) => b.code)).toContain('transfer-out');
    expect(section(m, 'others').rows.some((r) => r.tone === 'bad')).toBe(true);
  });

  it('a planner claim that the transaction does not meet blocks (kasLocked mismatch)', () => {
    const built = build('create.ask');
    const s = decode(built, { expected: { kasLocked: 1n } });
    const m = buildConfirmModel(s, { registry, clock: CLOCK });
    expect(m.canSign).toBe(false);
    expect(m.blocking.map((b) => b.code)).toContain('expected-kas-locked');
  });

  it('never shows a bare ticker: registry tokens carry their id and state, others are "unknown token"', () => {
    const ref = { covenantId: '70'.repeat(32), ticker: 'TST', decimals: 3, display: '', inRegistry: true, tradable: true };
    expect(tokenLabel(ref, (k, p) => t(k, p), registry)).toMatch(/^TST \(7070\.\.\.7070\) \[(verified|unverified)\]$/);
    expect(tokenLabel({ ...ref, covenantId: 'ab'.repeat(32) }, (k, p) => t(k, p), registry)).toBe('unknown token (abababab…abababab)');
    // an unknown token has no decimals: raw base units, never a guessed unit
    const m = model('create.ask', null);
    const card = section(m, 'create').cards[0]!;
    expect(rowOf(card.rows, 'amount').value).toContain('10000 base units');
    expect(card.rows.some((r) => r.value.includes('KAS / '))).toBe(false);
    expect(m.warnings.map((w) => w.code)).toContain('token-unlisted');
    expect(tokenAmountText({ ...ref, decimals: null, ticker: null }, 5n, (k, p) => t(k, p))).toBe('5 base units (unknown token)');
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

  it('advanced view lists every input and output and the payload records', () => {
    const m = model('create.ask');
    expect(m.advanced.inputs).toHaveLength(2);
    expect(m.advanced.outputs.map((o) => o.flagged)).toEqual([false, false, false, false]);
    expect(m.advanced.payload[0]).toMatch(/^order record for output 0/);
    expect(m.advanced.txid).toMatch(/^[0-9a-f]{64}$/);
    expect(m.advanced.signatures).toBe(2);
    expect(m.notices.some((n) => n.code === 'blind-tokens')).toBe(true);
  });
});

describe('translations of the model', () => {
  it(`no raw keys or unfilled placeholders leak into any string (${GOLDEN.length} golden transactions)`, () => {
    for (const name of GOLDEN) {
      const m = model(name);
      for (const s of strings(m)) {
        expect(s, `${name}: "${s}"`).not.toMatch(/\bconfirm\.[a-zA-Z]/);
        expect(s, `${name}: "${s}"`).not.toMatch(/\{\w+\}/);
        expect(s, `${name}: "${s}"`).not.toMatch(/undefined|NaN|\[object/);
      }
    }
  });

  it('uses the trading terms of the spec', () => {
    const text = strings(model('create.ifdBid.repeat')).join('\n');
    expect(text).toMatch(/repeat/i);
    expect(text).toMatch(/IF[DO]/);
    expect(strings(model('create.condAsk')).join('\n')).toMatch(/stop|take-profit|OCO|trailing/i);
    expect(strings(model('create.ask.day')).join('\n')).toMatch(/day order/i);
    expect(strings(model('cancel.ask')).join('\n')).toMatch(/cancel/i);
  });
});

describe('trigger evidence on the confirmation screen (touch rule)', () => {
  const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
  const vector = (name: string): BuiltTx => clone(goldenTx(name).built as BuiltTx);
  const screen = (built: BuiltTx): ConfirmModel =>
    buildConfirmModel(decodeSigning({ kob, built, maker: built.sign[0]?.pubkey ?? MAKER.pk, registry, nodeInputs: nodeFactsOf(built) }), { registry, clock: CLOCK, tr: (k, p) => t(k, p) });
  const card = (m: ConfirmModel, input: number) => {
    const c = section(m, 'close').cards.find((x) => x.rows.some((r) => r.id === 'trigger'));
    if (!c) throw new Error(`no armed order card (input ${input})`);
    return c;
  };

  it('an order armed in its fill shows how, the evidence input, its price against the stop, its size and rest, and the verdict', () => {
    const m = screen(vector('cond.ask.stop.trigger'));
    const rows = card(m, 0).rows;
    expect(rowOf(rows, 'trigger').value).toBe(t('confirm.trigger.armedInFill'));
    expect(rowOf(rows, 'triggerEvidence').value).toBe(t('confirm.trigger.evidenceValue', { input: 1, side: t('confirm.trigger.sideAsk') }));
    expect(rowOf(rows, 'triggerEvidence').detail).toBe(t('confirm.trigger.custody', { input: 3 }));
    for (const id of ['triggerQuote', 'triggerAmount', 'triggerRest']) expect(rowOf(rows, id).tone, id).toBe('normal');
    expect(rowOf(rows, 'triggerVerdict')).toMatchObject({ value: t('confirm.trigger.ok'), tone: 'good' });
    for (const s of strings(m)) expect(s, s).not.toMatch(/\bconfirm\.[a-zA-Z]|\{\w+\}|undefined|NaN/);
  });

  it('a trail by update shows the new stop; a bad evidence index shows the failed rule', () => {
    const trail = card(screen(vector('cond.ask.update.trail')), 2).rows;
    expect(rowOf(trail, 'trigger').value).toMatch(/^stop trailed to /);
    const bad = vector('cond.ask.update.arm');
    const p = bad.plans[2]!;
    if (p.kind !== 'entry') throw new Error('not an entry');
    p.args[0] = { kind: 'int', value: '1' };
    const m = screen(bad);
    const rows = card(m, 2).rows;
    expect(rowOf(rows, 'triggerEvidence').tone).toBe('bad');
    expect(rowOf(rows, 'triggerVerdict')).toMatchObject({ value: t('confirm.trigger.notOk'), tone: 'bad' });
    expect(m.canSign).toBe(false);
    expect(m.blocking.map((b) => b.code)).toContain('trigger-evidence-invalid');
  });
});
