import { describe, expect, it } from 'vitest';
import type { ComponentChildren, VNode } from 'preact';
import { unitParts } from './UnitText';

/** The plain text of a vnode tree, and the text inside `.unit-per` spans. */
function walk(n: ComponentChildren, small: boolean, out: { all: string; small: string }) {
  if (n === null || n === undefined || typeof n === 'boolean') return;
  if (Array.isArray(n)) return void n.forEach((c) => walk(c, small, out));
  if (typeof n === 'string' || typeof n === 'number') {
    out.all += n;
    if (small) out.small += n;
    return;
  }
  const v = n as VNode<{ class?: string; children?: ComponentChildren }>;
  walk(v.props.children, small || v.props.class === 'unit-per', out);
}
const read = (c: ComponentChildren) => {
  const out = { all: '', small: '' };
  walk(c, false, out);
  return out;
};

describe('unitParts', () => {
  it('keeps the text and marks only the "per TOKEN" part', () => {
    expect(read(unitParts('KAS per TUSD'))).toEqual({ all: 'KAS per TUSD', small: 'per TUSD' });
    expect(read(unitParts('Price (KAS per EXKCC)'))).toEqual({ all: 'Price (KAS per EXKCC)', small: 'per EXKCC' });
    expect(read(unitParts('per TUSD'))).toEqual({ all: 'per TUSD', small: 'per TUSD' });
  });
  it('leaves a text without a unit alone', () => {
    expect(unitParts('Price (KAS)')).toBe('Price (KAS)');
  });
  it('single: only "per" + one word', () => {
    expect(read(unitParts('1.5 KAS per TUSD', true)).small).toBe('per TUSD');
    expect(unitParts('one update per 5 min at most', true)).toBe('one update per 5 min at most');
  });
});
