// Health assessment of the indexer (pure). It turns a `/v1/health` answer, or the failure to get
// one, into one level and code that the status bar, the banners and the market view translate.
// The thresholds are deliberately generous: a block takes about 1 s, and the indexer settles at
// depth.
import type { HealthView } from '../../data/indexer-types';
import { normalizeNetwork } from '../../data/kaspa-sdk';

export type HealthLevel = 'ok' | 'warn' | 'bad' | 'unknown';
export const HEALTH_CODES = [
  'ok', 'not-configured', 'loading', 'unreachable', 'starting', 'catching-up', 'lagging', 'stale', 'node-unavailable', 'gap', 'unknown-state', 'wrong-network',
  // The follower reports no progress for 10 minutes (`stale: true`), or it died (state `stopped`).
  'hung', 'stopped',
  // Standalone mode switched on in Settings: the indexer is not asked (ui/shell/status.ts).
  'standalone',
] as const;
export type HealthCode = (typeof HEALTH_CODES)[number];

export interface HealthAssessment {
  level: HealthLevel;
  code: HealthCode;
  lagSeconds: number | null;
  /**
   * The lag in seconds from `lag_seconds`, else from `lag_daa` (10 DAA per second). It is null when
   * the indexer reports none.
   */
  lagEstimate?: number | null;
  /** The trading data can be trusted as current. */
  fresh: boolean;
}

/**
 * The indexer is announced (banner, "possibly out of date" mark on the book, warning in the status
 * bar) only when it is MORE than this many seconds behind the chain, whatever state it reports. A
 * `catching_up` follower a few seconds behind is not worth a banner. Above STALE it is an error
 * ("stale"), as the book cannot be trusted.
 */
export const LAG_WARN_SECONDS = 120;
export const LAG_STALE_SECONDS = 600;

export interface HealthInput {
  /** The last successful answer, if any. */
  health: HealthView | null;
  /** The error of the LATEST attempt. A stale `health` may still be present. */
  error: Error | null | undefined;
  /** `config.indexerUrl` is set. */
  configured: boolean;
  /** The network the app is configured for. */
  network?: string;
}

export function assessIndexerHealth(i: HealthInput): HealthAssessment {
  const est = !i.error && i.health ? (typeof i.health.lag_seconds === 'number' ? i.health.lag_seconds : typeof i.health.lag_daa === 'number' ? i.health.lag_daa / 10 : null) : null;
  const mk = (level: HealthLevel, code: HealthCode, lagSeconds: number | null = null): HealthAssessment => ({ level, code, lagSeconds, lagEstimate: est, fresh: level === 'ok' });
  if (!i.configured) return mk('unknown', 'not-configured');
  if (i.error) return mk('bad', 'unreachable');
  const h = i.health;
  if (!h) return mk('unknown', 'loading');
  if (i.network && h.network && normalizeNetwork(h.network) !== normalizeNetwork(i.network)) return mk('bad', 'wrong-network');
  const lag = typeof h.lag_seconds === 'number' ? h.lag_seconds : null;
  // The lag the thresholds read: seconds, else the DAA lag (about 10 DAA per second). Null means
  // unknown.
  const lagKnown = lag ?? (typeof h.lag_daa === 'number' ? h.lag_daa / 10 : null);
  const withinTolerance = lagKnown !== null && lagKnown <= LAG_WARN_SECONDS;
  // A follower that died, or one that still says following but has not moved for 10 minutes, is
  // as unusable as an indexer that does not answer. An older indexer sends no `stale`: false.
  if (h.state === 'stopped') return mk('bad', 'stopped', lag);
  if (h.stale === true) return mk('bad', 'hung', lag);
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
