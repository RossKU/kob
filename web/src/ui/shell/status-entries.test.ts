import { describe, expect, it, vi } from 'vitest';
import { t } from '../../i18n';
import type { HealthAssessment } from './health';
import { SECURITY_POLICY_URL } from './preaudit';
import { statusEntries, worstTone, type StatusFacts } from './status-entries';

const healthy: HealthAssessment = { level: 'ok', code: 'ok', lagSeconds: 0, fresh: true } as HealthAssessment;
const facts = (over: Partial<StatusFacts> = {}): StatusFacts => ({
  warnings: [], storedOverrides: [], testMode: false, network: 'testnet-10', nodeUrl: 'wss://node.example', registryError: null, notice: null, dismissNotice: () => undefined,
  walletError: null, node: { reachable: true, error: null }, backup: null, standalone: null, indexer: healthy, preAudit: false, dismissPreAudit: () => undefined, ...over,
});
const ids = (f: Partial<StatusFacts>) => statusEntries(facts(f)).map((e) => e.testid);
const one = (f: Partial<StatusFacts>) => {
  const list = statusEntries(facts(f));
  expect(list).toHaveLength(1);
  return list[0];
};
const backup = (historyPartial: boolean) => ({ onBackup: true, host: 'backup.example', historyPartial }) as StatusFacts['backup'];
const standalone = (over: Partial<NonNullable<StatusFacts['standalone']>> = {}) =>
  ({ active: true, reason: 'auto', phase: 'live', sinceMs: 0, version: 0, orders: 0, indexerBack: false, ...over }) as StatusFacts['standalone'];
const bad = (over: Partial<HealthAssessment>) => ({ ...healthy, level: 'bad', fresh: false, ...over }) as HealthAssessment;

describe('worstTone', () => {
  it('is null with no entries, and error over warn over info whatever the order', () => {
    expect(worstTone([])).toBeNull();
    expect(worstTone([{ tone: 'info' }])).toBe('info');
    expect(worstTone([{ tone: 'info' }, { tone: 'warn' }, { tone: 'info' }])).toBe('warn');
    expect(worstTone([{ tone: 'error' }, { tone: 'warn' }])).toBe('error');
    expect(worstTone([{ tone: 'warn' }, { tone: 'info' }, { tone: 'error' }])).toBe('error');
  });
});

describe('statusEntries', () => {
  it('is empty when nothing holds, and a wallet on another network is no entry (it stays on the page)', () => {
    expect(statusEntries(facts())).toEqual([]);
    expect(ids({ notice: { kind: 'network-changed', wallet: 'KasWare', to: { address: 'a', network: 'mainnet' } } })).toEqual([]);
  });

  it('the settings conditions: the ignored settings as a list, test mode, the registry', () => {
    const w = [{ layer: 'query', field: 'node', message: 'invalid url, ignored' }];
    expect(one({ warnings: w })).toMatchObject({ testid: 'banner-config', tone: 'warn', title: t('shell.banner.configWarnings'), warnings: w });
    expect(one({ testMode: true })).toMatchObject({ testid: 'banner-test-mode', tone: 'warn', text: t('shell.banner.testMode') });
    expect(one({ registryError: 'boom' })).toMatchObject({ testid: 'banner-registry', tone: 'warn', text: t('shell.banner.registry'), raw: 'boom' });
  });

  it('a stored node, indexer, indexer list or query switch: named, with a link to Settings', () => {
    const settings = { href: '#/settings', text: t('shell.banner.storedSettings') };
    const node = one({ storedOverrides: [{ field: 'nodeUrl', value: 'wss://mine', deployment: '' }] });
    expect(node).toMatchObject({ testid: 'banner-stored-node', tone: 'warn', link: settings });
    expect(node.text).toBe(t('shell.banner.storedNode', { url: 'wss://mine', deployment: t('settings.nodeUrl.resolver') }));
    const indexer = one({ storedOverrides: [{ field: 'indexerUrl', value: '', deployment: 'https://site' }] });
    expect(indexer).toMatchObject({ testid: 'banner-stored-indexer', link: settings });
    expect(indexer.text).toBe(t('shell.banner.storedIndexer', { url: t('shell.banner.storedNone'), deployment: 'https://site' }));
    const extra = one({ storedOverrides: [{ field: 'extraIndexerUrls', value: 'https://b', deployment: '' }] });
    expect(extra).toMatchObject({ testid: 'banner-stored-extra-indexers', link: settings });
    expect(extra.text).toBe(t('shell.banner.storedExtraIndexers', { urls: 'https://b', deployment: t('shell.banner.storedNone') }));
    expect(one({ storedOverrides: [{ field: 'allowQueryOverrides', value: 'true', deployment: 'false' }] })).toMatchObject({
      testid: 'banner-stored-query-overrides', text: t('shell.banner.storedQueryOverrides'), link: settings,
    });
  });

  it('the wallet notices carry their dismiss, the wallet error has none', () => {
    const dismissNotice = vi.fn();
    const to = { address: 'kaspatest:qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq', network: 'testnet-10' };
    const account = one({ dismissNotice, notice: { kind: 'account-changed', wallet: 'KasWare', to } });
    expect(account).toMatchObject({ testid: 'banner-wallet-account-changed', tone: 'warn', title: t('shell.wallet.noticeAccountTitle', { wallet: 'KasWare' }) });
    account.onDismiss?.();
    expect(dismissNotice).toHaveBeenCalledOnce();
    expect(one({ dismissNotice, notice: { kind: 'network-restored', wallet: 'KasWare', to } })).toMatchObject({
      testid: 'banner-wallet-network-restored', tone: 'info', text: t('shell.wallet.noticeRestored'), onDismiss: dismissNotice,
    });
    expect(one({ dismissNotice, notice: { kind: 'wallet-lost', wallet: 'KasWare' } })).toMatchObject({ testid: 'banner-wallet-lost', tone: 'error', onDismiss: dismissNotice });
    const error = one({ walletError: 'User rejected the request' });
    expect(error).toMatchObject({ testid: 'banner-wallet-error', tone: 'error', text: `${t('shell.wallet.error')} ${t('common.error.refused')}`, raw: 'User rejected the request' });
    expect(error.onDismiss).toBeUndefined();
  });

  it('the node: nothing until it is known to be down, then its URL or the resolver, with the reason', () => {
    expect(ids({ node: { reachable: undefined, error: null } })).toEqual([]);
    const reason = t('shell.banner.nodeNoAnswer');
    expect(one({ node: { reachable: false, error: null } })).toMatchObject({
      testid: 'banner-node', tone: 'error', title: t('shell.banner.nodeTitle'), text: t('shell.banner.nodeDown', { url: 'wss://node.example', network: 'testnet-10' }), raw: reason,
    });
    const resolver = one({ nodeUrl: '', node: { reachable: false, error: new Error('no route') } });
    expect(resolver.text).toBe(t('shell.banner.nodeDownResolver', { network: 'testnet-10' }));
    expect(resolver.raw).toBe('no route');
  });

  it('a backup indexer is info, a warning once its history may be partial', () => {
    const full = one({ backup: backup(false) });
    expect(full).toMatchObject({ testid: 'banner-backup-indexer', tone: 'info', title: t('shell.banner.backupTitle', { host: 'backup.example' }), text: t('shell.banner.backupBody') });
    const partial = one({ backup: backup(true) });
    expect(partial.tone).toBe('warn');
    expect(partial.text).toContain(t('shell.banner.backupPartial'));
  });

  it('standalone mode: one calm line (automatic or by the setting), the connecting line, the way back and the link; the indexer has no entry of its own', () => {
    const unreachable = bad({ code: 'unreachable' });
    const auto = one({ standalone: standalone({ sinceMs: Date.parse('2026-11-13T09:30:00') }), indexer: unreachable });
    expect(auto).toMatchObject({ testid: 'banner-standalone', tone: 'info', class: 'banner-compact', standalone: { connecting: false, back: false }, link: { href: '#/settings', text: t('shell.banner.standaloneSettings') } });
    expect(auto.text).toBe(t('shell.banner.standaloneAuto', { time: '09:30' }));
    expect(auto.title).toBeUndefined();
    expect(one({ standalone: standalone({ reason: 'manual' }) })).toMatchObject({ tone: 'info', text: t('shell.banner.standalone') });
    expect(one({ standalone: standalone({ phase: 'connecting', indexerBack: true }) }).standalone).toEqual({ connecting: true, back: true });
    expect(ids({ standalone: standalone({ active: false }), indexer: unreachable })).toEqual(['banner-indexer']);
  });

  it('the indexer: none configured, switching to the node, unreachable, and by its level; never an error; a healthy or still loading one has no entry', () => {
    expect(one({ indexer: { ...healthy, level: 'unknown', code: 'not-configured' } as HealthAssessment })).toMatchObject({ testid: 'banner-no-indexer', tone: 'info', text: t('shell.banner.noIndexer') });
    // standalone mode takes over: one calm line until it has
    expect(one({ indexer: bad({ code: 'unreachable' }), fallback: true })).toMatchObject({ testid: 'banner-switching', tone: 'info', text: t('shell.banner.switching') });
    expect(one({ indexer: bad({ code: 'stale', lagSeconds: 700 }), fallback: true })).toMatchObject({ testid: 'banner-switching' });
    // no fallback: the indexer's own entry, a warning at worst
    expect(one({ indexer: bad({ code: 'unreachable' }) })).toMatchObject({ testid: 'banner-indexer', tone: 'warn', title: t('shell.banner.indexerTitle'), text: t('shell.banner.unreachable') });
    expect(one({ indexer: bad({ code: 'stale', lagSeconds: 700 }) })).toMatchObject({ tone: 'warn' });
    expect(one({ indexer: bad({ level: 'warn', code: 'lagging', lagSeconds: 200 }) })).toMatchObject({ testid: 'banner-indexer', tone: 'info' });
    expect(one({ indexer: bad({ level: 'warn', code: 'lagging', lagSeconds: 200 }), fallback: true })).toMatchObject({ testid: 'banner-indexer', tone: 'info' });
    expect(ids({ indexer: { ...healthy, level: 'unknown', code: 'loading' } as HealthAssessment })).toEqual([]);
  });

  it('the open review notice: info (no alarm), with the policy link and its dismiss, only until dismissed', () => {
    const dismissPreAudit = vi.fn();
    const e = one({ preAudit: true, dismissPreAudit });
    expect(e).toMatchObject({ testid: 'banner-preaudit', tone: 'info', text: t('preaudit.banner'), link: { href: SECURITY_POLICY_URL, text: t('preaudit.policy'), external: true } });
    e.onDismiss?.();
    expect(dismissPreAudit).toHaveBeenCalledOnce();
    expect(worstTone(statusEntries(facts({ preAudit: true })))).toBe('info');
    expect(ids({ preAudit: false })).toEqual([]);
  });

  it('lists the worst first, keeping the order within a tone', () => {
    expect(ids({ preAudit: true, testMode: true, registryError: 'boom', node: { reachable: false, error: null }, walletError: 'User rejected the request' })).toEqual([
      'banner-wallet-error', 'banner-node', 'banner-test-mode', 'banner-registry', 'banner-preaudit',
    ]);
  });
});
