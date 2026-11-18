// A small popover for the header: a button that toggles a panel under it. It has no dependency and
// is keyboard accessible. The panel follows the button in the tab order. Escape closes it and puts
// the focus back on the button, and so does moving the focus out of it. It closes on a press
// outside and on every navigation (a hash change). `anchor: 'header'` makes the panel as wide as
// the header and hang under it (the phone menu). Otherwise it hangs under the button.
import type { ComponentChildren } from 'preact';
import { useId, useLayoutEffect, useRef, useState } from 'preact/hooks';

export interface PopoverProps {
  /** The accessible name of the button and of the panel. */
  label: string;
  /** The button's content. */
  trigger: ComponentChildren;
  /** Extra attributes of the button (data-testid, data-level, title ...). */
  triggerAttrs?: Record<string, string | number | boolean | undefined>;
  triggerClass?: string;
  anchor?: 'self' | 'header';
  /** The panel's align under the button. The default is the end of the row. */
  align?: 'start' | 'end';
  panelTestId?: string;
  /** An extra class of the panel (the bell's list is wider than a menu). */
  panelClass?: string;
  /**
   * Extra attributes of the wrapper that holds the button and the panel (a test id of the whole
   * control, data-* of its state).
   */
  wrapAttrs?: Record<string, string | number | boolean | undefined>;
  /**
   * A tap on a link inside the panel closes it, also when the link leads to the page already open
   * and there is no hash change.
   */
  closeOnLink?: boolean;
  /**
   * A click on an element marked `data-pick` inside the panel closes it and puts the focus back on
   * the button. It is for a choice list that is done after one pick.
   */
  closeOnPick?: boolean;
  /**
   * The panel. A function gets `close`, which closes the panel and puts the focus back on the
   * button (a Close button, a Done).
   */
  children: ComponentChildren | ((close: () => void) => ComponentChildren);
}

export function Popover(props: PopoverProps) {
  const [open, setOpen] = useState(false);
  const id = `hp-${useId()}`;
  const wrap = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  // A layout effect, so the listeners are on as soon as the panel is in the page. A post-paint
  // effect can lag behind a fast Escape.
  useLayoutEffect(() => {
    if (!open) return;
    const onDown = (e: Event) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      setOpen(false);
      button.current?.focus();
    };
    const onHash = () => setOpen(false);
    document.addEventListener('pointerdown', onDown);
    document.addEventListener('keydown', onKey);
    window.addEventListener('hashchange', onHash);
    return () => {
      document.removeEventListener('pointerdown', onDown);
      document.removeEventListener('keydown', onKey);
      window.removeEventListener('hashchange', onHash);
    };
  }, [open]);
  const close = () => {
    setOpen(false);
    button.current?.focus();
  };
  return (
    <div
      class={`hp${props.anchor === 'header' ? ' hp-header' : ''}`}
      ref={wrap}
      {...props.wrapAttrs}
      onFocusOut={(e) => {
        const next = (e as FocusEvent).relatedTarget as Node | null;
        // The clipboard fallback's textarea (kit/CopyText.tsx) holds the focus for an instant. That
        // is not a departure.
        if (next && !wrap.current?.contains(next) && !(next as HTMLElement).hasAttribute?.('data-copy-shim')) setOpen(false);
      }}
    >
      <button
        ref={button}
        type="button"
        class={`btn btn-ghost btn-sm hp-trigger${props.triggerClass ? ` ${props.triggerClass}` : ''}`}
        aria-expanded={open ? 'true' : 'false'}
        aria-controls={id}
        aria-label={props.label}
        onClick={() => setOpen((o) => !o)}
        {...props.triggerAttrs}
      >
        {props.trigger}
      </button>
      {open ? (
        <div
          id={id}
          class={`hp-panel${props.align === 'start' ? ' hp-start' : ''}${props.panelClass ? ` ${props.panelClass}` : ''}`}
          role="region"
          aria-label={props.label}
          data-testid={props.panelTestId}
          onClick={
            props.closeOnLink || props.closeOnPick
              ? (e) => {
                  const el = e.target as HTMLElement;
                  if (props.closeOnLink && el.closest('a[href]')) setOpen(false);
                  else if (props.closeOnPick && el.closest('[data-pick]')) {
                    setOpen(false);
                    button.current?.focus();
                  }
                }
              : undefined
          }
        >
          {typeof props.children === 'function' ? props.children(close) : props.children}
        </div>
      ) : null}
    </div>
  );
}
