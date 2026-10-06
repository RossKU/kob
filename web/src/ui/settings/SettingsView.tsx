import { useMemo, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { saveSettings, type ConfigWarning, type NetworkName } from '../../config';
import { t } from '../../i18n';
import { version as appVersion } from '../../../package.json';
import {
  Amount, Badge, Banner, Button, Checkbox, CopyText, downloadText, exportFileName, Field, jsonText, KeyValueList, Modal, Section, Table, TableMessage,
  showToast, toError,
} from '../kit';
import { NotifySettingsSection } from '../notify/NotifySettings';
import { TrustSources } from '../shell/RegistryNote';
import { changedFields, changesServers, clearKobStorage, describeStorageKey, formFromConfig, kobStorageKeys, settingsPatch, validateSettings, type SettingsForm } from './settings-model';

const reload = () => window.location.reload();

function ConnectionSettings() {
  const { config } = useServices();
  const [form, setForm] = useState<SettingsForm>(() => formFromConfig(config));
  const [ack, setAck] = useState(false);
  const [saved, setSaved] = useState<'ok' | 'failed' | null>(null);
  const v = validateSettings(form);
  const changes = changedFields(config, form);
  const servers = changesServers(changes);
  const set = <K extends keyof SettingsForm>(k: K, val: SettingsForm[K]) => {
    setForm((f) => ({ ...f, [k]: val }));
    setSaved(null);
  };
  const err = (code: string | undefined) => (code ? t(`settings.error.${code}`) : null);

  const save = () => {
    if (!v.ok) return;
    const ok = saveSettings(settingsPatch(v.clean));
    setSaved(ok ? 'ok' : 'failed');
  };

  return (
    <Section title={t('settings.connection.title')} data-testid="settings-connection">
      <Banner tone="warn" title={t('settings.servers.warnTitle')} data-testid="settings-server-warning">
        {t('settings.servers.warn')}
      </Banner>
      <div class="stack-sm" style="margin-top:12px">
        <Field
          as="select"
          label={t('settings.network')}
          value={form.network}
          options={[{ value: 'mainnet', label: 'mainnet' }, { value: 'testnet-10', label: 'testnet-10' }]}
          onValue={(x) => set('network', x as NetworkName)}
          error={err(v.errors.network)}
          hint={t('settings.network.hint')}
          data-testid="settings-network"
        />
        <Field label={t('settings.indexerUrl')} value={form.indexerUrl} onValue={(x) => set('indexerUrl', x)} error={err(v.errors.indexerUrl)} hint={t('settings.indexerUrl.hint')} placeholder="https://" data-testid="settings-indexer-url" />
        <Field label={t('settings.nodeUrl')} value={form.nodeUrl} onValue={(x) => set('nodeUrl', x)} error={err(v.errors.nodeUrl)} hint={t('settings.nodeUrl.hint')} placeholder="wss://" data-testid="settings-node-url" />
        <Field label={t('settings.registryUrl')} value={form.registryUrl} onValue={(x) => set('registryUrl', x)} error={err(v.errors.registryUrl)} hint={t('settings.registryUrl.hint')} data-testid="settings-registry-url" />
        <Checkbox checked={form.kastle} onChange={(c) => set('kastle', c)} label={t('settings.kastle')} hint={t('settings.kastle.hint')} data-testid="settings-kastle" />
        <Checkbox checked={form.priorityFee} onChange={(c) => set('priorityFee', c)} label={t('settings.priorityFee')} hint={t('settings.priorityFee.hint')} data-testid="settings-priority-fee" />
        <Checkbox checked={form.dynamicFee} onChange={(c) => set('dynamicFee', c)} label={t('settings.dynamicFee')} hint={t('settings.dynamicFee.hint')} data-testid="settings-dynamic-fee" />
        {servers ? <Checkbox checked={ack} onChange={setAck} label={t('settings.servers.ack')} data-testid="settings-servers-ack" /> : null}
        <div class="row">
          <Button variant="primary" disabled={!v.ok || changes.length === 0 || (servers && !ack)} onClick={save} data-testid="settings-save">
            {t('common.save')}
          </Button>
          {saved === 'ok' ? <Button onClick={reload} data-testid="settings-reload">{t('settings.reload')}</Button> : null}
        </div>
        {saved === 'ok' ? <Banner tone="ok" data-testid="settings-saved">{t('settings.saved')}</Banner> : null}
        {saved === 'failed' ? <Banner tone="error" data-testid="settings-save-failed">{t('settings.saveFailed')}</Banner> : null}
      </div>
    </Section>
  );
}

function Versions() {
  const { kob, sdk, config } = useServices();
  let kobV = '?';
  let sdkV = '?';
  try {
    kobV = kob.version();
  } catch {
    /* reported by the boot screen */
  }
  try {
    sdkV = (sdk as unknown as { version?: () => string }).version?.() ?? '?';
  } catch {
    /* the SDK build may not expose it */
  }
  return (
    <Section title={t('settings.versions.title')} data-testid="settings-versions">
      <KeyValueList
        compact
        items={[
          { label: t('settings.versions.app'), value: appVersion, 'data-testid': 'version-app' },
          { label: 'kob-wasm', value: kobV, 'data-testid': 'version-kob' },
          { label: 'kaspa-wasm SDK', value: sdkV, 'data-testid': 'version-sdk' },
          { label: t('common.network'), value: config.network },
          { label: t('settings.versions.node'), value: config.nodeUrl || t('settings.nodeUrl.resolver') },
          { label: t('settings.versions.indexer'), value: config.indexerUrl || t('common.none') },
        ]}
      />
    </Section>
  );
}

function TrustSection() {
  return (
    <Section title={t('settings.trust.title')} data-testid="settings-trust">
      <TrustSources />
    </Section>
  );
}

function RegistrySummary() {
  const { registry, registryError, config } = useServices();
  return (
    <Section title={t('settings.registry.title')} data-testid="settings-registry">
      {registryError ? <Banner tone="warn">{t('shell.banner.registry', { message: registryError })}</Banner> : null}
      <KeyValueList
        compact
        items={[
          { label: t('settings.registry.url'), value: <code class="wrap-anywhere">{config.registryUrl}</code> },
          { label: t('common.network'), value: registry.network },
          { label: t('settings.registry.tokens'), value: registry.tokens.length, 'data-testid': 'registry-token-count' },
        ]}
      />
      <Table dense caption={t('settings.registry.templates')}>
        <thead>
          <tr>
            <th>{t('market.tpl.id')}</th>
            <th>{t('market.tpl.registryHash')}</th>
            <th>{t('market.tpl.review')}</th>
            <th>{t('market.tpl.verdict')}</th>
          </tr>
        </thead>
        <tbody>
          {registry.templates.length === 0 ? (
            <TableMessage colSpan={4}>{t('settings.registry.none')}</TableMessage>
          ) : (
            registry.templates.map((tpl) => {
              const check = registry.templateChecks.find((c) => c.id === tpl.id);
              return (
                <tr key={tpl.id} data-testid={`registry-template-${tpl.id}`}>
                  <td>{tpl.id}</td>
                  <td><code class="wrap-anywhere small">{tpl.template_hash.slice(0, 16)}{'…'}</code></td>
                  <td>{tpl.review_status === 'reviewed' ? t('market.tpl.reviewed') : t('market.tpl.pendingReview')}</td>
                  <td>
                    <Badge tone={check?.matchesPinned ? 'ok' : 'bad'} title={check?.problems.map((p) => t(`market.tpl.problem.${p}`)).join(', ')}>
                      {check?.matchesPinned ? t('market.tpl.matches') : check?.problems.length ? check.problems.map((p) => t(`market.tpl.problem.${p}`)).join(', ') : t('market.tpl.differs')}
                    </Badge>
                  </td>
                </tr>
              );
            })
          )}
        </tbody>
      </Table>
    </Section>
  );
}

function TrackerSettings() {
  const { tracker, config } = useServices();
  const wallet = useWallet();
  const pubkey = wallet.info?.pubkey;
  const [rev, setRev] = useState(0);
  const [text, setText] = useState('');
  const [message, setMessage] = useState<{ tone: 'ok' | 'error'; text: string } | null>(null);
  const list = useMemo(() => (pubkey ? tracker.list(pubkey) : []), [tracker, pubkey, rev]);

  if (!pubkey) {
    return (
      <Section title={t('settings.tracker.title')} data-testid="settings-tracker">
        <p class="muted">{t('settings.tracker.connect')}</p>
      </Section>
    );
  }
  const doImport = () => {
    try {
      const n = tracker.importJson(pubkey, text);
      setMessage({ tone: 'ok', text: t('settings.tracker.imported', { count: n }) });
      setText('');
      setRev((x) => x + 1);
    } catch (e) {
      setMessage({ tone: 'error', text: toError(e).message });
    }
  };
  return (
    <Section title={t('settings.tracker.title')} data-testid="settings-tracker">
      <p class="small muted">{t('settings.tracker.explain')}</p>
      <Table dense data-testid="tracker-list">
        <thead>
          <tr>
            <th>{t('settings.tracker.outpoint')}</th>
            <th class="right">{t('common.quantity')}</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {list.length === 0 ? (
            <TableMessage colSpan={3} data-testid="tracker-empty">{t('settings.tracker.empty')}</TableMessage>
          ) : (
            list.map((x) => (
              <tr key={`${x.transactionId}:${x.index}`} data-testid="tracker-row">
                <td><CopyText value={`${x.transactionId}:${x.index}`} head={8} tail={8} /></td>
                <td class="right"><Amount kind="token" value={BigInt(x.state.amount)} decimals={0} unit={t('settings.tracker.baseUnits')} /></td>
                <td class="right">
                  <Button small variant="danger" onClick={() => (tracker.remove(pubkey, x.transactionId, x.index), setRev((n) => n + 1))}>{t('settings.tracker.remove')}</Button>
                </td>
              </tr>
            ))
          )}
        </tbody>
      </Table>
      <div class="row" style="margin:8px 0">
        <Button
          disabled={list.length === 0}
          onClick={() => downloadText(exportFileName('tokens', config.network), jsonText(list))}
          data-testid="tracker-export"
        >
          {t('common.export')}
        </Button>
      </div>
      <Field as="textarea" label={t('settings.tracker.importLabel')} rows={4} value={text} onValue={setText} hint={t('settings.tracker.importHint')} data-testid="tracker-import-text" />
      <div class="row" style="margin-top:8px">
        <Button disabled={!text.trim()} onClick={doImport} data-testid="tracker-import">{t('common.import')}</Button>
      </div>
      {message ? <Banner tone={message.tone} data-testid="tracker-message">{message.text}</Banner> : null}
    </Section>
  );
}

function LocalData() {
  const [confirming, setConfirming] = useState(false);
  const keys = (() => {
    try {
      return kobStorageKeys(localStorage);
    } catch {
      return [];
    }
  })();
  const counts = keys.reduce<Record<string, number>>((m, k) => ((m[describeStorageKey(k)] = (m[describeStorageKey(k)] ?? 0) + 1), m), {});
  const clear = () => {
    try {
      clearKobStorage(localStorage);
    } catch {
      showToast(t('settings.local.failed'), 'error');
      return;
    }
    reload();
  };
  return (
    <Section title={t('settings.local.title')} data-testid="settings-local">
      <p>{t('settings.local.explain')}</p>
      <ul data-testid="local-data-list">
        {(['settings', 'records', 'tokens', 'other'] as const)
          .filter((k) => counts[k])
          .map((k) => (
            <li key={k}>{t(`settings.local.kind.${k}`, { count: counts[k] })}</li>
          ))}
        {keys.length === 0 ? <li>{t('settings.local.nothing')}</li> : null}
      </ul>
      <Banner tone="warn">{t('settings.local.backupFirst')} <a href="#/orders">{t('shell.footer.recover')}</a></Banner>
      <div class="row" style="margin-top:8px">
        <Button variant="danger" disabled={keys.length === 0} onClick={() => setConfirming(true)} data-testid="settings-clear">
          {t('settings.local.clear')}
        </Button>
      </div>
      {confirming ? (
        <Modal
          title={t('settings.local.confirmTitle')}
          onClose={() => setConfirming(false)}
          data-testid="clear-confirm"
          footer={
            <>
              <Button onClick={() => setConfirming(false)} data-autofocus>{t('common.cancel')}</Button>
              <Button variant="danger" onClick={clear} data-testid="clear-confirm-yes">{t('settings.local.clear')}</Button>
            </>
          }
        >
          <p>{t('settings.local.confirmBody', { count: keys.length })}</p>
        </Modal>
      ) : null}
    </Section>
  );
}

export function SettingsView(props: { warnings?: ConfigWarning[] }) {
  const warnings = props.warnings ?? [];
  return (
    <div class="stack" data-testid="settings-view">
      <h1>{t('shell.nav.settings')}</h1>
      {warnings.length ? (
        <Banner tone="warn" title={t('shell.banner.configWarnings')} data-testid="settings-warnings">
          <ul>
            {warnings.map((w, i) => (
              <li key={i}>
                <code>{w.layer}.{w.field}</code>: {w.message}
              </li>
            ))}
          </ul>
        </Banner>
      ) : null}
      <div class="grid-2">
        <div class="stack">
          <ConnectionSettings />
          <NotifySettingsSection />
          <Versions />
        </div>
        <div class="stack">
          <TrustSection />
          <RegistrySummary />
          <TrackerSettings />
          <LocalData />
        </div>
      </div>
    </div>
  );
}
