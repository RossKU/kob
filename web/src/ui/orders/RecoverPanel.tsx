import { useRef, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import type { PlacementRecord, RecordStore, ResolvedRecord } from '../../kob/records';
import type { Hex } from '../../kob/types';
import { Badge, Banner, Button, CopyText, Section, Table, TableMessage, downloadText, exportFileName, readFileText, showToast, toError } from '../kit';
import { resolveRecords } from './orders-data';
import { applyImport, backupFileText, parseImport, recoveryFileText, type ParsedImport } from './recover';

export interface RecoverPanelProps {
  records: PlacementRecord[];
  pubkey: Hex;
  store: RecordStore;
  /** cancel a live order found from its record (the My orders row's cancel, through the confirm screen) */
  onCancel?(covenantId: string): void;
  /** records changed (import): reload the orders */
  onChanged(): void;
}

interface Rescan {
  rows: { record: PlacementRecord; resolved: ResolvedRecord | null }[];
}

/**
 * Recovery of your orders without the indexer: your placement records (kept in this browser) are what lets the app find and cancel an order even
 * when the indexer is gone. Export them, import them on another browser, or rescan them against the node.
 */
export function RecoverPanel(props: RecoverPanelProps) {
  const services = useServices();
  const { config, kob } = services;
  const fileInput = useRef<HTMLInputElement>(null);
  const [pending, setPending] = useState<{ parsed: Extract<ParsedImport, { ok: true }>; name: string } | null>(null);
  const [message, setMessage] = useState<{ tone: 'ok' | 'error' | 'warn'; text: string } | null>(null);
  const [rescan, setRescan] = useState<Rescan | null>(null);
  const [busy, setBusy] = useState<'rescan' | 'import' | null>(null);
  const n = props.records.length;

  const exportBackup = () => {
    downloadText(exportFileName('backup', config.network), backupFileText(props.records, config.network));
    showToast(t('orders.recover.exported', { count: n }), 'ok');
  };
  const exportIndexer = () => {
    downloadText(exportFileName('maker-recovery', config.network), recoveryFileText(props.records, config.network));
    showToast(t('orders.recover.exported', { count: n }), 'ok');
  };

  const onFile = async (e: Event) => {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = ''; // picking the same file twice must fire again
    if (!file) return;
    setMessage(null);
    setPending(null);
    try {
      const text = await readFileText(file);
      const parsed = parseImport(text, kob, { network: config.network, maker: props.pubkey });
      if (!parsed.ok) {
        setMessage({ tone: 'error', text: t(`orders.recover.error.${parsed.reason}`) });
        return;
      }
      setPending({ parsed, name: file.name });
    } catch (err) {
      setMessage({ tone: 'error', text: toError(err).message });
    }
  };

  const apply = async () => {
    if (!pending) return;
    setBusy('import');
    try {
      const res = await applyImport(props.store, pending.parsed.result);
      setMessage({ tone: 'ok', text: t('orders.recover.imported', { added: res.added, kept: res.kept }) });
      setPending(null);
      props.onChanged();
    } catch (err) {
      setMessage({ tone: 'error', text: toError(err).message });
    } finally {
      setBusy(null);
    }
  };

  const doRescan = async () => {
    setBusy('rescan');
    setMessage(null);
    try {
      const map = await resolveRecords(services, props.records);
      setRescan({ rows: props.records.map((record) => ({ record, resolved: map.get(record.covenantId) ?? null })) });
    } catch (err) {
      setMessage({ tone: 'error', text: toError(err).message });
    } finally {
      setBusy(null);
    }
  };

  const summary = (r: ResolvedRecord | null): { tone: 'ok' | 'neutral' | 'warn'; key: string } =>
    !r
      ? { tone: 'warn', key: 'failed' }
      : r.status === 'live'
        ? { tone: 'ok', key: 'live' }
        : r.status === 'spent'
          ? { tone: 'neutral', key: 'spent' }
          : r.status === 'old-template'
            ? { tone: 'warn', key: 'oldTemplate' }
            : { tone: 'warn', key: 'unknown' };

  return (
    <Section title={t("orders.recover.title")} data-testid="recover-panel">
      <p>{t('orders.recover.why')}</p>
      <p class="small muted" data-testid="recover-count">{t('orders.recover.count', { count: n })}</p>
      <div class="row">
        <Button onClick={exportBackup} disabled={n === 0} data-testid="orders-export">{t('orders.recover.exportBackup')}</Button>
        <Button onClick={exportIndexer} disabled={n === 0} data-testid="orders-export-indexer">{t('orders.recover.exportIndexer')}</Button>
        <Button onClick={() => fileInput.current?.click()} data-testid="orders-import-button">{t('orders.recover.import')}</Button>
        <input ref={fileInput} type="file" accept=".json,application/json" class="sr-only" tabIndex={-1} aria-label={t('orders.recover.import')} onChange={(e) => void onFile(e)} data-testid="orders-import" />
        <Button onClick={() => void doRescan()} loading={busy === 'rescan'} disabled={n === 0} data-testid="orders-rescan">{t('orders.recover.rescan')}</Button>
      </div>
      <p class="small muted" style="margin-top:8px">{t('orders.recover.formats')}</p>

      {message ? <Banner tone={message.tone} data-testid="recover-message">{message.text}</Banner> : null}

      {pending ? (
        <div class="stack-sm" data-testid="import-preview" style="margin-top:8px">
          <Banner tone={pending.parsed.result.rejected.length ? 'warn' : 'info'} title={t('orders.recover.previewTitle', { name: pending.name })}>
            {t('orders.recover.preview', { kind: t(`orders.recover.kind.${pending.parsed.kind}`), ok: pending.parsed.result.records.length, bad: pending.parsed.result.rejected.length })}
            {pending.parsed.result.rejected.length ? (
              <ul data-testid="import-rejected">
                {pending.parsed.result.rejected.slice(0, 20).map((r, i) => (
                  <li key={i}>{r.index >= 0 ? `#${r.index + 1}: ` : ''}{r.reason}</li>
                ))}
              </ul>
            ) : null}
          </Banner>
          <div class="row">
            <Button variant="primary" disabled={pending.parsed.result.records.length === 0} loading={busy === 'import'} onClick={() => void apply()} data-testid="orders-import-apply">
              {t('orders.recover.apply', { count: pending.parsed.result.records.length })}
            </Button>
            <Button onClick={() => setPending(null)}>{t('common.cancel')}</Button>
          </div>
        </div>
      ) : null}

      {rescan ? (
        <Table dense caption={t('orders.recover.rescanResult')} data-testid="rescan-result">
          <thead>
            <tr>
              <th>{t('orders.orderId')}</th>
              <th>{t('orders.recover.kindCol')}</th>
              <th>{t('common.status')}</th>
            </tr>
          </thead>
          <tbody>
            {rescan.rows.length === 0 ? (
              <TableMessage colSpan={3}>{t('orders.recover.none')}</TableMessage>
            ) : (
              rescan.rows.map(({ record, resolved }) => {
                const s = summary(resolved);
                return (
                  <tr key={record.covenantId} data-testid="rescan-row" data-status={s.key}>
                    <td><CopyText value={record.covenantId} head={8} tail={6} /></td>
                    <td>{record.kind}</td>
                    <td>
                      <Badge tone={s.tone}>{t(`orders.recover.status.${s.key}`)}</Badge>
                      {resolved?.stateChanged ? <span class="small muted"> {t('orders.recover.changed')}</span> : null}
                      {resolved?.status === 'live' && props.onCancel ? (
                        <Button small variant="danger" onClick={() => props.onCancel!(record.covenantId)} data-testid={`rescan-cancel-${record.covenantId}`}>
                          {t('orders.cancel')}
                        </Button>
                      ) : null}
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </Table>
      ) : null}
    </Section>
  );
}
