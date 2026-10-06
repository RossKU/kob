import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { COND_TYPES } from '../intent-cond';
import { ISSUE_CATALOG } from './common-issues';
import { COND_ISSUE_CATALOG, COND_ISSUE_CODES, condIssue } from './cond-issues';

const here = (f: string): string => readFileSync(fileURLToPath(new URL(f, import.meta.url)), 'utf8');

describe('conditional issue catalogue', () => {
  it('has unique COND_ codes that do not collide with the shared catalogue, each with severity and English text', () => {
    expect(new Set(COND_ISSUE_CODES).size).toBe(COND_ISSUE_CODES.length);
    for (const code of COND_ISSUE_CODES) {
      expect(code.startsWith('COND_'), code).toBe(true);
      expect(code in ISSUE_CATALOG, code).toBe(false);
      expect(COND_ISSUE_CATALOG[code].message.length, code).toBeGreaterThan(10);
      expect(['error', 'warning', 'info']).toContain(COND_ISSUE_CATALOG[code].severity);
    }
  });

  it('fills placeholders and formats KAS amounts', () => {
    const i = condIssue('COND_PREFUND_SHORT', { needed: 28_400_000n, given: 1_000_000n }, 'prefund');
    expect(i).toEqual({
      code: 'COND_PREFUND_SHORT', severity: 'error', field: 'prefund', params: { needed: 28_400_000n, given: 1_000_000n },
      message: "The prefund does not cover the exit's worst buy-back: at least 0.284 KAS per token are needed (you gave 0.01 KAS).",
    });
    expect(condIssue('COND_TP_NOT_PROFITABLE', { profitPerToken: -500_000n }, undefined, 'warning')).toMatchObject({
      severity: 'warning', message: expect.stringContaining('-0.005 KAS'),
    });
    // an unknown placeholder is left visible rather than silently dropped
    expect(condIssue('COND_MIN_FILL_INVALID').message).toContain('{amount}');
  });

  it('every issue code is exercised by a test (validation errors are not decorative)', () => {
    const tests = ['cond.test.ts', 'cond-ifd.test.ts', 'cond-legs.test.ts'].map(here).join('\n');
    for (const code of COND_ISSUE_CODES) expect(tests.includes(`'${code}'`), `no test mentions ${code}`).toBe(true);
  });

  it('COND_TYPES lists exactly the intents the planner handles', () => {
    expect([...COND_TYPES].sort()).toEqual(['ifd', 'ifo', 'oco', 'repeatIfd', 'repeatIfo', 'stopLimit', 'stopMarket', 'takeProfit', 'trailingStop']);
  });
});
