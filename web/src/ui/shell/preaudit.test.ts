import { describe, expect, it } from 'vitest';
import { dismissPreAudit, isPreAuditDismissed, PREAUDIT_KEY } from './preaudit';

function memory(): Storage {
  const m = new Map<string, string>();
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
  } as unknown as Storage;
}

const broken = {
  getItem: () => {
    throw new Error('blocked');
  },
  setItem: () => {
    throw new Error('blocked');
  },
};

describe('pre-audit banner dismissal', () => {
  it('is shown on the first visit and stays dismissed once remembered', () => {
    const s = memory();
    expect(isPreAuditDismissed(s)).toBe(false);
    expect(dismissPreAudit(s)).toBe(true);
    expect(s.getItem(PREAUDIT_KEY)).toBe('1');
    expect(isPreAuditDismissed(s)).toBe(true);
  });

  it('ignores any stored value other than the dismissal marker', () => {
    const s = memory();
    s.setItem(PREAUDIT_KEY, 'garbage');
    expect(isPreAuditDismissed(s)).toBe(false);
  });

  it('blocked or missing storage never throws: not dismissed, nothing remembered', () => {
    expect(isPreAuditDismissed(broken)).toBe(false);
    expect(dismissPreAudit(broken)).toBe(false);
    expect(isPreAuditDismissed(null)).toBe(false);
    expect(dismissPreAudit(null)).toBe(false);
  });
});
