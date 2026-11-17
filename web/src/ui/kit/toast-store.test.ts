import { describe, expect, it } from 'vitest';
import { MAX_TOASTS, ToastStore, hostView, type ToastItem } from './toast-store';

describe('toast store', () => {
  it('while a flow card holds it, no new notification shows and none is replayed after the release; an error still shows, and the bell listeners get it too', () => {
    const s = new ToastStore();
    s.push({ message: 'before', tone: 'ok' });
    const errors: string[] = [];
    s.onHeldError((m) => errors.push(m));
    const release = s.hold();
    expect(s.held).toBe(true);
    expect(s.push({ message: 'Payment received', tone: 'ok' })).toBe(0);
    // founder 2026-11-17: a failure is always seen at the top right, never only in the bell
    const err = s.push({ message: 'the node failed', tone: 'error' });
    expect(err).toBeGreaterThan(0);
    expect(s.snapshot().map((t) => t.message)).toEqual(['before', 'the node failed']);
    expect(errors).toEqual(['the node failed']);
    // the same failure again: one toast, counted
    expect(s.push({ message: 'the node failed', tone: 'error' })).toBe(err);
    expect(s.snapshot().find((t) => t.id === err)?.count).toBe(2);
    s.dismiss(err);
    // two cards: the hold lasts until both are gone; a release twice counts once
    const second = s.hold();
    release();
    release();
    expect(s.held).toBe(true);
    second();
    expect(s.held).toBe(false);
    expect(s.snapshot().map((t) => t.message)).toEqual(['before']);
    s.push({ message: 'after', tone: 'info' });
    expect(s.snapshot().map((t) => t.message)).toEqual(['before', 'after']);
  });

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

describe('what the host shows', () => {
  const item = (id: number, tone: ToastItem['tone']): ToastItem => ({ id, tone, message: `m${id}`, timeoutMs: 0, count: 1 });
  const list = [item(1, 'error'), item(2, 'ok'), item(3, 'info')];

  it('a desktop: every toast, nothing waits', () => {
    expect(hostView(list, 1, false)).toEqual({ shown: list, waiting: 0, waitingError: false });
  });

  it('a phone: the newest error before a newer notification, the rest behind the chip', () => {
    const v = hostView(list, 0, true);
    expect(v.shown.map((x) => x.id)).toEqual([1]);
    expect(v.waiting).toBe(2);
    expect(v.waitingError).toBe(false);
    expect(hostView([item(2, 'ok'), item(3, 'info')], 0, true).shown.map((x) => x.id)).toEqual([3]);
    expect(hostView([item(4, 'error'), item(5, 'error')], 0, true).shown.map((x) => x.id)).toEqual([5]);
    expect(hostView([item(4, 'error'), item(5, 'error')], 0, true).waitingError).toBe(true);
    expect(hostView([], 0, true)).toEqual({ shown: [], waiting: 0, waitingError: false });
  });

  it('a phone with a card on screen: the card is shown, every toast waits, the chip is red while an error waits', () => {
    expect(hostView(list, 1, true)).toEqual({ shown: [], waiting: 3, waitingError: true });
    expect(hostView([item(2, 'ok')], 2, true)).toEqual({ shown: [], waiting: 2, waitingError: false });
  });
});
