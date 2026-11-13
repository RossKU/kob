import { useServices } from '../../app/context';
import { t, formatNumber } from '../../i18n';
import { useFailoverState } from './failover';
import { formatLag } from './health';
import { clockText, useStandalone } from './standalone';
import { useSystemStatus, type SystemStatus } from './StatusProvider';
import { tradingStatus, type TradingCode, type TradingLevel } from './trading-status';

export type Level = 'ok' | 'warn' | 'bad' | 'unknown';

const dot = (level: Level) => <span class={`dot ${level === 'unknown' ? '' : level}`} aria-hidden="true" />;

const nodeLevelOf = (node: SystemStatus['node']): Level => (node.reachable === undefined ? 'unknown' : node.reachable ? 'ok' : 'bad');
const nodeTextOf = (node: SystemStatus['node']): string => (node.reachable === undefined ? t('shell.status.nodeChecking') : node.reachable ? t('shell.status.nodeOk') : t('shell.status.nodeDown'));
const indexerTextOf = (a: SystemStatus['indexer']['assessment']): string => t(`shell.health.${a.code}`, { lag: formatLag(a.lagSeconds) });

/** Whether trading works and how (trading-status.ts), from the live status, the standalone mode and the failover; with the line that says it. */
export function useTradingStatus(): { level: TradingLevel; code: TradingCode; text: string } {
  const { node, indexer } = useSystemStatus();
  const { standalone, failover } = useServices();
  const sa = useStandalone(standalone);
  const backup = useFailoverState(failover);
  const s = tradingStatus({ nodeReachable: node.reachable, indexer: indexer.assessment, standalone: sa, fallback: !!standalone && standalone.prefs.auto, backup: !!backup?.onBackup });
  const text = t(`shell.trading.${s.code}`, { time: clockText(sa?.sinceMs ?? Date.now()), host: backup?.host ?? '', lag: formatLag(indexer.assessment.lagSeconds) });
  return { ...s, text };
}

/**
 * The status lines: whether trading works and how, node reachability + virtual DAA and indexer health (state, lag). Each text that changes is the only
 * content of its own element (app/translation-safe.ts: a machine-translated page would otherwise keep showing the first value). In the status popover of the
 * header and in the phone menu.
 */
export function StatusLines() {
  const { node, indexer } = useSystemStatus();
  const trading = useTradingStatus();
  const a = indexer.assessment;
  const nodeLevel = nodeLevelOf(node);
  return (
    <div class="status-lines" data-testid="status-bar">
      <span data-testid="status-trading" data-level={trading.level} data-code={trading.code}>
        {dot(trading.level)}
        <span>{`${t('shell.status.trading')}: ${trading.text}`}</span>
      </span>
      <span data-testid="status-node" data-level={nodeLevel}>
        {dot(nodeLevel)}
        <span>{`${t('shell.status.node')}: ${nodeTextOf(node)}`}</span>
        {node.daa !== null ? <span class="num muted" translate={false}>{` - ${t('shell.status.daa', { daa: formatNumber(node.daa) })}`}</span> : null}
      </span>
      <span data-testid="status-indexer" data-level={a.level} data-code={a.code}>
        {dot(a.level)}
        <span>{`${t('shell.status.indexer')}: ${indexerTextOf(a)}`}</span>
      </span>
    </div>
  );
}

/**
 * What the status dot inside the header's network chip says: whether trading works (trading-status.ts: green the indexer, yellow the node / a backup /
 * lagging / switching, red the node unreachable) as its level, the node and indexer levels and the indexer's code for the tests and the page's own
 * scripts, and the one-line summary for its tooltip. While it is not green the bell keeps one calm line of it (status-entries.ts).
 */
export interface StatusSummary {
  level: Level;
  trading: TradingCode;
  nodeLevel: Level;
  indexerLevel: Level;
  indexerCode: string;
  summary: string;
}

export function useStatusSummary(): StatusSummary {
  const { node, indexer } = useSystemStatus();
  const trading = useTradingStatus();
  const a = indexer.assessment;
  return {
    level: trading.level,
    trading: trading.code,
    nodeLevel: nodeLevelOf(node),
    indexerLevel: a.level,
    indexerCode: a.code,
    summary: `${t('shell.status.trading')}: ${trading.text} · ${t('shell.status.node')}: ${nodeTextOf(node)} · ${t('shell.status.indexer')}: ${indexerTextOf(a)}`,
  };
}
