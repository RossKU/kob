// The one status light of the header (the dot of the network chip): whether trading works, and how. Pure: the facts in, the light out.
//   * green: the indexer is healthy (the market comes from it, every trade is still checked on the node);
//   * yellow: trading works another way or with less: through the node (standalone mode, automatic or switched on in Settings), through a backup
//     indexer, through an indexer that is behind the chain, or while the app is switching to the node;
//   * red: the node cannot be reached (nothing can be read or signed), or the indexer is down with no fallback to the node.
// The details are the tooltip and the lines of the chip's panel (StatusBar.tsx); the bell keeps one calm line while it is not green.
import type { StandaloneState } from '../../data/standalone';
import type { HealthAssessment } from './health';

export type TradingLevel = 'ok' | 'warn' | 'bad' | 'unknown';
export type TradingCode = 'checking' | 'indexer' | 'lagging' | 'backup' | 'switching' | 'node' | 'manual' | 'unavailable' | 'node-down';
export const TRADING_CODES: readonly TradingCode[] = ['checking', 'indexer', 'lagging', 'backup', 'switching', 'node', 'manual', 'unavailable', 'node-down'];

export interface TradingFacts {
  /** the node answers (undefined: not known yet) */
  nodeReachable: boolean | undefined;
  indexer: HealthAssessment;
  standalone: Pick<StandaloneState, 'active' | 'reason'> | null;
  /** standalone mode takes over by itself when the indexer cannot be used (the mode exists and its automatic fallback is on) */
  fallback: boolean;
  /** the market comes from a backup indexer of the host's list */
  backup: boolean;
}

export interface TradingStatus {
  level: TradingLevel;
  code: TradingCode;
}

export function tradingStatus(f: TradingFacts): TradingStatus {
  if (f.nodeReachable === false) return { level: 'bad', code: 'node-down' };
  if (f.standalone?.active) return { level: 'warn', code: f.standalone.reason === 'manual' ? 'manual' : 'node' };
  if (f.backup) return { level: 'warn', code: 'backup' };
  const a = f.indexer;
  if (a.level === 'bad') return f.fallback ? { level: 'warn', code: 'switching' } : { level: 'bad', code: 'unavailable' };
  if (a.level === 'warn') return { level: 'warn', code: 'lagging' };
  if (a.level === 'ok') return f.nodeReachable === undefined ? { level: 'unknown', code: 'checking' } : { level: 'ok', code: 'indexer' };
  return { level: 'unknown', code: 'checking' };
}
