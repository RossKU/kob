import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DEEP_REORG_BLOCKS, REORG_INDICATOR_MS, ReorgNotice, isDeepReorg } from './reorg-notice';

describe('isDeepReorg: ordinary re-orgs are silent', () => {
  it('only a re-org replacing more than a few seconds of chain counts', () => {
    for (const n of [undefined, 0, 1, 3, 10, DEEP_REORG_BLOCKS]) expect(isDeepReorg(n), String(n)).toBe(false);
    expect(isDeepReorg(DEEP_REORG_BLOCKS + 1)).toBe(true);
    expect(isDeepReorg(500)).toBe(true);
    expect(DEEP_REORG_BLOCKS).toBeGreaterThanOrEqual(10); // at least a second of chain at 10 blocks per second
  });
});

describe('ReorgNotice (the deep re-org indicator)', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const make = () => {
    const seen: boolean[] = [];
    const n = new ReorgNotice((v) => seen.push(v));
    return { n, seen };
  };

  it('is hidden until a deep re-org is counted, then auto-hides', () => {
    const { n, seen } = make();
    n.update(0);
    expect(n.visible).toBe(false);
    n.update(1);
    expect(n.visible).toBe(true);
    vi.advanceTimersByTime(REORG_INDICATOR_MS - 1);
    expect(n.visible).toBe(true);
    vi.advanceTimersByTime(1);
    expect(n.visible).toBe(false);
    expect(seen).toEqual([true, false]);
    expect(REORG_INDICATOR_MS).toBeGreaterThanOrEqual(60_000); // a subtle mark, not a flash
  });

  it('a newer re-org re-arms the timer', () => {
    const { n } = make();
    n.update(1);
    vi.advanceTimersByTime(REORG_INDICATOR_MS - 1000);
    n.update(2);
    vi.advanceTimersByTime(REORG_INDICATOR_MS - 1);
    expect(n.visible).toBe(true);
    vi.advanceTimersByTime(1);
    expect(n.visible).toBe(false);
  });

  it('an unchanged count neither shows nor re-arms; a manual dismiss hides at once and stays hidden', () => {
    const { n, seen } = make();
    n.update(1);
    vi.advanceTimersByTime(1000);
    n.update(1);
    vi.advanceTimersByTime(REORG_INDICATOR_MS - 1000);
    expect(n.visible).toBe(false);
    n.update(2);
    n.dismiss();
    expect(n.visible).toBe(false);
    vi.advanceTimersByTime(REORG_INDICATOR_MS * 2);
    expect(n.visible).toBe(false);
    expect(seen).toEqual([true, false, true, false]);
    n.update(3);
    expect(n.visible).toBe(true);
  });

  it('dispose cancels the pending timer', () => {
    const { n, seen } = make();
    n.update(1);
    n.dispose();
    vi.advanceTimersByTime(REORG_INDICATOR_MS * 2);
    expect(seen).toEqual([true]);
  });
});
