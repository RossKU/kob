import { describe, expect, it } from 'vitest';
import { has, t } from '../../i18n';
import { UNLISTED_REASONS, unlistedHint, unlistedReasonKey, unlistedText } from './unlisted';

describe('unlisted reasons in plain words', () => {
  it('every reason the executor reports has its own text, and none repeats the raw code', () => {
    expect(UNLISTED_REASONS).toHaveLength(11);
    const seen = new Set<string>();
    for (const code of UNLISTED_REASONS) {
      expect(unlistedReasonKey(code), code).toBe(code);
      expect(has(`orders.unlisted.reason.${code}`), code).toBe(true);
      const text = unlistedText(code);
      expect(text, code).toMatch(/^Not listed by the indexer: .+\.$/);
      expect(text, code).not.toContain('_');
      seen.add(text);
    }
    expect(seen.size).toBe(UNLISTED_REASONS.length);
  });

  it('the minimum value is said in words', () => {
    expect(unlistedText('order_value_below_minimum')).toBe('Not listed by the indexer: the order is worth less than the minimum order value of this indexer\'s executor.');
  });

  it('bad_state reasons, with or without details, get the state wording', () => {
    for (const code of ['bad_state', 'bad_state:scale:not positive', 'bad_state:sanity']) {
      expect(unlistedReasonKey(code)).toBe('bad_state');
      expect(unlistedText(code)).toBe(`Not listed by the indexer: ${t('orders.unlisted.reason.bad_state')}.`);
      expect(unlistedText(code)).not.toContain(':scale');
    }
  });

  it('a pair order refused for its quote token says so, and words the reason', () => {
    expect(unlistedText('quote_token_not_allowlisted')).toBe('Not listed by the indexer: for the quote token of this pair: this indexer does not list this token.');
    expect(unlistedText('quote_bad_state:x')).toContain(t('orders.unlisted.reason.bad_state'));
  });

  it('an unknown or missing reason falls back to a generic sentence, never the raw code alone', () => {
    for (const code of ['something_new', 'template_mismatch', 'x:y', '']) {
      expect(unlistedReasonKey(code)).toBe('other');
      const text = unlistedText(code);
      expect(text).toBe(`Not listed by the indexer: ${t('orders.unlisted.reason.other')}.`);
      expect(text).not.toContain(code || '\u0000');
    }
    expect(unlistedText(null)).toBe(unlistedText(''));
    expect(unlistedText(undefined)).toBe(unlistedText(''));
  });

  it('the hint offers a larger order only for a value below the minimum, and cancelling always', () => {
    expect(unlistedHint('order_value_below_minimum')).toBe('It is in no book, so it will not fill. Cancel it, or replace it with a larger order.');
    for (const code of ['token_not_allowlisted', 'bad_state:x', null, 'whatever']) {
      expect(unlistedHint(code)).toContain('cancel it');
      expect(unlistedHint(code)).not.toContain('larger');
    }
  });
});
