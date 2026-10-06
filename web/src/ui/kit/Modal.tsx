import type { ComponentChildren } from 'preact';
import { useEffect, useId, useRef } from 'preact/hooks';
import { t } from '../../i18n';
import { Button } from './Button';

export interface ModalProps {
  title: ComponentChildren;
  onClose: () => void;
  children?: ComponentChildren;
  /** footer buttons */
  footer?: ComponentChildren;
  /** wide layout (confirmation screens) */
  large?: boolean;
  /** false: Escape and a click on the backdrop do not close (a signing flow in progress) */
  dismissable?: boolean;
  /** hides the header close button (use with dismissable=false while busy) */
  hideClose?: boolean;
  'data-testid'?: string;
}

const FOCUSABLE = 'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

/**
 * Modal dialog: `role=dialog`, focus moves into it, Tab is trapped, Escape and backdrop click close it (unless `dismissable=false`),
 * focus returns to the opener on close, and the page behind does not scroll. Render it conditionally: mounted = open.
 */
export function Modal(props: ModalProps) {
  const box = useRef<HTMLDivElement>(null);
  const titleId = `modal-title-${useId()}`;
  const dismissable = props.dismissable ?? true;
  const closeRef = useRef(props.onClose);
  closeRef.current = props.onClose;
  const dismissRef = useRef(dismissable);
  dismissRef.current = dismissable;

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    document.body.classList.add('modal-open');
    const el = box.current;
    (el?.querySelector<HTMLElement>('[data-autofocus]') ?? el?.querySelector<HTMLElement>('.modal-body') ?? el)?.focus?.();
    return () => {
      document.body.classList.remove('modal-open');
      opener?.focus?.();
    };
  }, []);

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Escape' && dismissRef.current) {
      e.stopPropagation();
      closeRef.current();
      return;
    }
    if (e.key !== 'Tab' || !box.current) return;
    const items = [...box.current.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((x) => x.offsetParent !== null);
    if (!items.length) {
      e.preventDefault();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || active === box.current)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div
      class="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && dismissable) props.onClose();
      }}
    >
      <div ref={box} class={`modal${props.large ? ' modal-lg' : ''}`} role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1} onKeyDown={onKeyDown} data-testid={props['data-testid']}>
        <div class="modal-head">
          <h2 id={titleId}>{props.title}</h2>
          {props.hideClose ? null : (
            <Button small variant="ghost" onClick={props.onClose} aria-label={t('common.close')} disabled={!dismissable}>
              {'×'}
            </Button>
          )}
        </div>
        <div class="modal-body" tabIndex={-1}>{props.children}</div>
        {props.footer ? <div class="modal-foot">{props.footer}</div> : null}
      </div>
    </div>
  );
}

/** Alias: a Modal is the app's only dialog primitive. */
export const Dialog = Modal;
