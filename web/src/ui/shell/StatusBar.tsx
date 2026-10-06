import { t, formatNumber } from '../../i18n';
import { formatLag } from './health';
import { useSystemStatus } from './StatusProvider';

const dot = (level: 'ok' | 'warn' | 'bad' | 'unknown') => <span class={`dot ${level === 'unknown' ? '' : level}`} aria-hidden="true" />;

/** Thin global status line: node reachability + virtual DAA, indexer health (state, lag), feed connection. */
export function StatusBar() {
  const { node, indexer } = useSystemStatus();
  const a = indexer.assessment;
  const nodeLevel = node.reachable === undefined ? 'unknown' : node.reachable ? 'ok' : 'bad';
  return (
    <div class="statusbar" role="region" aria-label={t('shell.status.label')} data-testid="status-bar">
      <div class="container statusbar-inner">
        <span data-testid="status-node" data-level={nodeLevel}>
          {dot(nodeLevel)}
          {t('shell.status.node')}: {node.reachable === undefined ? t('shell.status.nodeChecking') : node.reachable ? t('shell.status.nodeOk') : t('shell.status.nodeDown')}
          {node.daa !== null ? <span class="num muted"> - {t('shell.status.daa', { daa: formatNumber(node.daa) })}</span> : null}
        </span>
        <span data-testid="status-indexer" data-level={a.level} data-code={a.code}>
          {dot(a.level)}
          {t('shell.status.indexer')}: {t(`shell.health.${a.code}`, { lag: formatLag(a.lagSeconds) })}
        </span>
      </div>
    </div>
  );
}
