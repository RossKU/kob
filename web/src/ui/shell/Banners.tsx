import { useServices, useWallet } from '../../app/context';
import type { ConfigWarning, StoredOverride } from '../../config';
import { t } from '../../i18n';
import { Banner } from '../kit';
import { shortAddress } from '../kit/format';
import { formatLag } from './health';
import { useSystemStatus } from './StatusProvider';
import { CustomRegistryBanner } from './RegistryNote';

/**
 * Global banners under the status bar: settings that were ignored, test mode, registry failure, wallet trouble, node / indexer health.
 * There is deliberately none for chain re-organisations: shallow ones are routine (the indexer handles them), what one does to the user's own orders is
 * a notification, and a deep one is a header indicator (ReorgIndicator).
 * Each has a `data-testid` starting with `banner-`.
 */
export function GlobalBanners(props: { warnings: ConfigWarning[]; storedOverrides?: StoredOverride[] }) {
  const { config, registryError } = useServices();
  const wallet = useWallet();
  const { node, indexer } = useSystemStatus();
  const a = indexer.assessment;

  const banners = [];
  if (props.warnings.length) {
    banners.push(
      <Banner key="config" tone="warn" title={t('shell.banner.configWarnings')} data-testid="banner-config">
        <ul>
          {props.warnings.map((w, i) => (
            <li key={i}>
              <code>{w.layer}.{w.field}</code>: {w.message}
            </li>
          ))}
        </ul>
      </Banner>,
    );
  }
  if (config.features.test) banners.push(<Banner key="test" tone="warn" data-testid="banner-test-mode">{t('shell.banner.testMode')}</Banner>);
  banners.push(<CustomRegistryBanner key="custom-registry" />);
  // a node / indexer saved in this browser's settings applies to every visit: said on every page, like a non-default registry
  const shown = (o: StoredOverride, url: string): string => url || t(o.field === 'nodeUrl' ? 'settings.nodeUrl.resolver' : 'shell.banner.storedNone');
  for (const o of props.storedOverrides ?? []) {
    banners.push(
      <Banner key={`stored-${o.field}`} tone="warn" class="banner-compact" data-testid={`banner-stored-${o.field === 'nodeUrl' ? 'node' : 'indexer'}`}>
        {t(o.field === 'nodeUrl' ? 'shell.banner.storedNode' : 'shell.banner.storedIndexer', { url: shown(o, o.value), deployment: shown(o, o.deployment) })}{' '}
        <a href="#/settings">{t('shell.banner.storedSettings')}</a>
      </Banner>,
    );
  }
  if (registryError) {
    banners.push(
      <Banner key="registry" tone="warn" data-testid="banner-registry">
        {t('shell.banner.registry', { message: registryError })}
      </Banner>,
    );
  }
  if (wallet.networkMismatch && wallet.info) {
    banners.push(
      <Banner key="mismatch" tone="error" title={t('shell.wallet.mismatchTitle')} data-testid="banner-network-mismatch">
        {t('shell.wallet.mismatch', { walletNetwork: wallet.info.network, appNetwork: config.network })}
      </Banner>,
    );
  }
  // the network-changed case needs no banner of its own: the mismatch banner above says it
  const notice = wallet.notice;
  if (notice?.kind === 'account-changed' && notice.to) {
    banners.push(
      <Banner key="acct" tone="warn" title={t('shell.wallet.noticeAccountTitle', { wallet: notice.wallet })} onDismiss={wallet.dismissNotice} data-testid="banner-wallet-account-changed">
        {t('shell.wallet.noticeAccount', { address: shortAddress(notice.to.address) })}
      </Banner>,
    );
  } else if (notice?.kind === 'network-restored' && notice.to) {
    banners.push(
      <Banner key="restored" tone="info" title={t('shell.wallet.noticeRestoredTitle', { wallet: notice.wallet, network: notice.to.network })} onDismiss={wallet.dismissNotice} data-testid="banner-wallet-network-restored">
        {t('shell.wallet.noticeRestored')}
      </Banner>,
    );
  } else if (notice?.kind === 'wallet-lost') {
    banners.push(
      <Banner key="lost" tone="error" title={t('shell.wallet.noticeLostTitle', { wallet: notice.wallet })} onDismiss={wallet.dismissNotice} data-testid="banner-wallet-lost">
        {t('shell.wallet.noticeLost')}
      </Banner>,
    );
  }
  if (wallet.error) {
    banners.push(
      <Banner key="wallet" tone="error" data-testid="banner-wallet-error">
        {t('shell.wallet.error', { message: wallet.error })}
      </Banner>,
    );
  }
  if (node.reachable === false) {
    banners.push(<Banner key="node" tone="error" data-testid="banner-node">{t('shell.banner.nodeDown')}</Banner>);
  }
  if (a.code === 'not-configured') {
    banners.push(<Banner key="noidx" tone="info" data-testid="banner-no-indexer">{t('shell.banner.noIndexer')}</Banner>);
  } else if (a.code === 'unreachable') {
    banners.push(
      <Banner key="idx" tone="error" title={t('shell.banner.indexerTitle')} data-testid="banner-indexer" actions={undefined}>
        {t('shell.banner.unreachable')}
      </Banner>,
    );
  } else if (a.level === 'warn' || a.level === 'bad') {
    banners.push(
      <Banner key="idx" tone={a.level === 'bad' ? 'error' : 'warn'} title={t('shell.banner.indexerTitle')} data-testid="banner-indexer">
        {t(`shell.banner.${a.code}`, { lag: formatLag(a.lagSeconds), network: config.network })}
      </Banner>,
    );
  }

  if (!banners.length) return null;
  return <div class="stack-sm" style="margin-bottom:16px" data-testid="global-banners">{banners}</div>;
}
