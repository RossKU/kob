import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { indexerTrust } from '../../data/indexer-crosscheck';
import type { RegistryIdentity } from '../../kob/registry-source';
import { Badge, Banner, CopyText, KeyValueList } from '../kit';
import { isCustomRegistry, RegistryTip } from './RegistryTip';

/** One-line description of the registry in use: `default registry · testnet-10 · sha256 abcd1234 · 3 tokens`. */
export const registryLine = (r: RegistryIdentity): string =>
  t(r.failed ? 'shell.registry.lineFailed' : r.isDefault ? 'shell.registry.lineDefault' : 'shell.registry.lineCustom', {
    source: t(`shell.registry.source.${r.source}`),
    network: r.network,
    hash: r.shortHash ?? '-',
    tokens: r.tokens,
  });

/**
 * Small note "which registry says this token is listed" (token page, order ticket, footer): "official" means "listed in the registry this UI pins".
 * Shown only for a non-default registry, in the warning tone; the default registry is described in the network badge's popover only.
 */
export function RegistryNote(props: { 'data-testid'?: string }) {
  const { registryIdentity: r } = useServices();
  if (!r || r.isDefault) return null;
  return (
    <p class="small wrap-anywhere" data-testid={props['data-testid'] ?? 'registry-note'} data-default={r.isDefault ? 'true' : 'false'}>
      <Badge tone={r.isDefault ? 'ok' : 'warn'} title={t('shell.registry.officialMeans')}>
        {t(r.isDefault ? 'shell.registry.badgeDefault' : 'shell.registry.badgeCustom')}
      </Badge>{' '}
      <span class="muted">{registryLine(r)}</span>
    </p>
  );
}

/**
 * Mainnet only: a compact one-line notice when the registry is not the build-pinned default (there it is a real risk). The long text lives in the
 * tooltip of the "details" trigger and of the network badge in the header; on testnets only that badge indicator is shown.
 */
export function CustomRegistryBanner() {
  const { registryIdentity: r, config } = useServices();
  if (!isCustomRegistry(r) || config.network !== 'mainnet') return null;
  return (
    <Banner tone="warn" class="banner-compact" data-testid="banner-custom-registry">
      {t('shell.banner.customRegistryShort')} {'—'}{' '}
      <RegistryTip label={t('shell.banner.customRegistryDetails')} class="reg-tip-link" data-testid="banner-custom-registry-details" data-tip-testid="banner-custom-registry-tip">
        {t('shell.banner.customRegistryDetails')}
      </RegistryTip>
    </Banner>
  );
}

/** "Trust sources": the registry in use (full identity) and how many indexers back the data. Settings panel; the footer shows the short form. */
export function TrustSources() {
  const { registryIdentity: r, indexer, verifiers } = useServices();
  const trust = indexerTrust(!!indexer, verifiers?.length ?? 0);
  return (
    <div class="stack-sm" data-testid="trust-sources">
      <KeyValueList
        compact
        items={[
          { label: t('shell.registry.what'), value: <Badge tone={r.isDefault ? 'ok' : 'warn'}>{t(r.isDefault ? 'shell.registry.badgeDefault' : 'shell.registry.badgeCustom')}</Badge>, 'data-testid': 'registry-default-flag' },
          { label: t('shell.registry.sourceLabel'), value: `${t(`shell.registry.source.${r.source}`)}: ${r.url}` },
          { label: t('common.network'), value: r.network },
          { label: 'sha256', value: r.sha256 ? <CopyText value={r.sha256} short={false} data-testid="registry-sha256" /> : '-' },
          { label: t('shell.registry.pinned'), value: r.defaultSha256 ? <code class="wrap-anywhere small" data-testid="registry-pin">{r.defaultSha256}</code> : t('common.none') },
          { label: t('shell.registry.contents'), value: t('shell.registry.counts', { tokens: r.tokens, templates: r.templates, version: r.schemaVersion ?? '-' }) },
        ]}
      />
      <p class="small muted">{t('shell.registry.officialMeans')}</p>
      <p class="small" data-testid="indexer-trust" data-trust={trust}>
        <Badge tone={trust === 'multi' ? 'ok' : 'warn'}>{t(`shell.indexers.badge.${trust}`)}</Badge>{' '}
        {t(`shell.indexers.${trust}`, { count: (verifiers?.length ?? 0) + 1 })}
      </p>
    </div>
  );
}
