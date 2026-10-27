import { afterEach, describe, expect, it, vi } from 'vitest';
import type { HealthView } from '../../data/indexer-types';
import { HEALTH_POLL_MS, HEALTH_PUSH_GAP_MS, NODE_WAIT_MS, Throttle, healthPushDue, readNodeStatus } from './status';

const view = (state: string): HealthView => ({ state, ok: true, network: 'testnet-10', cursor_daa: 1, node_daa: 1, lag_daa: 0, lag_seconds: 0, settle_depth_daa: 1 });

describe('health reads', () => {
  it('poll no more often than every 10-15 s', () => {
    expect(HEALTH_POLL_MS).toBeGreaterThanOrEqual(10_000);
    expect(HEALTH_PUSH_GAP_MS).toBeGreaterThanOrEqual(10_000);
  });

  it('a per-block cursor frame never asks for /v1/health; the periodic health frame only when the state changed', () => {
    expect(healthPushDue({ type: 'cursor', data: { cursor_daa: 5, cursor_hash: null, added_blocks: 1, reverted_blocks: 0 } }, view('following'))).toBe(false);
    expect(healthPushDue({ type: 'health', data: { state: 'following', cursor_daa: 5, node_daa: 5, lag_daa: 0 } }, view('following'))).toBe(false);
    expect(healthPushDue({ type: 'health', data: { state: 'catching_up', cursor_daa: 5, node_daa: 9, lag_daa: 4 } }, view('following'))).toBe(true);
    expect(healthPushDue({ type: 'health', data: { state: 'following', cursor_daa: 5, node_daa: 5, lag_daa: 0 } }, null)).toBe(true);
    expect(healthPushDue({ type: 'health', data: null }, view('following'))).toBe(false);
  });
});

describe('Throttle', () => {
  afterEach(() => vi.useRealTimers());

  it('runs at once, then at most once per gap: calls inside the gap become ONE run at its end', () => {
    vi.useFakeTimers();
    let runs = 0;
    const t = new Throttle(() => runs++, 10_000);
    t.call();
    expect(runs).toBe(1);
    for (let i = 0; i < 30; i++) {
      vi.advanceTimersByTime(100); // a block a tenth of a second
      t.call();
    }
    expect(runs).toBe(1);
    vi.advanceTimersByTime(10_000);
    expect(runs).toBe(2);
    vi.advanceTimersByTime(60_000);
    expect(runs).toBe(2); // nothing more came in
  });

  it('a run by other means (the poll) restarts the gap; cancel drops the pending run', () => {
    vi.useFakeTimers();
    let runs = 0;
    const t = new Throttle(() => runs++, 10_000);
    t.ran();
    t.call();
    expect(runs).toBe(0);
    t.cancel();
    vi.advanceTimersByTime(20_000);
    expect(runs).toBe(0);
    t.call();
    expect(runs).toBe(1);
  });
});

describe('node status', () => {
  afterEach(() => vi.useRealTimers());

  it('a node that answers is reachable with its DAA score', async () => {
    const r = await readNodeStatus({ getClock: async () => ({ daa: 7n, unixSeconds: 1n, rateMilli: null }) }, NODE_WAIT_MS);
    expect(r).toEqual({ reachable: true, daa: 7n, error: null });
  });

  it('a node that never answers (a hung connect, the SDK resolver on testnet-10) ends as unreachable with a reason after the wait, not as "checking" for ever', async () => {
    vi.useFakeTimers();
    const pending = readNodeStatus({ getClock: () => new Promise(() => undefined) }, NODE_WAIT_MS, { reachable: true, daa: 5n, error: null });
    let done = false;
    void pending.then(() => (done = true));
    await vi.advanceTimersByTimeAsync(NODE_WAIT_MS - 1);
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(2);
    const r = await pending;
    expect(r.reachable).toBe(false);
    expect(r.daa).toBe(5n);
    expect(r.error?.message).toBe('The node did not answer within 25 s');
  });

  it('a node that refuses is unreachable at once with its own error', async () => {
    const r = await readNodeStatus({ getClock: async () => Promise.reject(new Error('resolver: 404')) }, NODE_WAIT_MS);
    expect(r).toMatchObject({ reachable: false, daa: null });
    expect(r.error?.message).toBe('resolver: 404');
  });
});
