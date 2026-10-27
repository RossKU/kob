import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { version as appVersion } from '../../../package.json';
import { indexerTrust } from '../../data/indexer-crosscheck';
import { useFailoverState } from './failover';
import { SECURITY_POLICY_URL } from './preaudit';

/** A version worth showing: a real release number, not the 0.0.0 placeholder of an unreleased build (nor an unknown one). */
export const releasedVersion = (v: string): boolean => /^\d+\.\d+\.\d+/.test(v) && !/^0\.0\.0(?![.\d])/.test(v);

/** Non-custodial statement, versions, links. */
export function Footer() {
  const services = useServices();
  const { kob, indexer } = services;
  useFailoverState(services.failover); // re-render on a switch: the verifiers are those of the indexer in use
  const verifiers = services.verifiers;
  const trust = indexerTrust(!!indexer, verifiers?.length ?? 0);
  let kobVersion = '?';
  try {
    kobVersion = kob.version();
  } catch {
    /* an unusable engine is reported by the boot screen, not here */
  }
  return (
    <footer class="footer" data-testid="footer">
      <div class="container footer-inner">
        <div class="grow">
          <p data-testid="footer-preaudit">
            <strong>{t('preaudit.footer')}</strong>{' '}
            <a href={SECURITY_POLICY_URL} target="_blank" rel="noopener noreferrer">{t('preaudit.policy')}</a>
          </p>
          <p data-testid="footer-statement">{t('shell.footer.statement')}</p>
          {/* an unreleased engine build reports 0.0.0: no version to show */}
          <p class="small" data-testid="footer-versions">{releasedVersion(kobVersion) ? t('shell.footer.versions', { app: appVersion, kob: kobVersion }) : t('shell.footer.version', { app: appVersion })}</p>
          <p class="small" data-testid="footer-indexer-trust" data-trust={trust}><a href="#/settings">{t(`shell.indexers.badge.${trust}`)}</a></p>
        </div>
        <nav class="stack-sm small" aria-label={t('shell.footer.protocol')}>
          <span>{t('shell.footer.protocol')}</span>
          <a href="#/settings">{t('shell.footer.settings')}</a>
          <a href="#/orders">{t('shell.footer.recover')}</a>
          <a href="https://kaspa.org" target="_blank" rel="noopener noreferrer">{t('shell.footer.kaspa')}</a>
          {/* Apache-2.0 attribution of the chart library (its NOTICE and a link to TradingView, required on every page that shows its charts) */}
          <span data-testid="footer-chart-notice">
            {t('shell.footer.charts')}{' '}
            <a href="https://www.tradingview.com/" target="_blank" rel="noopener noreferrer">TradingView Lightweight Charts™</a>
            {' '}© 2025 TradingView, Inc.
          </span>
        </nav>
      </div>
    </footer>
  );
}
