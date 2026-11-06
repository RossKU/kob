import { useEffect, useMemo, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { floorOf } from '../../kob/fee-policy';
import { useInterval } from '../kit';
import { CONGESTION_POLL_MS, CongestionProbe } from './congestion';

/**
 * Whether blocks are contended, for the note of the final confirmation (ticket-notes.ts): KOB transactions are ranked by their storage mass, which the normal
 * fee does not cover. Reads the node's fee estimate when the ticket is shown and then every 30 s while the page is visible; false while the priority fee is
 * already on and when the node gives no estimate. No per-transaction choice and no automatic switching.
 */
export function useCongestion(): boolean {
  const { config, fees, node } = useServices();
  const active = !config.features?.priorityFee;
  const probe = useMemo(
    () => new CongestionProbe(async () => (fees?.oracle ? await fees.oracle.get() : node.getFeeEstimate ? await node.getFeeEstimate() : null), fees ? floorOf(fees.policy) : 100n),
    [fees, node],
  );
  const [contended, setContended] = useState(probe.contended);
  const poll = () => void probe.probe().then(setContended);
  useEffect(() => {
    if (active) poll();
  }, [active, probe]);
  useInterval(poll, CONGESTION_POLL_MS, active);
  return active && contended;
}
