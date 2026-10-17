import { describe, expect, it } from 'vitest';
import { textOnly, translationKey } from './translation-safe';

const v = (type: unknown, props: Record<string, unknown>, extra: { key?: unknown; ref?: unknown } = {}) => ({ type, props, ...extra });

describe('translationKey (a machine-translated page re-creates an element whose text changes)', () => {
  it('keys a text-only element by its text, so a new text is a new element', () => {
    expect(translationKey(v('span', { children: 'checking...' }))).not.toBeNull();
    expect(translationKey(v('span', { children: ['Node', ': ', 'reachable'] }))).toBe(translationKey(v('span', { children: 'Node: reachable' })));
    expect(translationKey(v('span', { children: 'checking...' }))).not.toBe(translationKey(v('span', { children: 'reachable' })));
    expect(translationKey(v('td', { children: 42 }))).not.toBeNull();
    expect(translationKey(v('span', { children: ['a', null, false, 'b'] }))).toBe(translationKey(v('span', { children: 'ab' })));
  });

  it('leaves alone mixed content, components, text vnodes, empty elements', () => {
    expect(translationKey(v('span', { children: ['Node: ', { type: 'b', props: {} }] }))).toBeNull();
    expect(translationKey(v(() => null, { children: 'x' }))).toBeNull();
    expect(translationKey(v(null, 'x' as unknown as Record<string, unknown>))).toBeNull();
    expect(translationKey(v('span', {}))).toBeNull();
    expect(translationKey(v('span', { children: '' }))).toBeNull();
  });

  it('never re-creates what holds focus, form state or a handle: interactive elements, refs, explicit keys, focusable, untranslated', () => {
    for (const type of ['button', 'a', 'input', 'select', 'option', 'textarea', 'label', 'summary']) expect(translationKey(v(type, { children: 'x' }))).toBeNull();
    expect(translationKey(v('span', { children: 'x' }, { ref: () => {} }))).toBeNull();
    expect(translationKey(v('li', { children: 'x' }, { key: 'k1' }))).toBeNull();
    expect(translationKey(v('span', { children: 'x', tabIndex: 0 }))).toBeNull();
    expect(translationKey(v('span', { children: 'x', translate: false }))).toBeNull();
  });

  it('textOnly joins text pieces and refuses elements', () => {
    expect(textOnly(['Indexer', ': ', 'following, lag 2 s'])).toBe('Indexer: following, lag 2 s');
    expect(textOnly([['a'], ['b', 1n]])).toBe('ab1');
    expect(textOnly(['a', { type: 'i' }])).toBeNull();
  });
});
