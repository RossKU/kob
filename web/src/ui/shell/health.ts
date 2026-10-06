// Health assessment of the indexer (pure): turns a `/v1/health` answer (or the failure to get one) into one level + code the status bar,
// the banners and the market view translate. Thresholds are deliberately generous: a block takes ~1 s, the indexer settles at depth.
import type { HealthView } from '../../data/indexer-types';
import { normalizeNetwork } from '../../data/kaspa-sdk';

export type HealthLevel = 'ok' | 'warn' | 'bad' | 'unknown';
export const HEALTH_CODES = [
  'ok', 'not-configured', 'loading', 'unreachable', 'starting', 'catching-up', 'lagging', 'stale', 'node-unavailable', 'gap', 'unknown-state', 'wrong-network',
] as const;
export type HealthCode = (typeof HEALTH_CODES)[number];

export interface HealthAssessment {
  level: HealthLevel;
  code: HealthCode;
  lagSeconds: number | null;
  /** the trading data can be trusted as current */
  fresh: boolean;
}

/**
 * The indexer is announced (banner, "possibly out of date" mark on the book, warning in the status bar) only when it is MORE than this many seconds behind the
 * chain, whatever state it reports (a `catching_up` follower a few seconds behind is not worth a banner). Above STALE it is an error ("stale": the book cannot
 * be trusted).
 */
export const LAG_WARN_SECONDS = 30;
export const LAG_STALE_SECONDS = 600;

export interface HealthInput {
  /** last successful answer, if any */
  health: HealthView | null;
  /** error of the LATEST attempt (a stale `health` may still be present) */
  error: Error | null | undefined;
  /** `config.indexerUrl` is set */
  configured: boolean;
  /** the network the app is configured for */
  network?: string;
}

export function assessIndexerHealth(i: HealthInput): HealthAssessment {
  const mk = (level: HealthLevel, code: HealthCode, lagSeconds: number | null = null): HealthAssessment => ({ level, code, lagSeconds, fresh: level === 'ok' });
  if (!i.configured) return mk('unknown', 'not-configured');
  if (i.error) return mk('bad', 'unreachable');
  const h = i.health;
  if (!h) return mk('unknown', 'loading');
  if (i.network && h.network && normalizeNetwork(h.network) !== normalizeNetwork(i.network)) return mk('bad', 'wrong-network');
  const lag = typeof h.lag_seconds === 'number' ? h.lag_seconds : null;
  // the lag the thresholds read: seconds, else the DAA lag (about 10 DAA per second); null = unknown
  const lagKnown = lag ?? (typeof h.lag_daa === 'number' ? h.lag_daa / 10 : null);
  const withinTolerance = lagKnown !== null && lagKnown <= LAG_WARN_SECONDS;
  switch (h.state) {
    case 'following':
      if (lag === null || lag <= LAG_WARN_SECONDS) return mk('ok', 'ok', lag);
      return lag <= LAG_STALE_SECONDS ? mk('warn', 'lagging', lag) : mk('bad', 'stale', lag);
    case 'starting':
      return withinTolerance ? mk('ok', 'ok', lag) : mk('warn', 'starting', lag);
    case 'catching_up':
      return withinTolerance ? mk('ok', 'ok', lag) : mk('warn', 'catching-up', lag);
    case 'node_unavailable':
      return mk('bad', 'node-unavailable', lag);
    case 'gap':
      return mk('bad', 'gap', lag);
    default:
      return h.ok ? mk('ok', 'ok', lag) : mk('warn', 'unknown-state', lag);
  }
}

/** Human duration of a lag in seconds: `5 s`, `3 min`, `2 h`. */
export function formatLag(seconds: number | null): string {
  if (seconds === null) return '-';
  const s = Math.max(0, Math.round(seconds));
  if (s < 90) return `${s} s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  return `${Math.round(s / 3600)} h`;
}
