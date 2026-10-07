// The amounts of an inverted KAS market (KAS/TOKEN): the shown base is KAS, so the amount boxes count KAS and the order is the native one sized
// at its own price; a flip of a filled form never changes the order that is built (form-state.ts, KAS amounts).
import { describe, expect, it } from 'vitest';
import {
  BASE_SUFFIX, amountBaseOf, amountPriceOf, baseToKas, buildIntent, initialForm, intentSizingPrice, kasToBase, primaryPriceField, reorientAmounts,
  setAmountBase, setValue, switchType, type TicketCtx, type TicketForm,
} from './form-state';
import { CASES, CTX, TOKEN, form, json } from './ticket-fixtures';

/** CTX with the default test book's best prices (ask 2.50, bid 2.45 KAS per token): what market and close convert at. */
const BOOK: TicketCtx = { ...CTX, ref: { buy: 250_000_000n, sell: 245_000_000n } };
const kasForm = (type: Parameters<typeof form>[0], side: Parameters<typeof form>[1], values: Record<string, string>): TicketForm => ({ ...form(type, side, values), kas: true });

describe('KAS amounts: the conversion', () => {
  it('a shown Buy of 10 KAS at 0.025 KAS per token is a native sell of exactly 400 tokens', () => {
    const r = buildIntent(kasForm('limit', 'sell', { amount: '10', price: '0.025' }), BOOK);
    expect(r.errors).toEqual([]);
    expect((r.intent as { amount: bigint }).amount).toBe(400n * TOKEN);
    expect(r.intent?.type).toBe('limit');
    expect((r.intent as { side: string }).side).toBe('sell');
  });

  it('rounds the token amount down: the KAS value of the order never exceeds what was typed', () => {
    // a shown Sell of 10 KAS at 0.024 KAS per token: 416.666666666... tokens -> 416.66666666 (8 decimals)
    const r = buildIntent(kasForm('limit', 'buy', { amount: '10', price: '0.024' }), BOOK);
    const n = (r.intent as { amount: bigint }).amount;
    expect(n).toBe(41_666_666_666n);
    const kas = baseToKas(n, 2_400_000n, CTX.scale);
    expect(kas).toBeLessThanOrEqual(10n * 100_000_000n);
    // and one more base unit would exceed it
    expect(((n + 1n) * 2_400_000n) / CTX.scale).toBeGreaterThanOrEqual(10n * 100_000_000n - 1n);
    expect(kasToBase(10n * 100_000_000n, 2_400_000n, CTX.scale)).toBe(n);
  });

  it('each amount converts at the price of its own leg', () => {
    const f = kasForm('ifd', 'buy', { amount: '10', price: '0.02', 'exit.takeProfit': '0.04', minFill: '1', 'exit.minFill': '2' });
    expect(amountPriceOf(f, BOOK, 'amount')).toEqual({ price: 2_000_000n, basis: 'own' });
    expect(amountPriceOf(f, BOOK, 'minFill')).toEqual({ price: 2_000_000n, basis: 'own' });
    expect(amountPriceOf(f, BOOK, 'exit.minFill')).toEqual({ price: 4_000_000n, basis: 'exit' });
    const r = buildIntent(f, BOOK);
    expect(r.errors).toEqual([]);
    const i = r.intent as unknown as { amount: bigint; minFill: bigint; exit: { minFill: bigint } };
    expect(i.amount).toBe(500n * TOKEN);
    expect(i.minFill).toBe(50n * TOKEN);
    expect(i.exit.minFill).toBe(50n * TOKEN);
    // a stop order is sized at its stop, a stop-limit at its limit, OCO at its take-profit, streaming at its displayed price
    expect(primaryPriceField('stopMarket')).toBe('stop');
    expect(primaryPriceField('stopLimit')).toBe('limit');
    expect(primaryPriceField('oco')).toBe('takeProfit');
    expect(primaryPriceField('streaming')).toBe('displayedPrice');
    expect(primaryPriceField('market')).toBeNull();
  });

  it('an order without a price converts at the book: the best ask for a buy, the best bid for a sell; no book = a visible error', () => {
    expect(amountPriceOf(kasForm('market', 'buy', { amount: '25' }), BOOK, 'amount')).toEqual({ price: 250_000_000n, basis: 'book' });
    expect(amountPriceOf(kasForm('market', 'sell', { amount: '24.5' }), BOOK, 'amount')).toEqual({ price: 245_000_000n, basis: 'book' });
    expect((buildIntent(kasForm('market', 'buy', { amount: '25' }), BOOK).intent as { amount: bigint }).amount).toBe(10n * TOKEN);
    const none = buildIntent(kasForm('market', 'buy', { amount: '25' }), CTX);
    expect(none.intent).toBeNull();
    expect(none.errors).toEqual([{ field: 'amount', code: 'noRef' }]);
  });

  it('a KAS amount without its price yet is incomplete (the price box reports itself), not a visible error', () => {
    const r = buildIntent(kasForm('limit', 'sell', { amount: '10' }), BOOK);
    expect(r.intent).toBeNull();
    expect(r.errors.filter((e) => e.field === 'amount').map((e) => e.code)).toEqual(['required']);
    expect(amountBaseOf(kasForm('limit', 'sell', { amount: '10' }), BOOK, 'amount')).toEqual({ ok: false, code: 'required' });
  });

  it('a KAS amount worth less than one base unit is refused', () => {
    const r = buildIntent(kasForm('limit', 'sell', { amount: '0.00000001', price: '1000' }), BOOK);
    expect(r.errors).toContainEqual({ field: 'amount', code: 'positive' });
  });

  it('a custom trigger threshold is KAS at the stop; presets stay shares of the order', () => {
    const f = kasForm('stopMarket', 'sell', { amount: '10', stop: '0.02', minTouch: '5' });
    const i = buildIntent(f, BOOK).intent as unknown as { amount: bigint; minTouch: bigint };
    expect(i.amount).toBe(500n * TOKEN);
    expect(i.minTouch).toBe(250n * TOKEN);
    const half = buildIntent({ ...f, values: { ...f.values, minTouch: '50%' } }, BOOK).intent as unknown as { minTouch: bigint };
    expect(half.minTouch).toBe(250n * TOKEN);
  });
});

describe('KAS amounts: exact amounts kept for a box', () => {
  it('Max / a prefill keeps the exact token amount and shows its KAS value; typing replaces it', () => {
    let f = kasForm('limit', 'sell', { price: '0.024' });
    f = setAmountBase(f, 'amount', 123_456_789n, BOOK);
    expect(f.values['amount' + BASE_SUFFIX]).toBe('123456789');
    expect(f.values.amount).toBe('0.02962962');
    expect((buildIntent(f, BOOK).intent as { amount: bigint }).amount).toBe(123_456_789n);
    // a price change keeps the token amount (the KAS shown follows it)
    f = setValue(f, 'price', '0.03');
    expect((buildIntent(f, BOOK).intent as { amount: bigint }).amount).toBe(123_456_789n);
    // typing in the box is the amount now
    f = setValue(f, 'amount', '3');
    expect(f.values['amount' + BASE_SUFFIX]).toBeUndefined();
    expect((buildIntent(f, BOOK).intent as { amount: bigint }).amount).toBe(100n * TOKEN);
  });

  it('a type switch keeps the unit and the kept amount', () => {
    const f = setAmountBase(kasForm('limit', 'sell', { price: '0.024' }), 'amount', 7n, BOOK);
    const g = switchType(f, 'ioc');
    expect(g.kas).toBe(true);
    expect(g.values['amount' + BASE_SUFFIX]).toBe('7');
    expect(initialForm('limit', 'sell', true).kas).toBe(true);
    expect(initialForm('limit', 'sell').kas).toBeUndefined();
  });

  it('a native form is unchanged: amounts are token units', () => {
    const f = setAmountBase(form('limit', 'sell', { price: '2.6' }), 'amount', 5n * TOKEN, BOOK);
    expect(f.values.amount).toBe('5');
    expect(f.values['amount' + BASE_SUFFIX]).toBeUndefined();
  });
});

describe('a flip of a filled form builds the very same order, every type, both ways', () => {
  it.each(CASES)('%s', (_name, type, side, values) => {
    const native = form(type, side, values);
    const before = buildIntent(native, BOOK);
    expect(before.errors).toEqual([]);
    const inv = reorientAmounts(native, true, BOOK);
    expect(inv.kas).toBe(true);
    expect(json(buildIntent(inv, BOOK).intent)).toBe(json(before.intent));
    const back = reorientAmounts(inv, false, BOOK);
    expect(back.kas).toBeUndefined();
    expect(json(buildIntent(back, BOOK).intent)).toBe(json(before.intent));
    // the native text comes back exactly (amounts are exact base units both ways)
    for (const id of ['amount', 'minFill', 'sliceAmount']) if (values[id] !== undefined) expect(back.values[id]).toBe(values[id]);
  });

  it('the inverted form shows the amount in KAS at the order price', () => {
    const inv = reorientAmounts(form('limit', 'sell', { amount: '1', price: '0.04' }), true, BOOK);
    expect(inv.values.amount).toBe('0.04');
    // typed afresh in KAS: the same order
    const typed = setValue(inv, 'amount', '0.04');
    expect((buildIntent(typed, BOOK).intent as { amount: bigint }).amount).toBe(TOKEN);
  });
});

describe('intentSizingPrice', () => {
  it.each(CASES)('%s: the price the KAS amount converts at', (_name, type, side, values) => {
    const f = form(type, side, values);
    const i = buildIntent(f, BOOK).intent!;
    const p = amountPriceOf({ ...f, kas: true }, BOOK, 'amount');
    if (primaryPriceField(type) === null) expect(intentSizingPrice(i)).toBeNull();
    else expect(intentSizingPrice(i)).toBe(p?.price);
  });
});
