// Every key the notification UI builds dynamically exists, and describeEvent renders every kind without leftover placeholders.
import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { NOTIFY_KINDS, type NotificationEvent } from '../../kob/notifications';
import type { TokenInfo } from '../../kob/registry';
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
    expect(fill).toContain('Your fill of 3 EXT at 0.0002505 KAS/EXT');
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
