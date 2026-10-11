import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { version as appVersion } from '../../../package.json';
import { indexerTrust } from '../../data/indexer-crosscheck';

/** Non-custodial statement, versions, links. */
export function Footer() {
  const { kob, indexer, verifiers } = useServices();
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
          <p data-testid="footer-statement">{t('shell.footer.statement')}</p>
          <p class="small" data-testid="footer-versions">{t('shell.footer.versions', { app: appVersion, kob: kobVersion })}</p>
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
