// The app-wide runner of the automatic refund (auto-refund.ts) and its opt-in toggle (shown in My orders).
import { useEffect, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { t } from '../../i18n';
import { AUTO_REFUND_EVENT, loadAutoRefund, setAutoRefund, useAutoRefund } from './auto-refund';

function useAutoRefundSetting(): boolean {
  const [on, setOn] = useState(loadAutoRefund);
  useEffect(() => {
    const h = () => setOn(loadAutoRefund());
    window.addEventListener(AUTO_REFUND_EVENT, h);
    return () => window.removeEventListener(AUTO_REFUND_EVENT, h);
  }, []);
  return on;
}

/** Mounted once in the app shell: refunds the connected wallet's due orders while the opt-in is on. Renders nothing. */
export function AutoRefundRunner() {
  const services = useServices();
  const wallet = useWallet();
  const on = useAutoRefundSetting();
  const pubkey = wallet.info && !wallet.networkMismatch ? wallet.info.pubkey : null;
  useAutoRefund(services, pubkey, wallet.records, on);
  return null;
}

/** The opt-in checkbox. */
export function AutoRefundToggle() {
  const on = useAutoRefundSetting();
  return (
    <label class="row small" style="gap:8px" data-testid="orders-auto-refund-label">
      <input type="checkbox" checked={on} onChange={(e) => setAutoRefund((e.currentTarget as HTMLInputElement).checked)} data-testid="orders-auto-refund" />
      <span>{t('orders.autoRefund.toggle')}</span>
    </label>
  );
}
