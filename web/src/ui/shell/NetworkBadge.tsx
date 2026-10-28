import { t, has } from '../../i18n';

/**
 * The network and the pre-audit notice as ONE small chip of the header ("testnet-10 · pre-audit"; a narrow window shortens the network, "tn10"). The two
 * words keep their own test ids; the tooltip says both in full (the pre-audit banner and the footer say it again on the page).
 */
export function NetworkChip(props: { network: string; short?: boolean; /** the status as a dot in the chip (the phone layout: the menu has the lines) */ level?: 'ok' | 'warn' | 'bad' | 'unknown' }) {
  const mainnet = props.network === 'mainnet';
  const shortKey = `shell.network.short.${props.network}`;
  const name = props.short && has(shortKey) ? t(shortKey) : props.network;
  return (
    <span
      class={`net-chip ${mainnet ? 'net-chip-info' : 'net-chip-warn'}`}
      title={`${t('shell.network.label', { network: props.network })} · ${t('preaudit.badgeTip')}`}
      data-testid="network-chip"
      data-level={props.level}
    >
      {props.level ? <span class={`dot ${props.level === 'unknown' ? '' : props.level}`} aria-hidden="true" data-testid="chip-dot" /> : null}
      <span data-testid="network-badge" data-network={props.network}>{name}</span>
      <span class="net-pre" aria-hidden="true"> {'·'} </span>
      <span class="net-pre" data-testid="preaudit-badge">{t('preaudit.badge')}</span>
    </span>
  );
}
