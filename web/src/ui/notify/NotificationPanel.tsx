// The notification centre: the bell of the header and the dialog it opens: the current status (what holds now, ui/shell/status-entries.ts), then the
// notifications (newest first, mark all read, clear, the x402 invoice watch box, a link to Settings). The current status does not depend on the notifications:
// it is there with them off or no wallet connected, and Clear does not touch it; its worst tone colours the bell.
// The dialog always lists what the bell counts (TN10 2026-10-17: a badge "5" opened a dialog that only said the notifications were off); whether
// new ones are collected is one line above the list, with its switch.
import { useEffect, useRef, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { useFiat } from '../../app/use-fiat';
import type { ConfigWarning, StoredOverride } from '../../config';
import { formatDateTime, t } from '../../i18n';
import type { NotificationEvent } from '../../kob/notifications';
import { Badge, Button, Field, Modal, shortId } from '../kit';
import { StatusBanner, useStatusEntries } from '../shell/Banners';
import { worstTone, type StatusEntry } from '../shell/status-entries';
import { describeEvent } from './describe';
import { useNotify } from './NotifyProvider';
import './notify.css';

/** The bell is shown even while the notifications are off (the dialog then offers "Turn on"): set false to hide it until the user enables them in Settings, unless there is a current status to read. */
export const SHOW_BELL_WHEN_OFF = true;

function BellIcon() {
  return (
    <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <path d="M6 9a6 6 0 1 1 12 0c0 6 2.5 7.5 2.5 7.5h-17S6 15 6 9z" />
      <path d="M10 20a2 2 0 0 0 4 0" />
    </svg>
  );
}

// A click anywhere on a notification (its link included) marks that one read; "Mark all read" stays for the rest.
function Item(props: { e: NotificationEvent; onNavigate: () => void }) {
  const { registry } = useServices();
  const n = useNotify();
  const d = describeEvent(props.e, registry, useFiat());
  return (
    <li class={`notify-item${props.e.read ? '' : ' notify-unread'}`} data-testid={`notify-item-${props.e.kind}`} data-read={props.e.read ? 'true' : 'false'} onClick={() => n.markRead(props.e.id)}>
      <div class="notify-item-head">
        <strong><span key={d.title}>{d.title}</span></strong>
        <time class="muted" dateTime={new Date(props.e.at).toISOString()}>{formatDateTime(Math.floor(props.e.at / 1000), undefined)}</time>
      </div>
      <div><span key={d.body}>{d.body}</span></div>
      <a href={d.href} onClick={props.onNavigate}>{props.e.kind === 'payment' && props.e.token ? t('notify.viewToken') : props.e.kind === 'invoice' ? t('notify.settings') : t('notify.viewOrders')}</a>
    </li>
  );
}

function InvoiceWatch() {
  const n = useNotify();
  const [text, setText] = useState('');
  const [error, setError] = useState<string | null>(null);
  const add = () => {
    const r = n.addInvoice(text);
    if (r === 'ok') {
      setText('');
      setError(null);
    } else setError(t(r === 'invalid' ? 'notify.invoice.invalid' : 'notify.invoice.duplicate'));
  };
  return (
    <div class="stack-sm notify-invoice">
      <h3>{t('notify.invoice.title')}</h3>
      <Field value={text} onValue={(x) => { setText(x); setError(null); }} placeholder={t('notify.invoice.placeholder')} hint={t('notify.invoice.hint')} error={error} data-testid="notify-invoice-input" />
      <div class="row">
        <Button small variant="primary" disabled={!text.trim()} onClick={add} data-testid="notify-invoice-add">{t('notify.invoice.add')}</Button>
      </div>
      {n.invoices.length ? (
        <ul class="notify-invoices">
          {n.invoices.map((w) => (
            <li key={w.id} class="row" data-testid="notify-invoice-item">
              <span>{t('notify.invoice.item', { id: w.reference || shortId(w.id, 6, 4), status: w.status ? t(`notify.status.${w.status}`) : t('notify.invoice.unknownStatus') })}</span>
              <Button small variant="ghost" onClick={() => n.removeInvoice(w.id)} data-testid="notify-invoice-remove">{t('notify.invoice.remove')}</Button>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function NotificationPanel(props: { status: readonly StatusEntry[]; onClose: () => void }) {
  const n = useNotify();
  const close = props.onClose;
  const body = useRef<HTMLDivElement>(null);
  // an entry that left took the focus with it (its dismiss button, "Use the indexer again"): it stays in the dialog, or Escape and Tab would stop working here
  const count = props.status.length;
  useEffect(() => {
    if (document.activeElement === document.body) body.current?.closest<HTMLElement>('.modal-body')?.focus({ preventScroll: true });
  }, [count]);
  return (
    <Modal title={t('notify.title')} onClose={close} data-testid="notify-panel">
      <div class="stack" ref={body}>
        {count ? (
          <section class="stack-sm notify-status" data-testid="notify-status">
            <h3>{t('notify.current.title')}</h3>
            {props.status.map((e) => <StatusBanner key={e.key} entry={e} onNavigate={close} />)}
          </section>
        ) : null}
        {!n.settings.enabled ? (
          // off: nothing new is collected; what was collected before is still listed below
          <div class="row notify-off" data-testid="notify-disabled">
            <span class="small muted">{t('notify.offLine')}</span>
            <Button small variant="primary" onClick={() => n.update({ enabled: true })} data-testid="notify-enable">{t('notify.turnOn')}</Button>
          </div>
        ) : null}
        {!n.connected ? (
          <p data-testid="notify-connect">{t('notify.connect')}</p>
        ) : (
          <>
            {n.items.length ? (
              <div class="row">
                <Button small onClick={n.markAllRead} disabled={n.unread === 0} data-testid="notify-mark-read">{t('notify.markRead')}</Button>
                <Button small variant="ghost" onClick={n.clear} data-testid="notify-clear">{t('notify.clear')}</Button>
              </div>
            ) : null}
            {n.items.length === 0 ? (
              <p class="muted" data-testid="notify-empty">{t('notify.empty')}</p>
            ) : (
              <ul class="notify-list" data-testid="notify-list">
                {n.items.map((e) => <Item key={e.id} e={e} onNavigate={close} />)}
              </ul>
            )}
            {n.settings.enabled ? <InvoiceWatch /> : null}
          </>
        )}
        <a href="#/settings" onClick={close} data-testid="notify-settings-link">{t('notify.settings')}</a>
      </div>
    </Modal>
  );
}

/** Header bell with the unread badge, coloured by the worst of the current status; opens the panel. */
export function NotifyBell(props: { warnings: ConfigWarning[]; storedOverrides: StoredOverride[] }) {
  const n = useNotify();
  const status = useStatusEntries(props.warnings, props.storedOverrides);
  const [open, setOpen] = useState(false);
  if (!SHOW_BELL_WHEN_OFF && !n.settings.enabled && !status.length) return null;
  const current = status.length ? t('notify.current.count', { count: status.length }) : '';
  const notifications = n.unread > 0 ? t('notify.bell.unread', { count: n.unread }) : t('notify.bell');
  const label = current ? `${notifications}. ${current}` : notifications;
  return (
    <>
      {/* one item of the header's tools: the row's measured width (narrow.ts) sums them, so the live region must not count as one */}
      <span class="notify-bell-item">
        <Button
          small
          variant="ghost"
          class="notify-bell"
          data-testid="notify-bell"
          data-status-tone={worstTone(status) ?? 'none'}
          data-status-count={status.length}
          aria-label={label}
          title={label}
          onClick={() => setOpen(true)}
        >
          <BellIcon />
          {n.unread > 0 ? <Badge tone="info" data-testid="notify-unread">{n.unread > 99 ? '99+' : n.unread}</Badge> : null}
        </Button>
        {/* a banner on the page was announced when it appeared: the count is, now that the entries are behind the bell */}
        <span class="sr-only" role="status" data-testid="notify-status-live">{current}</span>
      </span>
      {open ? <NotificationPanel status={status} onClose={() => setOpen(false)} /> : null}
    </>
  );
}
