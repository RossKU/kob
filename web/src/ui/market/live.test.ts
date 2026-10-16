import { describe, expect, it } from 'vitest';
import { LiveGate, liveMode } from './live';

describe('liveMode', () => {
  it('is off without an indexer, live only while the socket is open, polling otherwise', () => {
    expect(liveMode(false, 'open')).toBe('off');
    expect(liveMode(true, 'open')).toBe('live');
    for (const s of ['idle', 'connecting', 'reconnecting', 'closed'] as const) expect(liveMode(true, s)).toBe('polling');
    expect(liveMode(true, null)).toBe('polling');
  });
});

describe('LiveGate (a hidden tab does not refetch)', () => {
  it('a shown page refreshes on every notice', () => {
    const g = new LiveGate(true);
    expect(g.notice(false)).toBe(true);
    expect(g.notice(true)).toBe(true);
    expect(g.shown(true)).toBeNull(); // nothing missed
  });

  it('a hidden page skips the notices and refreshes ONCE when shown again', () => {
    const g = new LiveGate(true);
    expect(g.shown(false)).toBeNull();
    for (let i = 0; i < 50; i++) expect(g.notice(false)).toBe(false);
    expect(g.shown(true)).toEqual({ full: false });
    expect(g.shown(true)).toBeNull(); // only once
    expect(g.notice(false)).toBe(true);
  });

  it('a reorg or resync missed while hidden makes the refresh on show a full one', () => {
    const g = new LiveGate(false);
    g.notice(false);
    g.notice(true);
    g.notice(false);
    expect(g.shown(true)).toEqual({ full: true });
  });

  it('hidden with nothing missed: no refresh when shown', () => {
    const g = new LiveGate(false);
    expect(g.shown(true)).toBeNull();
  });
});
