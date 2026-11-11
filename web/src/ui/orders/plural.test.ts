import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { COUNT_NOUNS, countText, tCount } from './plural';

describe('counts in sentences', () => {
  it('picks one for exactly one and other for everything else', () => {
    expect(tCount('orders.cancelAll.confirm', 1)).toBe('Cancel 1 order');
    expect(tCount('orders.cancelAll.confirm', 0)).toBe('Cancel 0 orders');
    expect(tCount('orders.cancelAll.confirm', 2)).toBe('Cancel 2 orders');
    expect(tCount('orders.recover.count', 1n)).toBe('1 placement record is stored in this browser.');
    expect(tCount('orders.recover.count', 4)).toBe('4 placement records are stored in this browser.');
  });

  it('names a count with its noun', () => {
    expect(countText('order', 1)).toBe('1 order');
    expect(countText('utxo', 3)).toBe('3 stray token UTXOs');
    expect(countText('newRecord', 1)).toBe('1 new record');
  });

  it('every noun and every counted text has both forms, and no text says "(s)"', () => {
    for (const n of COUNT_NOUNS) for (const f of ['one', 'other']) expect(has(`orders.n.${n}.${f}`), `${n}.${f}`).toBe(true);
    for (const base of [
      'orders.summary.unpriced', 'orders.cancelAll.body', 'orders.cancelAll.confirm', 'orders.oldTemplate.body', 'orders.straysValue', 'orders.straysValueSweep',
      'orders.flow.sweepSubmitted', 'orders.flow.submitted', 'orders.flow.done', 'orders.flow.remaining', 'orders.recover.count', 'orders.recover.exported', 'orders.recover.apply',
    ]) {
      expect(has(`${base}.one`), `${base}.one`).toBe(true);
      expect(has(`${base}.other`), `${base}.other`).toBe(true);
    }
  });
});
