import { describe, expect, it } from 'vitest';
import { ORDER_TYPES } from './form-state';
import { labelText, orderLabel } from './order-label';

describe('order labels', () => {
  it('name the side and the type in the ticket\'s words', () => {
    expect(orderLabel('ifd', 'buy')).toBe('Buy · IFD (if done)');
    expect(orderLabel('limit', 'sell')).toBe('Sell · Limit');
    expect(orderLabel('limit', 'buy', { level: 2, levels: 5 })).toBe('Buy · Limit (2/5)');
    expect(orderLabel('limit', 'buy', { levels: 5 })).toBe('Buy · Limit ×5');
    expect(orderLabel('limit', 'buy', { levels: 1 })).toBe('Buy · Limit');
  });

  it('never show a form id for any order type', () => {
    for (const type of ORDER_TYPES) {
      const l = orderLabel(type, 'sell', undefined, 'EXKCC');
      expect(l, type).not.toContain(`${type} `);
      expect(l, type).not.toMatch(/ticket\.|\{/);
    }
  });

  it('show the labels older versions stored ("ifd buy") the same way; other notes stay as they are', () => {
    expect(labelText('ifd buy')).toBe('Buy · IFD (if done)');
    expect(labelText('limit sell 2/5')).toBe('Sell · Limit (2/5)');
    expect(labelText('limit buy x3')).toBe('Buy · Limit ×3');
    expect(labelText('repeatIfd sell')).toBe(orderLabel('repeatIfd', 'sell'));
    expect(labelText('Amended')).toBe('Amended');
    expect(labelText('banana buy')).toBe('banana buy');
    expect(labelText(null)).toBeNull();
    expect(labelText('')).toBeNull();
  });
});
