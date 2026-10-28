import { t, formatNumber } from '../../i18n';
import { formatLag } from './health';
import { Popover } from './Popover';
import { useSystemStatus, type SystemStatus } from './StatusProvider';

type Level = 'ok' | 'warn' | 'bad' | 'unknown';

const dot = (level: Level) => <span class={`dot ${level === 'unknown' ? '' : level}`} aria-hidden="true" />;

const nodeLevelOf = (node: SystemStatus['node']): Level => (node.reachable === undefined ? 'unknown' : node.reachable ? 'ok' : 'bad');
const nodeTextOf = (node: SystemStatus['node']): string => (node.reachable === undefined ? t('shell.status.nodeChecking') : node.reachable ? t('shell.status.nodeOk') : t('shell.status.nodeDown'));
const indexerTextOf = (a: SystemStatus['indexer']['assessment']): string => t(`shell.health.${a.code}`, { lag: formatLag(a.lagSeconds) });

/** The worse of two levels (bad > warn > unknown > ok). */
const RANK: Record<Level, number> = { ok: 0, unknown: 1, warn: 2, bad: 3 };
const worst = (a: Level, b: Level): Level => (RANK[a] >= RANK[b] ? a : b);

/**
 * The status lines: node reachability + virtual DAA and indexer health (state, lag). Each text that changes is the only content of its own element
 * (app/translation-safe.ts: a machine-translated page would otherwise keep showing the first value). In the status popover of the header and in the phone
 * menu.
 */
export function StatusLines() {
  const { node, indexer } = useSystemStatus();
  const a = indexer.assessment;
  const nodeLevel = nodeLevelOf(node);
  return (
    <div class="status-lines" data-testid="status-bar">
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

/** The status dot of the header's one row: the worse of the node and the indexer; a press opens the lines. A problem is also a full-width banner (Banners.tsx). */
export function StatusDot() {
  const { node, indexer } = useSystemStatus();
  const a = indexer.assessment;
  const nodeLevel = nodeLevelOf(node);
  const level = worst(nodeLevel, a.level);
  const summary = `${t('shell.status.node')}: ${nodeTextOf(node)} · ${t('shell.status.indexer')}: ${indexerTextOf(a)}`;
  return (
    <Popover
      label={t('shell.status.label')}
      trigger={dot(level)}
      triggerClass="status-dot"
      triggerAttrs={{ 'data-testid': 'status-dot', 'data-level': level, 'data-node-level': nodeLevel, 'data-indexer-level': a.level, 'data-indexer-code': a.code, title: summary }}
      panelTestId="status-popover"
    >
      <StatusLines />
    </Popover>
  );
}

/** The level a menu button shows as a small dot. */
export function useStatusLevel(): Level {
  const { node, indexer } = useSystemStatus();
  return worst(nodeLevelOf(node), indexer.assessment.level);
}
