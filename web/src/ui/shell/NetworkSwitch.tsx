import { useState } from 'preact/hooks';
import { saveSettings, type AppConfig, type NetworkName } from '../../config';
import { t } from '../../i18n';
import { NetworkChip } from './NetworkBadge';
import { networkSwitchPatch, networkSwitchState, otherNetwork, SWITCH_NETWORKS } from './network-switch';
import { Popover } from './Popover';

/**
 * The header's network chip as a control (founder, 2026-11-11: switch the network from the header, like other DEXes): the chip is the button of a small panel
 * that lists the two networks (the current one marked), says what the chip's tooltip says (the open review notice) and that the page reloads (and that mainnet
 * uses real funds). Choosing the other one saves it like the Settings screen does (network only) and reloads, because the network is fixed at load. A
 * deployment that pins its network (`lockNetwork`) keeps the plain chip; a browser with saved servers gets the reason and the way to Settings instead of a
 * switch (network-switch.ts says why).
 */
export function NetworkSwitch(props: { config: AppConfig; short?: boolean; level?: 'ok' | 'warn' | 'bad' | 'unknown'; narrow: boolean }) {
  const { config } = props;
  const [failed, setFailed] = useState(false);
  // the saved servers are read again on every render and at the choice itself: Settings can have saved some since the header was drawn
  const [, recheck] = useState(0);
  const state = networkSwitchState(config);
  const chip = <NetworkChip network={config.network} short={props.short} {...(props.level ? { level: props.level } : {})} />;
  if (state.kind === 'pinned') return chip;
  const choose = (n: NetworkName) => {
    if (n === config.network) return;
    if (networkSwitchState(config).kind !== 'open') return recheck((x) => x + 1);
    if (!saveSettings(networkSwitchPatch(config, n))) return setFailed(true);
    window.location.reload();
  };
  const blockedFields = state.kind === 'blocked' ? state.fields : null;
  // the deployment's servers are for its own network: on the other one the page has none (config.ts withoutDeploymentServers)
  const home = config.deploymentNetwork;
  const losesServers = config.network === home && config.indexerUrl !== '';
  return (
    <Popover
      label={t('netswitch.trigger', { network: config.network })}
      anchor={props.narrow ? 'header' : 'self'}
      triggerClass="net-chip-trigger"
      triggerAttrs={{ 'data-testid': 'network-switch', 'aria-haspopup': 'true' }}
      trigger={chip}
      panelTestId="network-switch-panel"
    >
      <div class="net-options" role="group" aria-label={t('netswitch.title')}>
        {SWITCH_NETWORKS.map((n) => {
          const current = n === config.network;
          return (
            <button
              key={n}
              type="button"
              class="net-option"
              aria-current={current ? 'true' : undefined}
              disabled={blockedFields !== null && !current}
              onClick={() => choose(n)}
              data-testid={`network-option-${n}`}
            >
              <span>{n}</span>
              {current ? <span class="muted small">{t('netswitch.current')}</span> : null}
            </button>
          );
        })}
      </div>
      {blockedFields ? (
        <p class="small menu-note" data-testid="network-switch-blocked">
          {t('netswitch.blocked', { fields: blockedFields.join(', ') })} <a href="#/settings" data-testid="network-switch-settings">{t('netswitch.settings')}</a>
        </p>
      ) : (
        <p class="small muted menu-note" data-testid="network-switch-reload">
          {t('netswitch.note')} {t('netswitch.noteMainnet')}
        </p>
      )}
      {blockedFields === null && losesServers ? (
        <p class="small muted menu-note" data-testid="network-switch-servers">{t('netswitch.otherServers', { home, network: otherNetwork(home) })}</p>
      ) : null}
      {failed ? <p class="small menu-note" role="alert" data-testid="network-switch-failed">{t('netswitch.failed')}</p> : null}
      <p class="small muted menu-note" data-testid="network-switch-note">{t('preaudit.badgeTip')}</p>
    </Popover>
  );
}
