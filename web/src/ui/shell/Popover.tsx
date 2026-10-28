// A small popover for the header: a button that toggles a panel under it. No dependency; keyboard accessible (the panel follows the button in the tab
// order, Escape closes it and puts the focus back on the button, so does moving the focus out of it), closes on a press outside and on every navigation
// (a hash change). `anchor: 'header'` makes the panel as wide as the header and hang under it (the phone menu); otherwise it hangs under the button.
import type { ComponentChildren } from 'preact';
import { useEffect, useId, useRef, useState } from 'preact/hooks';

export interface PopoverProps {
  /** accessible name of the button and of the panel */
  label: string;
  /** the button's content */
  trigger: ComponentChildren;
  /** extra attributes of the button (data-testid, data-level, title ...) */
  triggerAttrs?: Record<string, string | number | boolean | undefined>;
  triggerClass?: string;
  anchor?: 'self' | 'header';
  /** the panel's align under the button: the end of the row by default */
  align?: 'start' | 'end';
  panelTestId?: string;
  children: ComponentChildren;
}

export function Popover(props: PopoverProps) {
  const [open, setOpen] = useState(false);
  const id = `hp-${useId()}`;
  const wrap = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
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
  return (
    <div
      class={`hp${props.anchor === 'header' ? ' hp-header' : ''}`}
      ref={wrap}
      onFocusOut={(e) => {
        const next = (e as FocusEvent).relatedTarget as Node | null;
        if (next && !wrap.current?.contains(next)) setOpen(false);
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
        <div id={id} class={`hp-panel${props.align === 'start' ? ' hp-start' : ''}`} role="region" aria-label={props.label} data-testid={props.panelTestId}>
          {props.children}
        </div>
      ) : null}
    </div>
  );
}
