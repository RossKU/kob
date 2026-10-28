import { t, has } from '../../i18n';

/**
 * The network and the pre-audit notice as ONE small chip of the header ("testnet-10 · pre-audit"; a narrow window shortens the network, "tn10"). The two
 * words keep their own test ids; the tooltip says both in full (the pre-audit banner and the footer say it again on the page).
 */
export function NetworkChip(props: { network: string; short?: boolean }) {
  const mainnet = props.network === 'mainnet';
  const shortKey = `shell.network.short.${props.network}`;
  const name = props.short && has(shortKey) ? t(shortKey) : props.network;
  return (
    <span
      class={`net-chip ${mainnet ? 'net-chip-info' : 'net-chip-warn'}`}
      title={`${t('shell.network.label', { network: props.network })} · ${t('preaudit.badgeTip')}`}
      data-testid="network-chip"
    >
      <span data-testid="network-badge" data-network={props.network}>{name}</span>
      <span aria-hidden="true"> {'·'} </span>
      <span data-testid="preaudit-badge">{t('preaudit.badge')}</span>
    </span>
  );
}
