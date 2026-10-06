import { describe, expect, it } from 'vitest';
import { MAX_TOASTS, ToastStore } from './toast-store';

describe('toast store', () => {
  it('adds, merges identical messages and dismisses', () => {
    const s = new ToastStore();
    let calls = 0;
    s.subscribe(() => calls++);
    const a = s.push({ message: 'saved', tone: 'ok' });
    const b = s.push({ message: 'saved', tone: 'ok' });
    expect(b).toBe(a);
    expect(s.snapshot()).toHaveLength(1);
    expect(s.snapshot()[0].count).toBe(2);
    // same text in another tone is another toast
    s.push({ message: 'saved', tone: 'error' });
    expect(s.snapshot()).toHaveLength(2);
    s.dismiss(a);
    expect(s.snapshot().map((t) => t.tone)).toEqual(['error']);
    s.dismiss(9999); // unknown id: no-op, no notification
    expect(calls).toBe(4);
  });

  it('errors stay until dismissed, others expire', () => {
    const s = new ToastStore();
    s.push({ message: 'x', tone: 'error' });
    s.push({ message: 'y', tone: 'info' });
    const [err, info] = s.snapshot();
    expect(err.timeoutMs).toBe(0);
    expect(info.timeoutMs).toBeGreaterThan(0);
  });

  it('caps the queue and evicts non-errors first', () => {
    const s = new ToastStore();
    s.push({ message: 'keep me', tone: 'error' });
    for (let i = 0; i < MAX_TOASTS + 3; i++) s.push({ message: `n${i}`, tone: 'info' });
    expect(s.snapshot()).toHaveLength(MAX_TOASTS);
    expect(s.snapshot().some((t) => t.message === 'keep me')).toBe(true);
    expect(s.snapshot().at(-1)?.message).toBe(`n${MAX_TOASTS + 2}`);
  });

  it('notifies subscribers until they unsubscribe and can be cleared', () => {
    const s = new ToastStore();
    let n = 0;
    const off = s.subscribe(() => n++);
    s.push({ message: 'a' });
    off();
    s.push({ message: 'b' });
    expect(n).toBe(1);
    s.clear();
    expect(s.snapshot()).toHaveLength(0);
  });
});
