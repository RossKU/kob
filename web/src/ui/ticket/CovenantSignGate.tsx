// The covenant-signing gate of the order tickets (C5-10): a wallet not known to sign covenant inputs is asked once for a test signature before its
// first order (wallet/covenant-probe.ts). Placement stays blocked until the wallet passed; a wallet that cannot sign a cancel is refused for good.
import { useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { t } from '../../i18n';
import type { PlanEnv } from '../../kob/plan-types';
import { covenantSigningKnown, probeCovenantSigning, type ProbeOutcome } from '../../wallet/covenant-probe';
import type { JSX } from 'preact';
import { Banner, Button } from '../kit';

export interface CovenantGate {
  /** orders may be placed with the connected wallet */
  ready: boolean;
  /** the banner to render (null when ready or no wallet) */
  element: JSX.Element | null;
}

export function useCovenantSignGate(env: PlanEnv | null): CovenantGate {
  const services = useServices();
  const wallet = useWallet();
  const [outcome, setOutcome] = useState<ProbeOutcome | null>(null);
  const [busy, setBusy] = useState(false);
  const adapter = wallet.adapter;
  const pubkey = wallet.info?.pubkey ?? null;
  if (!adapter || !pubkey) return { ready: true, element: null };
  const known = covenantSigningKnown(adapter, pubkey);
  const verdict = outcome === 'ok' || outcome === 'unsupported' ? outcome : known;
  if (verdict === 'ok') return { ready: true, element: null };
  if (verdict === 'unsupported') {
    return {
      ready: false,
      element: (
        <Banner tone="error" title={t('ticket.covenantSign.unsupportedTitle', { wallet: adapter.label })} data-testid="order-covenant-unsupported">
          {t('ticket.covenantSign.unsupported')}
        </Banner>
      ),
    };
  }
  const run = async () => {
    if (!env) return;
    setBusy(true);
    try {
      setOutcome(await probeCovenantSigning(services.kob, adapter, env, services.config.network));
    } finally {
      setBusy(false);
    }
  };
  return {
    ready: false,
    element: (
      <Banner
        tone="warn"
        title={t('ticket.covenantSign.title')}
        data-testid="order-covenant-check"
        actions={
          <Button small variant="primary" onClick={() => void run()} loading={busy} disabled={!env} data-testid="order-covenant-check-run">
            {t('ticket.covenantSign.run')}
          </Button>
        }
      >
        <p>{t('ticket.covenantSign.body', { wallet: adapter.label })}</p>
        {outcome === 'declined' ? <p class="small">{t('ticket.covenantSign.declined')}</p> : null}
        {outcome === 'error' ? <p class="small">{t('ticket.covenantSign.error')}</p> : null}
      </Banner>
    ),
  };
}
