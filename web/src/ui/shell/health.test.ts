import { describe, expect, it } from 'vitest';
import type { HealthView } from '../../data/indexer-types';
import { assessIndexerHealth, formatLag, LAG_STALE_SECONDS, LAG_WARN_SECONDS } from './health';

const view = (over: Partial<HealthView> = {}): HealthView => ({
  state: 'following', ok: true, network: 'testnet-10', cursor_daa: 100, node_daa: 100, lag_daa: 0, lag_seconds: 0, settle_depth_daa: 100, ...over,
});
const base = { error: null, configured: true, network: 'testnet-10' };

describe('assessIndexerHealth', () => {
  it('reports an unconfigured indexer as unknown, not as a failure', () => {
    expect(assessIndexerHealth({ ...base, health: null, configured: false })).toMatchObject({ level: 'unknown', code: 'not-configured', fresh: false });
  });

  it('is loading until the first answer, unreachable after a failure (even with an old answer)', () => {
    expect(assessIndexerHealth({ ...base, health: null }).code).toBe('loading');
    expect(assessIndexerHealth({ ...base, health: null, error: new Error('x') })).toMatchObject({ level: 'bad', code: 'unreachable' });
    expect(assessIndexerHealth({ ...base, health: view(), error: new Error('x') }).code).toBe('unreachable');
  });

  it('grades a following indexer by its lag at the exact thresholds', () => {
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: 0 }) })).toMatchObject({ level: 'ok', fresh: true });
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: LAG_WARN_SECONDS }) }).level).toBe('ok');
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: LAG_WARN_SECONDS + 1 }) })).toMatchObject({ level: 'warn', code: 'lagging', fresh: false });
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: LAG_STALE_SECONDS }) }).code).toBe('lagging');
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: LAG_STALE_SECONDS + 1 }) })).toMatchObject({ level: 'bad', code: 'stale' });
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: null }) }).level).toBe('ok');
  });

  it('announces nothing until the indexer is more than 30 s behind, in every state', () => {
    expect(LAG_WARN_SECONDS).toBe(30);
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: 30 }) })).toMatchObject({ level: 'ok', fresh: true });
    expect(assessIndexerHealth({ ...base, health: view({ lag_seconds: 31 }) })).toMatchObject({ level: 'warn', code: 'lagging' });
    // a follower that says catching_up / starting but is within 30 s is fine (seconds, else the DAA lag at ~10 DAA per second)
    expect(assessIndexerHealth({ ...base, health: view({ state: 'catching_up', lag_seconds: 5, lag_daa: 52 }) })).toMatchObject({ level: 'ok', code: 'ok' });
    expect(assessIndexerHealth({ ...base, health: view({ state: 'catching_up', lag_seconds: 30 }) }).level).toBe('ok');
    expect(assessIndexerHealth({ ...base, health: view({ state: 'catching_up', lag_seconds: null, lag_daa: 299 }) }).level).toBe('ok');
    expect(assessIndexerHealth({ ...base, health: view({ state: 'catching_up', lag_seconds: null, lag_daa: 301 }) })).toMatchObject({ level: 'warn', code: 'catching-up' });
    expect(assessIndexerHealth({ ...base, health: view({ state: 'starting', lag_seconds: 12 }) }).level).toBe('ok');
    // a node outage or a gap is not a lag question: always announced
    expect(assessIndexerHealth({ ...base, health: view({ state: 'gap', lag_seconds: 1 }) }).level).toBe('bad');
  });

  it('maps the follower states', () => {
    expect(assessIndexerHealth({ ...base, health: view({ state: 'starting', lag_seconds: null, lag_daa: null }) })).toMatchObject({ level: 'warn', code: 'starting' });
    expect(assessIndexerHealth({ ...base, health: view({ state: 'catching_up', lag_seconds: LAG_WARN_SECONDS + 1 }) })).toMatchObject({ level: 'warn', code: 'catching-up', lagSeconds: LAG_WARN_SECONDS + 1 });
    expect(assessIndexerHealth({ ...base, health: view({ state: 'node_unavailable' }) })).toMatchObject({ level: 'bad', code: 'node-unavailable' });
    expect(assessIndexerHealth({ ...base, health: view({ state: 'gap' }) })).toMatchObject({ level: 'bad', code: 'gap' });
    expect(assessIndexerHealth({ ...base, health: view({ state: 'weird', ok: false }) })).toMatchObject({ level: 'warn', code: 'unknown-state' });
  });

  it('flags an indexer on another network (spelling differences do not count)', () => {
    expect(assessIndexerHealth({ ...base, health: view({ network: 'mainnet' }) })).toMatchObject({ level: 'bad', code: 'wrong-network' });
    expect(assessIndexerHealth({ ...base, health: view({ network: 'kaspa_testnet_10' }) }).code).toBe('ok');
  });
});

describe('formatLag', () => {
  it('picks a readable unit', () => {
    expect(formatLag(null)).toBe('-');
    expect(formatLag(4.4)).toBe('4 s');
    expect(formatLag(89)).toBe('89 s');
    expect(formatLag(180)).toBe('3 min');
    expect(formatLag(7200)).toBe('2 h');
    expect(formatLag(-3)).toBe('0 s');
  });
});
