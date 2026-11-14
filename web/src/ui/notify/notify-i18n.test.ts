// Every key the notification UI builds dynamically exists, and describeEvent renders every kind without leftover placeholders.
import { afterEach, describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { NOTIFY_KINDS, type NotificationEvent } from '../../kob/notifications';
import type { TokenInfo } from '../../kob/registry';
import { rateOf } from '../../app/fiat';
import { noteMarketDecimals, resetDeclarationsForTests } from '../../data/token-declarations';
import { REORG_WHATS, describeEvent } from './describe';

describe('notify i18n', () => {
  it('has title, body and settings label for every kind', () => {
    const missing: string[] = [];
    for (const k of NOTIFY_KINDS) for (const key of [`notify.ev.${k}.title`, `notify.ev.${k}.body`, `notify.kind.${k}`]) if (!has(key)) missing.push(key);
    for (const s of ['unpaid', 'pending', 'paid', 'expired', 'failed']) if (!has(`notify.status.${s}`)) missing.push(`notify.status.${s}`);
    for (const s of ['sell', 'buy']) if (!has(`notify.side.${s}`)) missing.push(`notify.side.${s}`);
    expect(missing).toEqual([]);
  });

  it('renders every kind without unresolved placeholders', () => {
    const tok = 'cc'.repeat(32);
    for (const kind of NOTIFY_KINDS) {
      const e: NotificationEvent = {
        id: 'x', kind, orderId: 'aa'.repeat(32), token: tok, side: 'sell', at: 1_800_000_000_000, read: false,
        params: { total: '5', at: 1_800_003_600, amount: '123456789', status: 'paid', reference: 'inv-1', invoice: 'ab'.repeat(32) },
      };
      const d = describeEvent(e, null);
      expect(`${d.title} ${d.body}`).not.toMatch(/[{}]/);
      expect(d.href.startsWith('#/')).toBe(true);
    }
  });

  it('describes every kind of reorg change with specifics and no placeholders', () => {
    const tok = 'cc'.repeat(32);
    const ev = (params: NotificationEvent['params']): NotificationEvent => ({ id: 'r', kind: 'reorg', orderId: 'aa'.repeat(32), token: tok, side: 'sell', at: 1, read: false, params });
    for (const what of REORG_WHATS) {
      expect(has(`notify.ev.reorg.${what}.body`), what).toBe(true);
      const d = describeEvent(ev({ what, amount: '300', filled: '0', total: '1000', state: 'open' }), null);
      expect(d.body).not.toMatch(/[{}]/);
      expect(d.body).toMatch(/reorganisation/);
    }
    // a known token: amounts in token units (2 decimals), the price per whole token
    const reg = { byCovenantId: new Map([[tok, { ticker: 'EXT', decimals: 2 } as unknown as TokenInfo]]) };
    const fill = describeEvent(ev({ what: 'fill', amount: '300', filled: '0', total: '1000', state: 'open', price: '25050' }), reg).body;
    expect(fill).toContain('Your fill of 3 EXT at 0.0002505 KAS per EXT');
    expect(fill).toContain(' at ');
    expect(fill).toContain('reverted');
    expect(fill).toContain('open again');
    const part = describeEvent(ev({ what: 'fill', amount: '400', filled: '650', total: '1000', state: 'partial' }), reg).body;
    expect(part).toContain('4 EXT');
    expect(part).toContain('6.5 of 10');
    // an unknown token: raw base units
    expect(describeEvent(ev({ what: 'fill', amount: '400', filled: '650', total: '1000', state: 'partial' }), null).body).toContain('650 of 1000');
    expect(part).not.toContain(' at ');
    expect(describeEvent(ev({}), null).body).not.toMatch(/[{}]/); // unknown "what": the generic text
  });
});

describe('notifications with the USD display', () => {
  const fiat = { rate: rateOf(0.0298)!, quote: { usd: 0.0298, at: 1_800_000_000, source: 'coingecko' }, testnet: false };
  const tok = 'cc'.repeat(32);
  const ev = (over: Partial<NotificationEvent>): NotificationEvent => ({ id: 'p', kind: 'payment', at: 1, read: false, params: {}, ...over });

  it('a KAS payment leads with the KAS amount and adds the dollar estimate; a token payment is never converted', () => {
    const kas = describeEvent(ev({ params: { amount: '3580000000000' } }), null, fiat);
    expect(kas.body).toBe('Received 35,800 KAS (about $1,066.84) at your address.');
    expect(describeEvent(ev({ params: { amount: '3580000000000' } }), null).body).toBe('Received 35,800 KAS at your address.');
    expect(describeEvent(ev({ params: { amount: '2946000' } }), null, fiat).body).toBe('Received 0.0295 KAS (about $0.0008779) at your address.');
    const reg = { byCovenantId: new Map([[tok, { ticker: 'EXT', decimals: 2 } as unknown as TokenInfo]]) };
    expect(describeEvent(ev({ token: tok, params: { amount: '12345' } }), reg, fiat).body).toBe('Received 123.45 EXT at your address.');
  });

  it('the price of a reverted fill is dollars per token', () => {
    const reg = { byCovenantId: new Map([[tok, { ticker: 'EXT', decimals: 2 } as unknown as TokenInfo]]) };
    // 2,510,000 sompi per whole token = 0.0251 KAS = $0.000748 at $0.0298
    const d = describeEvent(ev({ kind: 'reorg', orderId: 'aa'.repeat(32), token: tok, side: 'sell', params: { what: 'fill', amount: '300', filled: '0', total: '1000', state: 'open', price: '2510000' } }), reg, fiat);
    expect(d.body).toContain('$0.000748 per EXT');
    expect(d.body).not.toContain('KAS');
  });
});

describe('payment notifications name the amount and the unit like the portfolio does', () => {
  const fiat = { rate: rateOf(0.0298)!, quote: { usd: 0.0298, at: 1_800_000_000, source: 'coingecko' }, testnet: false };
  const id = '55aac2b5' + '0'.repeat(48) + '773e07bf';
  const ev = (over: Partial<NotificationEvent>): NotificationEvent => ({ id: 'p', kind: 'payment', at: 1, read: false, params: {}, ...over });
  afterEach(() => resetDeclarationsForTests());

  it('a token with decimals and a ticker: whole tokens and the ticker, with or without USD', () => {
    const reg = { byCovenantId: new Map([[id, { ticker: 'TUSD', decimals: 8 } as unknown as TokenInfo]]) };
    for (const f of [null, fiat]) expect(describeEvent(ev({ token: id, params: { amount: '2663250640' } }), reg, f, 'testnet-10').body).toBe('Received 26.6325 TUSD at your address.');
  });

  it('an unnamed token: its short id, in whole tokens where its decimals are known, else the raw amount', () => {
    const none = { byCovenantId: new Map() };
    expect(describeEvent(ev({ token: id, params: { amount: '2663250640' } }), none, fiat, 'testnet-10').body).toBe('Received 2,663,250,640 55aa…07bf at your address.');
    noteMarketDecimals('testnet-10', id, 8);
    expect(describeEvent(ev({ token: id, params: { amount: '2663250640' } }), none, fiat, 'testnet-10').body).toBe('Received 26.6325 55aa…07bf at your address.');
  });

  it('KAS without USD, and with USD after the KAS amount', () => {
    expect(describeEvent(ev({ params: { amount: '2946000' } }), null, null, 'testnet-10').body).toBe('Received 0.0295 KAS at your address.');
    const usd = describeEvent(ev({ params: { amount: '2946000' } }), null, fiat, 'testnet-10');
    expect(usd.title).toBe('Payment received');
    expect(usd.body).toBe('Received 0.0295 KAS (about $0.0008779) at your address.');
  });
});
