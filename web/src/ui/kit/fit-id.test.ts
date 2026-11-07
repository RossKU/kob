import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { longId, wideId } from '../../kob/registry';
import { fitForms, FIT_FORMS } from './FitId';

const ID = '0123456789abcdef'.repeat(4);

describe('fitForms', () => {
  it('a covenant id: whole, the market list\'s 12 + 12, then 8 + 8 and never less (longId: a token without a ticker)', () => {
    expect(fitForms(ID, 'covenant')).toEqual([ID, wideId(ID), longId(ID)]);
    expect(fitForms(ID, 'covenant')[2]).toBe('01234567…89abcdef');
    expect(Math.min(...FIT_FORMS.covenant)).toBeGreaterThanOrEqual(8);
  });

  it('an id in the log gives way in steps: whole, 20 + 20, 14 + 14, 8 + 8, then 4 + 4', () => {
    const forms = fitForms(ID, 'log');
    expect(forms.map((f) => f.length)).toEqual([64, 41, 29, 17, 9]);
    expect(forms.slice(3)).toEqual(['01234567…89abcdef', '0123…cdef']);
  });

  it('an id shorter than a form stays as it is', () => {
    expect(fitForms('abcdef', 'log')).toEqual(Array(5).fill('abcdef'));
    expect(fitForms('01234567…89abcdef', 'log')[1]).toBe('01234567…89abcdef');
  });
});

describe('the stylesheet', () => {
  const css = readFileSync(new URL('../../styles.css', import.meta.url), 'utf8');
  const limitOf = (rule: RegExp) => Number(rule.exec(css)?.[1]);

  it('a box takes each form only with room for its characters', () => {
    expect(limitOf(/@container fit-box \(max-width: ([\d.]+)ch\) \{\s*\.fit-in-box > span:first-child/)).toBeGreaterThanOrEqual(64);
    // the limit that hides a form is at least that form's length
    for (const [nth, length] of [[2, 41], [3, 29], [4, 17]] as const) {
      expect(limitOf(new RegExp(`@container fit-box \\(max-width: ([\\d.]+)ch\\) \\{\\s*\\.fit-log\\.fit-in-box > span:nth-child\\(${nth}\\)`))).toBeGreaterThanOrEqual(length);
    }
  });

  it('every form but the first starts hidden, so an id is never on screen twice', () => {
    expect(css).toMatch(/\.fit-id > span \{ display: none; \}/);
    expect(css).toMatch(/\.fit-id > span:first-child \{ display: inline; \}/);
  });
});
