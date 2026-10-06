// The notification centre: the bell of the header and the dialog it opens (newest first, mark all read, clear, the x402 invoice watch box, a link to Settings).
import { useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { formatDateTime, t } from '../../i18n';
import type { NotificationEvent } from '../../kob/notifications';
import { Badge, Button, Field, Modal, shortId } from '../kit';
import { describeEvent } from './describe';
import { useNotify } from './NotifyProvider';
import './notify.css';

/** The bell is shown even while the notifications are off (the dialog then offers "Turn on"): set false to hide it until the user enables them in Settings. */
export const SHOW_BELL_WHEN_OFF = true;

function BellIcon() {
  return (
    <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <path d="M6 9a6 6 0 1 1 12 0c0 6 2.5 7.5 2.5 7.5h-17S6 15 6 9z" />
      <path d="M10 20a2 2 0 0 0 4 0" />
    </svg>
  );
}

function Item(props: { e: NotificationEvent; onNavigate: () => void }) {
  const { registry } = useServices();
  const d = describeEvent(props.e, registry);
  return (
    <li class={`notify-item${props.e.read ? '' : ' notify-unread'}`} data-testid={`notify-item-${props.e.kind}`} data-read={props.e.read ? 'true' : 'false'}>
      <div class="notify-item-head">
        <strong>{d.title}</strong>
        <time class="muted" dateTime={new Date(props.e.at).toISOString()}>{formatDateTime(Math.floor(props.e.at / 1000), undefined)}</time>
      </div>
      <div>{d.body}</div>
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

export function NotificationPanel(props: { onClose: () => void }) {
  const n = useNotify();
  const close = props.onClose;
  return (
    <Modal title={t('notify.title')} onClose={close} data-testid="notify-panel">
      <div class="stack">
        {!n.settings.enabled ? (
          <div class="stack-sm" data-testid="notify-disabled">
            <p>{t('notify.disabled')}</p>
            <div class="row">
              <Button variant="primary" onClick={() => n.update({ enabled: true })} data-testid="notify-enable">{t('notify.turnOn')}</Button>
            </div>
          </div>
        ) : !n.connected ? (
          <p data-testid="notify-connect">{t('notify.connect')}</p>
        ) : (
          <>
            <div class="row">
              <Button small onClick={n.markAllRead} disabled={n.unread === 0} data-testid="notify-mark-read">{t('notify.markRead')}</Button>
              <Button small variant="ghost" onClick={n.clear} disabled={n.items.length === 0} data-testid="notify-clear">{t('notify.clear')}</Button>
            </div>
            {n.items.length === 0 ? (
              <p class="muted" data-testid="notify-empty">{t('notify.empty')}</p>
            ) : (
              <ul class="notify-list">
                {n.items.map((e) => <Item key={e.id} e={e} onNavigate={close} />)}
              </ul>
            )}
            <InvoiceWatch />
          </>
        )}
        <a href="#/settings" onClick={close} data-testid="notify-settings-link">{t('notify.settings')}</a>
      </div>
    </Modal>
  );
}

/** Header bell with the unread badge; opens the panel. */
export function NotifyBell() {
  const n = useNotify();
  const [open, setOpen] = useState(false);
  if (!SHOW_BELL_WHEN_OFF && !n.settings.enabled) return null;
  const label = n.unread > 0 ? t('notify.bell.unread', { count: n.unread }) : t('notify.bell');
  return (
    <>
      <Button small variant="ghost" class="notify-bell" data-testid="notify-bell" aria-label={label} title={label} onClick={() => setOpen(true)}>
        <BellIcon />
        {n.unread > 0 ? <Badge tone="info" data-testid="notify-unread">{n.unread > 99 ? '99+' : n.unread}</Badge> : null}
      </Button>
      {open ? <NotificationPanel onClose={() => setOpen(false)} /> : null}
    </>
  );
}
