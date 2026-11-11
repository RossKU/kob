import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { PHONE_TOAST_QUERY, useMedia } from '../shell/narrow';
import { Button } from './Button';
import { toasts, type ToastItem } from './toast-store';

/** Subscribes to the toast store and returns the current list. */
export function useToasts(): readonly ToastItem[] {
  const [list, setList] = useState<readonly ToastItem[]>(toasts.snapshot());
  useEffect(() => toasts.subscribe(() => setList(toasts.snapshot())), []);
  return list;
}

/** A swipe up of at least this many px (mostly vertical) dismisses a phone's toast. */
const SWIPE_UP_PX = 28;

function ToastView({ item, phone }: { item: ToastItem; phone: boolean }) {
  // a finger or the pointer on a phone's toast holds its timer; letting go starts it again
  const [held, setHeld] = useState(false);
  // every merge of an identical message restarts the timer
  useEffect(() => {
    if (item.timeoutMs <= 0 || held) return;
    const id = setTimeout(() => toasts.dismiss(item.id), item.timeoutMs);
    return () => clearTimeout(id);
  }, [item.id, item.count, item.timeoutMs, held]);
  const start = useRef<{ x: number; y: number } | null>(null);
  const phoneHandlers = phone
    ? {
        onPointerEnter: () => setHeld(true),
        onPointerLeave: () => setHeld(false),
        onTouchStart: (e: TouchEvent) => {
          const p = e.touches[0];
          start.current = p ? { x: p.clientX, y: p.clientY } : null;
          setHeld(true);
        },
        onTouchEnd: (e: TouchEvent) => {
          setHeld(false);
          const p = e.changedTouches[0];
          const s = start.current;
          start.current = null;
          if (p && s && s.y - p.clientY >= SWIPE_UP_PX && Math.abs(p.clientX - s.x) < s.y - p.clientY) toasts.dismiss(item.id);
        },
        onTouchCancel: () => {
          start.current = null;
          setHeld(false);
        },
      }
    : {};
  return (
    <div class={`toast toast-${item.tone}`} role={item.tone === 'error' ? 'alert' : 'status'} data-testid={`toast-${item.tone}`} {...phoneHandlers}>
      <div class="grow wrap-anywhere toast-text">
        <span>{item.message}</span>
        {item.count > 1 ? <span class="muted"> {`×${item.count}`}</span> : null}
      </div>
      <Button small variant="ghost" onClick={() => toasts.dismiss(item.id)} aria-label={t('common.dismiss')}>
        {'×'}
      </Button>
    </div>
  );
}

/** How many cards other components put into the host (the send status, a flow: `.send-toast`); watched only while `on`. */
function useHostCards(host: { current: HTMLElement | null }, on: boolean): number {
  const [n, setN] = useState(0);
  useEffect(() => {
    const el = host.current;
    if (!on || !el || typeof MutationObserver !== 'function') return setN(0);
    const count = () => setN(el.querySelectorAll(':scope > .send-toast').length);
    count();
    const mo = new MutationObserver(count);
    mo.observe(el, { childList: true });
    return () => mo.disconnect();
  }, [on]);
  return on ? n : 0;
}

/** Keeps `--toast-top` at the bottom edge of the sticky header, so a phone's toast hangs just under it. */
function useHeaderOffset(host: { current: HTMLElement | null }, on: boolean): void {
  useEffect(() => {
    const el = host.current;
    if (!on || !el) return;
    const header = document.querySelector<HTMLElement>('.header');
    const set = () => el.style.setProperty('--toast-top', `${Math.max(0, Math.round(header?.getBoundingClientRect().bottom ?? 0))}px`);
    set();
    if (!header || typeof ResizeObserver !== 'function') return;
    const ro = new ResizeObserver(set);
    ro.observe(header);
    window.addEventListener('resize', set);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', set);
    };
  }, [on]);
}

/**
 * Mount ONCE (App does). Show messages with `showToast(text, tone)` from toast-store. A desktop: bottom right, stacked. A phone (PHONE_TOAST_QUERY): at the
 * top just under the header, ONE at a time (a card another component put in the host first, else the newest toast) with a "+N" chip for the rest, which opens
 * the bell; only the shown toast runs its timer. styles.css `.toast-host`.
 */
export function ToastHost() {
  const list = useToasts();
  const phone = useMedia(PHONE_TOAST_QUERY);
  const ref = useRef<HTMLDivElement>(null);
  const cards = useHostCards(ref, phone);
  useHeaderOffset(ref, phone);
  // on a phone a send / flow card (it may wait for the user's approval) is the one shown; the toasts wait behind it
  const shown = phone ? (cards > 0 ? [] : list.slice(-1)) : list;
  const waiting = phone ? Math.max(0, cards - 1) + list.length - shown.length : 0;
  return (
    <div class="toast-host" aria-live="polite" data-testid="toast-host" {...(phone ? { 'data-phone': 'true' } : {})} ref={ref}>
      {shown.map((item) => (
        <ToastView key={item.id} item={item} phone={phone} />
      ))}
      {waiting > 0 ? (
        <button
          type="button"
          class="toast-more"
          data-testid="toast-more"
          aria-label={t('notify.toastMoreLabel', { count: waiting })}
          onClick={() => document.querySelector<HTMLElement>('[data-testid="notify-bell"]')?.click()}
        >
          {t('notify.toastMore', { count: waiting })}
        </button>
      ) : null}
    </div>
  );
}
