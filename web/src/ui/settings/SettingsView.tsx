import { useMemo, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { useCreatorImagePref, useIconPref } from '../../app/icon-pref';
import { useDisplayCurrency, type DisplayCurrency } from '../../app/currency-pref';
import { FiatNote } from '../../app/use-fiat';
import { displayToken } from '../../app/token-display';
import type { TrackedToken } from '../../data/token-tracker';
import { TokenNote } from '../market/TokenNote';
import { recoverIssuedToken } from './recover-issued';
import { readSettings, saveSettings, type AppConfig, type ConfigWarning, type NetworkName } from '../../config';
import { t } from '../../i18n';
import type { ComponentChildren } from 'preact';
import { version as appVersion } from '../../../package.json';
import {
  Amount, Badge, Banner, Button, Checkbox, CopyText, downloadText, EmptyState, exportFileName, Field, friendlyErrorText, jsonText, KeyValueList, Modal, RawDetails, Section, Segmented, Table, TableMessage,
  showToast, toError,
} from '../kit';
import { NotifySettingsSection } from '../notify/NotifySettings';
import { Learn } from './Learn';
import { IndexerTrust } from '../shell/IndexerTrust';
import { visibilityCheckSupported } from '../../signer/seen';
import { privateKeySigningAllowed } from '../../app/mirror';
import './settings.css';
import { changedFields, changesServers, clearKobStorage, configWithForm, describeStorageKey, formFromConfig, kobStorageKeys, resetServersPatch, settingsPatch, storedServerFields, validateSettings, type SettingsForm } from './settings-model';

const reload = () => window.location.reload();

/** A count in a sentence: the singular or the plural key (no "(s)"). */
const tCount = (base: string, count: number) => t(`${base}.${count === 1 ? 'one' : 'other'}`, { count });

/** A fold inside a card (the card's Save button stays below it): the header text is the toggle, the body stays in the DOM, hidden, like the kit's collapsible Section. */
function Fold(props: { title: string; children: ComponentChildren; 'data-testid'?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div class={`settings-fold${open ? ' is-open' : ''}`} data-testid={props['data-testid']}>
      <button type="button" class="disclosure settings-fold-toggle" aria-expanded={open ? 'true' : 'false'} onClick={() => setOpen(!open)}>
        {props.title}
      </button>
      <div class="settings-fold-body">{props.children}</div>
    </div>
  );
}

function ConnectionSettings() {
  const { config, standalone } = useServices();
  // the settings in force: the page's, then what this form saved (the standalone switches as the mode follows them now)
  const [base, setBase] = useState<AppConfig>(() => (standalone ? { ...config, standalone: { ...config.standalone, ...standalone.prefs } } : config));
  const [form, setForm] = useState<SettingsForm>(() => formFromConfig(base));
  const [ack, setAck] = useState(false);
  const [saved, setSaved] = useState<'ok' | 'failed' | null>(null);
  const v = validateSettings(form);
  const changes = changedFields(base, form);
  const servers = changesServers(changes);
  const set = <K extends keyof SettingsForm>(k: K, val: SettingsForm[K]) => {
    setForm((f) => ({ ...f, [k]: val }));
    setSaved(null);
  };
  const err = (code: string | undefined) => (code ? t(`settings.error.${code}`) : null);

  // servers saved in this browser (the banner on every page says so): one click gives the site's own back
  const [storedServers, setStoredServers] = useState(() => storedServerFields(readSettings()));
  const save = () => {
    if (!v.ok) return;
    const ok = saveSettings(settingsPatch(v.clean, changes));
    setSaved(ok ? 'ok' : 'failed');
    setStoredServers(storedServerFields(readSettings()));
    // the standalone switches apply at once (data/standalone.ts); saved, they also apply on the next visit
    if (ok) standalone?.setPrefs({ manual: v.clean.standaloneManual, auto: v.clean.standaloneAuto, autoReturn: v.clean.standaloneAutoReturn });
    if (ok) setBase((b) => configWithForm(b, v.clean));
  };
  const resetServers = () => {
    const ok = saveSettings(resetServersPatch());
    setSaved(ok ? 'ok' : 'failed');
    setStoredServers(storedServerFields(readSettings()));
  };

  return (
    <Section title={t('settings.connection.title')} data-testid="settings-connection">
      {/* every setting that needs the Save button is in this one card (review M-10): the instant-apply cards are separate */}
      <div class="stack-sm">
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
        <Checkbox checked={form.kastle} onChange={(c) => set('kastle', c)} label={t('settings.kastle')} hint={t('settings.kastle.hint')} data-testid="settings-kastle" />
        <Checkbox
          checked={form.priorityFee}
          onChange={(c) => set('priorityFee', c)}
          label={t('settings.priorityFee')}
          hint={<Learn short={t('settings.priorityFee.short')}>{t('settings.priorityFee.hint')}</Learn>}
          data-testid="settings-priority-fee"
        />
        <Checkbox
          checked={form.dynamicFee}
          onChange={(c) => set('dynamicFee', c)}
          label={t('settings.dynamicFee')}
          hint={<Learn short={t('settings.dynamicFee.short')}>{t('settings.dynamicFee.hint')}</Learn>}
          data-testid="settings-dynamic-fee"
        />
        <Checkbox
          checked={form.standaloneManual}
          onChange={(c) => set('standaloneManual', c)}
          label={t('settings.standalone.manual')}
          hint={<Learn short={t('settings.standalone.manual.short')}>{t('settings.standalone.manual.hint')}</Learn>}
          data-testid="settings-standalone"
        />
        <Checkbox
          checked={form.standaloneAuto}
          onChange={(c) => set('standaloneAuto', c)}
          label={t('settings.standalone.auto')}
          hint={<Learn short={t('settings.standalone.auto.short')}>{t('settings.standalone.auto.hint')}</Learn>}
          data-testid="settings-standalone-auto"
        />
        <Checkbox checked={form.standaloneAutoReturn} onChange={(c) => set('standaloneAutoReturn', c)} label={t('settings.standalone.autoReturn')} hint={t('settings.standalone.autoReturn.hint')} data-testid="settings-standalone-return" />
        {/* the servers are settings of this card too (one Save for all), folded: an operator's choice, not a trader's */}
        <Fold title={t('settings.servers.title')} data-testid="settings-servers-fold">
          <div class="stack-sm" data-testid="settings-servers">
            <Banner tone="warn" title={t('settings.servers.warnTitle')} data-testid="settings-server-warning">
              <Learn short={t('settings.servers.warn.short')}>{t('settings.servers.warn')}</Learn>
            </Banner>
            <Field label={t('settings.indexerUrl')} value={form.indexerUrl} onValue={(x) => set('indexerUrl', x)} error={err(v.errors.indexerUrl)} hint={t('settings.indexerUrl.hint')} placeholder="https://" data-testid="settings-indexer-url" />
            <Field
              label={t('settings.nodeUrl')}
              value={form.nodeUrl}
              onValue={(x) => set('nodeUrl', x)}
              error={err(v.errors.nodeUrl)}
              hint={<Learn short={t('settings.nodeUrl.short')}>{t('settings.nodeUrl.hint')}</Learn>}
              placeholder="wss://"
              data-testid="settings-node-url"
            />
            <Field label={t('settings.registryUrl')} value={form.registryUrl} onValue={(x) => set('registryUrl', x)} error={err(v.errors.registryUrl)} hint={t('settings.registryUrl.hint')} data-testid="settings-registry-url" />
            {storedServers.length ? (
              <div class="row">
                <Button onClick={resetServers} title={t('settings.resetServers.hint')} data-testid="settings-reset-servers">
                  {t('settings.resetServers')}
                </Button>
              </div>
            ) : null}
          </div>
        </Fold>
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

/** Everything for an operator or a power user, folded: the registry and its template hashes, the token tracker, the indexer cross-check (the servers are in the connection card, saved with its Save button). */
function AdvancedSettings() {
  return (
    <Section title={t('settings.advanced.title')} collapsible defaultOpen={false} class="settings-advanced" data-testid="settings-advanced">
      <div class="stack">
        <Learn short={t('settings.advanced.short')}>{t('settings.advanced.explain')}</Learn>
        <RegistrySummary />
        <div class="settings-adv-grid">
          <TrackerSettings />
          <TrustSection />
        </div>
      </div>
    </Section>
  );
}

/**
 * The display currency (app/currency-pref.ts): KAS, or approximate USD for the values the viewer reads. Applies at once, kept in this browser; the order book, the
 * ticket and everything signed stay in KAS (the explanation says so).
 */
function DisplayCurrencySettings() {
  const [currency, setCurrency] = useDisplayCurrency();
  return (
    <Section title={t('settings.currency.title')} data-testid="settings-currency">
      <Learn short={t('settings.currency.short')}>{t('settings.currency.explain')}</Learn>
      {/* what the dollars are now: the source, how old, an estimate (the pages say nothing of it, only why they show KAS when they do) */}
      <FiatNote full />
      <div class="stack-sm">
        <span class="small" id="settings-currency-label">{t('settings.currency.label')}</span>
        <Segmented<DisplayCurrency>
          options={[
            { value: 'kas', label: t('settings.currency.kas'), 'data-testid': 'settings-currency-kas' },
            { value: 'usd', label: t('settings.currency.usd'), 'data-testid': 'settings-currency-usd' },
          ]}
          value={currency}
          onChange={setCurrency}
          aria-label={t('settings.currency.label')}
          data-testid="settings-currency-choice"
        />
        <Learn short={t('settings.currency.hint.short')}>{t('settings.currency.hint')}</Learn>
      </div>
    </Section>
  );
}

/**
 * Token names and icons of the market list: where they come from and the viewer's two choices (on by default, per browser): load icons at all, and load
 * logos from the address a token's creator gave (the creator's site then sees the viewer's IP address; data/logos.ts).
 */
function TokenIconsSettings() {
  const { config } = useServices();
  const [on, setOn] = useIconPref();
  const [creatorOn, setCreatorOn] = useCreatorImagePref();
  return (
    <Section title={t('settings.icons.title')} data-testid="settings-icons">
      <Learn short={t('settings.icons.short')}>{t('settings.icons.explain')}</Learn>
      {config.tokenNamesUrl ? (
        <p class="small muted wrap-anywhere" data-testid="settings-icons-source">{t('settings.icons.source', { url: config.tokenNamesUrl })}</p>
      ) : (
        <Banner tone="info" data-testid="settings-icons-off">{t('settings.icons.off')}</Banner>
      )}
      <Checkbox checked={on} onChange={setOn} label={t('settings.icons.label')} hint={t('settings.icons.hint')} data-testid="settings-icons-toggle" />
      <Checkbox checked={creatorOn} onChange={setCreatorOn} label={t('settings.icons.creator.label')} hint={t('settings.icons.creator.hint')} disabled={!on} data-testid="settings-icons-creator-toggle" />
    </Section>
  );
}

/** The private key mode, now that the header has one Connect button: a desktop with an extension wallet reaches it here (a browser without one is taken to it by Connect). */
function PrivateKeySettings() {
  const w = useWallet();
  const supported = visibilityCheckSupported();
  // the GitHub Pages mirror (app/mirror.ts): no private key option, one line in its place says why
  if (!privateKeySigningAllowed()) {
    return (
      <Section title={t('settings.key.title')} data-testid="settings-private-key-section">
        <p class="small muted" data-testid="settings-private-key-mirror">{t('signer.mirrorOff')}</p>
      </Section>
    );
  }
  return (
    <Section title={t('settings.key.title')} data-testid="settings-private-key-section">
      <Learn short={t('settings.key.short')}>{t('settings.key.explain')}</Learn>
      {supported ? null : <p class="small muted" data-testid="settings-private-key-unsupported">{t('signer.unsupportedWhy')}</p>}
      <Button disabled={!supported || !!w.info || w.connecting} onClick={w.openPrivateKey} data-testid="settings-private-key">
        {t('settings.key.open')}
      </Button>
      {w.info ? <p class="small muted">{t('settings.key.disconnectFirst')}</p> : null}
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
      <IndexerTrust />
    </Section>
  );
}

function RegistrySummary() {
  const { registry, registryError, config } = useServices();
  return (
    <Section title={t('settings.registry.title')} data-testid="settings-registry">
      {registryError ? (
        <Banner tone="warn" data-testid="settings-registry-error">
          {t('settings.registry.loadFailed')}
          <RawDetails text={registryError} />
        </Banner>
      ) : null}
      <KeyValueList
        compact
        items={[
          { label: t('settings.registry.url'), value: <code class="wrap-anywhere">{config.registryUrl}</code> },
          { label: t('common.network'), value: registry.network },
          { label: t('settings.registry.tokens'), value: registry.tokens.length, 'data-testid': 'registry-token-count' },
        ]}
      />
      <Table dense caption={t('settings.registry.templates')} class="registry-templates">
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

/** A tracked token output's amount in whole tokens with its name (app/token-display.ts: also for a token the registry does not list, marked). */
function TrackedAmount(props: { utxo: TrackedToken }) {
  const { registry, config } = useServices();
  const tk = displayToken(registry, config.network, props.utxo.tokenCovId)!;
  return (
    <>
      <Amount kind="token" value={BigInt(props.utxo.state.amount)} decimals={tk.decimals} unit={tk.ticker} group />
      <TokenNote token={tk} />
    </>
  );
}

function TrackerSettings() {
  const { tracker, config, kob, indexer } = useServices();
  const wallet = useWallet();
  const pubkey = wallet.info?.pubkey;
  const [rev, setRev] = useState(0);
  const [text, setText] = useState('');
  const [message, setMessage] = useState<{ tone: 'ok' | 'error'; text: string; raw?: string } | null>(null);
  // "Find a token you created earlier"
  const [findId, setFindId] = useState('');
  const [findAmount, setFindAmount] = useState('');
  const [findDecimals, setFindDecimals] = useState('');
  const [needDecimals, setNeedDecimals] = useState(false);
  const [finding, setFinding] = useState(false);
  const [findMessage, setFindMessage] = useState<{ tone: 'ok' | 'error'; text: string; raw?: string } | null>(null);
  const list = useMemo(() => (pubkey ? tracker.list(pubkey) : []), [tracker, pubkey, rev]);

  if (!pubkey) {
    return (
      <Section title={t('settings.tracker.title')} data-testid="settings-tracker">
        <EmptyState icon="wallet" compact data-testid="tracker-connect">{t('settings.tracker.connect')}</EmptyState>
      </Section>
    );
  }
  const doImport = () => {
    try {
      const n = tracker.importJson(pubkey, text);
      setMessage({ tone: 'ok', text: tCount('settings.tracker.imported', n) });
      setText('');
      setRev((x) => x + 1);
    } catch (e) {
      setMessage({ tone: 'error', text: toError(e).message });
    }
  };
  const doFind = async () => {
    setFinding(true);
    try {
      const r = await recoverIssuedToken({ kob, tracker, declarations: indexer, network: config.network }, pubkey, findId, findAmount, findDecimals);
      if (r.kind === 'found') {
        setFindMessage({ tone: 'ok', text: tCount('settings.recover.found', r.found.length) });
        setRev((x) => x + 1);
      } else {
        if (r.code === 'decimals') setNeedDecimals(true);
        const key = { id: 'settings.recover.errorId', amount: 'settings.recover.errorAmount', decimals: 'settings.recover.errorDecimals', none: 'settings.recover.errorNone' } as const;
        setFindMessage({ tone: 'error', text: t(key[r.code]) });
      }
    } catch (e) {
      // a node or network failure: the plain sentence first, the raw text behind "Details"
      const raw = toError(e).message;
      setFindMessage({ tone: 'error', text: friendlyErrorText(raw), raw });
    } finally {
      setFinding(false);
    }
  };
  return (
    <Section title={t('settings.tracker.title')} data-testid="settings-tracker">
      <Learn short={t('settings.tracker.short')}>{t('settings.tracker.explain')}</Learn>
      <Learn short={t('settings.tracker.backup.short')} data-testid="tracker-backup">{t('settings.tracker.backup')}</Learn>
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
                <td><CopyText value={`${x.transactionId}:${x.index}`} chars={16} /></td>
                <td class="right"><TrackedAmount utxo={x} /></td>
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
      <h3 style="margin-top:16px">{t('settings.recover.title')}</h3>
      <Learn short={t('settings.recover.short')}>{t('settings.recover.explain')}</Learn>
      <Field label={t('settings.recover.id')} value={findId} onValue={setFindId} data-testid="recover-id" />
      <Field label={t('settings.recover.amount')} value={findAmount} onValue={setFindAmount} hint={t('settings.recover.amountHint')} data-testid="recover-amount" />
      {needDecimals ? (
        <Field label={t('settings.recover.decimals')} value={findDecimals} onValue={setFindDecimals} hint={t('settings.recover.decimalsHint')} data-testid="recover-decimals" />
      ) : null}
      <div class="row" style="margin-top:8px">
        <Button disabled={finding || !findId.trim() || !findAmount.trim()} onClick={() => void doFind()} data-testid="recover-find">{t('settings.recover.find')}</Button>
      </div>
      {findMessage ? (
        <Banner tone={findMessage.tone} data-testid="recover-message">
          {findMessage.text}
          <RawDetails text={findMessage.raw} />
        </Banner>
      ) : null}
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
      <Learn short={t('settings.local.short')}>{t('settings.local.explain')}</Learn>
      <ul data-testid="local-data-list">
        {(['settings', 'records', 'tokens', 'other'] as const)
          .filter((k) => counts[k])
          .map((k) => (
            <li key={k}>{t(`settings.local.kind.${k}`, { count: counts[k] })}</li>
          ))}
        {keys.length === 0 ? <li>{t('settings.local.nothing')}</li> : null}
      </ul>
      <Banner tone="warn">{t('settings.local.backupFirst')} <a href="#/portfolio/recover">{t('shell.footer.recover')}</a></Banner>
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
          <p>{tCount('settings.local.confirmBody', keys.length)}</p>
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
      {/* two stacks of about the same height (the columns are fixed, so a message inside a card never moves another card across): left, the one card with a Save button
          (everything that needs it) and the cards that are a button of their own; right, the cards that apply at once. On a narrow screen the stacks dissolve and the cards
          follow in the order of `--slot`: Save card first, then what applies at once. The folded Advanced card is full width below, so opening it leaves no empty
          column beside it (review M-10) */}
      <div class="grid-2 settings-grid">
        <div class="stack settings-col">
          <div class="settings-slot" style="--slot:1"><ConnectionSettings /></div>
          <div class="settings-slot" style="--slot:5"><PrivateKeySettings /></div>
          <div class="settings-slot" style="--slot:6"><Versions /></div>
          <div class="settings-slot" style="--slot:7"><LocalData /></div>
        </div>
        <div class="stack settings-col">
          <div class="settings-slot" style="--slot:2"><NotifySettingsSection /></div>
          <div class="settings-slot" style="--slot:3"><DisplayCurrencySettings /></div>
          <div class="settings-slot" style="--slot:4"><TokenIconsSettings /></div>
        </div>
      </div>
      <AdvancedSettings />
    </div>
  );
}
