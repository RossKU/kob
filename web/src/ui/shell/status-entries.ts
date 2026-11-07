// The current status of the bell's dialog: the conditions that hold now (settings that were ignored, test mode, registry failure, wallet trouble, node / indexer
// health, the open review notice). Live state, not history: an entry is here while its condition holds, is never stored and has nothing to do with the
// notifications (kob/notifications.ts). Pure: the facts in, the entries out (Banners.tsx gathers the facts and draws the entries).
// There is deliberately none for chain re-organisations: shallow ones are routine (the indexer handles them), what one does to the user's own orders is a
// notification, and a deep one is a header indicator (ReorgIndicator). Nor for a wallet on another network: that one stays at the top of the page
// (NetworkMismatchBanner).
import type { WalletNotice } from '../../app/wallet-events';
import type { ConfigWarning, StoredOverride } from '../../config';
import type { FailoverState } from '../../data/indexer-failover';
import type { StandaloneState } from '../../data/standalone';
import { t } from '../../i18n';
import { shortAddress } from '../kit/format';
import { formatLag, type HealthAssessment } from './health';
import { SECURITY_POLICY_URL } from './preaudit';
import { clockText } from './standalone';

export type StatusTone = 'info' | 'warn' | 'error';

export interface StatusEntry {
  /** stable while the condition holds */
  key: string;
  tone: StatusTone;
  /** starts with `banner-` */
  testid: string;
  class?: string;
  title?: string;
  text?: string;
  /** the settings that were ignored, as a list */
  warnings?: readonly ConfigWarning[];
  /** standalone mode: the connecting line, and the offer to go back to the indexer */
  standalone?: { connecting: boolean; back: boolean };
  link?: { href: string; text: string; external?: boolean };
  onDismiss?: () => void;
}

export interface StatusFacts {
  warnings: readonly ConfigWarning[];
  storedOverrides: readonly StoredOverride[];
  testMode: boolean;
  network: string;
  nodeUrl: string | null | undefined;
  registryError: string | null;
  notice: WalletNotice | null;
  dismissNotice: () => void;
  walletError: string | null;
  node: { reachable: boolean | undefined; error: Error | null };
  backup: FailoverState | null;
  standalone: StandaloneState | null;
  indexer: HealthAssessment;
  /** the open review notice was not dismissed yet (preaudit.ts) */
  preAudit: boolean;
  dismissPreAudit: () => void;
}

const RANK: Record<StatusTone, number> = { info: 0, warn: 1, error: 2 };

/** The tone of the worst entry; null: there are none. */
export function worstTone(entries: readonly Pick<StatusEntry, 'tone'>[]): StatusTone | null {
  let worst: StatusTone | null = null;
  for (const e of entries) if (worst === null || RANK[e.tone] > RANK[worst]) worst = e.tone;
  return worst;
}

function storedEntry(o: StoredOverride): StatusEntry {
  const link = { href: '#/settings', text: t('shell.banner.storedSettings') };
  if (o.field === 'extraIndexerUrls') {
    const text = t('shell.banner.storedExtraIndexers', { urls: o.value || t('shell.banner.storedNone'), deployment: o.deployment || t('shell.banner.storedNone') });
    return { key: `stored-${o.field}`, tone: 'warn', class: 'banner-compact', testid: 'banner-stored-extra-indexers', text, link };
  }
  if (o.field === 'allowQueryOverrides') {
    return { key: `stored-${o.field}`, tone: 'warn', class: 'banner-compact', testid: 'banner-stored-query-overrides', text: t('shell.banner.storedQueryOverrides'), link };
  }
  const shown = (url: string): string => url || t(o.field === 'nodeUrl' ? 'settings.nodeUrl.resolver' : 'shell.banner.storedNone');
  const text = t(o.field === 'nodeUrl' ? 'shell.banner.storedNode' : 'shell.banner.storedIndexer', { url: shown(o.value), deployment: shown(o.deployment) });
  return { key: `stored-${o.field}`, tone: 'warn', class: 'banner-compact', testid: `banner-stored-${o.field === 'nodeUrl' ? 'node' : 'indexer'}`, text, link };
}

function walletNoticeEntry(notice: WalletNotice | null, onDismiss: () => void): StatusEntry | null {
  // the network-changed case needs no entry of its own: the mismatch banner says it
  if (notice?.kind === 'account-changed' && notice.to) {
    return {
      key: 'acct', tone: 'warn', testid: 'banner-wallet-account-changed', onDismiss,
      title: t('shell.wallet.noticeAccountTitle', { wallet: notice.wallet }), text: t('shell.wallet.noticeAccount', { address: shortAddress(notice.to.address) }),
    };
  }
  if (notice?.kind === 'network-restored' && notice.to) {
    return {
      key: 'restored', tone: 'info', testid: 'banner-wallet-network-restored', onDismiss,
      title: t('shell.wallet.noticeRestoredTitle', { wallet: notice.wallet, network: notice.to.network }), text: t('shell.wallet.noticeRestored'),
    };
  }
  if (notice?.kind === 'wallet-lost') {
    return { key: 'lost', tone: 'error', testid: 'banner-wallet-lost', onDismiss, title: t('shell.wallet.noticeLostTitle', { wallet: notice.wallet }), text: t('shell.wallet.noticeLost') };
  }
  return null;
}

function indexerEntry(a: HealthAssessment, network: string): StatusEntry | null {
  if (a.code === 'not-configured') return { key: 'noidx', tone: 'info', testid: 'banner-no-indexer', text: t('shell.banner.noIndexer') };
  if (a.code === 'unreachable') return { key: 'idx', tone: 'error', testid: 'banner-indexer', title: t('shell.banner.indexerTitle'), text: t('shell.banner.unreachable') };
  if (a.level === 'warn' || a.level === 'bad') {
    return {
      key: 'idx', tone: a.level === 'bad' ? 'error' : 'warn', testid: 'banner-indexer',
      title: t('shell.banner.indexerTitle'), text: t(`shell.banner.${a.code}`, { lag: formatLag(a.lagSeconds), network }),
    };
  }
  return null;
}

/** What holds now, the worst first (the order below within a tone). */
export function statusEntries(f: StatusFacts): StatusEntry[] {
  const out: StatusEntry[] = [];
  if (f.preAudit) {
    out.push({
      key: 'preaudit', tone: 'info', class: 'banner-compact', testid: 'banner-preaudit', text: t('preaudit.banner'), onDismiss: f.dismissPreAudit,
      link: { href: SECURITY_POLICY_URL, text: t('preaudit.policy'), external: true },
    });
  }
  if (f.warnings.length) out.push({ key: 'config', tone: 'warn', testid: 'banner-config', title: t('shell.banner.configWarnings'), warnings: f.warnings });
  if (f.testMode) out.push({ key: 'test', tone: 'warn', testid: 'banner-test-mode', text: t('shell.banner.testMode') });
  // a node / indexer saved in this browser's settings applies to every visit: said on every page
  out.push(...f.storedOverrides.map(storedEntry));
  if (f.registryError) out.push({ key: 'registry', tone: 'warn', testid: 'banner-registry', text: t('shell.banner.registry', { message: f.registryError }) });
  const notice = walletNoticeEntry(f.notice, f.dismissNotice);
  if (notice) out.push(notice);
  if (f.walletError) out.push({ key: 'wallet', tone: 'error', testid: 'banner-wallet-error', text: t('shell.wallet.error', { message: f.walletError }) });
  if (f.node.reachable === false) {
    // no node URL: the SDK's public resolver is asked for one (it does not work in a browser on testnet-10); a URL: that node did not answer
    const reason = f.node.error?.message || t('shell.banner.nodeNoAnswer');
    out.push({
      key: 'node', tone: 'error', testid: 'banner-node', title: t('shell.banner.nodeTitle'),
      text: f.nodeUrl ? t('shell.banner.nodeDown', { url: f.nodeUrl, reason, network: f.network }) : t('shell.banner.nodeDownResolver', { reason, network: f.network }),
    });
  }
  // the book, charts and orders come from a backup indexer of the host's list while the main one is down, not ready or on another network
  if (f.backup?.onBackup) {
    out.push({
      key: 'backup', tone: f.backup.historyPartial ? 'warn' : 'info', class: 'wrap-anywhere', testid: 'banner-backup-indexer',
      title: t('shell.banner.backupTitle', { host: f.backup.host }),
      text: f.backup.historyPartial ? `${t('shell.banner.backupBody')} ${t('shell.banner.backupPartial')}` : t('shell.banner.backupBody'),
    });
  }
  // standalone mode (data/standalone.ts): what the page shows comes from the node since the mode started. It says what the indexer's state means for the
  // page (or, on by the setting, the indexer is not asked at all), so the indexer has no entry of its own then
  const sa = f.standalone;
  if (sa?.active) {
    const time = clockText(sa.sinceMs ?? Date.now());
    const manual = sa.reason === 'manual';
    out.push({
      key: 'standalone', tone: manual ? 'info' : 'warn', testid: 'banner-standalone', title: t('shell.banner.standaloneTitle', { time }),
      text: t(manual ? 'shell.banner.standalone' : 'shell.banner.standaloneAuto', { time }),
      standalone: { connecting: sa.phase === 'connecting', back: sa.indexerBack },
      link: { href: '#/settings', text: t('shell.banner.standaloneSettings') },
    });
  } else {
    const indexer = indexerEntry(f.indexer, f.network);
    if (indexer) out.push(indexer);
  }
  return out.sort((a, b) => RANK[b.tone] - RANK[a.tone]);
}
