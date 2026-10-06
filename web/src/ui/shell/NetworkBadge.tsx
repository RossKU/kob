import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { Badge } from '../kit';
import { isCustomRegistry, RegistryTip } from './RegistryTip';

/**
 * The network label of the header. When the token registry is not the build-pinned default it carries a small dot, and hovering / focusing /
 * tapping the badge shows the full registry explanation (RegistryTip). Otherwise it is a plain badge.
 */
export function NetworkBadge(props: { network: string }) {
  const { registryIdentity: r } = useServices();
  const tone = props.network === 'mainnet' ? 'info' : 'warn';
  if (!isCustomRegistry(r)) {
    return (
      <Badge tone={tone} data-testid="network-badge" title={t('shell.network.label', { network: props.network })}>
        {props.network}
      </Badge>
    );
  }
  return (
    <RegistryTip
      label={t('shell.registry.tipLabel', { network: t('shell.network.label', { network: props.network }), title: t('shell.banner.customRegistryTitle') })}
      class={`badge badge-${tone} reg-tip-badge`}
      data-testid="network-badge"
    >
      {props.network}
      <span class="reg-dot" aria-hidden="true" data-testid="registry-dot" />
    </RegistryTip>
  );
}
