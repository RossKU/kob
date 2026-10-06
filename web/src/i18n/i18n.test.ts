import { describe, expect, it } from 'vitest';
import { allKeys, rawEntry, t, tIssue } from './index';

describe('i18n', () => {
  it('has a non-empty English sentence for every key', () => {
    const keys = allKeys();
    expect(keys.length).toBeGreaterThan(100);
    for (const k of keys) expect(rawEntry(k) ?? '', k).not.toBe('');
  });

  it('is English only: no CJK text in the dictionary', () => {
    const cjk = /[\u3040-\u30ff\u4e00-\u9fff]/;
    expect(allKeys().filter((k) => cjk.test(rawEntry(k) ?? ''))).toEqual([]);
  });

  it('interpolates and falls back to the key', () => {
    expect(t('common.buy')).toBe('Buy');
    expect(t('does.not.exist')).toBe('does.not.exist');
    expect(t('x {a}', { a: 1 })).toBe('x 1');
  });

  it('prefers a keyed issue message and falls back to the finding\'s own message', () => {
    expect(tIssue('nope', { code: 'zzz', message: 'English text' })).toBe('English text');
  });
});
