import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import type { HealthAssessment } from './health';
import { TRADING_CODES, tradingStatus, type TradingFacts } from './trading-status';

const ok: HealthAssessment = { level: 'ok', code: 'ok', lagSeconds: 0, fresh: true };
const down: HealthAssessment = { level: 'bad', code: 'unreachable', lagSeconds: null, fresh: false };
const lagging: HealthAssessment = { level: 'warn', code: 'lagging', lagSeconds: 300, fresh: false };
const loading: HealthAssessment = { level: 'unknown', code: 'loading', lagSeconds: null, fresh: false };
const facts = (over: Partial<TradingFacts> = {}): TradingFacts => ({ nodeReachable: true, indexer: ok, standalone: null, fallback: true, backup: false, ...over });
const of = (over: Partial<TradingFacts>) => tradingStatus(facts(over));

describe('tradingStatus: the one light of the header', () => {
  it('green only with a healthy indexer and a node that answers', () => {
    expect(of({})).toEqual({ level: 'ok', code: 'indexer' });
    expect(of({ nodeReachable: undefined })).toEqual({ level: 'unknown', code: 'checking' });
    expect(of({ indexer: loading })).toEqual({ level: 'unknown', code: 'checking' });
  });

  it('yellow: through the node (automatic or by the setting), a backup indexer, a lagging one, or switching to the node', () => {
    expect(of({ indexer: down, standalone: { active: true, reason: 'auto' } })).toEqual({ level: 'warn', code: 'node' });
    expect(of({ standalone: { active: true, reason: 'manual' } })).toEqual({ level: 'warn', code: 'manual' });
    expect(of({ backup: true })).toEqual({ level: 'warn', code: 'backup' });
    expect(of({ indexer: lagging })).toEqual({ level: 'warn', code: 'lagging' });
    expect(of({ indexer: down })).toEqual({ level: 'warn', code: 'switching' });
  });

  it('red: the node cannot be reached (whatever else holds), or the indexer is down with nothing to take over', () => {
    expect(of({ nodeReachable: false })).toEqual({ level: 'bad', code: 'node-down' });
    expect(of({ nodeReachable: false, standalone: { active: true, reason: 'auto' } })).toEqual({ level: 'bad', code: 'node-down' });
    expect(of({ indexer: down, fallback: false })).toEqual({ level: 'bad', code: 'unavailable' });
  });

  it('every code has its words', () => {
    for (const c of TRADING_CODES) expect(has(`shell.trading.${c}`), c).toBe(true);
  });
});
