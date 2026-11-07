import { useServices, useWallet } from '../../app/context';
import type { ConfigWarning, StoredOverride } from '../../config';
import { t } from '../../i18n';
import { Banner, Button } from '../kit';
import { useFailoverState } from './failover';
import { usePreAuditBanner } from './preaudit';
import { useSystemStatus } from './StatusProvider';
import { useStandalone } from './standalone';
import { statusEntries, type StatusEntry } from './status-entries';

/** The one banner of the shell: a wallet on another network than the app's stops the user from signing, so it stays on the page as one line (a press unfolds it). */
export function NetworkMismatchBanner() {
  const { config } = useServices();
  const wallet = useWallet();
  if (!wallet.networkMismatch || !wallet.info) return null;
  return (
    <div style="margin-bottom:16px">
      <Banner tone="error" class="banner-compact banner-line" data-testid="banner-network-mismatch">
        <details>
          <summary>
            <strong>{t('shell.wallet.mismatchTitle')}</strong>{' '}
            <span>{t('shell.wallet.mismatch', { walletNetwork: wallet.info.network, appNetwork: config.network })}</span>
          </summary>
        </details>
      </Banner>
    </div>
  );
}

/** What holds now (status-entries.ts), kept current: the entries of the bell's dialog. */
export function useStatusEntries(warnings: readonly ConfigWarning[], storedOverrides: readonly StoredOverride[]): StatusEntry[] {
  const { config, registryError, failover, standalone } = useServices();
  const backup = useFailoverState(failover);
  const sa = useStandalone(standalone);
  const wallet = useWallet();
  const { node, indexer } = useSystemStatus();
  const [preAudit, dismissPreAudit] = usePreAuditBanner();
  return statusEntries({
    warnings, storedOverrides, registryError, node, backup, preAudit, dismissPreAudit,
    testMode: !!config.features.test, network: config.network, nodeUrl: config.nodeUrl,
    notice: wallet.notice, dismissNotice: wallet.dismissNotice, walletError: wallet.error, standalone: sa, indexer: indexer.assessment,
  });
}

/** One entry of the current status. `onNavigate`: a link to a page of the app was followed. */
export function StatusBanner(props: { entry: StatusEntry; onNavigate: () => void }) {
  const { standalone } = useServices();
  const e = props.entry;
  return (
    <Banner tone={e.tone} title={e.title} class={e.class} onDismiss={e.onDismiss} data-testid={e.testid}>
      {e.warnings ? (
        <ul>
          {e.warnings.map((w, i) => (
            <li key={i}>
              <code>{w.layer}.{w.field}</code>: {w.message}
            </li>
          ))}
        </ul>
      ) : null}
      {e.text ? <span>{e.text}</span> : null}
      {e.standalone?.connecting ? <div class="small muted" data-testid="standalone-connecting">{t('shell.banner.standaloneConnecting')}</div> : null}
      {e.standalone?.back ? (
        <div class="row" style="margin-top:6px" data-testid="standalone-back">
          <span>{t('shell.banner.standaloneBack')}</span>
          <Button small variant="primary" onClick={() => standalone?.returnToIndexer()} data-testid="standalone-return">{t('shell.banner.standaloneReturn')}</Button>
        </div>
      ) : null}
      {e.link ? (
        <>
          {' '}
          {e.link.external
            ? <a href={e.link.href} target="_blank" rel="noopener noreferrer">{e.link.text}</a>
            : <a href={e.link.href} onClick={props.onNavigate}>{e.link.text}</a>}
        </>
      ) : null}
    </Banner>
  );
}
