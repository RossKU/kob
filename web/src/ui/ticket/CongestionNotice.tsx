import { useEffect, useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { floorOf } from '../../kob/fee-policy';
import { t } from '../../i18n';
import { Banner, useInterval } from '../kit';
import { CONGESTION_POLL_MS, CongestionProbe, congestionDismissed, dismissCongestion } from './congestion';

/**
 * Small dismissible notice below the order button: blocks are contended, a transaction at the relay minimum may wait, and where the one control is
 * (Settings: "Pay the priority fee"). Reads the node's fee estimate when the ticket is shown and then every 30 s while the page is visible; nothing while
 * the toggle is already on, nothing when the node gives no estimate. No per-transaction choice and no automatic switching.
 */
export function CongestionNotice(props: { active: boolean }) {
  const { config, fees, node } = useServices();
  const priorityOn = !!config.features?.priorityFee;
  const active = props.active && !priorityOn;
  const probe = useMemo(
    () => new CongestionProbe(async () => (fees?.oracle ? await fees.oracle.get() : node.getFeeEstimate ? await node.getFeeEstimate() : null), fees ? floorOf(fees.policy) : 100n),
    [fees, node],
  );
  const [contended, setContended] = useState(probe.contended);
  const [hidden, setHidden] = useState(congestionDismissed());
  const poll = () => void probe.probe().then(setContended);
  useEffect(() => {
    if (active) poll();
  }, [active, probe]);
  useInterval(poll, CONGESTION_POLL_MS, active);
  if (!active || !contended || hidden) return null;
  return (
    <Banner
      tone="info"
      class="banner-compact"
      onDismiss={() => {
        dismissCongestion();
        setHidden(true);
      }}
      data-testid="order-congestion"
    >
      {t('ticket.congestion')}{' '}
      <a href="#/settings" data-testid="order-congestion-settings">{t('ticket.congestion.settings')}</a>
    </Banner>
  );
}
