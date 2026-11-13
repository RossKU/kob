import { t, has } from '../../i18n';
import type { StatusSummary } from './StatusBar';

/**
 * The network, the status and the open review notice as ONE small chip of the header ("● testnet-10 · open review"; a narrow window shortens the network,
 * "tn10"). The dot says whether trading works (StatusBar.tsx: green the indexer, yellow the node / a backup / lagging / switching, red the node unreachable; the lines are in the chip's panel, NetworkSwitch.tsx). The
 * words keep their own test ids; the tooltip says them in full (the notice in the bell and the footer say it again on the page).
 */
export function NetworkChip(props: { network: string; short?: boolean; /** the status as a dot in the chip: absent where the page has no status (the landing page) */ status?: StatusSummary }) {
  const mainnet = props.network === 'mainnet';
  const shortKey = `shell.network.short.${props.network}`;
  const name = props.short && has(shortKey) ? t(shortKey) : props.network;
  const s = props.status;
  return (
    <span
      class={`net-chip ${mainnet ? 'net-chip-info' : 'net-chip-warn'}`}
      title={`${t('shell.network.label', { network: props.network })} · ${t('preaudit.badgeTip')}${s ? ` · ${s.summary}` : ''}`}
      data-testid="network-chip"
      data-level={s?.level}
    >
      {s ? (
        <span
          class={`dot ${s.level === 'unknown' ? '' : s.level}`}
          aria-hidden="true"
          data-testid="status-dot"
          data-level={s.level}
          data-trading={s.trading}
          data-node-level={s.nodeLevel}
          data-indexer-level={s.indexerLevel}
          data-indexer-code={s.indexerCode}
          title={s.summary}
        />
      ) : null}
      <span data-testid="network-badge" data-network={props.network}>{name}</span>
      <span class="net-pre" aria-hidden="true"> {'·'} </span>
      <span class="net-pre" data-testid="preaudit-badge">{t('preaudit.badge')}</span>
    </span>
  );
}
