import { useEffect, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { Button } from './Button';
import { toasts, type ToastItem } from './toast-store';

/** Subscribes to the toast store and returns the current list. */
export function useToasts(): readonly ToastItem[] {
  const [list, setList] = useState<readonly ToastItem[]>(toasts.snapshot());
  useEffect(() => toasts.subscribe(() => setList(toasts.snapshot())), []);
  return list;
}

function ToastView({ item }: { item: ToastItem }) {
  // every merge of an identical message restarts the timer
  useEffect(() => {
    if (item.timeoutMs <= 0) return;
    const id = setTimeout(() => toasts.dismiss(item.id), item.timeoutMs);
    return () => clearTimeout(id);
  }, [item.id, item.count, item.timeoutMs]);
  return (
    <div class={`toast toast-${item.tone}`} role={item.tone === 'error' ? 'alert' : 'status'} data-testid={`toast-${item.tone}`}>
      <div class="grow wrap-anywhere">
        {item.message}
        {item.count > 1 ? <span class="muted"> {`×${item.count}`}</span> : null}
      </div>
      <Button small variant="ghost" onClick={() => toasts.dismiss(item.id)} aria-label={t('common.dismiss')}>
        {'×'}
      </Button>
    </div>
  );
}

/** Mount ONCE (App does). Show messages with `showToast(text, tone)` from toast-store. */
export function ToastHost() {
  const list = useToasts();
  return (
    <div class="toast-host" aria-live="polite" data-testid="toast-host">
      {list.map((item) => (
        <ToastView key={item.id} item={item} />
      ))}
    </div>
  );
}
