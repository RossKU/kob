// The inverted-market wording of the ticket: every `.inv` / `...Inv` text exists next to its native one and renders without leftover placeholders.
import { describe, expect, it } from 'vitest';
import ticket from '../../i18n/en/ticket';
import market from '../../i18n/en/market';
import { t } from '../../i18n';

const dict = { ...ticket, ...market } as Record<string, string>;
const INV = Object.keys(dict).filter((k) => /\.inv$|Inv$/.test(k));

describe('inverted wording', () => {
  it('has the expected set', () => {
    expect(INV.length).toBeGreaterThan(10);
  });
  it.each(INV)('%s renders and has a native counterpart', (key) => {
    const native = key.replace(/\.inv$/, '').replace(/Inv$/, '');
    expect(dict[native] ?? dict[`${native}.name`] ?? dict[`${native}.body`], native).toBeDefined();
    const text = t(key, { name: 'TUSD', ticker: 'TUSD' });
    expect(text).not.toMatch(/[{}]/);
  });
  it('the inverted help speaks of Buy / Sell as displayed, not of the native side', () => {
    expect(dict['ticket.type.close.helpInv']).toMatch(/^Buys KAS/);
    expect(dict['ticket.type.twap.nameInv']).toContain('buy KAS');
    expect(dict['ticket.type.dca.nameInv']).toContain('sell KAS');
    expect(dict['ticket.help.tip.inv']).toContain('A Buy receives');
  });
});
