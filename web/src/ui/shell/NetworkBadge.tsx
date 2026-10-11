import { t } from '../../i18n';
import { Badge } from '../kit';

/** The network label of the header. */
export function NetworkBadge(props: { network: string }) {
  const tone = props.network === 'mainnet' ? 'info' : 'warn';
  return (
    <Badge tone={tone} data-testid="network-badge" title={t('shell.network.label', { network: props.network })}>
      {props.network}
    </Badge>
  );
}
