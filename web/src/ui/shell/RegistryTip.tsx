import type { ComponentChildren } from 'preact';
import { useId, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import type { RegistryIdentity } from '../../kob/registry-source';

/** The registry is loaded but is not the build-pinned default (a failed load has its own banner). */
export const isCustomRegistry = (r: RegistryIdentity | null | undefined): r is RegistryIdentity => !!r && !r.isDefault && !r.failed;

/** The full "non-default token registry" explanation: source, sha256, pinned default, "check who published this list". */
export function registryTipText(r: RegistryIdentity): string {
  return t('shell.banner.customRegistry', { source: t(`shell.registry.source.${r.source}`), url: r.url, hash: r.sha256 ?? '-', pin: r.defaultSha256 ? r.defaultSha256.slice(0, 8) : t('common.none') });
}

const MARGIN = 8;
const MAX_WIDTH = 384;

/**
 * Trigger + popover. The popover opens on hover, keyboard focus and tap/click (a click pins it open, a second click, Escape, blur or a tap
 * outside closes it). The trigger is a real button that points at the popover with `aria-describedby`; the popover stays in the DOM
 * (hidden) so assistive technology can always resolve the description. It is positioned under the trigger and clamped into the viewport.
 */
export function RegistryTip(props: { children: ComponentChildren; label: string; class?: string; 'data-testid'?: string; 'data-tip-testid'?: string }) {
  const { registryIdentity: r } = useServices();
  const id = useId();
  const [open, setOpen] = useState(false);
  const [pinned, setPinned] = useState(false);
  const root = useRef<HTMLSpanElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const tip = useRef<HTMLDivElement>(null);

  const close = () => {
    setOpen(false);
    setPinned(false);
  };

  useLayoutEffect(() => {
    const b = trigger.current;
    const p = tip.current;
    if (!open || !b || !p) return;
    const vw = document.documentElement.clientWidth;
    const width = Math.min(MAX_WIDTH, vw - 2 * MARGIN);
    const rect = b.getBoundingClientRect();
    p.style.width = `${width}px`;
    p.style.top = `${rect.bottom}px`;
    p.style.left = `${Math.max(MARGIN, Math.min(rect.left, vw - width - MARGIN))}px`;
  }, [open]);

  // A layout effect: the listeners are in place when the popover is shown (a passive effect runs only after the next paint, and an
  // Escape pressed right after the popover appeared was lost).
  useLayoutEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && close();
    const onOutside = (e: Event) => !root.current?.contains(e.target as Node) && close();
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onOutside);
    window.addEventListener('scroll', close, { passive: true });
    window.addEventListener('resize', close);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onOutside);
      window.removeEventListener('scroll', close);
      window.removeEventListener('resize', close);
    };
  }, [open]);

  if (!isCustomRegistry(r)) return null;
  return (
    <span class="reg-tip" ref={root} onMouseEnter={() => setOpen(true)} onMouseLeave={() => !pinned && setOpen(false)}>
      <button
        type="button"
        ref={trigger}
        class={`reg-tip-trigger${props.class ? ` ${props.class}` : ''}`}
        data-testid={props['data-testid']}
        aria-label={props.label}
        aria-describedby={id}
        aria-expanded={open}
        onFocus={() => setOpen(true)}
        onBlur={close}
        onClick={() => (pinned ? close() : (setOpen(true), setPinned(true)))}
      >
        {props.children}
      </button>
      <div ref={tip} id={id} role="tooltip" class="reg-tip-pop wrap-anywhere" hidden={!open} data-testid={props['data-tip-testid'] ?? 'registry-tip'}>
        <strong class="reg-tip-title">{t('shell.banner.customRegistryTitle')}</strong>
        <span>{registryTipText(r)}</span>
      </div>
    </span>
  );
}
