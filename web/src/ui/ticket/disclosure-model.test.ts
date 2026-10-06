import { describe, expect, it } from 'vitest';
import { planOrder } from '../../kob/plan';
import { errors } from '../../kob/plan-types';
import { CLOCK, kob } from '../../testing/fixtures';
import { NOTE_TAGS, buildDisclosureModel, kasText, type DisclosureCtx, type DisclosureModel, type DisclosureRow, type Txt } from './disclosure-model';
import { buildIntent } from './form-state';
import { CASES, CTX, MARKET, TOKEN, form, ticketEnv } from './ticket-fixtures';
import { formatTokenAmount } from '../../kob/units';

const CD: DisclosureCtx = { ticker: 'EXKCC', decimals: 8, scale: TOKEN, clock: CLOCK };

function modelOf(type: Parameters<typeof form>[0], side: 'buy' | 'sell', values: Record<string, string>): DisclosureModel {
  const built = buildIntent(form(type, side, values), CTX);
  expect(built.errors).toEqual([]);
  const plan = planOrder(ticketEnv(), built.intent!);
  expect(errors(plan)).toEqual([]);
  const m = buildDisclosureModel(plan, CD);
  expect(m).not.toBeNull();
  return m!;
}
const row = (rows: DisclosureRow[], id: string): DisclosureRow => {
  const r = rows.find((x) => x.id === id);
  if (!r) throw new Error(`no row ${id} in ${rows.map((x) => x.id).join(',')}`);
  return r;
};
const text = (t: Txt | undefined): string => (t === undefined ? '' : typeof t === 'string' ? t : `${t.key}:${JSON.stringify(t.params)}`);

describe('disclosure model', () => {
  it('a sell receives the limit minus the tip, a buy pays the limit plus the tip (all-in)', () => {
    const sell = modelOf('limit', 'sell', { amount: '4', price: '2.6', tip: '0.01' });
    expect(row(sell.price, 'limit').value).toBe('2.6 KAS / EXKCC');
    expect(row(sell.price, 'allInPrice').labelKey).toBe('ticket.disc.allInReceive');
    expect(row(sell.price, 'allInPrice').value).toBe('2.59 KAS / EXKCC');
    expect(row(sell.price, 'allInTotal').value).toBe('10.36 KAS');
    const buy = modelOf('limit', 'buy', { amount: '4', price: '2.3', tip: '0.01' });
    expect(row(buy.price, 'allInPrice').labelKey).toBe('ticket.disc.allInPay');
    expect(row(buy.price, 'allInPrice').value).toBe('2.31 KAS / EXKCC');
    expect(row(buy.price, 'allInTotal').labelKey).toBe('ticket.disc.totalPay');
    expect(row(buy.price, 'allInTotal').value).toBe('9.24 KAS');
    // the tip row shows only when there is a tip
    expect(modelOf('limit', 'sell', { amount: '4', price: '2.6' }).price.some((r) => r.id === 'tip')).toBe(false);
  });

  it('itemises every carrier, and the locked total is what the lines add up to', () => {
    const m = modelOf('limit', 'sell', { amount: '4', price: '2.6' });
    expect(m.carriers.map((c) => c.kind)).toEqual(['orderCarrier', 'tokenCarrier', 'tokenChangeCarrier']);
    expect(m.carriers.find((c) => c.kind === 'tokenChangeCarrier')!.kept).toBe(true);
    expect(m.kasLocked).toBe(kasText(20n * 100_000_000n));
    expect(m.tokensEscrowed).toBe('4 EXKCC');
    expect(m.fee).toMatch(/KAS$/);
    const b = modelOf('limit', 'buy', { amount: '4', price: '2.3' });
    expect(b.carriers.map((c) => c.kind)).toContain('escrow');
    expect(b.tokensEscrowed).toBeNull();
  });

  it('shows expected and worst price of a market order and its auction notes', () => {
    const m = modelOf('market', 'buy', { amount: '2' });
    expect(row(m.price, 'expected').value).toBe('2.5 KAS / EXKCC');
    expect(row(m.price, 'worst').tone).toBe('warn');
    expect(m.notes.map((n) => n.tag)).toEqual(expect.arrayContaining(['auction', 'iocRemainderReturned', 'market']));
  });

  it('day order: shows the 00:00 UTC deadline and its JST time; GTC shows the day-85 renewal', () => {
    const day = modelOf('limit', 'sell', { amount: '1', price: '2.6', lifetime: 'day' });
    const t = day.times.find((x) => x.id === 'expiry')!;
    expect(t.labelKey).toBe('ticket.time.day');
    expect(t.utc).toBe('2026-09-30 00:00 UTC');
    expect(t.jst).toBe('2026-09-30 09:00 JST');
    const gtc = modelOf('limit', 'sell', { amount: '1', price: '2.6' });
    const g = gtc.times.find((x) => x.id === 'expiry')!;
    expect(g.labelKey).toBe('ticket.time.gtc');
    expect(g.extra!.unix).toBe(CLOCK.unixSeconds + 85n * 86_400n);
    expect(g.unix).toBe(CLOCK.unixSeconds + 90n * 86_400n);
  });

  it('timed activation and IOC expiry rows', () => {
    const m = modelOf('ioc', 'sell', { amount: '1', price: '2.4', activeFrom: '2026-10-01T00:00' });
    expect(m.times.map((x) => x.labelKey)).toEqual(expect.arrayContaining(['ticket.time.activates', 'ticket.time.ioc']));
  });

  it('stops: trigger, worst price of the band, keeper reserve and the exposure sentence', () => {
    const m = modelOf('stopMarket', 'sell', { amount: '5', stop: '2.2' });
    expect(row(m.extras, 'stop').value).toBe('2.2 KAS / EXKCC');
    expect(row(m.extras, 'stopWorst').value).toBe('2.134 KAS / EXKCC');
    expect(text(row(m.extras, 'trigger').value)).toContain('ticket.val.trigger');
    const trigger = m.notes.find((n) => n.tag === 'stopTrigger')!;
    expect(trigger.params.exposure).toBe('5');
    // the default threshold is the order's minimum fill
    expect(trigger.params.volume).toBe(m.minTouch);
    expect(m.minTouch).toBe(m.minFill);
    const auction = m.notes.find((n) => n.tag === 'stopAuction')!;
    expect(auction.params.auction).toBe('30');
    expect(auction.params.slippage).toBe('3');
    // the default keeper tip of the token program (kob-wasm keeperTips)
    expect(m.notes.find((n) => n.tag === 'keeperReserve')!.params.tip).toBe(kasText(MARKET.keeperTip));
  });

  it('trailing: step, gap and rate', () => {
    const m = modelOf('trailingStop', 'sell', { amount: '5', stop: '2.2', 'trail.step': '0.05', 'trail.gap': '0.1' });
    const t = row(m.extras, 'trail');
    expect(text(t.value)).toContain('"step":"0.05 KAS"');
    expect(text(t.value)).toContain('"gap":"0.1 KAS"');
    expect(text(t.detail)).toContain('"minutes":"10"');
    expect(m.notes.find((n) => n.tag === 'trailing')!.params.wait).toBe('10');
  });

  it('IFD / repeat: entry, exit, profit per token and the repeat sentences', () => {
    const m = modelOf('repeatIfd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', 'repeat.count': '3' });
    expect(text(row(m.extras, 'repeat').value)).toContain('ticket.val.repeatCount');
    // 2.6 - 2.3 = 0.3 KAS per token, minus the merge tip every repeat pays
    expect(row(m.extras, 'profitPerToken').value).toBe('0.29 KAS / EXKCC');
    expect(row(m.extras, 'exitTakeProfit').value).toBe('2.6 KAS / EXKCC');
    expect(m.notes.map((n) => n.tag)).toEqual(expect.arrayContaining(['repeat', 'repeatCounted', 'repeatReBuys', 'repeatStopLossEnds', 'cancelPosition', 'position']));
    expect(m.notes.find((n) => n.tag === 'repeatCounted')!.params.count).toBe('3');
    const unlimited = modelOf('repeatIfd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6' });
    expect(text(row(unlimited.extras, 'repeat').value)).toContain('ticket.val.repeatUnlimited');
    expect(unlimited.notes.map((n) => n.tag)).toContain('repeatUnlimited');
  });

  it('sell-first shows the prefund', () => {
    const m = modelOf('ifd', 'sell', { amount: '4', price: '2.7', 'exit.takeProfit': '2.4' });
    expect(m.extras.some((r) => r.id === 'prefund')).toBe(true);
    expect(m.notes.map((n) => n.tag)).toContain('sellFirst');
  });

  it('returns null for a plan without a transaction', () => {
    const built = buildIntent(form('limit', 'buy', { amount: '5000', price: '2.3' }), CTX);
    const plan = planOrder(ticketEnv({ funding: [100_000_000n] }), built.intent!);
    expect(plan.ok).toBe(false);
    expect(buildDisclosureModel(plan, CD)).toBeNull();
  });

  it('every note tag any order type emits is a known tag (so it has a translation)', () => {
    const seen = new Set<string>();
    for (const [name, type, side, values] of CASES) {
      const m = modelOf(type, side, values);
      for (const n of m.notes) {
        seen.add(n.tag);
        expect(NOTE_TAGS as readonly string[], `${name}: unknown note tag ${n.tag}`).toContain(n.tag);
      }
    }
    // most of the vocabulary is exercised by the cases (the rest is emitted only by variants of them)
    expect(seen.size).toBeGreaterThan(30);
  });
});

describe('minimum fill and trigger threshold rows', () => {
  it('shows the default minimum fill of a resting order, the typed one, and the smallest unit of an immediate order', () => {
    const K = kob();
    const def = modelOf('limit', 'sell', { amount: '4', price: '2.6' });
    const want = K.defaultMinFill(4n * TOKEN, 260_000_000n, TOKEN);
    expect(want > 0n && want <= 4n * TOKEN).toBe(true);
    expect(def.minFill).toBe(`${formatTokenAmount(want, 8, { group: ',' })} EXKCC`);
    expect(modelOf('limit', 'sell', { amount: '4', price: '2.6', minFill: '1.5' }).minFill).toBe('1.5 EXKCC');
    expect(modelOf('ioc', 'sell', { amount: '1', price: '2.4' }).minFill).toBe('0.00000001 EXKCC');
    // a plain limit has no trigger
    expect(def.minTouch).toBeNull();
  });

  it('the trigger threshold follows the chosen preset', () => {
    expect(modelOf('stopMarket', 'sell', { amount: '8', stop: '2.2', minTouch: '50%' }).minTouch).toBe('4 EXKCC');
    expect(modelOf('stopMarket', 'sell', { amount: '8', stop: '2.2', minTouch: '0.3' }).minTouch).toBe('0.3 EXKCC');
  });
});

describe('exit lifetime and activation rows', () => {
  it('an IFD says its exits last until cancelled, or until the chosen date', () => {
    const gtc = modelOf('ifd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6' });
    expect(row(gtc.extras, 'exitLifetime').value).toEqual({ key: 'ticket.val.exitGtc', params: {} });
    const gtd = modelOf('ifo', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', 'exit.stop': '2.1', 'exit.lifetime': 'gtd', 'exit.lifetimeAt': '2026-10-05T00:00' });
    const v = row(gtd.extras, 'exitLifetime').value as { key: string; params: Record<string, string> };
    expect(v.key).toBe('ticket.val.exitGtd');
    expect(v.params.utc).toContain('2026-10-05');
  });

  it('a timed conditional shows when it activates', () => {
    expect(modelOf('stopMarket', 'sell', { amount: '5', stop: '2.2' }).times.some((r) => r.id === 'activates')).toBe(false);
    const m = modelOf('stopMarket', 'sell', { amount: '5', stop: '2.2', activeFrom: '2026-10-05T00:00' });
    expect(m.times.find((r) => r.id === 'activates')?.utc).toContain('2026-10-05');
    expect(modelOf('ifd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', activeFrom: '2026-10-05T00:00' }).times.some((r) => r.id === 'activates')).toBe(true);
  });
});

describe('disclosure model of a token/token pair', () => {
  // A = EXKCC (8 decimals), B = EXUSD (6 decimals): prices are B base units per whole A, tips KAS
  const PC: DisclosureCtx = { ticker: 'EXKCC', decimals: 8, scale: TOKEN, clock: CLOCK, quote: { ticker: 'EXUSD', decimals: 6 } };
  const disclosure = {
    side: 'sell' as const, tokenAmount: 400_000_000n, scale: TOKEN, minFill: 100_000_000n, minTouch: 100_000_000n,
    limitPrice: 2_500_000n, allInPrice: 2_500_000n, allInTotal: 10_000_001n, expectedPrice: null, worstPrice: null, tip: 100_000n,
    carriers: [{ kind: 'orderCarrier', amount: 1_000_000_000n, count: 1 }], kasLocked: 1_000_000_000n, tokensEscrowed: 400_000_000n, fee: 3_000_000n,
    refundTip: 6_400_000n, keeperTip: 3_600_000n, expiry: { kind: 'none' as const, daa: null, approxUnixSeconds: null, deadlineUnixSeconds: null }, activatesAt: null, notes: [],
  };
  const trigger = {
    kind: 'KobCondPair' as const, stop: '2200000', direction: 'fallsTo' as const, minTouch: '100000000', minTouchB: '2200000', minRestDaa: '50',
    arm: { kasBooks: { a: 'ask' as const, b: 'bid' as const }, pair: 'ask' as const }, trail: null,
  };
  const plan = { ok: true, issues: [], states: [], request: null, built: null, disclosure, pair: { escrowB: 0n, receiveMinB: 10_000_001n, tipKasTotal: 400_000n, trigger } };

  it('prices in B per whole A, the total in B, the tip in KAS, the pair trigger rule', () => {
    const m = buildDisclosureModel(plan as unknown as Parameters<typeof buildDisclosureModel>[0], PC)!;
    expect(row(m.price, 'limit').value).toBe('2.5 EXUSD / EXKCC');
    expect(row(m.price, 'tip').value).toBe('0.001 KAS / EXKCC');
    expect(row(m.price, 'allInTotal').value).toBe('10.000001 EXUSD');
    expect(m.kasLocked).toBe('10 KAS');
    expect(m.pair?.quoteTicker).toBe('EXUSD');
    const rows = m.pair!.rows;
    expect(rows.map((r) => r.id)).toEqual(['pairReceiveMinB', 'pairTipKas', 'pairTrigger']);
    expect(row(rows, 'pairReceiveMinB').value).toBe('10.000001 EXUSD');
    expect(row(rows, 'pairTipKas').value).toBe('0.004 KAS');
    const t = text(row(rows, 'pairTrigger').value);
    expect(t).toContain('ticket.val.pairTriggerSell');
    expect(t).toContain('"stop":"2.2 EXUSD / EXKCC"');
    expect(t).toContain('"seconds":"5"');
    expect(text(row(rows, 'pairTrigger').detail)).toContain('"b":"2.2 EXUSD"');
    // the same plan on a KAS market has no pair rows
    expect(buildDisclosureModel(plan as unknown as Parameters<typeof buildDisclosureModel>[0], CD)!.pair).toBeNull();
  });
});
