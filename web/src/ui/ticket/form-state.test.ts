import { describe, expect, it } from 'vitest';
import { errors } from '../../kob/plan-types';
import { planOrder } from '../../kob/plan';
import { consensusCheck, kob, makeEnv, market8x8 } from '../../testing/fixtures';
import { CASES, CTX, TOKEN, WIDE, form, json, ticketEnv } from './ticket-fixtures';
import {
  FIELDS, FIELD_DEFAULTS, ORDER_TYPES, TYPE_GROUPS, buildIntent, formFromIntent, formatLocalDateTime, initialForm, isIncomplete, layoutOf, maxAmount, parseAmount, parseLocalDateTime,
  parsePrice, parseTouch, parseWhole, priceInfo, setSide, setValue, sidesOf, switchType, visibleErrors, type OrderTypeId, type Side, type TicketCtx, type TicketForm,
} from './form-state';

const K = kob();

describe('vocabulary and layout', () => {
  it('lists every order type once, grouped', () => {
    const grouped = TYPE_GROUPS.flatMap((g) => g.types);
    expect([...grouped].sort()).toEqual([...ORDER_TYPES].sort());
    expect(new Set(grouped).size).toBe(grouped.length);
  });

  it('restricts the side of close, TWAP and DCA', () => {
    expect(sidesOf('close')).toEqual(['sell']);
    expect(sidesOf('twap')).toEqual(['sell']);
    expect(sidesOf('dca')).toEqual(['buy']);
    expect(sidesOf('limit')).toEqual(['buy', 'sell']);
    expect(setSide(initialForm('close', 'sell'), 'buy').side).toBe('sell');
  });

  it('every layout field is declared and the test ids are unique and follow the contract', () => {
    const ids = new Set<string>();
    for (const type of ORDER_TYPES) {
      for (const side of sidesOf(type)) {
        for (const values of [{} as Record<string, string>, { lifetime: 'gtd', 'exit.kind': 'stop', 'exit.trail': 'true' } as Record<string, string>]) {
          const l = layoutOf({ type, side, values });
          const all = [...l.main, ...l.advanced];
          expect(new Set(all).size, `${type}/${side} repeats a field`).toBe(all.length);
          expect(l.main, `${type} must offer an amount`).toContain('amount');
          // the minimum fill is an advanced option of every type that can fill partially (close sells everything at market)
          if (type !== 'close') expect(l.advanced, `${type} must offer the minimum fill`).toContain('minFill');
          for (const id of all) {
            expect(FIELDS[id], `${type}: undeclared field ${id}`).toBeDefined();
            ids.add(FIELDS[id]!.testid);
          }
        }
      }
    }
    expect(FIELDS.amount!.testid).toBe('order-amount');
    expect(FIELDS.minFill!.testid).toBe('field-minFill');
    expect(FIELDS.price!.testid).toBe('order-price');
    expect(FIELDS.tip!.testid).toBe('order-tip');
    expect(FIELDS.takeProfit!.testid).toBe('field-takeProfit');
    expect(FIELDS['trail.step']!.testid).toBe('field-trail.step');
    const all = Object.values(FIELDS).map((f) => f.testid);
    expect(new Set(all).size).toBe(all.length);
  });

  it('shows conditional fields only when their switch is on', () => {
    expect(layoutOf(form('limit', 'sell', {})).main).not.toContain('lifetimeAt');
    expect(layoutOf(form('limit', 'sell', { lifetime: 'gtd' })).main).toContain('lifetimeAt');
    expect(layoutOf(form('limit', 'sell', {})).advanced).not.toContain('maxFills');
    expect(layoutOf(form('limit', 'buy', {})).advanced).toContain('maxFills');
    expect(layoutOf(form('ifd', 'buy', {})).main).toContain('exit.takeProfit');
    expect(layoutOf(form('ifd', 'buy', { 'exit.kind': 'stop' })).main).toContain('exit.stop');
    expect(layoutOf(form('ifd', 'buy', { 'exit.kind': 'stop' })).main).not.toContain('exit.takeProfit');
    expect(layoutOf(form('ifd', 'buy', {})).advanced).not.toContain('prefund');
    expect(layoutOf(form('ifd', 'sell', {})).advanced).toContain('prefund');
    expect(layoutOf(form('ifd', 'buy', {})).advanced).toEqual(expect.arrayContaining(['minFill', 'exit.minFill']));
    expect(layoutOf(form('ifo', 'buy', {})).advanced).not.toContain('exit.trail.step');
    expect(layoutOf(form('ifo', 'buy', { 'exit.trail': 'true' })).advanced).toContain('exit.trail.step');
    expect(layoutOf(form('repeatIfd', 'buy', {})).main).toContain('repeat.count');
    expect(layoutOf(form('repeatIfd', 'buy', {})).main).not.toContain('exit.kind');
  });

  it('prefills the wallet defaults of matcher.md section 10', () => {
    const m = initialForm('market', 'buy').values;
    expect(m.slippageBps).toBe('3');
    expect(m.auction).toBe('20');
    expect(m.life).toBe('30');
    expect(initialForm('stopMarket', 'sell').values.bandDaa).toBe('30');
    expect(initialForm('stopMarket', 'sell').values.minRestDaa).toBe('5');
    // the trigger threshold: the order's minimum fill (founder 2026-10-03); every trigger kind shows it and R
    expect(initialForm('stopMarket', 'sell').values.minTouch).toBe('min');
    for (const type of ['stopMarket', 'stopLimit', 'trailingStop', 'oco'] as const) {
      expect(layoutOf(initialForm(type, 'sell')).advanced).toEqual(expect.arrayContaining(['minTouch', 'minRestDaa']));
    }
    for (const type of ['ifd', 'ifo', 'repeatIfd', 'repeatIfo'] as const) {
      expect(layoutOf(initialForm(type, 'buy')).advanced).toEqual(expect.arrayContaining(['entry.minTouch', 'entry.minRestDaa']));
    }
    expect(layoutOf(initialForm('ifo', 'buy')).advanced).toEqual(expect.arrayContaining(['exit.minTouch', 'exit.minRestDaa']));
    expect(layoutOf({ ...initialForm('ifd', 'buy'), values: { 'exit.kind': 'stop' } }).advanced).toEqual(expect.arrayContaining(['exit.minTouch', 'exit.minRestDaa']));
    expect(initialForm('ifo', 'buy').values['entry.minRestDaa']).toBe('5');
    expect(initialForm('ifo', 'buy').values['exit.minTouch']).toBe('min');
    // the minimum fill defaults to nothing typed: the planner's default is shown in the disclosure
    expect(initialForm('limit', 'sell').values.minFill).toBeUndefined();
    expect(initialForm('trailingStop', 'sell').values['trail.wait']).toBe('10');
    expect(FIELD_DEFAULTS.lifetime).toBe('gtc');
    // the tip defaults to nothing (0) and the repeat count to unlimited
    expect(initialForm('repeatIfd', 'buy').values.tip).toBeUndefined();
    expect(initialForm('repeatIfd', 'buy').values['repeat.count']).toBeUndefined();
  });

  it('switching the type keeps what applies and adds the defaults', () => {
    const f = setValue(setValue(initialForm('limit', 'buy'), 'amount', '7.25'), 'price', '1.5');
    const s = switchType(f, 'stopLimit');
    expect(s.side).toBe('buy');
    expect(s.values.amount).toBe('7.25');
    expect(s.values.price).toBeUndefined(); // stop-limit has no plain price
    expect(s.values.bandDaa).toBe('30');
    const t = switchType(f, 'twap');
    expect(t.side).toBe('sell'); // a buy cannot become a TWAP: the side is fixed
    expect(t.values.price).toBe('1.5');
  });
});

describe('price parsing', () => {
  it('rounds a sell up and a buy down onto the tick, never making the limit worse', () => {
    // 1.00000001 KAS/token = 100_000_001 sompi per whole token is off the tick of 100
    const sell = parsePrice('1.00000001', 'sell', CTX);
    const buy = parsePrice('1.00000001', 'buy', CTX);
    expect(sell.ok && sell.value).toEqual({ price: 100_000_100n, rounded: true });
    expect(buy.ok && buy.value).toEqual({ price: 100_000_000n, rounded: true });
    const exact = parsePrice('2.5', 'sell', CTX);
    expect(exact.ok && exact.value).toEqual({ price: 250_000_000n, rounded: false });
  });

  it('a token of more than 9 decimals quotes per 10^9 base units (the scale cap)', () => {
    // 12 decimals: one token = 10^12 base units, the state price is per 10^9 = a thousandth of a token
    const p = parsePrice('2', 'sell', WIDE);
    expect(p.ok && p.value).toEqual({ price: 200_000n, rounded: false });
    // 0.00000003 KAS/token = 3 sompi per token = 0.003 sompi per 10^9 base units: a sell rounds up to 1, a buy down to nothing
    const s = parsePrice('0.00000003', 'sell', WIDE);
    const b = parsePrice('0.00000003', 'buy', WIDE);
    expect(s.ok && s.value).toEqual({ price: 1n, rounded: true });
    expect(b.ok ? 'ok' : b.code).toBe('positive');
  });

  it('parses token amounts exactly in base units with the token decimals', () => {
    expect(parseAmount('1.5', 8)).toEqual({ ok: true, value: 150_000_000n });
    expect(parseAmount('0.00000001', 8)).toEqual({ ok: true, value: 1n });
    expect(parseAmount('2', 0)).toEqual({ ok: true, value: 2n });
    const code = (t: string, d = 8) => {
      const p = parseAmount(t, d);
      return p.ok ? 'ok' : p.code;
    };
    expect(code('0.000000001')).toBe('precision');
    expect(code('1.5', 0)).toBe('precision');
    expect(code('-1')).toBe('negative');
    expect(code('1,5')).toBe('format');
    expect(code('')).toBe('required');
  });

  it('refuses malformed, negative, zero, over-precise and overflowing text', () => {
    const code = (t: string) => {
      const p = parsePrice(t, 'sell', CTX);
      return p.ok ? 'ok' : p.code;
    };
    expect(code('')).toBe('required');
    expect(code('abc')).toBe('format');
    expect(code('-1')).toBe('negative');
    expect(code('0')).toBe('positive');
    expect(code('1.123456789')).toBe('precision');
    expect(code('1e3')).toBe('format');
    expect(code('9999999999999999999999')).toBe('range');
    expect(code(' 2.5 ')).toBe('ok');
    // a price that rounds down to nothing on a coarse tick is "positive", not "format"
    const tiny = parsePrice('0.00000001', 'buy', { decimals: 8, scale: 100_000_000n, tick: 100n });
    expect(tiny.ok).toBe(false);
  });

  it('describes the rounded price for display', () => {
    expect(priceInfo('1.00000001', 'sell', CTX)).toEqual({ price: 100_000_100n, perToken: '1.000001', rounded: true });
    expect(priceInfo('nope', 'sell', CTX)).toBeNull();
  });

  it('parses whole numbers strictly', () => {
    expect(parseWhole('12')).toEqual({ ok: true, value: 12n });
    expect(parseWhole('1.5').ok).toBe(false);
    expect(parseWhole('').ok).toBe(false);
    expect(parseWhole('9223372036854775808').ok).toBe(false);
  });
});

describe('date parsing', () => {
  it('converts wall-clock text with the browser offset to UTC unix seconds and back', () => {
    // 2026-10-01 09:00 in JST (offset -540) is 00:00 UTC
    const jst = parseLocalDateTime('2026-10-01T09:00', -540);
    const utc = parseLocalDateTime('2026-10-01T00:00', 0);
    expect(jst.ok && utc.ok && jst.value === utc.value).toBe(true);
    expect(formatLocalDateTime(utc.ok ? utc.value : 0n, -540)).toBe('2026-10-01T09:00');
    expect(formatLocalDateTime(utc.ok ? utc.value : 0n, 0)).toBe('2026-10-01T00:00');
    // US Pacific in summer: UTC-7 (offset +420)
    const pdt = parseLocalDateTime('2026-07-01T00:00', 420);
    const utc2 = parseLocalDateTime('2026-07-01T07:00', 0);
    expect(pdt.ok && utc2.ok && pdt.value === utc2.value).toBe(true);
  });

  it('refuses impossible dates', () => {
    expect(parseLocalDateTime('2026-02-31T10:00').ok).toBe(false);
    expect(parseLocalDateTime('2026-13-01T10:00').ok).toBe(false);
    expect(parseLocalDateTime('2026-01-01T24:00').ok).toBe(false);
    expect(parseLocalDateTime('tomorrow').ok).toBe(false);
    expect(parseLocalDateTime('').ok).toBe(false);
  });
});

describe('buildIntent', () => {
  const T = TOKEN;

  it('reports required, malformed and out-of-range fields per field', () => {
    const empty = buildIntent(initialForm('limit', 'sell'), CTX);
    expect(empty.intent).toBeNull();
    expect(empty.errors.map((e) => [e.field, e.code]).sort()).toEqual([['amount', 'required'], ['price', 'required']]);
    expect(isIncomplete(empty.errors)).toBe(true);
    expect(visibleErrors(empty.errors)).toEqual([]);

    const wrong = buildIntent(form('limit', 'sell', { amount: '2,5', price: '1,5', tip: 'x' }), CTX);
    expect(wrong.intent).toBeNull();
    expect(wrong.errors.map((e) => [e.field, e.code]).sort()).toEqual([['amount', 'format'], ['price', 'format'], ['tip', 'format']]);
    expect(visibleErrors(wrong.errors)).toHaveLength(3);
    expect(buildIntent(form('limit', 'sell', { amount: '0', price: '1' }), CTX).errors).toEqual([{ field: 'amount', code: 'positive' }]);
    // more decimals than the token has
    expect(buildIntent(form('limit', 'sell', { amount: '0.000000001', price: '1' }), CTX).errors.map((e) => [e.field, e.code])).toEqual([['amount', 'precision']]);
    expect(buildIntent(form('limit', 'sell', { amount: '1', price: '1', minFill: '0' }), CTX).errors).toEqual([{ field: 'minFill', code: 'positive' }]);
  });

  it('maps a limit to sompi per whole token, base units, tip and lifetime', () => {
    const r = buildIntent(form('limit', 'buy', { amount: '5.5', price: '1.5', tip: '0.0001', lifetime: 'day', crossing: 'reject', maxFills: '2', minFill: '0.25' }), CTX);
    expect(r.errors).toEqual([]);
    expect(r.intent).toEqual({
      type: 'limit', side: 'buy', amount: 550_000_000n, minFill: 25_000_000n, price: 150_000_000n, tip: 10_000n, lifetime: { kind: 'day' }, crossing: 'reject', maxFills: 2n,
    });
  });

  it('omits every unset optional so the planner applies the wallet defaults', () => {
    const r = buildIntent(form('limit', 'sell', { amount: '5', price: '1.5' }), CTX);
    expect(r.intent).toEqual({ type: 'limit', side: 'sell', amount: 5n * T, price: 150_000_000n, crossing: 'auction' });
    const c = buildIntent(form('stopMarket', 'sell', { amount: '5', stop: '2' }), CTX);
    expect(c.intent).toMatchObject({ type: 'stopMarket', amount: 5n * T, stop: 200_000_000n, slipBps: 300, bandDaa: 300n, minRestDaa: 50n });
    // the default threshold ("min fill") is left to the planner: kob-wasm defaultMinTouch of the order's minimum fill
    expect((c.intent as unknown as Record<string, unknown>).minTouch).toBeUndefined();
    expect((c.intent as unknown as Record<string, unknown>).keeperTip).toBeUndefined();
    expect((c.intent as unknown as Record<string, unknown>).minFill).toBeUndefined();
    const bare = buildIntent({ type: 'stopMarket', side: 'sell', values: { amount: '5', stop: '2' } }, CTX).intent as unknown as Record<string, unknown>;
    expect('minTouch' in bare || 'minRestDaa' in bare).toBe(false);
  });

  it('trigger threshold presets: min fill, 25% / 50% / 100% of the order rounded up to a base unit, or a custom amount', () => {
    const touch = (text: string, n = '10') => (buildIntent(form('stopMarket', 'sell', { amount: n, stop: '2', minTouch: text }), CTX).intent as any)?.minTouch;
    expect(touch('min')).toBe(undefined); // the wallet default: the order's minimum fill
    expect(touch('')).toBe(undefined);
    expect(touch('25%')).toBe(250_000_000n);
    expect(touch('50%')).toBe(5n * T);
    expect(touch('100%')).toBe(10n * T);
    expect(touch('25%', '0.00000003')).toBe(1n); // 0.75 base units round up to 1
    expect(touch('1.5')).toBe(150_000_000n); // custom, in tokens
    // the same on a stop entry and an exit stop, and on every stop family
    const ifo = buildIntent(form('ifo', 'buy', { amount: '8', price: '1.5', 'entry.stop': '1.6', 'exit.takeProfit': '2', 'exit.stop': '1.2', 'entry.minTouch': '50%', 'exit.minTouch': '100%' }), CTX);
    expect(ifo.errors).toEqual([]);
    expect((ifo.intent as any).entry.minTouch).toBe(4n * T);
    expect((ifo.intent as any).exit.minTouch).toBe(8n * T);
    for (const type of ['stopLimit', 'trailingStop', 'oco'] as const) {
      const r = buildIntent(form(type, 'sell', { amount: '4', stop: '2', limit: '1.9', takeProfit: '3', 'trail.step': '0.01', 'trail.gap': '0.02', minTouch: '100%' }), CTX);
      expect((r.intent as any)?.minTouch, type).toBe(4n * T);
    }
    // bad text is reported on the field
    for (const bad of ['0', '0%', '101%', '-1', 'x%', '0.000000001']) {
      const r = buildIntent(form('stopMarket', 'sell', { amount: '10', stop: '2', minTouch: bad }), CTX);
      expect(r.errors.map((e) => e.field), bad).toContain('minTouch');
    }
    // a percent of an order whose size is missing: only the amount field reports
    const noAmount = buildIntent(form('stopMarket', 'sell', { stop: '2', minTouch: '50%' }), CTX);
    expect(noAmount.errors.map((e) => e.field)).toEqual(['amount']);
    expect(parseTouch('100%', 7n, 8)).toEqual({ ok: true, value: 7n });
    expect(parseTouch('min', 7n, 8)).toEqual({ ok: true, value: null });
  });

  it('converts times: seconds, minutes and the DAA rate', () => {
    const m = buildIntent(form('market', 'sell', { amount: '1', auction: '20', life: '30', slippageBps: '0.5' }), CTX).intent as any;
    expect(m.auction).toEqual({ seconds: 20n });
    expect(m.life).toEqual({ seconds: 30n });
    expect(m.slippageBps).toBe(50n);
    const t = buildIntent(form('twap', 'sell', { amount: '4', sliceAmount: '2', interval: '2.5', price: '2' }), CTX).intent as any;
    expect(t.interval).toEqual({ seconds: 150n });
    expect(t.sliceAmount).toBe(2n * T);
    const s = buildIntent(form('stopMarket', 'sell', { amount: '1', stop: '2', bandDaa: '30', minRestDaa: '60' }), { ...CTX, rateMilli: 9_500 }).intent as any;
    expect(s.bandDaa).toBe(285n);
    expect(s.minRestDaa).toBe(570n);
  });

  it('maps the tip and price distances with the order scale', () => {
    // 12 decimals: prices per 10^9 base units (a thousandth of a token)
    const r = buildIntent(form('trailingStop', 'sell', { amount: '1', stop: '4', 'trail.step': '0.5', 'trail.gap': '0', tip: '0.2' }), WIDE).intent as any;
    expect(r.amount).toBe(1_000_000_000_000n);
    expect(r.stop).toBe(400_000n);
    expect(r.trail).toEqual({ step: 50_000n, gap: 0n, wait: 6000n, expectedUpdates: 20 });
    expect(r.tip).toBe(20_000n);
  });

  it('a trail step must be positive; a stop must not be zero', () => {
    expect(buildIntent(form('trailingStop', 'sell', { amount: '1', stop: '4', 'trail.step': '0', 'trail.gap': '1' }), CTX).errors).toEqual([{ field: 'trail.step', code: 'positive' }]);
    expect(buildIntent(form('stopMarket', 'sell', { amount: '1', stop: '0' }), CTX).errors).toEqual([{ field: 'stop', code: 'positive' }]);
    expect(buildIntent(form('stopMarket', 'sell', { amount: '1', stop: '3', slipBps: '150' }), CTX).errors).toEqual([{ field: 'slipBps', code: 'range' }]);
  });

  it('exit prices round in the direction of the exit side', () => {
    // buy-first: the exit sells, so its take-profit rounds UP; the entry buys, so it rounds DOWN
    const r = buildIntent(form('ifd', 'buy', { amount: '4', price: '2.00000001', 'exit.takeProfit': '2.60000001' }), CTX).intent as any;
    expect(r.entry.price).toBe(200_000_000n);
    expect(r.exit.takeProfit).toBe(260_000_100n);
  });

  it('asks for exactly the exit legs the type needs; the exit minimum fill and tip are optional', () => {
    expect(buildIntent(form('ifd', 'buy', { amount: '4', price: '2', 'exit.kind': 'stop' }), CTX).errors).toEqual([{ field: 'exit.stop', code: 'required' }]);
    const both = buildIntent(form('ifo', 'buy', { amount: '4', price: '2', 'exit.takeProfit': '3' }), CTX);
    expect(both.errors).toEqual([{ field: 'exit.stop', code: 'required' }]);
    const stopLimit = buildIntent(form('ifo', 'buy', { amount: '4', price: '2', 'exit.takeProfit': '3', 'exit.stop': '1.5', 'exit.stopLimit': '1.4' }), CTX).intent as any;
    expect(stopLimit.exit.stopLimit).toBe(140_000_000n);
    expect(stopLimit.exit.slipBps).toBeUndefined();
    expect(stopLimit.exit.minFill).toBeUndefined();
    const exitKnobs = buildIntent(form('ifd', 'sell', { amount: '4', price: '2.7', 'exit.takeProfit': '2.4', 'exit.minFill': '0.5', 'exit.tip': '0.001', prefund: '0.3', minFill: '1' }), CTX).intent as any;
    expect(exitKnobs.exit).toMatchObject({ minFill: 50_000_000n, tip: 100_000n });
    expect(exitKnobs.prefund).toBe(30_000_000n);
    expect(exitKnobs.minFill).toBe(T);
  });

  it('leaves the repeat count out for "unlimited" and sets it otherwise', () => {
    const unl = buildIntent(form('repeatIfd', 'buy', { amount: '4', price: '2', 'exit.takeProfit': '3' }), CTX).intent as any;
    expect(unl.repeat).toEqual({});
    const k = buildIntent(form('repeatIfd', 'buy', { amount: '4', price: '2', 'exit.takeProfit': '3', 'repeat.count': '5' }), CTX).intent as any;
    expect(k.repeat).toEqual({ count: 5n });
  });

  it('a GTD limit needs its date', () => {
    expect(buildIntent(form('limit', 'sell', { amount: '1', price: '1', lifetime: 'gtd' }), CTX).errors).toEqual([{ field: 'lifetimeAt', code: 'required' }]);
    const r = buildIntent(form('limit', 'sell', { amount: '1', price: '1', lifetime: 'gtd', lifetimeAt: '2026-10-01T00:00' }), CTX).intent as any;
    expect(r.lifetime).toEqual({ kind: 'gtd', at: 1_790_812_800n });
  });

  it('close sells the whole balance when the amount is empty', () => {
    expect(buildIntent(initialForm('close', 'sell'), CTX).intent).toMatchObject({ type: 'close' });
    expect((buildIntent(initialForm('close', 'sell'), CTX).intent as any).amount).toBeUndefined();
  });
});

describe('every order type: form -> intent -> plan (consensus-valid)', () => {
  it('covers all types', () => {
    expect(new Set(CASES.map((c) => c[1]))).toEqual(new Set(ORDER_TYPES));
  });

  for (const [name, type, side, values] of CASES) {
    it(name, () => {
      const f = form(type, side, values);
      const built = buildIntent(f, CTX);
      expect(built.errors, json(built.errors)).toEqual([]);
      const env = ticketEnv();
      const plan = planOrder(env, built.intent!);
      expect(errors(plan), json(errors(plan))).toEqual([]);
      expect(plan.ok).toBe(true);
      consensusCheck(K, plan.built!);
      // the disclosure describes the side and the exact amount the form asked for, and a minimum fill within it
      const d = plan.disclosure!;
      expect(d.side).toBe(type === 'close' ? 'sell' : side);
      const typed = values.amount !== undefined ? parseAmount(values.amount, 8) : null;
      expect(d.tokenAmount).toBe(typed && typed.ok ? typed.value : 100n * TOKEN);
      expect(d.scale).toBe(TOKEN);
      expect(d.minFill >= 1n && d.minFill <= d.tokenAmount, `minFill ${d.minFill}`).toBe(true);
    });

    it(`${name}: intent -> form -> intent is the identity`, () => {
      const built = buildIntent(form(type, side, values), CTX);
      const back = buildIntent(formFromIntent(built.intent!, CTX), CTX);
      expect(back.errors).toEqual([]);
      expect(json(back.intent)).toBe(json(built.intent));
    });
  }

  it('the minimum fill: the typed one, else the wallet default; immediate orders default to the smallest unit', () => {
    const plan = (values: Record<string, string>, type: 'limit' | 'ioc' = 'limit', side: 'buy' | 'sell' = 'sell') => planOrder(ticketEnv(), buildIntent(form(type, side, values), CTX).intent!);
    const typed = plan({ amount: '5', price: '2.6', minFill: '0.5' });
    expect(errors(typed)).toEqual([]);
    expect(typed.disclosure!.minFill).toBe(50_000_000n);
    // default of a resting order: kob-wasm defaultMinFill (about one delivery carrier of value), at most the whole amount
    const def = plan({ amount: '5', price: '2.6' });
    expect(def.disclosure!.minFill).toBe(K.defaultMinFill(5n * TOKEN, 260_000_000n, TOKEN));
    expect(plan({ amount: '3', price: '2.4' }, 'ioc').disclosure!.minFill).toBe(1n);
    // a minimum fill above the amount is refused by the planner
    expect(errors(plan({ amount: '1', price: '2.6', minFill: '2' })).map((i) => i.code)).toContain('MIN_FILL_INVALID');
    // the formatted minimum fill comes back from the intent unchanged
    const r = buildIntent(form('limit', 'sell', { amount: '5', price: '2.6', minFill: '0.12345678' }), CTX);
    expect(formFromIntent(r.intent!, CTX).values.minFill).toBe('0.12345678');
  });

  it('the planner reports its findings against the form field names', () => {
    const env = ticketEnv({ funding: [10n * 100_000_000n] });
    const r = buildIntent(form('limit', 'buy', { amount: '50', price: '2.3' }), CTX);
    const plan = planOrder(env, r.intent!);
    expect(plan.ok).toBe(false);
    expect(errors(plan).map((i) => i.code)).toContain('INSUFFICIENT_KAS');
  });

  it('a price between two ticks is rounded before it reaches the planner, so the planner never refuses the tick', () => {
    const r = buildIntent(form('limit', 'sell', { amount: '1', price: '2.60000001' }), CTX);
    const plan = planOrder(ticketEnv(), r.intent!);
    expect(errors(plan)).toEqual([]);
    expect((r.intent as any).price).toBe(260_000_100n);
  });

  it('a 12-decimal token: prices per 10^9 base units, amounts exact', () => {
    const wide = market8x8({ decimals: 12, tick: 1 });
    expect(wide.scale).toBe(1_000_000_000n);
    const r = buildIntent(form('limit', 'sell', { amount: '3.000000000001', price: '5.2' }), WIDE);
    expect(r.errors).toEqual([]);
    const plan = planOrder(makeEnv({ market: wide, tokenAmounts: [100n * 10n ** 12n], book: { asks: [], bids: [] } }), r.intent!);
    expect(errors(plan)).toEqual([]);
    expect((r.intent as any).price).toBe(520_000n);
    expect(plan.disclosure!.limitPrice).toBe(520_000n);
    expect(plan.disclosure!.tokenAmount).toBe(3_000_000_000_001n);
    consensusCheck(K, plan.built!);
  });
});

describe('maxAmount', () => {
  it('sell: the free token balance, exactly', () => {
    expect(maxAmount({ side: 'sell', tokenBalance: 1_050_000_001n, kasBalance: 0n, scale: TOKEN, carrier: 10n * 100_000_000n })).toBe(1_050_000_001n);
    expect(maxAmount({ side: 'sell', tokenBalance: 0n, kasBalance: 0n, scale: TOKEN, carrier: 0n })).toBe(0n);
  });
  it('buy: balance minus the carrier reserve over the all-in price; zero without a price or funds', () => {
    const base = { side: 'buy' as const, tokenBalance: 0n, scale: TOKEN, carrier: 10n * 100_000_000n };
    // (1000 - 4 x 10 - 0.2) KAS / 2.5 KAS per token = 383.92 tokens
    expect(maxAmount({ ...base, kasBalance: 1_000n * 100_000_000n, allInPrice: 250_000_000n })).toBe(38_392_000_000n);
    expect(maxAmount({ ...base, kasBalance: 1_000n * 100_000_000n })).toBe(0n);
    expect(maxAmount({ ...base, kasBalance: 10n * 100_000_000n, allInPrice: 250_000_000n })).toBe(0n);
  });
});

describe('exit lifetime, timed activation, trailing IFD exit', () => {
  const at = '2026-10-05T00:00';
  const unix = parseLocalDateTime(at, 0);

  it('the exit lifetime maps to exit.expiry and back (gtc leaves it out)', () => {
    for (const type of ['ifd', 'ifo', 'repeatIfd', 'repeatIfo'] as const) {
      const v = { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', ...(type.endsWith('fo') ? { 'exit.stop': '2.1' } : {}) };
      expect(layoutOf(form(type, 'buy', v)).advanced).toContain('exit.lifetime');
      expect(layoutOf(form(type, 'buy', v)).advanced).not.toContain('exit.lifetimeAt');
      expect(layoutOf(form(type, 'buy', { ...v, 'exit.lifetime': 'gtd' })).advanced).toContain('exit.lifetimeAt');
      expect(buildIntent(form(type, 'buy', v), CTX).intent).not.toHaveProperty('exit.expiry');
      const g = buildIntent(form(type, 'buy', { ...v, 'exit.lifetime': 'gtd', 'exit.lifetimeAt': at }), CTX);
      expect(g.errors).toEqual([]);
      expect((g.intent as { exit: { expiry: unknown } }).exit.expiry).toEqual({ kind: 'gtdUnix', atUnixSeconds: (unix as { value: bigint }).value });
      const back = buildIntent(formFromIntent(g.intent!, CTX), CTX);
      expect(json(back.intent)).toBe(json(g.intent));
    }
    // a date is required once "good till a date" is chosen
    expect(buildIntent(form('ifd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', 'exit.lifetime': 'gtd' }), CTX).errors.map((e) => e.field)).toContain('exit.lifetimeAt');
  });

  it('the planner takes the dated exit and a trailing single-stop IFD exit', () => {
    const f = form('ifd', 'buy', { amount: '4', price: '2.3', 'exit.kind': 'stop', 'exit.stop': '2.1', 'exit.trail': 'true', 'exit.trail.step': '0.05', 'exit.trail.gap': '0.1', 'exit.lifetime': 'gtd', 'exit.lifetimeAt': '2026-10-05T00:00' });
    expect(layoutOf(f).advanced).toEqual(expect.arrayContaining(['exit.trail', 'exit.trail.step', 'exit.trail.gap']));
    expect(layoutOf(form('ifd', 'buy', { 'exit.kind': 'takeProfit' })).advanced).not.toContain('exit.trail');
    const r = buildIntent(f, CTX);
    expect(r.errors).toEqual([]);
    const plan = planOrder(ticketEnv(), r.intent!);
    expect(errors(plan)).toEqual([]);
    expect(plan.disclosure!.notes).toEqual(expect.arrayContaining(['trailing', 'exitGtd']));
    expect(json(buildIntent(formFromIntent(r.intent!, CTX), CTX).intent)).toBe(json(r.intent));
  });

  it('activeFrom is offered on every conditional type, maps to the intent and back', () => {
    for (const type of ['stopMarket', 'stopLimit', 'trailingStop', 'takeProfit', 'oco', 'ifd', 'ifo', 'repeatIfd', 'repeatIfo'] as const) {
      expect(layoutOf(form(type, 'buy', {})).advanced, type).toContain('activeFrom');
    }
    const f = form('oco', 'sell', { amount: '5', takeProfit: '2.7', stop: '2.2', activeFrom: '2026-10-05T00:00' });
    const r = buildIntent(f, CTX);
    expect((r.intent as { activeFrom: unknown }).activeFrom).toEqual({ unixSeconds: (unix as { value: bigint }).value });
    expect(json(buildIntent(formFromIntent(r.intent!, CTX), CTX).intent)).toBe(json(r.intent));
    const i = buildIntent(form('ifd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', activeFrom: '2026-10-05T00:00' }), CTX);
    expect((i.intent as { activeFrom: unknown }).activeFrom).toEqual({ unixSeconds: (unix as { value: bigint }).value });
  });

  it('the ladder fields exist for the repeat types only; the step shows from two levels', () => {
    for (const type of ['repeatIfd', 'repeatIfo'] as const) {
      expect(layoutOf(form(type, 'buy', {})).advanced).toContain('ladder.levels');
      expect(layoutOf(form(type, 'buy', {})).advanced).not.toContain('ladder.step');
      expect(layoutOf(form(type, 'buy', { 'ladder.levels': '3' })).advanced).toContain('ladder.step');
    }
    for (const type of ['ifd', 'ifo', 'limit', 'oco'] as const) expect(layoutOf(form(type, 'buy', {})).advanced).not.toContain('ladder.levels');
    expect(FIELD_DEFAULTS['ladder.levels']).toBe('1');
  });
});

describe('pair ticket: prices in the quote token B, tips in KAS', () => {
  // A = the base (3 decimals, scale 1000), B = a quote token of 6 decimals: prices are B base units per whole A
  const PAIR: TicketCtx = { decimals: 3, scale: 1000n, tick: 1n, quoteDecimals: 6 };

  it('reads a price in B per whole A with the decimals of B and rounds it in the maker favour', () => {
    expect(parsePrice('2.5', 'sell', PAIR)).toEqual({ ok: true, value: { price: 2_500_000n, rounded: false } });
    expect(parsePrice('0.000001', 'buy', PAIR)).toEqual({ ok: true, value: { price: 1n, rounded: false } });
    // more decimals than B has is an error, never a rounding
    expect(parsePrice('0.0000001', 'sell', PAIR)).toMatchObject({ ok: false, code: 'precision', params: { decimals: 6 } });
    // a base token of more than 9 decimals quotes per 10^9 base units: a sell rounds up, a buy down
    const wide: TicketCtx = { decimals: 12, scale: 1_000_000_000n, tick: 1n, quoteDecimals: 6 };
    expect(parsePrice('0.000001', 'sell', wide)).toEqual({ ok: true, value: { price: 1n, rounded: true } });
    expect(parsePrice('0.000001', 'buy', wide)).toMatchObject({ ok: false, code: 'positive' });
    expect(priceInfo('1.234567', 'sell', PAIR)).toEqual({ price: 1_234_567n, perToken: '1.234567', rounded: false });
  });

  it('keeps the tip in KAS (sompi per whole A) and the prices in B through the intent and back', () => {
    const f = form('limit', 'sell', { amount: '4.5', price: '2.5', tip: '0.001' });
    const r = buildIntent(f, PAIR);
    expect(r.errors).toEqual([]);
    expect(r.intent).toMatchObject({ type: 'limit', side: 'sell', amount: 4_500n, price: 2_500_000n, tip: 100_000n });
    const back = formFromIntent(r.intent!, PAIR);
    expect(back.values.price).toBe('2.5');
    expect(back.values.tip).toBe('0.001');
    expect(json(buildIntent(back, PAIR).intent)).toBe(json(r.intent));
    // the same text on the KAS ticket: the price is KAS (8 decimals)
    expect(buildIntent(f, { ...PAIR, quoteDecimals: undefined }).intent).toMatchObject({ price: 250_000_000n, tip: 100_000n });
  });

  it('reads every price field of the if-done and trailing types in B (prefund and trail distances too), the exit tip in KAS', () => {
    const f = form('repeatIfo', 'sell', {
      amount: '10', price: '2', 'exit.takeProfit': '1.8', 'exit.stop': '2.4', 'repeat.count': '3', prefund: '0.2', 'exit.tip': '0.0005',
      'exit.trail': 'true', 'exit.trail.step': '0.01', 'exit.trail.gap': '0.02',
    });
    const r = buildIntent(f, PAIR);
    expect(r.errors).toEqual([]);
    expect(r.intent).toMatchObject({
      entry: { price: 2_000_000n }, prefund: 200_000n,
      exit: { takeProfit: 1_800_000n, stop: 2_400_000n, tip: 50_000n, trail: { step: 10_000n, gap: 20_000n } },
    });
    expect(json(buildIntent(formFromIntent(r.intent!, PAIR), PAIR).intent)).toBe(json(r.intent));
  });
});
